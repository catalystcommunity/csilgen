//! Simple Documentation Generator for CSIL
//!
//! This example shows how to create a minimal but useful CSIL generator
//! that produces human-readable documentation in Markdown format.
//! Perfect for getting started or generating API documentation.

use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupKey, CsilRuleType, CsilServiceDirection,
    CsilTypeExpression, GeneratedFile, GenerationStats, GeneratorCapability, GeneratorMetadata,
    WasmGeneratorInput, WasmGeneratorOutput, WarningLevel, GeneratorWarning,
    wasm_interface::*,
};
use std::collections::HashMap;

#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "docs-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Generate Markdown documentation from CSIL specifications".to_string(),
        target: "markdown".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/docs-generator".to_string()),
    };

    serialize_and_return_ptr(&metadata)
}

#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let start_time = std::time::Instant::now();
    
    let result = match deserialize_input(input_ptr, input_len) {
        Ok(input) => process_generation(input, start_time),
        Err(error_code) => return create_error_result(error_code),
    };

    match result {
        Ok(output) => serialize_and_return_ptr(&output),
        Err(_) => std::ptr::null_mut(),
    }
}

fn deserialize_input(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorInput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice)
        .map_err(|_| error_codes::INVALID_INPUT)?;

    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

fn process_generation(input: WasmGeneratorInput, start_time: std::time::Instant) -> Result<WasmGeneratorOutput, i32> {
    let config = DocsConfig::from_options(&input.config.options);
    let mut content = String::new();
    let mut warnings = Vec::new();

    // Generate documentation header
    generate_header(&mut content, &config);
    
    // Generate table of contents
    generate_toc(&mut content, &input);
    
    // Generate data types section
    generate_types_documentation(&mut content, &input, &mut warnings);
    
    // Generate services section  
    if input.csil_spec.service_count > 0 {
        generate_services_documentation(&mut content, &input, &mut warnings);
    }
    
    // Generate footer
    generate_footer(&mut content, &config);

    let generation_time = start_time.elapsed().as_millis() as u64;
    
    let files = vec![GeneratedFile {
        path: "API.md".to_string(),
        content,
    }];
    
    let total_size = files[0].content.len();

    let stats = GenerationStats {
        files_generated: 1,
        total_size_bytes: total_size,
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: generation_time,
        peak_memory_bytes: Some(2048),
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings,
        stats,
    })
}

#[derive(Debug)]
struct DocsConfig {
    title: String,
    include_metadata: bool,
    include_examples: bool,
}

impl DocsConfig {
    fn from_options(options: &HashMap<String, serde_json::Value>) -> Self {
        Self {
            title: options.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("API Documentation")
                .to_string(),
            include_metadata: options.get("include_metadata")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            include_examples: options.get("include_examples")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        }
    }
}

fn generate_header(content: &mut String, config: &DocsConfig) {
    content.push_str(&format!("# {}\n\n", config.title));
    content.push_str("This documentation is automatically generated from CSIL specifications.\n\n");
    
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    
    content.push_str(&format!("*Generated at: {}*\n\n", timestamp));
    content.push_str("---\n\n");
}

fn generate_toc(content: &mut String, input: &WasmGeneratorInput) {
    content.push_str("## Table of Contents\n\n");
    
    // Count different rule types
    let mut type_count = 0;
    let mut service_count = 0;
    
    for rule in &input.csil_spec.rules {
        match rule.rule_type {
            CsilRuleType::GroupDef(_) | CsilRuleType::TypeDef(_) => type_count += 1,
            CsilRuleType::ServiceDef(_) => service_count += 1,
            _ => {}
        }
    }
    
    if type_count > 0 {
        content.push_str("- [Data Types](#data-types)\n");
    }
    if service_count > 0 {
        content.push_str("- [Services](#services)\n");
    }
    
    content.push_str("\n");
}

