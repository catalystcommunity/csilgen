//! Custom CSIL Generator Template
//!
//! This template provides a starting point for implementing custom CSIL generators
//! as WASM modules. It includes all the required interface functions and demonstrates
//! how to process CSIL specifications with services and field metadata.
//!
//! ## Key Features
//! - Full WASM interface compliance
//! - Service definition processing
//! - Field metadata handling  
//! - Memory management
//! - Error reporting
//! - Warning generation
//! - Statistics tracking
//!
//! ## Customization Points
//! 1. Update the `GENERATOR_METADATA` constant with your generator's information
//! 2. Implement your code generation logic in `generate_code()`
//! 3. Add any custom configuration options to `process_config()`
//! 4. Extend the warning system in `validate_and_warn()`

pub mod helpers;

use csilgen_common::{
    wasm_interface::*, CsilFieldMetadata, CsilFieldVisibility, CsilRuleType, GeneratedFile,
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, SourceLocation,
    WarningLevel, WasmGeneratorInput, WasmGeneratorOutput,
};
use std::collections::HashMap;

// Re-export helpers for easy access
pub use helpers::*;

// Note: Generator metadata is created in get_metadata() function

/// Get generator metadata (WASM export)
///
/// This function returns metadata about your generator that the CSIL CLI
/// uses for discovery, compatibility checking, and user information.
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        // TODO: Customize these values for your generator
        name: "my-custom-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "My custom CSIL code generator".to_string(),
        target: "my-target".to_string(), // e.g., "go", "java", "c++", etc.
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            // Add more capabilities as you implement them
        ],
        author: Some("Your Name".to_string()),
        homepage: Some("https://github.com/your-org/your-generator".to_string()),
    };

    serialize_and_return_ptr(&metadata)
}

/// Memory allocation (WASM export)
///
/// WASM modules must provide their own memory allocation for the host to use.
/// This function allocates memory that the host can write input data into.
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf); // Prevent deallocation
    ptr
}

/// Memory deallocation (WASM export)
///
/// This function deallocates memory that was previously allocated by `allocate()`.
/// The host calls this to clean up after processing is complete.
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Main generator function (WASM export)
///
/// This is the entry point that the CSIL CLI calls to generate code.
/// It receives serialized `WasmGeneratorInput` and returns serialized `WasmGeneratorOutput`.
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let start_time = std::time::Instant::now();

    let result = match deserialize_input(input_ptr, input_len) {
        Ok(input) => process_generation_request(input, start_time),
        Err(error_code) => {
            return create_error_result(error_code);
        }
    };

    match result {
        Ok(output) => serialize_and_return_ptr(&output),
        Err(_error_code) => std::ptr::null_mut(),
    }
}

/// Deserialize input from WASM memory
fn deserialize_input(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorInput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = std::str::from_utf8(input_slice).map_err(|_| error_codes::INVALID_INPUT)?;

    serde_json::from_str::<WasmGeneratorInput>(input_str)
        .map_err(|_| error_codes::SERIALIZATION_ERROR)
}

/// Process the generation request
fn process_generation_request(
    input: WasmGeneratorInput,
    start_time: std::time::Instant,
) -> Result<WasmGeneratorOutput, i32> {
    // Process configuration options
    let config = process_config(&input.config.options);

    // Validate input and generate warnings
    let warnings = validate_and_warn(&input);

    // Generate the actual code
    let files = generate_code(&input, &config)?;

    // Calculate statistics
    let generation_time = start_time.elapsed().as_millis() as u64;
    let total_size: usize = files.iter().map(|f| f.content.len()).sum();

    let stats = GenerationStats {
        files_generated: files.len(),
        total_size_bytes: total_size,
        services_count: input.csil_spec.service_count,
        fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
        generation_time_ms: generation_time,
        peak_memory_bytes: Some(estimate_peak_memory_usage()),
    };

    Ok(WasmGeneratorOutput {
        files,
        warnings,
        stats,
    })
}

/// Configuration processing
///
/// Process generator-specific configuration options from the input.
/// Add your own configuration keys here.
#[derive(Debug, Default)]
struct GeneratorConfig {
    // TODO: Add your configuration options here
    // Examples:
    // pub use_tabs: bool,
    // pub max_line_length: usize,
    // pub generate_docs: bool,
}

fn process_config(_options: &HashMap<String, serde_json::Value>) -> GeneratorConfig {
    // TODO: Process your configuration options
    // Example:
    // if let Some(serde_json::Value::Bool(use_tabs)) = options.get("use_tabs") {
    //     config.use_tabs = *use_tabs;
    // }

    GeneratorConfig::default()
}

