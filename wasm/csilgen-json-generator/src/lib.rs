//! JSON Schema generator for CSIL specifications (WASM module)
//!
//! This generator produces JSON Schema documents from CSIL specifications,
//! including support for service operation schemas and field metadata.

use csilgen_common::{
    wasm_interface::*, CsilFieldMetadata, CsilFieldVisibility, CsilGroupExpression, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilTypeExpression,
    CsilValidationConstraint, GeneratedFile, GenerationStats, GeneratorCapability,
    GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput, WasmGeneratorOutput,
};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// Generator metadata for the JSON Schema generator
pub const JSON_GENERATOR_METADATA: GeneratorMetadata = GeneratorMetadata {
    name: String::new(), // Will be set at runtime
    version: String::new(),
    description: String::new(),
    target: String::new(),
    capabilities: Vec::new(),
    author: None,
    homepage: None,
};

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "json-schema-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "JSON Schema generator for CSIL specifications".to_string(),
        target: "json-schema".to_string(),
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
            "https://github.com/catalystcommunity/csilgen/json-schema-generator".to_string(),
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
        // Write length first (little-endian u32)
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);

        // Write the JSON data
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf); // Prevent deallocation
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
/// Returns pointer to result data (length-prefixed JSON string)
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => {
                    return std::ptr::null_mut();
                }
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                return std::ptr::null_mut();
            }

            unsafe {
                // Write length first (little-endian u32)
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);

                // Write the JSON data
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

    // Generate JSON Schema
    let mut generator = JsonSchemaGenerator::new(&input);
    let result = generator.generate();

    match result {
        Ok(files) => {
            let stats = GenerationStats {
                files_generated: files.len(),
                total_size_bytes: files.iter().map(|f| f.content.len()).sum(),
                services_count: input.csil_spec.service_count,
                fields_with_metadata_count: input.csil_spec.fields_with_metadata_count,
                generation_time_ms: 50,        // Mock generation time
                peak_memory_bytes: Some(2048), // Mock memory usage
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

/// JSON Schema generator implementation
struct JsonSchemaGenerator<'a> {
    input: &'a WasmGeneratorInput,
    warnings: Vec<GeneratorWarning>,
    definitions: Map<String, Value>,
}

impl<'a> JsonSchemaGenerator<'a> {
    fn new(input: &'a WasmGeneratorInput) -> Self {
        Self {
            input,
            warnings: Vec::new(),
            definitions: Map::new(),
        }
    }

    fn generate(&mut self) -> Result<Vec<GeneratedFile>, String> {
        let mut files = Vec::new();

        // Generate main schema file
        let schema = self.generate_main_schema()?;
        files.push(GeneratedFile {
            path: "schema.json".to_string(),
            content: serde_json::to_string_pretty(&schema).map_err(|e| e.to_string())?,
        });

        // Generate service operation schemas if services exist
        if self.input.csil_spec.service_count > 0 {
            let service_schemas = self.generate_service_schemas()?;
            for (name, schema) in service_schemas {
                files.push(GeneratedFile {
                    path: format!("{}-service.json", name.to_lowercase()),
                    content: serde_json::to_string_pretty(&schema).map_err(|e| e.to_string())?,
                });
            }
        }

        Ok(files)
    }

    fn generate_main_schema(&mut self) -> Result<Value, String> {
        let mut schema = Map::new();
        schema.insert(
            "$schema".to_string(),
            Value::String("https://json-schema.org/draft/2020-12/schema".to_string()),
        );
        schema.insert(
            "title".to_string(),
            Value::String("CSIL Generated Schema".to_string()),
        );
        schema.insert("type".to_string(), Value::String("object".to_string()));

        let mut properties = Map::new();
        let required = Vec::new();

        // Process all rules
        for rule in &self.input.csil_spec.rules {
            match &rule.rule_type {
                CsilRuleType::GroupDef(group) => {
                    let type_schema = self.generate_group_schema(group, &rule.name)?;
                    self.definitions
                        .insert(rule.name.clone(), type_schema.clone());
                    properties.insert(rule.name.clone(), json_ref(&rule.name));
                }
                CsilRuleType::TypeDef(type_expr) => {
                    let type_schema = self.generate_type_schema(type_expr)?;
                    self.definitions
                        .insert(rule.name.clone(), type_schema.clone());
                    properties.insert(rule.name.clone(), json_ref(&rule.name));
                }
                CsilRuleType::TypeChoice(choices) => {
                    let choice_schema = self.generate_choice_schema(choices)?;
                    self.definitions
                        .insert(rule.name.clone(), choice_schema.clone());
                    properties.insert(rule.name.clone(), json_ref(&rule.name));
                }
                CsilRuleType::GroupChoice(choices) => {
                    let choice_schema = self.generate_group_choice_schema(choices)?;
                    self.definitions
                        .insert(rule.name.clone(), choice_schema.clone());
                    properties.insert(rule.name.clone(), json_ref(&rule.name));
                }
                CsilRuleType::ServiceDef(_) => {
                    // Services are handled separately
                }
            }
        }

        if !properties.is_empty() {
            schema.insert("properties".to_string(), Value::Object(properties));
        }

        if !required.is_empty() {
            schema.insert(
                "required".to_string(),
                Value::Array(required.into_iter().map(Value::String).collect()),
            );
        }

        if !self.definitions.is_empty() {
            schema.insert("$defs".to_string(), Value::Object(self.definitions.clone()));
        }

        Ok(Value::Object(schema))
    }

    fn generate_service_schemas(&mut self) -> Result<HashMap<String, Value>, String> {
        let mut service_schemas = HashMap::new();

        for rule in &self.input.csil_spec.rules {
            if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
                let schema = self.generate_service_schema(service, &rule.name)?;
                service_schemas.insert(rule.name.clone(), schema);
            }
        }

        Ok(service_schemas)
    }

    fn generate_service_schema(
        &mut self,
        service: &CsilServiceDefinition,
        service_name: &str,
    ) -> Result<Value, String> {
        let mut schema = Map::new();
        schema.insert(
            "$schema".to_string(),
            Value::String("https://json-schema.org/draft/2020-12/schema".to_string()),
        );
        schema.insert(
            "title".to_string(),
            Value::String(format!("{service_name} Service Operations")),
        );
        schema.insert("type".to_string(), Value::String("object".to_string()));

        let mut operations = Map::new();

        for operation in &service.operations {
            let mut op_schema = Map::new();
            op_schema.insert("type".to_string(), Value::String("object".to_string()));
            op_schema.insert(
                "title".to_string(),
                Value::String(format!("{} Operation", operation.name)),
            );

            let mut op_properties = Map::new();

            // Input schema
            let input_schema = self.generate_type_schema(&operation.input_type)?;
            op_properties.insert("input".to_string(), input_schema);

            // Output schema
            let output_schema = self.generate_type_schema(&operation.output_type)?;
            op_properties.insert("output".to_string(), output_schema);

            op_schema.insert("properties".to_string(), Value::Object(op_properties));
            op_schema.insert(
                "required".to_string(),
                Value::Array(vec![
                    Value::String("input".to_string()),
                    Value::String("output".to_string()),
                ]),
            );

            operations.insert(operation.name.clone(), Value::Object(op_schema));
        }

        schema.insert("properties".to_string(), Value::Object(operations));

        Ok(Value::Object(schema))
    }

    fn generate_group_schema(
        &mut self,
        group: &CsilGroupExpression,
        _name: &str,
    ) -> Result<Value, String> {
        let mut schema = Map::new();
        schema.insert("type".to_string(), Value::String("object".to_string()));

        let mut properties = Map::new();
        let mut required = Vec::new();

        for entry in &group.entries {
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name.clone(),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => name.clone(),
                _ => return Err("Unsupported group key type".to_string()),
            };

            // Skip fields with receive-only visibility for request schemas
            let should_include = !entry.metadata.iter().any(|m| {
                matches!(
                    m,
                    CsilFieldMetadata::Visibility(CsilFieldVisibility::ReceiveOnly)
                )
            });

            if should_include {
                let mut field_schema = self.generate_type_schema(&entry.value_type)?;

                // Apply metadata constraints
                self.apply_field_metadata(&mut field_schema, &entry.metadata)?;

                properties.insert(field_name.clone(), field_schema);

                // Check if field is required
                let is_optional = entry
                    .occurrence
                    .as_ref()
                    .is_some_and(|occ| matches!(occ, CsilOccurrence::Optional));

                if !is_optional {
                    required.push(field_name);
                }
            }
        }

        schema.insert("properties".to_string(), Value::Object(properties));

        if !required.is_empty() {
            schema.insert(
                "required".to_string(),
                Value::Array(required.into_iter().map(Value::String).collect()),
            );
        }

        Ok(Value::Object(schema))
    }

    fn generate_type_schema(&mut self, type_expr: &CsilTypeExpression) -> Result<Value, String> {
        match type_expr {
            CsilTypeExpression::Builtin(name) => {
                Ok(match name.as_str() {
                    "text" => json_type("string"),
                    "bool" => json_type("boolean"),
                    "int" | "uint" => json_type("integer"),
                    "float" | "float16" | "float32" | "float64" => json_type("number"),
                    "bytes" => {
                        let mut schema = Map::new();
                        schema.insert("type".to_string(), Value::String("string".to_string()));
                        schema.insert(
                            "contentEncoding".to_string(),
                            Value::String("base64".to_string()),
                        );
                        Value::Object(schema)
                    }
                    "null" => json_type("null"),
                    "any" => Value::Object(Map::new()), // Empty schema allows any type
                    _ => json_type("string"),           // Default fallback
                })
            }
            CsilTypeExpression::Reference(name) => Ok(json_ref(name)),
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let mut schema = Map::new();
                schema.insert("type".to_string(), Value::String("array".to_string()));

                let item_schema = self.generate_type_schema(element_type)?;
                schema.insert("items".to_string(), item_schema);

                // Apply occurrence constraints
                if let Some(occ) = occurrence {
                    self.apply_array_occurrence(&mut schema, occ);
                }

                Ok(Value::Object(schema))
            }
            CsilTypeExpression::Map {
                key: _,
                value,
                occurrence,
            } => {
                let mut schema = Map::new();
                schema.insert("type".to_string(), Value::String("object".to_string()));

                let value_schema = self.generate_type_schema(value)?;
                schema.insert("additionalProperties".to_string(), value_schema);

                // Apply occurrence constraints
                if let Some(occ) = occurrence {
                    self.apply_object_occurrence(&mut schema, occ);
                }

                Ok(Value::Object(schema))
            }
            CsilTypeExpression::Group(group) => self.generate_group_schema(group, "inline_group"),
            CsilTypeExpression::Choice(choices) => self.generate_choice_schema(choices),
            CsilTypeExpression::Literal(literal) => Ok(match literal {
                CsilLiteralValue::Integer(n) => Value::Number((*n).into()),
                CsilLiteralValue::Float(f) => serde_json::Number::from_f64(*f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                CsilLiteralValue::Text(s) => Value::String(s.clone()),
                CsilLiteralValue::Bool(b) => Value::Bool(*b),
                CsilLiteralValue::Null => Value::Null,
                CsilLiteralValue::Bytes(_) => Value::String("binary".to_string()),
            }),
            CsilTypeExpression::Range { start, end, .. } => {
                let mut schema = Map::new();
                schema.insert("type".to_string(), Value::String("integer".to_string()));

                if let Some(min) = start {
                    schema.insert("minimum".to_string(), Value::Number((*min).into()));
                }

                if let Some(max) = end {
                    schema.insert("maximum".to_string(), Value::Number((*max).into()));
                }

                Ok(Value::Object(schema))
            }
            _ => {
                // Socket, Plug, and other advanced features not yet supported
                self.warnings.push(GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: format!("Unsupported type expression: {type_expr:?}"),
                    location: None,
                    suggestion: Some(
                        "Use basic CDDL types for better JSON Schema support".to_string(),
                    ),
                });
                Ok(json_type("object"))
            }
        }
    }

    fn generate_choice_schema(&mut self, choices: &[CsilTypeExpression]) -> Result<Value, String> {
        let mut schema = Map::new();

        let mut any_of = Vec::new();
        for choice in choices {
            let choice_schema = self.generate_type_schema(choice)?;
            any_of.push(choice_schema);
        }

        schema.insert("anyOf".to_string(), Value::Array(any_of));
        Ok(Value::Object(schema))
    }

    fn generate_group_choice_schema(
        &mut self,
        choices: &[CsilGroupExpression],
    ) -> Result<Value, String> {
        let mut schema = Map::new();

        let mut any_of = Vec::new();
        for (i, choice) in choices.iter().enumerate() {
            let choice_schema = self.generate_group_schema(choice, &format!("choice_{i}"))?;
            any_of.push(choice_schema);
        }

        schema.insert("anyOf".to_string(), Value::Array(any_of));
        Ok(Value::Object(schema))
    }

    fn apply_field_metadata(
        &self,
        schema: &mut Value,
        metadata: &[CsilFieldMetadata],
    ) -> Result<(), String> {
        if let Value::Object(schema_obj) = schema {
            for meta in metadata {
                match meta {
                    CsilFieldMetadata::Constraint(constraint) => {
                        self.apply_validation_constraint(schema_obj, constraint);
                    }
                    CsilFieldMetadata::Description(desc) => {
                        schema_obj.insert("description".to_string(), Value::String(desc.clone()));
                    }
                    CsilFieldMetadata::Visibility(visibility) => {
                        // Add custom property to indicate visibility
                        let visibility_str = match visibility {
                            CsilFieldVisibility::SendOnly => "send-only",
                            CsilFieldVisibility::ReceiveOnly => "receive-only",
                            CsilFieldVisibility::Bidirectional => "bidirectional",
                        };
                        schema_obj.insert(
                            "x-visibility".to_string(),
                            Value::String(visibility_str.to_string()),
                        );
                    }
                    CsilFieldMetadata::DependsOn { field, value } => {
                        // Add custom dependency annotation
                        let mut dep = Map::new();
                        dep.insert("field".to_string(), Value::String(field.clone()));
                        if let Some(val) = value {
                            dep.insert("value".to_string(), literal_to_json(val));
                        }
                        schema_obj.insert("x-depends-on".to_string(), Value::Object(dep));
                    }
                    CsilFieldMetadata::Custom {
                        name,
                        parameters: _,
                    } => {
                        // Add custom metadata as extension property
                        schema_obj.insert(format!("x-{name}"), Value::String("true".to_string()));
                    }
                }
            }
        }
        Ok(())
    }

    fn apply_validation_constraint(
        &self,
        schema: &mut Map<String, Value>,
        constraint: &CsilValidationConstraint,
    ) {
        match constraint {
            CsilValidationConstraint::MinLength(len) => {
                schema.insert("minLength".to_string(), Value::Number((*len).into()));
            }
            CsilValidationConstraint::MaxLength(len) => {
                schema.insert("maxLength".to_string(), Value::Number((*len).into()));
            }
            CsilValidationConstraint::MinItems(count) => {
                schema.insert("minItems".to_string(), Value::Number((*count).into()));
            }
            CsilValidationConstraint::MaxItems(count) => {
                schema.insert("maxItems".to_string(), Value::Number((*count).into()));
            }
            CsilValidationConstraint::MinValue(value) => {
                schema.insert("minimum".to_string(), literal_to_json(value));
            }
            CsilValidationConstraint::MaxValue(value) => {
                schema.insert("maximum".to_string(), literal_to_json(value));
            }
            CsilValidationConstraint::Custom { name, value } => {
                schema.insert(format!("x-constraint-{name}"), literal_to_json(value));
            }
        }
    }

    fn apply_array_occurrence(&self, schema: &mut Map<String, Value>, occurrence: &CsilOccurrence) {
        match occurrence {
            CsilOccurrence::ZeroOrMore => {
                schema.insert("minItems".to_string(), Value::Number(0.into()));
            }
            CsilOccurrence::OneOrMore => {
                schema.insert("minItems".to_string(), Value::Number(1.into()));
            }
            CsilOccurrence::Exact(count) => {
                schema.insert("minItems".to_string(), Value::Number((*count).into()));
                schema.insert("maxItems".to_string(), Value::Number((*count).into()));
            }
            CsilOccurrence::Range { min, max } => {
                if let Some(min_count) = min {
                    schema.insert("minItems".to_string(), Value::Number((*min_count).into()));
                }
                if let Some(max_count) = max {
                    schema.insert("maxItems".to_string(), Value::Number((*max_count).into()));
                }
            }
            _ => {}
        }
    }

    fn apply_object_occurrence(
        &self,
        schema: &mut Map<String, Value>,
        occurrence: &CsilOccurrence,
    ) {
        match occurrence {
            CsilOccurrence::ZeroOrMore => {
                schema.insert("minProperties".to_string(), Value::Number(0.into()));
            }
            CsilOccurrence::OneOrMore => {
                schema.insert("minProperties".to_string(), Value::Number(1.into()));
            }
            CsilOccurrence::Exact(count) => {
                schema.insert("minProperties".to_string(), Value::Number((*count).into()));
                schema.insert("maxProperties".to_string(), Value::Number((*count).into()));
            }
            CsilOccurrence::Range { min, max } => {
                if let Some(min_count) = min {
                    schema.insert(
                        "minProperties".to_string(),
                        Value::Number((*min_count).into()),
                    );
                }
                if let Some(max_count) = max {
                    schema.insert(
                        "maxProperties".to_string(),
                        Value::Number((*max_count).into()),
                    );
                }
            }
            _ => {}
        }
    }
}

