//! TypeScript code generator for CSIL specifications (WASM module)
//!
//! This generator produces TypeScript interfaces, types, and service definitions
//! with proper type safety and metadata-aware field handling.

use csilgen_common::{
    CsilFieldMetadata, CsilFieldVisibility, CsilGroupExpression, CsilGroupKey, CsilLiteralValue,
    CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilTypeExpression, GeneratedFile,
    GenerationStats, GeneratorCapability, GeneratorMetadata, GeneratorWarning, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
};
use std::collections::HashSet;

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "typescript-code-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "TypeScript interface and type generator with service support".to_string(),
        target: "typescript".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some(
            "https://github.com/catalystcommunity/csilgen/typescript-generator".to_string(),
        ),
    };

    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(_) => return std::ptr::null(),
    };

    let bytes = metadata_json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null();
    }

    unsafe {
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Memory deallocation (WASM export)
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
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => return std::ptr::null_mut(),
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated_ptr.add(4), bytes.len());
            }

            allocated_ptr
        }
        Err(_code) => std::ptr::null_mut(),
    }
}

/// Process the generation request
fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = match std::str::from_utf8(input_slice) {
        Ok(s) => s,
        Err(_e) => {
            return Err(error_codes::INVALID_INPUT);
        }
    };

    let input: WasmGeneratorInput = match serde_json::from_str(input_str) {
        Ok(input) => input,
        Err(_e) => {
            return Err(error_codes::SERIALIZATION_ERROR);
        }
    };

    // Generate TypeScript code
    let mut generator = TypeScriptGenerator::new(&input);
    let result = generator.generate();

    match result {
        Ok(files) => {
            let stats = GenerationStats {
                files_generated: files.len(),
                total_size_bytes: files.iter().map(|f| f.content.len()).sum(),
                services_count: input.csil_spec.service_count,
                fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
                generation_time_ms: 75,        // Mock generation time
                peak_memory_bytes: Some(4096), // Mock memory usage
            };

            let output = WasmGeneratorOutput {
                files,
                warnings: generator.warnings,
                stats,
            };

            Ok(output)
        }
        Err(_e) => Err(error_codes::GENERATION_ERROR),
    }
}

/// TypeScript code generator implementation
struct TypeScriptGenerator<'a> {
    input: &'a WasmGeneratorInput,
    warnings: Vec<GeneratorWarning>,
    type_imports: HashSet<String>,
    #[allow(dead_code)]
    service_imports: HashSet<String>,
}

impl<'a> TypeScriptGenerator<'a> {
    fn new(input: &'a WasmGeneratorInput) -> Self {
        Self {
            input,
            warnings: Vec::new(),
            type_imports: HashSet::new(),
            service_imports: HashSet::new(),
        }
    }

    fn generate(&mut self) -> Result<Vec<GeneratedFile>, String> {
        let mut files = Vec::new();

        // Generate main types file
        let types_content = self.generate_types_file()?;
        if !types_content.trim().is_empty() {
            files.push(GeneratedFile {
                path: "types.ts".to_string(),
                content: types_content,
            });
        }

        // Generate service interfaces if services exist
        if self.input.csil_spec.service_count > 0 {
            let services_content = self.generate_services_file()?;
            if !services_content.trim().is_empty() {
                files.push(GeneratedFile {
                    path: "services.ts".to_string(),
                    content: services_content,
                });
            }
        }

        // Generate utility functions
        let utils_content = self.generate_utils_file()?;
        if !utils_content.trim().is_empty() {
            files.push(GeneratedFile {
                path: "utils.ts".to_string(),
                content: utils_content,
            });
        }

        // Generate index file with exports
        let index_content = self.generate_index_file(&files)?;
        files.push(GeneratedFile {
            path: "index.ts".to_string(),
            content: index_content,
        });

        Ok(files)
    }