fn generate_types_documentation(content: &mut String, input: &WasmGeneratorInput, warnings: &mut Vec<GeneratorWarning>) {
    let mut has_types = false;
    
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::GroupDef(group) => {
                if !has_types {
                    content.push_str("## Data Types\n\n");
                    has_types = true;
                }
                
                content.push_str(&format!("### {}\n\n", rule.name));
                
                if group.entries.is_empty() {
                    content.push_str("*Empty structure*\n\n");
                } else {
                    content.push_str("| Field | Type | Required | Description | Visibility |\n");
                    content.push_str("|-------|------|----------|-------------|------------|\n");
                    
                    for entry in &group.entries {
                        if let Some(key) = &entry.key {
                            let field_name = match key {
                                CsilGroupKey::Bare(name) => name.clone(),
                                _ => "field".to_string(),
                            };
                            
                            let field_type = format_type_for_docs(&entry.value_type);
                            let required = match &entry.occurrence {
                                Some(_) => "No", // Optional or constrained
                                None => "Yes",
                            };
                            
                            let description = get_field_description(&entry.metadata)
                                .unwrap_or("*No description*");
                            
                            let visibility = match get_field_visibility(&entry.metadata) {
                                CsilFieldVisibility::SendOnly => "Send only",
                                CsilFieldVisibility::ReceiveOnly => "Receive only", 
                                CsilFieldVisibility::Bidirectional => "Bidirectional",
                            };
                            
                            content.push_str(&format!("| `{}` | `{}` | {} | {} | {} |\n",
                                field_name, field_type, required, description, visibility));
                            
                            // Check for missing descriptions
                            if description == "*No description*" {
                                warnings.push(GeneratorWarning {
                                    level: WarningLevel::Info,
                                    message: format!("Field '{}' in type '{}' has no description", field_name, rule.name),
                                    location: None,
                                    suggestion: Some("Add @description annotation for better documentation".to_string()),
                                });
                            }
                        }
                    }
                    
                    content.push_str("\n");
                }
            }
            CsilRuleType::TypeDef(type_expr) => {
                if !has_types {
                    content.push_str("## Data Types\n\n");
                    has_types = true;
                }
                
                content.push_str(&format!("### {}\n\n", rule.name));
                content.push_str(&format!("Type alias for: `{}`\n\n", format_type_for_docs(type_expr)));
            }
            _ => {}
        }
    }
}

fn generate_services_documentation(content: &mut String, input: &WasmGeneratorInput, _warnings: &mut Vec<GeneratorWarning>) {
    content.push_str("## Services\n\n");
    
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
            content.push_str(&format!("### {}\n\n", rule.name));
            
            if service.operations.is_empty() {
                content.push_str("*No operations defined*\n\n");
            } else {
                content.push_str("| Operation | Input | Output | Direction | Description |\n");
                content.push_str("|-----------|-------|--------|-----------|-------------|\n");
                
                for operation in &service.operations {
                    let input_type = format_type_for_docs(&operation.input_type);
                    let output_type = format_type_for_docs(&operation.output_type);
                    
                    let direction = match operation.direction {
                        CsilServiceDirection::Unidirectional => "Request → Response",
                        CsilServiceDirection::Bidirectional => "Streaming ↔",
                        CsilServiceDirection::Reverse => "Event ←",
                    };
                    
                    content.push_str(&format!("| `{}` | `{}` | `{}` | {} | *No description* |\n",
                        operation.name, input_type, output_type, direction));
                }
                
                content.push_str("\n");
            }
        }
    }
}

fn generate_footer(content: &mut String, _config: &DocsConfig) {
    content.push_str("---\n\n");
    content.push_str("*This documentation was generated automatically from CSIL specifications using the docs-generator.*\n");
}

fn format_type_for_docs(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Builtin(name) => name.clone(),
        CsilTypeExpression::Reference(name) => name.clone(),
        CsilTypeExpression::Array { element_type, .. } => {
            format!("[{}]", format_type_for_docs(element_type))
        }
        CsilTypeExpression::Map { key, value, .. } => {
            format!("map<{}, {}>", format_type_for_docs(key), format_type_for_docs(value))
        }
        CsilTypeExpression::Group(_) => "inline group".to_string(),
        CsilTypeExpression::Choice(choices) => {
            format!("choice<{} options>", choices.len())
        }
        _ => "complex type".to_string(),
    }
}

fn get_field_visibility(metadata: &[CsilFieldMetadata]) -> CsilFieldVisibility {
    for meta in metadata {
        if let CsilFieldMetadata::Visibility(vis) = meta {
            return vis.clone();
        }
    }
    CsilFieldVisibility::Bidirectional
}

fn get_field_description(metadata: &[CsilFieldMetadata]) -> Option<&str> {
    metadata.iter().find_map(|meta| {
        if let CsilFieldMetadata::Description(desc) = meta {
            Some(desc.as_str())
        } else {
            None
        }
    })
}

fn serialize_and_return_ptr<T: serde::Serialize>(data: &T) -> *mut u8 {
    let serialized = match serde_json::to_string(data) {
        Ok(json) => json,
        Err(_) => return std::ptr::null_mut(),
    };

    let bytes = serialized.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

fn create_error_result(error_code: i32) -> *mut u8 {
    let error_output = WasmGeneratorOutput {
        files: vec![],
        warnings: vec![GeneratorWarning {
            level: WarningLevel::Warning,
            message: format!("Generator failed with error code: {}", error_code),
            location: None,
            suggestion: None,
        }],
        stats: GenerationStats::default(),
    };
    
    serialize_and_return_ptr(&error_output)
}