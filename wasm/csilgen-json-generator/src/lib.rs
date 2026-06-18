//! JSON Schema generator for CSIL specifications (WASM module)
//!
//! This generator produces JSON Schema documents from CSIL specifications,
//! including support for service operation schemas and field metadata.

use csilgen_common::{
    CsilControlOperator, CsilDependsCompareOp, CsilDependsCondition, CsilFieldMetadata,
    CsilFieldVisibility, CsilGroupExpression, CsilGroupKey, CsilLiteralValue, CsilOccurrence,
    CsilRuleType, CsilServiceDefinition, CsilServiceDirection, CsilSizeConstraint,
    CsilTypeExpression, CsilValidationConstraint, GeneratedFile, GenerationStats,
    GeneratorCapability, GeneratorMetadata, GeneratorWarning, WarningLevel, WasmGeneratorInput,
    WasmGeneratorOutput, wasm_interface::*,
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
        // Validate options up front so a misconfigured spec fails fast with a
        // clear message instead of silently emitting a misleading schema, the
        // same validate-early idiom the other generators use.
        self.validate_options()?;

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

    /// `decimal_mapping` does not change the emitted schema (decimal is always
    /// `string`/`decimal` text here) but is validated for cross-generator
    /// consistency: the same option drives the in-memory type in the code
    /// targets, and an unknown value must be a hard error everywhere.
    fn validate_options(&self) -> Result<(), String> {
        if let Some(value) = self.input.config.options.get("decimal_mapping") {
            match value.as_str() {
                Some("csil") | Some("library") => {}
                Some(other) => {
                    return Err(format!(
                        "decimal_mapping must be \"csil\" or \"library\", got {other:?}"
                    ));
                }
                None => {
                    return Err(format!("decimal_mapping must be a string, got {value:?}"));
                }
            }
        }
        Ok(())
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
        // Operations skipped because JSON Schema doesn't meaningfully describe
        // their persistent-channel semantics. We record them as a vendor
        // extension so downstream tooling knows the schema is intentionally
        // incomplete for this service.
        let mut skipped = Vec::<Value>::new();

        for operation in &service.operations {
            if !matches!(operation.direction, CsilServiceDirection::Unidirectional) {
                let direction_label = match operation.direction {
                    CsilServiceDirection::Bidirectional => "bidirectional (<->)",
                    CsilServiceDirection::Reverse => "reverse (<-)",
                    CsilServiceDirection::Unidirectional => unreachable!(),
                };
                self.warnings.push(GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: format!(
                        "Skipping {service_name}.{op}: JSON Schema only meaningfully describes \
                         request/response (->) operations; {direction_label} operations require a \
                         persistent channel that JSON Schema cannot express.",
                        op = operation.name
                    ),
                    location: None,
                    suggestion: Some(
                        "Generate types from this spec with --target json for the data shapes, \
                         and use a code target (rust/go/typescript/python) for the service \
                         channel handlers."
                            .to_string(),
                    ),
                });
                let mut entry = Map::new();
                entry.insert("name".to_string(), Value::String(operation.name.clone()));
                entry.insert(
                    "direction".to_string(),
                    Value::String(direction_label.to_string()),
                );
                skipped.push(Value::Object(entry));
                continue;
            }

            let mut op_schema = Map::new();
            op_schema.insert("type".to_string(), Value::String("object".to_string()));
            op_schema.insert(
                "title".to_string(),
                Value::String(format!("{} Operation", operation.name)),
            );

            let mut op_properties = Map::new();

            let input_schema = self.generate_type_schema(&operation.input_type)?;
            op_properties.insert("input".to_string(), input_schema);

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
        if !skipped.is_empty() {
            schema.insert(
                "x-csil-skipped-operations".to_string(),
                Value::Array(skipped),
            );
        }

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
        // Keyless entries are group membership/spread (`{ shared_group, … }`); they
        // compose the referenced group's schema via `allOf` rather than naming a
        // property.
        let mut composed = Vec::new();
        // A spread of a non-object target cannot be composed: `allOf` would
        // intersect `type: object` with e.g. `type: string`, which no instance
        // can satisfy. Such targets are recorded as an annotation instead so the
        // emitted schema stays satisfiable.
        let mut dropped_spreads = Vec::new();

        for entry in &group.entries {
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name.clone(),
                Some(CsilGroupKey::Literal(CsilLiteralValue::Text(name))) => name.clone(),
                None => {
                    if self.type_is_object_like(&entry.value_type) {
                        composed.push(self.generate_type_schema(&entry.value_type)?);
                    } else {
                        dropped_spreads.push(Value::String(spread_target_label(&entry.value_type)));
                    }
                    continue;
                }
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

        if !dropped_spreads.is_empty() {
            schema.insert("x-csil-spread".to_string(), Value::Array(dropped_spreads));
        }

        if composed.is_empty() {
            Ok(Value::Object(schema))
        } else {
            // The object's own fields plus each spread group must all hold.
            composed.insert(0, Value::Object(schema));
            let mut wrapper = Map::new();
            wrapper.insert("allOf".to_string(), Value::Array(composed));
            Ok(Value::Object(wrapper))
        }
    }

    /// A keyless group entry spreads its target's shape into the enclosing
    /// object, which only yields a satisfiable `allOf` when that target is
    /// itself object-shaped. A reference is the canonical group-spread case, so
    /// it is only ruled out when it positively resolves to a non-object rule;
    /// an unknown or forward reference keeps the legitimate `allOf` behavior
    /// rather than being silently dropped.
    fn type_is_object_like(&self, type_expr: &CsilTypeExpression) -> bool {
        match type_expr {
            CsilTypeExpression::Group(_) | CsilTypeExpression::Map { .. } => true,
            CsilTypeExpression::Constrained { base_type, .. } => {
                self.type_is_object_like(base_type)
            }
            CsilTypeExpression::Reference(name) => self
                .input
                .csil_spec
                .rules
                .iter()
                .find(|rule| &rule.name == name)
                .map(|rule| match &rule.rule_type {
                    CsilRuleType::GroupDef(_) | CsilRuleType::GroupChoice(_) => true,
                    CsilRuleType::TypeDef(inner) => self.type_is_object_like(inner),
                    _ => false,
                })
                .unwrap_or(true),
            _ => false,
        }
    }

    /// A CSIL tuple is a fixed-shape array, so it maps to draft 2020-12
    /// positional validation: one `prefixItems` schema per entry in declaration
    /// order, with `items: false` to forbid extra elements. A keyed tuple
    /// (`[tag: text, value: any]`) carries only positional meaning in JSON, so
    /// the keys are dropped and just the entry value types are emitted.
    fn generate_tuple_schema(&mut self, group: &CsilGroupExpression) -> Result<Value, String> {
        let mut prefix_items = Vec::with_capacity(group.entries.len());
        for entry in &group.entries {
            let mut item_schema = self.generate_type_schema(&entry.value_type)?;
            self.apply_field_metadata(&mut item_schema, &entry.metadata)?;
            prefix_items.push(item_schema);
        }

        let count = prefix_items.len();
        let mut schema = Map::new();
        schema.insert("type".to_string(), Value::String("array".to_string()));
        schema.insert("prefixItems".to_string(), Value::Array(prefix_items));
        // A fixed-shape array admits neither extra nor missing positions, so the
        // length is pinned to the entry count on both ends.
        schema.insert("items".to_string(), Value::Bool(false));
        schema.insert("minItems".to_string(), Value::Number(count.into()));
        schema.insert("maxItems".to_string(), Value::Number(count.into()));

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
                    // timestamp is CBOR tag 0 (RFC 3339 UTC) on the wire; the
                    // JSON-Schema view of that instant is a date-time string.
                    "timestamp" => json_string_format("date-time"),
                    // decimal is CBOR tag 4 (exact base-10) on the wire; JSON has
                    // no exact-decimal number type, so the schema models it as the
                    // exact value carried as text.
                    "decimal" => json_string_format("decimal"),
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
            CsilTypeExpression::Tuple(group) => self.generate_tuple_schema(group),
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
                CsilLiteralValue::Array(elements) => {
                    let json_elements: Vec<Value> = elements.iter().map(literal_to_json).collect();
                    Value::Array(json_elements)
                }
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
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => {
                let mut base = self.generate_type_schema(base_type)?;
                // Constraints only refine an object schema; a bare `$ref` or a
                // non-object base has nowhere to hang keywords, so it passes
                // through unchanged rather than being silently dropped.
                if let Value::Object(obj) = &mut base {
                    self.apply_control_operators(obj, constraints)?;
                }
                Ok(base)
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
                    // The boolean `@depends-on` tree has no JSON Schema keyword,
                    // so it rides as a readable condition string in the same
                    // vendor-extension family as the simple form above.
                    CsilFieldMetadata::DependsOnExpr(condition) => {
                        schema_obj.insert(
                            "x-csil-depends-on".to_string(),
                            Value::String(render_depends_condition(condition)),
                        );
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
                insert_bound(schema, "minimum", "x-csil-minimum", value);
            }
            CsilValidationConstraint::MaxValue(value) => {
                insert_bound(schema, "maximum", "x-csil-maximum", value);
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

    /// Map the `.`-control-operator constraint system onto JSON Schema keywords.
    /// This is the second of two parallel constraint systems CSIL carries; the
    /// `@`-annotation system is handled separately in `apply_validation_constraint`.
    /// It is a method because `.and`/`.within` carry a type expression whose
    /// faithful rendering requires the same recursive converter used for every
    /// other type, rather than a placeholder debug string.
    fn apply_control_operators(
        &mut self,
        schema: &mut Map<String, Value>,
        constraints: &[CsilControlOperator],
    ) -> Result<(), String> {
        for constraint in constraints {
            match constraint {
                CsilControlOperator::Size(size) => apply_size_constraint(schema, size),
                CsilControlOperator::Regex(pattern) => {
                    schema.insert("pattern".to_string(), Value::String(pattern.clone()));
                }
                CsilControlOperator::Default(value) => {
                    schema.insert("default".to_string(), literal_to_json(value));
                }
                CsilControlOperator::GreaterEqual(value) => {
                    insert_bound(schema, "minimum", "x-csil-minimum", value);
                }
                CsilControlOperator::LessEqual(value) => {
                    insert_bound(schema, "maximum", "x-csil-maximum", value);
                }
                // Draft 2020-12 takes the bound itself, not a boolean, for the
                // exclusive variants.
                CsilControlOperator::GreaterThan(value) => {
                    insert_bound(
                        schema,
                        "exclusiveMinimum",
                        "x-csil-exclusive-minimum",
                        value,
                    );
                }
                CsilControlOperator::LessThan(value) => {
                    insert_bound(
                        schema,
                        "exclusiveMaximum",
                        "x-csil-exclusive-maximum",
                        value,
                    );
                }
                CsilControlOperator::Equal(value) => {
                    schema.insert("const".to_string(), literal_to_json(value));
                }
                // `.ne` has no direct keyword; the inverse of `const` is the value
                // negated under `not`.
                CsilControlOperator::NotEqual(value) => {
                    let mut excluded = Map::new();
                    excluded.insert("const".to_string(), literal_to_json(value));
                    schema.insert("not".to_string(), Value::Object(excluded));
                }
                CsilControlOperator::Json => {
                    schema.insert(
                        "contentMediaType".to_string(),
                        Value::String("application/json".to_string()),
                    );
                }
                // CBOR payloads are binary, so they ride as base64 text with the
                // matching media type so a validator treats them as opaque content.
                CsilControlOperator::Cbor => {
                    schema.insert(
                        "contentEncoding".to_string(),
                        Value::String("base64".to_string()),
                    );
                    schema.insert(
                        "contentMediaType".to_string(),
                        Value::String("application/cbor".to_string()),
                    );
                }
                CsilControlOperator::Cborseq => {
                    schema.insert(
                        "contentEncoding".to_string(),
                        Value::String("base64".to_string()),
                    );
                    schema.insert(
                        "contentMediaType".to_string(),
                        Value::String("application/cbor-seq".to_string()),
                    );
                }
                // This has no faithful JSON-Schema keyword; preserve it as a
                // vendor extension so the constraint is not silently lost.
                CsilControlOperator::Bits(name) => {
                    schema.insert("x-csil-bits".to_string(), Value::String(name.clone()));
                }
                // `.and` intersects the base with another type; JSON Schema models
                // intersection with `allOf`, so each `.and` contributes the
                // referenced type's real schema as one member.
                CsilControlOperator::And(expr) => {
                    let member = self.generate_type_schema(expr)?;
                    let all_of = schema
                        .entry("allOf".to_string())
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Value::Array(members) = all_of {
                        members.push(member);
                    }
                }
                // `.within` has no JSON-Schema keyword, but the referenced type
                // still has a real schema; carry that schema as the vendor
                // extension value rather than a debug string.
                CsilControlOperator::Within(expr) => {
                    let inner = self.generate_type_schema(expr)?;
                    schema.insert("x-csil-within".to_string(), inner);
                }
            }
        }
        Ok(())
    }
}

// Helper functions
fn json_type(type_name: &str) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String(type_name.to_string()));
    Value::Object(schema)
}

fn json_string_format(format: &str) -> Value {
    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("string".to_string()));
    schema.insert("format".to_string(), Value::String(format.to_string()));
    Value::Object(schema)
}

/// JSON Schema's `minimum`/`maximum` family is defined only for `number` and
/// `integer` instances. The two tagged core types are string-typed in schema
/// (`decimal` -> `{"type":"string","format":"decimal"}`, `timestamp` ->
/// `{"type":"string","format":"date-time"}`), so a numeric keyword on them is
/// simply ignored by validators while also being type-invalid.
fn schema_takes_numeric_bound(schema: &Map<String, Value>) -> bool {
    matches!(
        schema.get("type").and_then(Value::as_str),
        Some("number") | Some("integer")
    )
}

/// Place a comparison bound using the keyword that matches the schema's type.
/// Numeric schemas get the real `minimum`/`maximum`/exclusive keyword; the
/// string-typed `decimal`/`timestamp` schemas instead carry the bound as a
/// string in a vendor extension so the constraint survives without producing a
/// schema where a numeric keyword applies to a string instance.
fn insert_bound(
    schema: &mut Map<String, Value>,
    numeric_key: &str,
    vendor_key: &str,
    value: &CsilLiteralValue,
) {
    if schema_takes_numeric_bound(schema) {
        schema.insert(numeric_key.to_string(), literal_to_json(value));
    } else {
        schema.insert(vendor_key.to_string(), bound_as_string(value));
    }
}

/// The vendor-extension form always carries the bound as text, since the whole
/// reason for the extension is that the instance type is a string.
fn bound_as_string(value: &CsilLiteralValue) -> Value {
    match literal_to_json(value) {
        Value::String(s) => Value::String(s),
        other => Value::String(other.to_string()),
    }
}

/// `.size` measures string length or array element count depending on the base
/// type, so the keyword pair is chosen from the schema's declared `type`.
fn apply_size_constraint(schema: &mut Map<String, Value>, size: &CsilSizeConstraint) {
    let is_array = schema.get("type") == Some(&Value::String("array".to_string()));
    let (min_key, max_key) = if is_array {
        ("minItems", "maxItems")
    } else {
        ("minLength", "maxLength")
    };
    match size {
        CsilSizeConstraint::Exact(val) => {
            schema.insert(min_key.to_string(), Value::Number((*val).into()));
            schema.insert(max_key.to_string(), Value::Number((*val).into()));
        }
        CsilSizeConstraint::Range { min, max } => {
            schema.insert(min_key.to_string(), Value::Number((*min).into()));
            schema.insert(max_key.to_string(), Value::Number((*max).into()));
        }
        CsilSizeConstraint::Min(val) => {
            schema.insert(min_key.to_string(), Value::Number((*val).into()));
        }
        CsilSizeConstraint::Max(val) => {
            schema.insert(max_key.to_string(), Value::Number((*val).into()));
        }
    }
}

fn json_ref(type_name: &str) -> Value {
    let mut schema = Map::new();
    schema.insert(
        "$ref".to_string(),
        Value::String(format!("#/$defs/{type_name}")),
    );
    Value::Object(schema)
}

/// A dropped non-object spread is recorded by the most meaningful name
/// available so a reader can recover the intent: the referenced rule name or
/// builtin keyword when there is one, falling back to a structural description
/// for anonymous targets.
fn spread_target_label(type_expr: &CsilTypeExpression) -> String {
    match type_expr {
        CsilTypeExpression::Reference(name) | CsilTypeExpression::Builtin(name) => name.clone(),
        other => format!("{other:?}"),
    }
}

/// Render a boolean `@depends-on` tree as a human-readable condition string.
/// `All` joins with " and ", `Any` with " or ", and a presence check (no
/// operator) reads as "<field> is present" so the rendered form is meaningful
/// on its own without re-deriving the operator.
fn render_depends_condition(condition: &CsilDependsCondition) -> String {
    match condition {
        CsilDependsCondition::Compare { field, op, value } => match (op, value) {
            (Some(op), Some(value)) => {
                let op_str = depends_op_str(*op);
                let value_str = depends_value_str(value);
                format!("{field} {op_str} {value_str}")
            }
            _ => format!("{field} is present"),
        },
        // Nested groups are parenthesized so precedence stays unambiguous when
        // an `All` contains an `Any` or vice versa.
        CsilDependsCondition::All(parts) => join_conditions(parts, " and "),
        CsilDependsCondition::Any(parts) => join_conditions(parts, " or "),
    }
}

fn join_conditions(parts: &[CsilDependsCondition], sep: &str) -> String {
    parts
        .iter()
        .map(|part| match part {
            CsilDependsCondition::Compare { .. } => render_depends_condition(part),
            CsilDependsCondition::All(_) | CsilDependsCondition::Any(_) => {
                format!("({})", render_depends_condition(part))
            }
        })
        .collect::<Vec<_>>()
        .join(sep)
}

fn depends_op_str(op: CsilDependsCompareOp) -> &'static str {
    match op {
        CsilDependsCompareOp::Eq => "==",
        CsilDependsCompareOp::Ne => "!=",
        CsilDependsCompareOp::Lt => "<",
        CsilDependsCompareOp::Le => "<=",
        CsilDependsCompareOp::Gt => ">",
        CsilDependsCompareOp::Ge => ">=",
    }
}