    fn generate_types_file(&mut self) -> Result<String, String> {
        let mut content = String::new();

        // Add file header
        content.push_str("// Generated TypeScript types from CSIL specification\n");
        content.push_str("// This file contains all type definitions and interfaces\n\n");

        let mut type_definitions = Vec::new();

        // Process all rules to collect type definitions
        for rule in &self.input.csil_spec.rules {
            match &rule.rule_type {
                CsilRuleType::GroupDef(group) => {
                    let interface_def = self.generate_interface_definition(&rule.name, group)?;
                    type_definitions.push(interface_def);
                }
                CsilRuleType::TypeDef(type_expr) => {
                    let type_alias = self.generate_type_alias(&rule.name, type_expr)?;
                    type_definitions.push(type_alias);
                }
                CsilRuleType::TypeChoice(choices) => {
                    let union_type = self.generate_union_type(&rule.name, choices)?;
                    type_definitions.push(union_type);
                }
                CsilRuleType::GroupChoice(choices) => {
                    let group_union = self.generate_group_union_type(&rule.name, choices)?;
                    type_definitions.push(group_union);
                }
                CsilRuleType::ServiceDef(_) => {
                    // Services are handled in separate file
                }
            }
        }

        // Add all type definitions
        for type_def in type_definitions {
            content.push_str(&type_def);
            content.push_str("\n\n");
        }

        Ok(content)
    }