// Helper functions
fn json_type(type_name: &str) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String(type_name.to_string()));
    Value::Object(schema)
}

fn json_ref(type_name: &str) -> Value {
    let mut schema = Map::new();
    schema.insert(
        "$ref".to_string(),
        Value::String(format!("#/$defs/{type_name}")),
    );
    Value::Object(schema)
}

fn literal_to_json(literal: &CsilLiteralValue) -> Value {
    match literal {
        CsilLiteralValue::Integer(n) => Value::Number((*n).into()),
        CsilLiteralValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        CsilLiteralValue::Text(s) => Value::String(s.clone()),
        CsilLiteralValue::Bool(b) => Value::Bool(*b),
        CsilLiteralValue::Null => Value::Null,
        CsilLiteralValue::Bytes(_) => Value::String("binary".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::*;
    use std::collections::HashMap;

    fn create_test_input() -> WasmGeneratorInput {
        let metadata = GeneratorMetadata {
            name: "json-schema-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "JSON Schema generator".to_string(),
            target: "json-schema".to_string(),
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
            target: "json-schema".to_string(),
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
    fn test_basic_type_generation() {
        let input = create_test_input();
        let mut generator = JsonSchemaGenerator::new(&input);

        let schema = generator
            .generate_type_schema(&CsilTypeExpression::Builtin("text".to_string()))
            .unwrap();

        if let Value::Object(obj) = schema {
            assert_eq!(obj.get("type"), Some(&Value::String("string".to_string())));
        } else {
            panic!("Expected object schema");
        }
    }

    #[test]
    fn test_group_schema_generation() {
        let input = create_test_input();
        let mut generator = JsonSchemaGenerator::new(&input);

        let files = generator.generate().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "schema.json");

        // Parse generated schema to verify structure
        let schema: Value = serde_json::from_str(&files[0].content).unwrap();

        if let Value::Object(obj) = schema {
            assert_eq!(
                obj.get("$schema"),
                Some(&Value::String(
                    "https://json-schema.org/draft/2020-12/schema".to_string()
                ))
            );
            assert!(obj.contains_key("$defs"));
            assert!(obj.contains_key("properties"));
        } else {
            panic!("Expected object schema");
        }
    }

    #[test]
    fn test_field_metadata_application() {
        let input = create_test_input();
        let generator = JsonSchemaGenerator::new(&input);

        let metadata = vec![
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(5)),
            CsilFieldMetadata::Description("Test field".to_string()),
        ];

        let mut schema = json_type("string");
        generator
            .apply_field_metadata(&mut schema, &metadata)
            .unwrap();

        if let Value::Object(obj) = schema {
            assert_eq!(obj.get("minLength"), Some(&Value::Number(5.into())));
            assert_eq!(
                obj.get("description"),
                Some(&Value::String("Test field".to_string()))
            );
        } else {
            panic!("Expected object schema");
        }
    }

    #[test]
    fn test_array_type_generation() {
        let input = create_test_input();
        let mut generator = JsonSchemaGenerator::new(&input);

        let array_type = CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
            occurrence: Some(CsilOccurrence::OneOrMore),
        };

        let schema = generator.generate_type_schema(&array_type).unwrap();

        if let Value::Object(obj) = schema {
            assert_eq!(obj.get("type"), Some(&Value::String("array".to_string())));
            assert_eq!(obj.get("minItems"), Some(&Value::Number(1.into())));

            if let Some(Value::Object(items)) = obj.get("items") {
                assert_eq!(
                    items.get("type"),
                    Some(&Value::String("string".to_string()))
                );
            } else {
                panic!("Expected items schema");
            }
        } else {
            panic!("Expected object schema");
        }
    }

    #[test]
    fn test_choice_type_generation() {
        let input = create_test_input();
        let mut generator = JsonSchemaGenerator::new(&input);

        let choices = vec![
            CsilTypeExpression::Builtin("text".to_string()),
            CsilTypeExpression::Builtin("int".to_string()),
        ];

        let schema = generator.generate_choice_schema(&choices).unwrap();

        if let Value::Object(obj) = schema {
            if let Some(Value::Array(any_of)) = obj.get("anyOf") {
                assert_eq!(any_of.len(), 2);
            } else {
                panic!("Expected anyOf array");
            }
        } else {
            panic!("Expected object schema");
        }
    }

    #[test]
    fn test_service_schema_generation() {
        let mut input = create_test_input();

        // Add a service to test service schema generation
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

        let mut generator = JsonSchemaGenerator::new(&input);
        let files = generator.generate().unwrap();

        // Should generate main schema + service schema
        assert_eq!(files.len(), 2);

        let service_file = files
            .iter()
            .find(|f| f.path.contains("service"))
            .expect("Should have service schema file");

        let service_schema: Value = serde_json::from_str(&service_file.content).unwrap();
        if let Value::Object(obj) = service_schema {
            assert!(obj.contains_key("properties"));
        } else {
            panic!("Expected object schema");
        }
    }

    #[test]
    fn test_process_generation_full_workflow() {
        let input = create_test_input();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.stats.files_generated, 1);
        assert_eq!(output.stats.fields_with_metadata_count, 2);
    }
}