/// Text literals are quoted so the rendered condition distinguishes a string
/// value from a bare field name or number.
fn depends_value_str(value: &CsilLiteralValue) -> String {
    match value {
        CsilLiteralValue::Text(s) => format!("\"{s}\""),
        other => match literal_to_json(other) {
            Value::String(s) => s,
            json => json.to_string(),
        },
    }
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
        CsilLiteralValue::Array(elements) => {
            let json_elements: Vec<Value> = elements.iter().map(literal_to_json).collect();
            Value::Array(json_elements)
        }
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
                            doc_comments: Vec::new(),
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
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: Vec::new(),
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
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 4,
                column: 1,
                offset: 80,
            },
            doc_comments: Vec::new(),
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

    #[test]
    fn nonunidirectional_ops_are_skipped_with_warning() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "ChatService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    CsilServiceOperation {
                        name: "send_message".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Unidirectional,
                        position: CsilPosition {
                            line: 1,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                    },
                    CsilServiceOperation {
                        name: "subscribe".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Bidirectional,
                        position: CsilPosition {
                            line: 2,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                    },
                    CsilServiceOperation {
                        name: "notify".to_string(),
                        input_type: CsilTypeExpression::Reference("User".to_string()),
                        output_type: CsilTypeExpression::Reference("User".to_string()),
                        direction: CsilServiceDirection::Reverse,
                        position: CsilPosition {
                            line: 3,
                            column: 1,
                            offset: 0,
                        },
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = JsonSchemaGenerator::new(&input);
        let service = match &input.csil_spec.rules.last().unwrap().rule_type {
            CsilRuleType::ServiceDef(s) => s,
            _ => unreachable!(),
        };
        let schema = generator
            .generate_service_schema(service, "ChatService")
            .unwrap();

        let obj = match &schema {
            Value::Object(o) => o,
            _ => panic!("expected object"),
        };

        // Unidirectional op stays in `properties`; bidi/reverse are skipped.
        let properties = obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        assert!(properties.contains_key("send_message"));
        assert!(!properties.contains_key("subscribe"));
        assert!(!properties.contains_key("notify"));

        // Skipped ops surface as a vendor extension so consumers know the
        // schema is intentionally incomplete for those operations.
        let skipped = obj
            .get("x-csil-skipped-operations")
            .and_then(|v| v.as_array())
            .expect("x-csil-skipped-operations array");
        assert_eq!(skipped.len(), 2);
        let names: Vec<&str> = skipped
            .iter()
            .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"subscribe"));
        assert!(names.contains(&"notify"));

        // One warning per skipped op, naming both the service and the op.
        let warning_text: String = generator
            .warnings
            .iter()
            .map(|w| w.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(warning_text.contains("ChatService.subscribe"));
        assert!(warning_text.contains("ChatService.notify"));
        assert!(warning_text.contains("bidirectional"));
        assert!(warning_text.contains("reverse"));
    }

    fn schema_for(type_expr: CsilTypeExpression) -> Map<String, Value> {
        let input = create_test_input();
        let mut generator = JsonSchemaGenerator::new(&input);
        match generator.generate_type_schema(&type_expr).unwrap() {
            Value::Object(obj) => obj,
            other => panic!("expected object schema, got {other:?}"),
        }
    }

    fn constrained(
        base: CsilTypeExpression,
        constraints: Vec<CsilControlOperator>,
    ) -> Map<String, Value> {
        schema_for(CsilTypeExpression::Constrained {
            base_type: Box::new(base),
            constraints,
        })
    }

    #[test]
    fn timestamp_maps_to_date_time_string() {
        let obj = schema_for(CsilTypeExpression::Builtin("timestamp".to_string()));
        assert_eq!(obj.get("type"), Some(&Value::String("string".to_string())));
        assert_eq!(
            obj.get("format"),
            Some(&Value::String("date-time".to_string()))
        );
    }

    #[test]
    fn decimal_maps_to_decimal_string() {
        let obj = schema_for(CsilTypeExpression::Builtin("decimal".to_string()));
        assert_eq!(obj.get("type"), Some(&Value::String("string".to_string())));
        assert_eq!(
            obj.get("format"),
            Some(&Value::String("decimal".to_string()))
        );
    }

    #[test]
    fn decimal_mapping_option_is_validated() {
        for value in ["csil", "library"] {
            let mut input = create_test_input();
            input
                .config
                .options
                .insert("decimal_mapping".to_string(), Value::String(value.into()));
            let mut generator = JsonSchemaGenerator::new(&input);
            assert!(
                generator.generate().is_ok(),
                "decimal_mapping={value} should be accepted"
            );
        }

        let mut input = create_test_input();
        input.config.options.insert(
            "decimal_mapping".to_string(),
            Value::String("nonsense".into()),
        );
        let mut generator = JsonSchemaGenerator::new(&input);
        let err = generator.generate().unwrap_err();
        assert!(err.contains("decimal_mapping"), "got: {err}");

        // A non-string value is just as misconfigured as an unknown string.
        let mut input = create_test_input();
        input
            .config
            .options
            .insert("decimal_mapping".to_string(), Value::Bool(true));
        let mut generator = JsonSchemaGenerator::new(&input);
        assert!(generator.generate().is_err());
    }

    #[test]
    fn size_constraint_maps_by_base_type() {
        let string_obj = constrained(
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                min: 2,
                max: 8,
            })],
        );
        assert_eq!(string_obj.get("minLength"), Some(&Value::Number(2.into())));
        assert_eq!(string_obj.get("maxLength"), Some(&Value::Number(8.into())));

        let array_obj = constrained(
            CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            },
            vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        );
        assert_eq!(array_obj.get("minItems"), Some(&Value::Number(3.into())));
        assert!(!array_obj.contains_key("minLength"));
    }

    #[test]
    fn numeric_bounds_and_regex_and_default_map() {
        let obj = constrained(
            CsilTypeExpression::Builtin("int".to_string()),
            vec![
                CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(0)),
                CsilControlOperator::LessEqual(CsilLiteralValue::Integer(100)),
                CsilControlOperator::GreaterThan(CsilLiteralValue::Integer(-1)),
                CsilControlOperator::LessThan(CsilLiteralValue::Integer(101)),
                CsilControlOperator::Default(CsilLiteralValue::Integer(42)),
            ],
        );
        assert_eq!(obj.get("minimum"), Some(&Value::Number(0.into())));
        assert_eq!(obj.get("maximum"), Some(&Value::Number(100.into())));
        assert_eq!(
            obj.get("exclusiveMinimum"),
            Some(&Value::Number((-1).into()))
        );
        assert_eq!(
            obj.get("exclusiveMaximum"),
            Some(&Value::Number(101.into()))
        );
        assert_eq!(obj.get("default"), Some(&Value::Number(42.into())));

        let regex_obj = constrained(
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilControlOperator::Regex("^a+$".to_string())],
        );
        assert_eq!(
            regex_obj.get("pattern"),
            Some(&Value::String("^a+$".to_string()))
        );
    }

    #[test]
    fn eq_maps_to_const_and_ne_maps_to_not_const() {
        let eq_obj = constrained(
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilControlOperator::Equal(CsilLiteralValue::Text(
                "fixed".to_string(),
            ))],
        );
        assert_eq!(
            eq_obj.get("const"),
            Some(&Value::String("fixed".to_string()))
        );

        let ne_obj = constrained(
            CsilTypeExpression::Builtin("int".to_string()),
            vec![CsilControlOperator::NotEqual(CsilLiteralValue::Integer(7))],
        );
        let not = ne_obj
            .get("not")
            .and_then(|v| v.as_object())
            .expect("not object");
        assert_eq!(not.get("const"), Some(&Value::Number(7.into())));
    }

    #[test]
    fn encoding_constraints_map_to_content_media_type() {
        let json_obj = constrained(
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilControlOperator::Json],
        );
        assert_eq!(
            json_obj.get("contentMediaType"),
            Some(&Value::String("application/json".to_string()))
        );

        let cbor_obj = constrained(
            CsilTypeExpression::Builtin("bytes".to_string()),
            vec![CsilControlOperator::Cbor],
        );
        assert_eq!(
            cbor_obj.get("contentMediaType"),
            Some(&Value::String("application/cbor".to_string()))
        );

        let cborseq_obj = constrained(
            CsilTypeExpression::Builtin("bytes".to_string()),
            vec![CsilControlOperator::Cborseq],
        );
        assert_eq!(
            cborseq_obj.get("contentMediaType"),
            Some(&Value::String("application/cbor-seq".to_string()))
        );
    }

    #[test]
    fn unmapped_operators_survive_as_vendor_extensions() {
        let obj = constrained(
            CsilTypeExpression::Builtin("uint".to_string()),
            vec![CsilControlOperator::Bits("flags".to_string())],
        );
        assert_eq!(
            obj.get("x-csil-bits"),
            Some(&Value::String("flags".to_string()))
        );
    }

    // For `user = { balance: decimal .ge "0.00", created_at: timestamp .ge
    // "1970-01-01T00:00:00Z" }`, `balance` is a string-typed `decimal`, so a
    // numeric `minimum` would be type-invalid. The bound must instead survive
    // as the `x-csil-minimum` string extension.
    #[test]
    fn bound_on_decimal_uses_vendor_extension_not_numeric_minimum() {
        let balance = constrained(
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                "0.00".to_string(),
            ))],
        );
        assert!(
            !balance.contains_key("minimum"),
            "decimal field must not carry a string-valued numeric `minimum`"
        );
        assert_eq!(
            balance.get("x-csil-minimum"),
            Some(&Value::String("0.00".to_string()))
        );

        // A bound on a timestamp (date-time string) is treated the same way.
        let created_at = constrained(
            CsilTypeExpression::Builtin("timestamp".to_string()),
            vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                "1970-01-01T00:00:00Z".to_string(),
            ))],
        );
        assert!(!created_at.contains_key("minimum"));
        assert_eq!(
            created_at.get("x-csil-minimum"),
            Some(&Value::String("1970-01-01T00:00:00Z".to_string()))
        );

        // A numeric field still gets the real numeric keyword.
        let count = constrained(
            CsilTypeExpression::Builtin("int".to_string()),
            vec![CsilControlOperator::GreaterEqual(
                CsilLiteralValue::Integer(0),
            )],
        );
        assert_eq!(count.get("minimum"), Some(&Value::Number(0.into())));
        assert!(!count.contains_key("x-csil-minimum"));

        // Exclusive bounds on a decimal use the exclusive vendor key.
        let exclusive = constrained(
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![CsilControlOperator::GreaterThan(CsilLiteralValue::Text(
                "0.00".to_string(),
            ))],
        );
        assert!(!exclusive.contains_key("exclusiveMinimum"));
        assert_eq!(
            exclusive.get("x-csil-exclusive-minimum"),
            Some(&Value::String("0.00".to_string()))
        );
    }

    // The core now guarantees a `decimal` bound is an Integer literal or a
    // well-formed decimal Text literal. An integer bound such as `.ge 0` must
    // render as the decimal string `"0"`, the same shape as a text bound, so a
    // reader never has to special-case integer vs. fractional decimals.
    #[test]
    fn integer_decimal_bound_renders_as_decimal_string() {
        let balance = constrained(
            CsilTypeExpression::Builtin("decimal".to_string()),
            vec![CsilControlOperator::GreaterEqual(
                CsilLiteralValue::Integer(0),
            )],
        );
        assert!(!balance.contains_key("minimum"));
        assert_eq!(
            balance.get("x-csil-minimum"),
            Some(&Value::String("0".to_string()))
        );
    }

    // `.and SomeType` and `.within SomeType` must carry the referenced type's
    // real schema, not a `Reference("SomeType")` debug blob. `.and` is an
    // `allOf` intersection member and `.within` rides as the vendor-extension
    // schema value.
    #[test]
    fn and_and_within_emit_real_type_schemas() {
        let and_obj = constrained(
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilControlOperator::And(Box::new(
                CsilTypeExpression::Reference("AllowedSet".to_string()),
            ))],
        );
        let all_of = and_obj
            .get("allOf")
            .and_then(Value::as_array)
            .expect("allOf array");
        assert_eq!(all_of.len(), 1);
        assert_eq!(
            all_of[0].get("$ref"),
            Some(&Value::String("#/$defs/AllowedSet".to_string()))
        );

        let within_obj = constrained(
            CsilTypeExpression::Builtin("text".to_string()),
            vec![CsilControlOperator::Within(Box::new(
                CsilTypeExpression::Reference("AllowedSet".to_string()),
            ))],
        );
        let within = within_obj.get("x-csil-within").expect("x-csil-within");
        // The bug rendered this as the string `"Reference(\"AllowedSet\")"`;
        // it must now be a real schema object carrying the `$ref`.
        assert!(within.is_object(), "x-csil-within must be a real schema");
        assert_eq!(
            within.get("$ref"),
            Some(&Value::String("#/$defs/AllowedSet".to_string()))
        );
    }

    fn tuple_entry(key: Option<&str>, value_type: CsilTypeExpression) -> CsilGroupEntry {
        CsilGroupEntry {
            key: key.map(|k| CsilGroupKey::Bare(k.to_string())),
            value_type,
            occurrence: None,
            metadata: Vec::new(),
            doc_comments: Vec::new(),
        }
    }

    #[test]
    fn keyless_group_entry_composes_via_all_of() {
        // `{ shared, b: bool }` — a keyless entry is group spread, composed via allOf
        // rather than crashing the generator.
        let obj = schema_for(CsilTypeExpression::Group(CsilGroupExpression {
            entries: vec![
                tuple_entry(None, CsilTypeExpression::Reference("shared".to_string())),
                tuple_entry(Some("b"), CsilTypeExpression::Builtin("bool".to_string())),
            ],
        }));
        let all_of = obj
            .get("allOf")
            .and_then(Value::as_array)
            .expect("allOf composition");
        assert_eq!(all_of.len(), 2);
    }

    fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
        CsilRule {
            name: name.to_string(),
            rule_type,
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        }
    }

    fn input_with_rules(rules: Vec<CsilRule>) -> WasmGeneratorInput {
        let mut input = create_test_input();
        input.csil_spec.rules = rules;
        input
    }

    #[test]
    fn keyless_spread_of_scalar_target_is_satisfiable() {
        // `g = text; r = { g, b: bool }` — spreading a scalar must not produce an
        // `allOf` that intersects `type: object` with `type: string`, which no
        // instance can satisfy. The spread is recorded as an annotation instead.
        let input = input_with_rules(vec![rule(
            "g",
            CsilRuleType::TypeDef(CsilTypeExpression::Builtin("text".to_string())),
        )]);
        let mut generator = JsonSchemaGenerator::new(&input);
        let schema = generator
            .generate_group_schema(
                &CsilGroupExpression {
                    entries: vec![
                        tuple_entry(None, CsilTypeExpression::Reference("g".to_string())),
                        tuple_entry(Some("b"), CsilTypeExpression::Builtin("bool".to_string())),
                    ],
                },
                "r",
            )
            .unwrap();
        let obj = schema.as_object().expect("object schema");
        assert!(
            obj.get("allOf").is_none(),
            "scalar spread must not compose via allOf"
        );
        assert_eq!(obj.get("type"), Some(&Value::String("object".to_string())));
        let props = obj
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties");
        assert!(props.contains_key("b"));
        let spread = obj
            .get("x-csil-spread")
            .and_then(Value::as_array)
            .expect("x-csil-spread note");
        assert_eq!(spread, &vec![Value::String("g".to_string())]);
    }

    #[test]
    fn keyless_spread_of_group_target_composes_via_all_of() {
        // `g = { x: int }; r = { g, b: bool }` — the legitimate group-spread case
        // must keep composing via a valid `allOf`.
        let input = input_with_rules(vec![rule(
            "g",
            CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![tuple_entry(
                    Some("x"),
                    CsilTypeExpression::Builtin("int".to_string()),
                )],
            }),
        )]);
        let mut generator = JsonSchemaGenerator::new(&input);
        let schema = generator
            .generate_group_schema(
                &CsilGroupExpression {
                    entries: vec![
                        tuple_entry(None, CsilTypeExpression::Reference("g".to_string())),
                        tuple_entry(Some("b"), CsilTypeExpression::Builtin("bool".to_string())),
                    ],
                },
                "r",
            )
            .unwrap();
        let all_of = schema
            .get("allOf")
            .and_then(Value::as_array)
            .expect("allOf composition");
        assert_eq!(all_of.len(), 2);
        assert!(
            schema.get("x-csil-spread").is_none(),
            "object spread must compose, not annotate"
        );
    }

    #[test]
    fn depends_on_simple_renders_x_depends_on_object() {
        // The simple `@depends-on field: value` form rides as an `x-depends-on`
        // object alongside the boolean form's `x-csil-depends-on` string.
        let input = create_test_input();
        let generator = JsonSchemaGenerator::new(&input);
        let mut schema = json_type("string");
        generator
            .apply_field_metadata(
                &mut schema,
                &[CsilFieldMetadata::DependsOn {
                    field: "kind".to_string(),
                    value: Some(CsilLiteralValue::Text("paid".to_string())),
                }],
            )
            .unwrap();
        let dep = schema
            .get("x-depends-on")
            .and_then(Value::as_object)
            .expect("x-depends-on object");
        assert_eq!(dep.get("field"), Some(&Value::String("kind".to_string())));
        assert_eq!(dep.get("value"), Some(&Value::String("paid".to_string())));
    }

    // A bare tuple `[text, int, bool]` becomes draft 2020-12 positional
    // validation: one `prefixItems` schema per entry, `items: false`, and the
    // length pinned to the entry count.
    #[test]
    fn tuple_maps_to_prefix_items_with_fixed_length() {
        let obj = schema_for(CsilTypeExpression::Tuple(CsilGroupExpression {
            entries: vec![
                tuple_entry(None, CsilTypeExpression::Builtin("text".to_string())),
                tuple_entry(None, CsilTypeExpression::Builtin("int".to_string())),
                tuple_entry(None, CsilTypeExpression::Builtin("bool".to_string())),
            ],
        }));

        assert_eq!(obj.get("type"), Some(&Value::String("array".to_string())));
        assert_eq!(obj.get("items"), Some(&Value::Bool(false)));
        assert_eq!(obj.get("minItems"), Some(&Value::Number(3.into())));
        assert_eq!(obj.get("maxItems"), Some(&Value::Number(3.into())));

        let prefix = obj
            .get("prefixItems")
            .and_then(Value::as_array)
            .expect("prefixItems array");
        assert_eq!(prefix.len(), 3);
        assert_eq!(
            prefix[0].get("type"),
            Some(&Value::String("string".to_string()))
        );
        assert_eq!(
            prefix[1].get("type"),
            Some(&Value::String("integer".to_string()))
        );
        assert_eq!(
            prefix[2].get("type"),
            Some(&Value::String("boolean".to_string()))
        );
    }

    // A keyed tuple `[tag: text, value: any]` carries only positional meaning in
    // JSON, so the keys are dropped and the entry value types drive the
    // positional schemas.
    #[test]
    fn keyed_tuple_drops_keys_keeps_positions() {
        let obj = schema_for(CsilTypeExpression::Tuple(CsilGroupExpression {
            entries: vec![
                tuple_entry(Some("tag"), CsilTypeExpression::Builtin("text".to_string())),
                tuple_entry(
                    Some("value"),
                    CsilTypeExpression::Builtin("any".to_string()),
                ),
            ],
        }));

        let prefix = obj
            .get("prefixItems")
            .and_then(Value::as_array)
            .expect("prefixItems array");
        assert_eq!(prefix.len(), 2);
        assert_eq!(
            prefix[0].get("type"),
            Some(&Value::String("string".to_string()))
        );
        // `any` is the empty schema, so it has no `type` keyword.
        assert!(prefix[1].as_object().is_some_and(|m| m.is_empty()));
        assert_eq!(obj.get("minItems"), Some(&Value::Number(2.into())));
    }

    fn depends_extension(condition: CsilDependsCondition) -> String {
        let input = create_test_input();
        let generator = JsonSchemaGenerator::new(&input);
        let mut schema = json_type("string");
        generator
            .apply_field_metadata(&mut schema, &[CsilFieldMetadata::DependsOnExpr(condition)])
            .unwrap();
        match schema {
            Value::Object(obj) => obj
                .get("x-csil-depends-on")
                .and_then(Value::as_str)
                .expect("x-csil-depends-on string")
                .to_string(),
            other => panic!("expected object schema, got {other:?}"),
        }
    }

    #[test]
    fn depends_on_expr_renders_readable_condition_strings() {
        // A plain comparison renders operator and quoted text value.
        assert_eq!(
            depends_extension(CsilDependsCondition::Compare {
                field: "kind".to_string(),
                op: Some(CsilDependsCompareOp::Eq),
                value: Some(CsilLiteralValue::Text("paid".to_string())),
            }),
            "kind == \"paid\""
        );

        // A bare field (no operator) is a presence check.
        assert_eq!(
            depends_extension(CsilDependsCondition::Compare {
                field: "token".to_string(),
                op: None,
                value: None,
            }),
            "token is present"
        );

        // `All` joins with " and "; `Any` joins with " or "; numeric values are
        // unquoted.
        assert_eq!(
            depends_extension(CsilDependsCondition::All(vec![
                CsilDependsCondition::Compare {
                    field: "kind".to_string(),
                    op: Some(CsilDependsCompareOp::Eq),
                    value: Some(CsilLiteralValue::Text("paid".to_string())),
                },
                CsilDependsCondition::Compare {
                    field: "amount".to_string(),
                    op: Some(CsilDependsCompareOp::Gt),
                    value: Some(CsilLiteralValue::Integer(0)),
                },
            ])),
            "kind == \"paid\" and amount > 0"
        );

        // A nested group is parenthesized so precedence is unambiguous.
        assert_eq!(
            depends_extension(CsilDependsCondition::Any(vec![
                CsilDependsCondition::Compare {
                    field: "a".to_string(),
                    op: None,
                    value: None,
                },
                CsilDependsCondition::All(vec![
                    CsilDependsCondition::Compare {
                        field: "b".to_string(),
                        op: Some(CsilDependsCompareOp::Ne),
                        value: Some(CsilLiteralValue::Bool(true)),
                    },
                    CsilDependsCondition::Compare {
                        field: "c".to_string(),
                        op: None,
                        value: None,
                    },
                ]),
            ])),
            "a is present or (b != true and c is present)"
        );
    }

    // A push-only operation carries `input_type = null`; schema generation must
    // not crash and should emit a sensible request schema for the input.
    #[test]
    fn null_input_operation_emits_sensible_request_schema() {
        let mut input = create_test_input();
        input.csil_spec.rules.push(CsilRule {
            name: "PushService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "heartbeat".to_string(),
                    input_type: CsilTypeExpression::Builtin("null".to_string()),
                    output_type: CsilTypeExpression::Reference("User".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                    doc_comments: Vec::new(),
                }],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
            doc_comments: Vec::new(),
        });
        input.csil_spec.service_count = 1;

        let mut generator = JsonSchemaGenerator::new(&input);
        let files = generator.generate().expect("generation must not crash");

        let service_file = files
            .iter()
            .find(|f| f.path.contains("service"))
            .expect("service schema file");
        let schema: Value = serde_json::from_str(&service_file.content).unwrap();

        let input_schema = schema
            .get("properties")
            .and_then(|p| p.get("heartbeat"))
            .and_then(|op| op.get("properties"))
            .and_then(|p| p.get("input"))
            .expect("input schema for push-only op");
        assert_eq!(
            input_schema.get("type"),
            Some(&Value::String("null".to_string()))
        );
    }
}