    fn generate_services_file(&mut self) -> Result<String, String> {
        let mut content = String::new();

        // Add file header and imports
        content.push_str("// Generated TypeScript service interfaces from CSIL specification\n");
        content.push_str("// This file contains service definitions and client interfaces\n\n");

        if !self.type_imports.is_empty() {
            let imports: Vec<String> = self.type_imports.iter().cloned().collect();
            content.push_str(&format!(
                "import {{ {} }} from './types';\n\n",
                imports.join(", ")
            ));
        }

        let mut service_definitions = Vec::new();

        // Process service definitions
        for rule in &self.input.csil_spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let service_interface = self.generate_service_interface(&rule.name, service)?;
                service_definitions.push(service_interface);

                let client_class = self.generate_service_client(&rule.name, service)?;
                service_definitions.push(client_class);
            }
        }

        // Add all service definitions
        for service_def in service_definitions {
            content.push_str(&service_def);
            content.push_str("\n\n");
        }

        Ok(content)
    }

    fn generate_utils_file(&mut self) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("// Generated TypeScript utility functions from CSIL specification\n");
        content
            .push_str("// This file contains serialization, validation, and helper functions\n\n");

        // Generate JSON serialization utilities
        content.push_str(&self.generate_serialization_utils()?);
        content.push_str("\n\n");

        // Generate validation functions
        content.push_str(&self.generate_validation_functions()?);

        Ok(content)
    }

    fn generate_index_file(&self, files: &[GeneratedFile]) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("// Generated TypeScript module exports\n");
        content.push_str("// This file re-exports all types, services, and utilities\n\n");

        // Export types
        if files.iter().any(|f| f.path == "types.ts") {
            content.push_str("export * from './types';\n");
        }

        // Export services
        if files.iter().any(|f| f.path == "services.ts") {
            content.push_str("export * from './services';\n");
        }

        // Export utilities
        if files.iter().any(|f| f.path == "utils.ts") {
            content.push_str("export * from './utils';\n");
        }

        Ok(content)
    }

    fn generate_interface_definition(
        &mut self,
        name: &str,
        group: &CsilGroupExpression,
    ) -> Result<String, String> {
        let interface_name = self.to_type_name(name);
        let mut content = String::new();

        // Generate interface documentation
        if let Some(doc) = self.extract_documentation_from_group(group) {
            content.push_str(&format!("/**\n * {doc}\n */\n"));
        }

        content.push_str(&format!("export interface {interface_name} {{\n"));

        // Generate fields
        for entry in &group.entries {
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => self.to_field_name(name),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    self.to_field_name(name)
                }
                _ => return Err("Unsupported group key type in interface".to_string()),
            };

            // Check field visibility - skip receive-only fields from interfaces
            let should_include = !entry.metadata.iter().any(|m| {
                matches!(
                    m,
                    CsilFieldMetadata::Visibility(CsilFieldVisibility::ReceiveOnly)
                )
            });

            if should_include {
                // Generate field documentation
                if let Some(doc) = self.extract_field_documentation(&entry.metadata) {
                    content.push_str(&format!("  /**\n   * {doc}\n   */\n"));
                }

                let field_type = self.generate_typescript_type(&entry.value_type)?;
                let is_optional = entry
                    .occurrence
                    .as_ref()
                    .is_some_and(|occ| matches!(occ, CsilOccurrence::Optional));

                let optional_marker = if is_optional { "?" } else { "" };
                content.push_str(&format!("  {field_name}{optional_marker}: {field_type};\n"));

                // Track type imports
                self.collect_type_imports(&entry.value_type);
            }
        }

        content.push('}');

        Ok(content)
    }

    fn generate_type_alias(
        &mut self,
        name: &str,
        type_expr: &CsilTypeExpression,
    ) -> Result<String, String> {
        let type_name = self.to_type_name(name);
        let ts_type = self.generate_typescript_type(type_expr)?;

        self.collect_type_imports(type_expr);

        Ok(format!("export type {type_name} = {ts_type};"))
    }

    fn generate_union_type(
        &mut self,
        name: &str,
        choices: &[CsilTypeExpression],
    ) -> Result<String, String> {
        let type_name = self.to_type_name(name);
        let mut union_types = Vec::new();

        for choice in choices {
            let ts_type = self.generate_typescript_type(choice)?;
            union_types.push(ts_type);
            self.collect_type_imports(choice);
        }

        let union_type = union_types.join(" | ");
        Ok(format!("export type {type_name} = {union_type};"))
    }

    fn generate_group_union_type(
        &mut self,
        name: &str,
        choices: &[CsilGroupExpression],
    ) -> Result<String, String> {
        let type_name = self.to_type_name(name);
        let mut union_types = Vec::new();

        for choice in choices.iter() {
            // For inline choices, we generate anonymous object types
            let object_type = self.generate_inline_object_type(choice)?;
            union_types.push(object_type);
        }

        let union_type = union_types.join(" | ");
        Ok(format!("export type {type_name} = {union_type};"))
    }

    fn generate_service_interface(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let service_name = self.to_type_name(name);
        let mut content = String::new();

        content.push_str(&format!("export interface {service_name} {{\n"));

        for operation in &service.operations {
            let method_name = self.to_method_name(&operation.name);
            let input_type = self.generate_typescript_type(&operation.input_type)?;
            let output_type = self.generate_typescript_type(&operation.output_type)?;

            self.collect_type_imports(&operation.input_type);
            self.collect_type_imports(&operation.output_type);

            content.push_str(&format!(
                "  {method_name}(input: {input_type}): Promise<{output_type}>;\n"
            ));
        }

        content.push('}');

        Ok(content)
    }

    fn generate_service_client(
        &mut self,
        name: &str,
        service: &CsilServiceDefinition,
    ) -> Result<String, String> {
        let service_name = self.to_type_name(name);
        let client_name = format!("{service_name}Client");
        let mut content = String::new();

        content.push_str(&format!(
            "export class {client_name} implements {service_name} {{\n"
        ));
        content.push_str("  private baseUrl: string;\n\n");
        content.push_str("  constructor(baseUrl: string) {\n");
        content.push_str("    this.baseUrl = baseUrl;\n");
        content.push_str("  }\n\n");

        for operation in &service.operations {
            let method_name = self.to_method_name(&operation.name);
            let input_type = self.generate_typescript_type(&operation.input_type)?;
            let output_type = self.generate_typescript_type(&operation.output_type)?;

            content.push_str(&format!(
                "  async {method_name}(input: {input_type}): Promise<{output_type}> {{\n"
            ));
            let endpoint = operation.name.replace('_', "-");
            content.push_str(&format!(
                "    const response = await fetch(`${{this.baseUrl}}/{endpoint}`), {{\n"
            ));
            content.push_str("      method: 'POST',\n");
            content.push_str("      headers: { 'Content-Type': 'application/json' },\n");
            content.push_str("      body: JSON.stringify(input)\n");
            content.push_str("    });\n");
            content.push_str("    \n");
            content.push_str("    if (!response.ok) {\n");
            content.push_str("      throw new Error(`HTTP error! status: ${response.status}`);\n");
            content.push_str("    }\n");
            content.push_str("    \n");
            content.push_str(&format!("    return response.json() as {output_type};\n"));
            content.push_str("  }\n\n");
        }

        content.push('}');

        Ok(content)
    }

    fn generate_serialization_utils(&self) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("// JSON serialization utilities with field visibility support\n\n");
        content.push_str("export function serializeWithVisibility<T>(obj: T, visibility: 'send' | 'receive' | 'both' = 'both'): string {\n");
        content.push_str("  // Implementation would filter fields based on visibility metadata\n");
        content.push_str("  // STUB: Field visibility filtering not yet implemented\n");
        content.push_str("  return JSON.stringify(obj);\n");
        content.push_str("}\n\n");

        content.push_str("export function deserializeWithValidation<T>(json: string, validator?: (obj: any) => obj is T): T {\n");
        content.push_str("  const obj = JSON.parse(json);\n");
        content.push_str("  if (validator && !validator(obj)) {\n");
        content.push_str("    throw new Error('Validation failed during deserialization');\n");
        content.push_str("  }\n");
        content.push_str("  return obj as T;\n");
        content.push('}');

        Ok(content)
    }

    fn generate_validation_functions(&mut self) -> Result<String, String> {
        let mut content = String::new();

        content.push_str("// Validation functions from field dependencies\n\n");

        // Generate validators for types with field dependencies
        for rule in &self.input.csil_spec.rules {
            if let CsilRuleType::GroupDef(group) = &rule.rule_type && self.has_field_dependencies(group) {
                let validator = self.generate_dependency_validator(&rule.name, group)?;
                content.push_str(&validator);
                content.push_str("\n\n");
            }
        }

        // Generate general validation utilities
        content.push_str("export function validateStringLength(value: string, minLength?: number, maxLength?: number): boolean {\n");
        content
            .push_str("  if (minLength !== undefined && value.length < minLength) return false;\n");
        content
            .push_str("  if (maxLength !== undefined && value.length > maxLength) return false;\n");
        content.push_str("  return true;\n");
        content.push_str("}\n\n");

        content.push_str("export function validateArrayLength<T>(value: T[], minItems?: number, maxItems?: number): boolean {\n");
        content
            .push_str("  if (minItems !== undefined && value.length < minItems) return false;\n");
        content
            .push_str("  if (maxItems !== undefined && value.length > maxItems) return false;\n");
        content.push_str("  return true;\n");
        content.push('}');

        Ok(content)
    }

    fn generate_dependency_validator(
        &self,
        type_name: &str,
        group: &CsilGroupExpression,
    ) -> Result<String, String> {
        let type_name_pascal = self.to_type_name(type_name);
        let validator_name = format!("validate{type_name_pascal}");

        let mut content = String::new();
        content.push_str(&format!(
            "export function {validator_name}(obj: {type_name_pascal}): boolean {{\n"
        ));

        for entry in &group.entries {
            for metadata in &entry.metadata {
                if let CsilFieldMetadata::DependsOn { field, value } = metadata {
                    let field_name = match &entry.key {
                        Some(CsilGroupKey::Bare(name)) => self.to_field_name(name),
                        Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                            self.to_field_name(name)
                        }
                        _ => continue,
                    };

                    let dependency_field = self.to_field_name(field);

                    if let Some(dep_value) = value {
                        let ts_value = self.literal_to_typescript(dep_value);
                        content.push_str(&format!(
                            "  if (obj.{field_name} !== undefined && obj.{dependency_field} !== {ts_value}) {{\n"
                        ));
                        content.push_str(&format!(
                            "    // Field '{field_name}' depends on '{dependency_field}' being {ts_value}\n"
                        ));
                        content.push_str("    return false;\n");
                        content.push_str("  }\n");
                    }
                }
            }
        }

        content.push_str("  return true;\n");
        content.push('}');

        Ok(content)
    }

    fn generate_typescript_type(&self, type_expr: &CsilTypeExpression) -> Result<String, String> {
        match type_expr {
            CsilTypeExpression::Builtin(name) => {
                Ok(match name.as_str() {
                    "text" => "string".to_string(),
                    "bool" => "boolean".to_string(),
                    "int" | "uint" => "number".to_string(),
                    "float" | "float16" | "float32" | "float64" => "number".to_string(),
                    "bytes" => "Uint8Array".to_string(),
                    "null" => "null".to_string(),
                    "any" => "any".to_string(),
                    _ => "unknown".to_string(), // Fallback for unsupported types
                })
            }
            CsilTypeExpression::Reference(name) => Ok(self.to_type_name(name)),
            CsilTypeExpression::Array {
                element_type,
                occurrence: _,
            } => {
                let element_ts_type = self.generate_typescript_type(element_type)?;
                Ok(format!("{element_ts_type}[]"))
            }
            CsilTypeExpression::Map {
                key: _,
                value,
                occurrence: _,
            } => {
                let value_ts_type = self.generate_typescript_type(value)?;
                Ok(format!("Record<string, {value_ts_type}>"))
            }
            CsilTypeExpression::Group(group) => self.generate_inline_object_type(group),
            CsilTypeExpression::Choice(choices) => {
                let mut union_types = Vec::new();
                for choice in choices {
                    let ts_type = self.generate_typescript_type(choice)?;
                    union_types.push(ts_type);
                }
                Ok(union_types.join(" | "))
            }
            CsilTypeExpression::Literal(literal) => Ok(self.literal_to_typescript(literal)),
            CsilTypeExpression::Range { .. } => {
                // For ranges, we just use number but could add validation
                Ok("number".to_string())
            }
            _ => {
                // Socket, Plug, and other advanced features
                Ok("unknown".to_string())
            }
        }
    }

    fn generate_inline_object_type(&self, group: &CsilGroupExpression) -> Result<String, String> {
        let mut content = String::new();
        content.push_str("{ ");

        for (i, entry) in group.entries.iter().enumerate() {
            if i > 0 {
                content.push_str("; ");
            }

            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => self.to_field_name(name),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => {
                    self.to_field_name(name)
                }
                _ => continue,
            };

            let field_type = self.generate_typescript_type(&entry.value_type)?;
            let is_optional = entry
                .occurrence
                .as_ref()
                .is_some_and(|occ| matches!(occ, CsilOccurrence::Optional));

            let optional_marker = if is_optional { "?" } else { "" };
            content.push_str(&format!("{field_name}{optional_marker}: {field_type}"));
        }

        content.push_str(" }");
        Ok(content)
    }

    fn literal_to_typescript(&self, literal: &CsilLiteralValue) -> String {
        match literal {
            CsilLiteralValue::Integer(n) => n.to_string(),
            CsilLiteralValue::Float(f) => f.to_string(),
            CsilLiteralValue::Text(s) => format!("'{}'", s.replace('\'', "\\'")),
            CsilLiteralValue::Bool(b) => b.to_string(),
            CsilLiteralValue::Null => "null".to_string(),
            CsilLiteralValue::Bytes(_) => "new Uint8Array()".to_string(),
            CsilLiteralValue::Array(elements) => {
                let formatted: Vec<String> = elements.iter().map(|e| self.literal_to_typescript(e)).collect();
                format!("[{}]", formatted.join(", "))
            }
        }
    }

    fn to_type_name(&self, name: &str) -> String {
        // Convert to PascalCase for TypeScript types
        let mut result = String::new();
        let mut capitalize_next = true;

        for char in name.chars() {
            if char == '_' || char == '-' {
                capitalize_next = true;
            } else if capitalize_next {
                result.push(char.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(char);
            }
        }

        result
    }

    fn to_field_name(&self, name: &str) -> String {
        // Convert to camelCase for TypeScript fields
        let mut result = String::new();
        let mut capitalize_next = false;

        for char in name.chars() {
            if char == '_' || char == '-' {
                capitalize_next = true;
            } else if capitalize_next {
                result.push(char.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                result.push(char);
            }
        }

        result
    }

    fn to_method_name(&self, name: &str) -> String {
        // Same as field name for methods
        self.to_field_name(name)
    }

    fn collect_type_imports(&mut self, type_expr: &CsilTypeExpression) {
        match type_expr {
            CsilTypeExpression::Reference(name) => {
                self.type_imports.insert(self.to_type_name(name));
            }
            CsilTypeExpression::Array { element_type, .. } => {
                self.collect_type_imports(element_type);
            }
            CsilTypeExpression::Map { key, value, .. } => {
                self.collect_type_imports(key);
                self.collect_type_imports(value);
            }
            CsilTypeExpression::Group(group) => {
                for entry in &group.entries {
                    self.collect_type_imports(&entry.value_type);
                }
            }
            CsilTypeExpression::Choice(choices) => {
                for choice in choices {
                    self.collect_type_imports(choice);
                }
            }
            _ => {}
        }
    }

    fn extract_documentation_from_group(&self, group: &CsilGroupExpression) -> Option<String> {
        // Look for description metadata in the first field as interface documentation
        group.entries.first().and_then(|entry| {
            entry.metadata.iter().find_map(|meta| {
                if let CsilFieldMetadata::Description(desc) = meta {
                    Some(desc.clone())
                } else {
                    None
                }
            })
        })
    }

    fn extract_field_documentation(&self, metadata: &[CsilFieldMetadata]) -> Option<String> {
        metadata.iter().find_map(|meta| {
            if let CsilFieldMetadata::Description(desc) = meta {
                Some(desc.clone())
            } else {
                None
            }
        })
    }

    fn has_field_dependencies(&self, group: &CsilGroupExpression) -> bool {
        group.entries.iter().any(|entry| {
            entry
                .metadata
                .iter()
                .any(|meta| matches!(meta, CsilFieldMetadata::DependsOn { .. }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::*;
    use std::collections::HashMap;

    fn create_test_input() -> WasmGeneratorInput {
        let metadata = GeneratorMetadata {
            name: "typescript-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "TypeScript generator".to_string(),
            target: "typescript".to_string(),
            capabilities: vec![
                GeneratorCapability::BasicTypes,
                GeneratorCapability::ComplexStructures,
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: None,
            homepage: None,
        };

        let config = GeneratorConfig {
            target: "typescript".to_string(),
            output_dir: "/tmp/output".to_string(),
            options: HashMap::new(),
        };

        let spec = CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![
                                CsilFieldMetadata::Visibility(CsilFieldVisibility::Bidirectional),
                                CsilFieldMetadata::Description("User's display name".to_string()),
                            ],
                        },
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("email".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: Some(CsilOccurrence::Optional),
                            metadata: vec![
                                CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly),
                                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(
                                    5,
                                )),
                            ],
                        },
                    ],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: Some("User = { name: text, email?: text }".to_string()),
            service_count: 0,
            fields_with_metadata_count: 2,
        };

        WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        }
    }

    #[test]
    fn test_type_name_conversion() {
        let input = create_test_input();
        let generator = TypeScriptGenerator::new(&input);

        assert_eq!(generator.to_type_name("user_profile"), "UserProfile");
        assert_eq!(generator.to_type_name("api-response"), "ApiResponse");
        assert_eq!(generator.to_type_name("simple"), "Simple");
    }

    #[test]
    fn test_field_name_conversion() {
        let input = create_test_input();
        let generator = TypeScriptGenerator::new(&input);

        assert_eq!(generator.to_field_name("user_name"), "userName");
        assert_eq!(generator.to_field_name("api-key"), "apiKey");
        assert_eq!(generator.to_field_name("simple"), "simple");
    }

    #[test]
    fn test_basic_type_generation() {
        let input = create_test_input();
        let generator = TypeScriptGenerator::new(&input);

        assert_eq!(
            generator
                .generate_typescript_type(&CsilTypeExpression::Builtin("text".to_string()))
                .unwrap(),
            "string"
        );
        assert_eq!(
            generator
                .generate_typescript_type(&CsilTypeExpression::Builtin("bool".to_string()))
                .unwrap(),
            "boolean"
        );
        assert_eq!(
            generator
                .generate_typescript_type(&CsilTypeExpression::Builtin("int".to_string()))
                .unwrap(),
            "number"
        );
        assert_eq!(
            generator
                .generate_typescript_type(&CsilTypeExpression::Builtin("bytes".to_string()))
                .unwrap(),
            "Uint8Array"
        );
    }

    #[test]
    fn test_array_type_generation() {
        let input = create_test_input();
        let generator = TypeScriptGenerator::new(&input);

        let array_type = CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
            occurrence: Some(CsilOccurrence::OneOrMore),
        };

        assert_eq!(
            generator.generate_typescript_type(&array_type).unwrap(),
            "string[]"
        );
    }

    #[test]
    fn test_interface_generation() {
        let input = create_test_input();
        let mut generator = TypeScriptGenerator::new(&input);

        let group = CsilGroupExpression {
            entries: vec![
                CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("name".to_string())),
                    value_type: CsilTypeExpression::Builtin("text".to_string()),
                    occurrence: None,
                    metadata: vec![CsilFieldMetadata::Description("User's name".to_string())],
                },
                CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("age".to_string())),
                    value_type: CsilTypeExpression::Builtin("int".to_string()),
                    occurrence: Some(CsilOccurrence::Optional),
                    metadata: vec![],
                },
            ],
        };

        let interface_def = generator
            .generate_interface_definition("User", &group)
            .unwrap();

        assert!(interface_def.contains("export interface User"));
        assert!(interface_def.contains("name: string;"));
        assert!(interface_def.contains("age?: number;"));
        assert!(interface_def.contains("User's name"));
    }

    #[test]
    fn test_full_generation_workflow() {
        let input = create_test_input();
        let mut generator = TypeScriptGenerator::new(&input);

        let files = generator.generate().unwrap();

        // Should generate at least types and index files
        assert!(!files.is_empty());

        let types_file = files.iter().find(|f| f.path == "types.ts");
        assert!(types_file.is_some());

        let index_file = files.iter().find(|f| f.path == "index.ts");
        assert!(index_file.is_some());

        // Types file should contain User interface
        let types_content = &types_file.unwrap().content;
        assert!(types_content.contains("export interface User"));
        assert!(types_content.contains("name: string;"));
        assert!(types_content.contains("email?: string;"));
    }

    #[test]
    fn test_service_generation() {
        let mut input = create_test_input();

        // Add a service to test service generation
        input.csil_spec.rules.push(CsilRule {
            name: "UserService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "create_user".to_string(),
                    input_type: CsilTypeExpression::Reference("User".to_string()),
                    output_type: CsilTypeExpression::Reference("User".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 5,
                        column: 4,
                        offset: 100,
                    },
                }],
            }),
            position: CsilPosition {
                line: 4,
                column: 1,
                offset: 80,
            },
        });
        input.csil_spec.service_count = 1;

        let mut generator = TypeScriptGenerator::new(&input);
        let files = generator.generate().unwrap();

        // Should generate services file
        let services_file = files.iter().find(|f| f.path == "services.ts");
        assert!(services_file.is_some());

        let services_content = &services_file.unwrap().content;
        assert!(services_content.contains("export interface UserService"));
        assert!(services_content.contains("createUser(input: User): Promise<User>"));
        assert!(services_content.contains("export class UserServiceClient"));
    }

    #[test]
    fn test_process_generation_full_workflow() {
        let input = create_test_input();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(!output.files.is_empty());
        assert_eq!(output.stats.fields_with_metadata_count, 2);
    }
}