/// Input validation and warning generation
///
/// Analyze the input CSIL specification and generate warnings for potential issues.
/// This is where you can add domain-specific validation for your target language.
fn validate_and_warn(input: &WasmGeneratorInput) -> Vec<GeneratorWarning> {
    let mut warnings = Vec::new();

    // Check for empty specification
    if input.csil_spec.service_count == 0 {
        warnings.push(GeneratorWarning {
            level: WarningLevel::Info,
            message: "No services found in CSIL specification".to_string(),
            location: None,
            suggestion: Some(
                "Consider adding service definitions for complete API generation".to_string(),
            ),
        });
    }

    if input.csil_spec.fields_with_metadata_count == 0 {
        warnings.push(GeneratorWarning {
            level: WarningLevel::Warning,
            message: "No fields with metadata annotations found".to_string(),
            location: None,
            suggestion: Some(
                "Add @send-only, @receive-only, or other metadata to improve generated code"
                    .to_string(),
            ),
        });
    }

    // TODO: Add your domain-specific warnings
    // Examples:
    // - Check for naming conflicts with target language keywords
    // - Validate that required metadata is present for your generator
    // - Warn about unsupported CSIL features
    // - Check for performance implications (e.g., deeply nested structures)

    // Iterate through all rules to find specific issues
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::ServiceDef(service) => {
                // TODO: Add service-specific validation
                // Example: Check that all operations have valid names for your target
                for operation in &service.operations {
                    if operation.name.contains('-') {
                        warnings.push(GeneratorWarning {
                            level: WarningLevel::Warning,
                            message: format!("Operation name '{}' contains hyphens which may not be valid in target language", operation.name),
                            location: Some(SourceLocation {
                                line: operation.position.line,
                                column: operation.position.column,
                                offset: operation.position.offset,
                                context: Some(format!("{}.{}", rule.name, operation.name)),
                            }),
                            suggestion: Some("Consider using camelCase or snake_case naming".to_string()),
                        });
                    }
                }
            }
            CsilRuleType::GroupDef(group) => {
                // Check for fields without proper metadata
                for entry in &group.entries {
                    if entry.metadata.is_empty() {
                        if let Some(key) = &entry.key {
                            warnings.push(GeneratorWarning {
                                level: WarningLevel::Info,
                                message: "Field has no metadata annotations".to_string(),
                                location: Some(SourceLocation {
                                    line: rule.position.line,
                                    column: rule.position.column,
                                    offset: rule.position.offset,
                                    context: Some(format!("{}.{:?}", rule.name, key)),
                                }),
                                suggestion: Some(
                                    "Consider adding visibility or validation metadata".to_string(),
                                ),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    warnings
}

/// Main code generation logic
///
/// This is where you implement your actual code generation.
/// The function receives the parsed CSIL specification and should return
/// a vector of generated files with their paths and contents.
fn generate_code(
    input: &WasmGeneratorInput,
    _config: &GeneratorConfig,
) -> Result<Vec<GeneratedFile>, i32> {
    let mut files = Vec::new();

    // TODO: Implement your code generation logic here

    // Example: Generate a simple summary file
    let mut summary_content = String::new();
    summary_content.push_str(&format!(
        "// Generated by {} v{}\n",
        input.generator_metadata.name, input.generator_metadata.version
    ));
    summary_content.push_str(&format!("// Target: {}\n", input.generator_metadata.target));
    summary_content.push_str(&format!("// Services: {}\n", input.csil_spec.service_count));
    summary_content.push_str(&format!(
        "// Fields with metadata: {}\n\n",
        input.csil_spec.fields_with_metadata_count
    ));

    // Iterate through CSIL rules and generate code for each
    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::TypeDef(type_expr) => {
                // TODO: Generate code for type definitions
                summary_content.push_str(&format!("// Type: {} = {:?}\n", rule.name, type_expr));
            }
            CsilRuleType::GroupDef(group) => {
                // TODO: Generate code for group definitions (structs, classes, etc.)
                summary_content.push_str(&format!(
                    "// Group: {} with {} fields\n",
                    rule.name,
                    group.entries.len()
                ));

                for entry in &group.entries {
                    if let Some(key) = &entry.key {
                        let metadata_info = if entry.metadata.is_empty() {
                            "no metadata".to_string()
                        } else {
                            format!("{} metadata items", entry.metadata.len())
                        };
                        summary_content.push_str(&format!(
                            "//   Field: {:?} ({:?}) - {}\n",
                            key, entry.value_type, metadata_info
                        ));

                        // Example: Handle field visibility
                        for metadata in &entry.metadata {
                            if let CsilFieldMetadata::Visibility(visibility) = metadata {
                                match visibility {
                                    CsilFieldVisibility::SendOnly => {
                                        summary_content.push_str("//     @send-only\n")
                                    }
                                    CsilFieldVisibility::ReceiveOnly => {
                                        summary_content.push_str("//     @receive-only\n")
                                    }
                                    CsilFieldVisibility::Bidirectional => {
                                        summary_content.push_str("//     @bidirectional\n")
                                    }
                                }
                            }
                        }
                    }
                }
            }
            CsilRuleType::ServiceDef(service) => {
                // TODO: Generate code for service definitions (interfaces, traits, etc.)
                summary_content.push_str(&format!(
                    "// Service: {} with {} operations\n",
                    rule.name,
                    service.operations.len()
                ));

                for operation in &service.operations {
                    summary_content.push_str(&format!(
                        "//   Operation: {} ({:?} -> {:?})\n",
                        operation.name, operation.input_type, operation.output_type
                    ));
                }
            }
            CsilRuleType::TypeChoice(choices) => {
                // TODO: Generate code for type choices (unions, enums, etc.)
                summary_content.push_str(&format!(
                    "// Type choice: {} with {} options\n",
                    rule.name,
                    choices.len()
                ));
            }
            CsilRuleType::GroupChoice(choices) => {
                // TODO: Generate code for group choices
                summary_content.push_str(&format!(
                    "// Group choice: {} with {} options\n",
                    rule.name,
                    choices.len()
                ));
            }
        }
    }

    // Example: Create a main output file
    files.push(GeneratedFile {
        path: "generated.txt".to_string(), // TODO: Use appropriate file extension for your target
        content: summary_content,
    });

    // TODO: Generate additional files as needed
    // Examples:
    // - Separate files for types, services, utilities
    // - Configuration or build files
    // - Documentation files
    // - Test files

    Ok(files)
}

/// Estimate peak memory usage for statistics
fn estimate_peak_memory_usage() -> usize {
    // TODO: Implement actual memory usage tracking if needed
    // This is a placeholder implementation
    1024 // 1KB default estimate
}

/// Serialize data and return pointer for WASM boundary
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
        // Write length first (little-endian u32)
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);

        // Write the JSON data
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Create error result pointer
fn create_error_result(error_code: i32) -> *mut u8 {
    let error_output = WasmGeneratorOutput {
        files: vec![],
        warnings: vec![GeneratorWarning {
            level: WarningLevel::Warning,
            message: format!("Generator failed with error code: {error_code}"),
            location: None,
            suggestion: None,
        }],
        stats: GenerationStats::default(),
    };

    serialize_and_return_ptr(&error_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
        CsilPosition, CsilRule, CsilRuleType, CsilSpecSerialized, CsilTypeExpression,
        GeneratorCapability, GeneratorMetadata, WasmGeneratorInput,
    };
    use std::collections::HashMap;

    fn create_test_input() -> WasmGeneratorInput {
        let metadata = GeneratorMetadata {
            name: "test-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "test".to_string(),
            capabilities: vec![
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: None,
            homepage: None,
        };

        let config = csilgen_common::GeneratorConfig {
            target: "test".to_string(),
            output_dir: "/tmp/test".to_string(),
            options: HashMap::new(),
        };

        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::Bidirectional,
                        )],
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: Some("Test CSIL".to_string()),
            service_count: 0,
            fields_with_metadata_count: 1,
        };

        WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        }
    }

    #[test]
    fn test_generate_code() {
        let input = create_test_input();
        let config = super::GeneratorConfig::default();

        let result = generate_code(&input, &config);
        assert!(result.is_ok());

        let files = result.unwrap();
        assert!(!files.is_empty());
        assert_eq!(files[0].path, "generated.txt");
        assert!(files[0].content.contains("Generated by"));
    }

    #[test]
    fn test_validate_and_warn() {
        let input = create_test_input();
        let warnings = validate_and_warn(&input);

        // Should warn about no services
        assert!(warnings
            .iter()
            .any(|w| w.message.contains("No services found")));
    }

    #[test]
    fn test_process_config() {
        let mut options = HashMap::new();
        options.insert("test_option".to_string(), serde_json::Value::Bool(true));

        let _config = process_config(&options);
        // Add assertions for your specific configuration options
    }

    #[test]
    fn test_memory_functions() {
        let size = 100;
        let ptr = allocate(size);
        assert!(!ptr.is_null());

        deallocate(ptr, size);
        // Test passes if no crash occurs
    }

    #[test]
    fn test_serialize_and_return_ptr() {
        let test_data = GeneratorMetadata {
            name: "test".to_string(),
            version: "1.0.0".to_string(),
            description: "test".to_string(),
            target: "test".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        };

        let ptr = serialize_and_return_ptr(&test_data);
        assert!(!ptr.is_null());

        // Read back the length
        unsafe {
            let len = std::ptr::read(ptr as *const u32);
            assert!(len > 0);
            deallocate(ptr, len as usize + 4);
        }
    }
}
