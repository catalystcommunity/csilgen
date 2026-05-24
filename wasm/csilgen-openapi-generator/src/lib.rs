//! OpenAPI specification generator for csilgen (WASM module).
//!
//! Discovered dynamically as `--target openapi` from
//! `csilgen_openapi_generator.wasm`. Operates directly on
//! `csilgen_common::CsilSpecSerialized` like the other live generators — no
//! `csilgen-core` dependency, no `Serialized → Core` shim.

mod wasm;

use csilgen_common::{
    CsilControlOperator, CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupKey,
    CsilLiteralValue, CsilOccurrence, CsilRuleType, CsilServiceDefinition, CsilServiceDirection,
    CsilServiceOperation, CsilSizeConstraint, CsilSpecSerialized, CsilTypeExpression,
    CsilValidationConstraint, GeneratedFile, GeneratedFiles, GeneratorConfig, GeneratorWarning,
    Result, WarningLevel,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Generate an OpenAPI specification from a CSIL specification, along with any
/// warnings raised during generation (e.g. operations OpenAPI cannot natively
/// model — see `generate_operation_path`).
pub fn generate_openapi_spec(
    spec: &CsilSpecSerialized,
    config: &GeneratorConfig,
) -> Result<(GeneratedFiles, Vec<GeneratorWarning>)> {
    let mut generator = OpenApiGenerator::new(config);
    let files = generator.generate(spec)?;
    Ok((files, generator.warnings))
}

struct OpenApiGenerator {
    config: GeneratorConfig,
    warnings: Vec<GeneratorWarning>,
}

impl OpenApiGenerator {
    fn new(config: &GeneratorConfig) -> Self {
        Self {
            config: config.clone(),
            warnings: Vec::new(),
        }
    }

    fn generate(&mut self, spec: &CsilSpecSerialized) -> Result<GeneratedFiles> {
        let mut openapi_spec = json!({
            "openapi": "3.0.3",
            "info": {
                "title": self.config.options.get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Generated API"),
                "version": self.config.options.get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("1.0.0"),
                "description": self.config.options.get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("API generated from CSIL specification")
            },
            "components": {
                "schemas": {}
            },
            "paths": {}
        });

        let mut type_definitions = HashMap::new();
        let mut services = Vec::new();

        for rule in &spec.rules {
            match &rule.rule_type {
                CsilRuleType::TypeDef(type_expr) => {
                    type_definitions.insert(rule.name.clone(), type_expr.clone());
                }
                CsilRuleType::GroupDef(group_expr) => {
                    type_definitions.insert(
                        rule.name.clone(),
                        CsilTypeExpression::Group(group_expr.clone()),
                    );
                }
                CsilRuleType::ServiceDef(service_def) => {
                    services.push((rule.name.clone(), service_def));
                }
                CsilRuleType::TypeChoice(choices) => {
                    type_definitions.insert(
                        rule.name.clone(),
                        CsilTypeExpression::Choice(choices.clone()),
                    );
                }
                CsilRuleType::GroupChoice(groups) => {
                    if let Some(first_group) = groups.first() {
                        type_definitions.insert(
                            rule.name.clone(),
                            CsilTypeExpression::Group(first_group.clone()),
                        );
                    }
                }
            }
        }

        self.generate_schemas(&mut openapi_spec, &type_definitions)?;

        self.generate_paths(&mut openapi_spec, &services, &type_definitions)?;

        let content = serde_json::to_string_pretty(&openapi_spec).map_err(|e| {
            csilgen_common::CsilgenError::GenerationError(format!(
                "Failed to serialize OpenAPI spec: {e}"
            ))
        })?;

        Ok(vec![GeneratedFile {
            path: "openapi.json".to_string(),
            content,
        }])
    }

    fn generate_schemas(
        &self,
        openapi_spec: &mut Value,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<()> {
        let schemas = openapi_spec
            .get_mut("components")
            .and_then(|c| c.get_mut("schemas"))
            .unwrap();

        for (name, type_expr) in type_definitions {
            let schema = self.type_expression_to_schema(type_expr, type_definitions)?;
            schemas[name] = schema;
        }

        Ok(())
    }

    fn generate_paths(
        &mut self,
        openapi_spec: &mut Value,
        services: &[(String, &CsilServiceDefinition)],
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<()> {
        let paths = openapi_spec.get_mut("paths").unwrap();

        for (service_name, service_def) in services {
            for operation in &service_def.operations {
                self.generate_operation_path(paths, service_name, operation, type_definitions)?;
            }
        }

        Ok(())
    }

    fn generate_operation_path(
        &mut self,
        paths: &mut Value,
        service_name: &str,
        operation: &CsilServiceOperation,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<()> {
        let path = format!("/{}/{}", service_name.to_lowercase(), operation.name);
        // OpenAPI describes HTTP. `->` maps to a request/response POST cleanly;
        // `<->` and `<-` need a persistent channel that OpenAPI cannot model
        // natively, so we surface a warning and tag the operation with
        // `x-csil-direction` so consumers know the HTTP shape is a documentation
        // placeholder rather than a faithful spec.
        let (method, csil_direction) = match operation.direction {
            CsilServiceDirection::Unidirectional => ("post", None),
            CsilServiceDirection::Bidirectional => ("post", Some("bidirectional")),
            CsilServiceDirection::Reverse => ("get", Some("reverse")),
        };

        if let Some(direction_label) = csil_direction {
            self.warnings.push(GeneratorWarning {
                level: WarningLevel::Warning,
                message: format!(
                    "{service_name}.{op}: OpenAPI doesn't natively model {direction_label} \
                     ({arrow}) operations; the generated HTTP {method} is a documentation \
                     placeholder. The real semantics require a persistent channel (e.g. \
                     WebSocket). The operation carries `x-csil-direction: \"{direction_label}\"` \
                     so consumers can detect it.",
                    op = operation.name,
                    arrow = match operation.direction {
                        CsilServiceDirection::Bidirectional => "<->",
                        CsilServiceDirection::Reverse => "<-",
                        CsilServiceDirection::Unidirectional => unreachable!(),
                    },
                ),
                location: None,
                suggestion: Some(
                    "Use a code target (rust/go/typescript/python) to generate the actual \
                     channel handlers + router for this operation."
                        .to_string(),
                ),
            });
        }

        let request_schema =
            self.type_expression_to_schema(&operation.input_type, type_definitions)?;
        let response_schema =
            self.type_expression_to_schema(&operation.output_type, type_definitions)?;

        let mut operation_spec = json!({
            "summary": format!("{} operation", operation.name),
            "operationId": format!("{}_{}", service_name.to_lowercase(), operation.name),
            "requestBody": {
                "required": true,
                "content": {
                    "application/json": {
                        "schema": request_schema
                    }
                }
            },
            "responses": {
                "200": {
                    "description": "Successful response",
                    "content": {
                        "application/json": {
                            "schema": response_schema
                        }
                    }
                },
                "400": {
                    "description": "Bad Request"
                },
                "500": {
                    "description": "Internal Server Error"
                }
            }
        });
        if let Some(direction_label) = csil_direction {
            operation_spec["x-csil-direction"] = json!(direction_label);
        }

        if paths.get(&path).is_none() {
            paths[&path] = json!({});
        }

        paths[&path][method] = operation_spec;

        Ok(())
    }

    fn type_expression_to_schema(
        &self,
        type_expr: &CsilTypeExpression,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<Value> {
        match type_expr {
            CsilTypeExpression::Builtin(builtin_type) => {
                Ok(self.builtin_type_to_schema(builtin_type))
            }
            CsilTypeExpression::Reference(ref_name) => Ok(json!({
                "$ref": format!("#/components/schemas/{}", ref_name)
            })),
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                let items_schema =
                    self.type_expression_to_schema(element_type, type_definitions)?;
                let mut array_schema = json!({
                    "type": "array",
                    "items": items_schema
                });

                if let Some(occ) = occurrence {
                    self.add_occurrence_constraints(&mut array_schema, occ);
                }

                Ok(array_schema)
            }
            CsilTypeExpression::Map {
                key: _,
                value,
                occurrence,
            } => {
                let additional_properties =
                    self.type_expression_to_schema(value, type_definitions)?;
                let mut object_schema = json!({
                    "type": "object",
                    "additionalProperties": additional_properties
                });

                if let Some(occ) = occurrence {
                    self.add_occurrence_constraints(&mut object_schema, occ);
                }

                Ok(object_schema)
            }
            CsilTypeExpression::Group(group_expr) => {
                let mut properties = Map::new();
                let mut required = Vec::new();

                for entry in &group_expr.entries {
                    if let Some(key) = &entry.key {
                        let property_name = self.group_key_to_string(key);
                        let property_schema =
                            self.generate_property_schema(entry, type_definitions)?;

                        properties.insert(property_name.clone(), property_schema);

                        if entry.occurrence.is_none()
                            || !self.is_optional_occurrence(&entry.occurrence)
                        {
                            required.push(Value::String(property_name));
                        }
                    }
                }

                let mut schema = json!({
                    "type": "object",
                    "properties": properties
                });

                if !required.is_empty() {
                    schema["required"] = Value::Array(required);
                }

                Ok(schema)
            }
            CsilTypeExpression::Choice(choices) => {
                let choice_schemas: Result<Vec<Value>> = choices
                    .iter()
                    .map(|choice| self.type_expression_to_schema(choice, type_definitions))
                    .collect();

                Ok(json!({
                    "oneOf": choice_schemas?
                }))
            }
            CsilTypeExpression::Range {
                start,
                end,
                inclusive: _,
            } => {
                let mut schema = json!({
                    "type": "integer"
                });

                if let Some(min) = start {
                    schema["minimum"] = json!(min);
                }

                if let Some(max) = end {
                    schema["maximum"] = json!(max);
                }

                Ok(schema)
            }
            CsilTypeExpression::Literal(literal) => Ok(self.literal_to_schema(literal)),
            CsilTypeExpression::Socket(_) | CsilTypeExpression::Plug(_) => Ok(json!({})),
            CsilTypeExpression::Constrained {
                base_type,
                constraints,
            } => {
                let mut schema = self.type_expression_to_schema(base_type, type_definitions)?;

                // Apply constraints to the schema
                for constraint in constraints {
                    match constraint {
                        CsilControlOperator::Size(size_constraint) => {
                            match size_constraint {
                                CsilSizeConstraint::Exact(val) => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(val);
                                        schema["maxLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(val);
                                        schema["maxItems"] = json!(val);
                                    }
                                }
                                CsilSizeConstraint::Range { min, max } => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(min);
                                        schema["maxLength"] = json!(max);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(min);
                                        schema["maxItems"] = json!(max);
                                    }
                                }
                                CsilSizeConstraint::Min(val) => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(val);
                                    }
                                }
                                CsilSizeConstraint::Max(val) => {
                                    if schema["type"] == "string" {
                                        schema["maxLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["maxItems"] = json!(val);
                                    }
                                }
                            }
                        }
                        CsilControlOperator::Regex(pattern)
                            if schema["type"] == "string" => {
                                schema["pattern"] = json!(pattern);
                            }
                        CsilControlOperator::Default(value) => {
                            schema["default"] = self.literal_to_json_value(value);
                        }
                        CsilControlOperator::GreaterEqual(value) => {
                            if let CsilLiteralValue::Integer(i) = value {
                                schema["minimum"] = json!(i);
                            } else if let CsilLiteralValue::Float(f) = value {
                                schema["minimum"] = json!(f);
                            }
                        }
                        CsilControlOperator::LessEqual(value) => {
                            if let CsilLiteralValue::Integer(i) = value {
                                schema["maximum"] = json!(i);
                            } else if let CsilLiteralValue::Float(f) = value {
                                schema["maximum"] = json!(f);
                            }
                        }
                        CsilControlOperator::GreaterThan(value) => {
                            if let CsilLiteralValue::Integer(i) = value {
                                schema["exclusiveMinimum"] = json!(true);
                                schema["minimum"] = json!(i);
                            } else if let CsilLiteralValue::Float(f) = value {
                                schema["exclusiveMinimum"] = json!(true);
                                schema["minimum"] = json!(f);
                            }
                        }
                        CsilControlOperator::LessThan(value) => {
                            if let CsilLiteralValue::Integer(i) = value {
                                schema["exclusiveMaximum"] = json!(true);
                                schema["maximum"] = json!(i);
                            } else if let CsilLiteralValue::Float(f) = value {
                                schema["exclusiveMaximum"] = json!(true);
                                schema["maximum"] = json!(f);
                            }
                        }
                        CsilControlOperator::Json
                            // For .json constraint in OpenAPI, we can use format
                            if schema["type"] == "string" => {
                                schema["format"] = json!("json");
                            }
                        CsilControlOperator::Cbor
                            // For .cbor constraint in OpenAPI, we can use format
                            if schema["type"] == "string" => {
                                schema["format"] = json!("byte");
                                schema["contentMediaType"] = json!("application/cbor");
                            }
                        CsilControlOperator::Cborseq
                            // For .cborseq constraint in OpenAPI, we can use format
                            if schema["type"] == "string" => {
                                schema["format"] = json!("byte");
                                schema["contentMediaType"] = json!("application/cbor-seq");
                            }
                        // Other operators not yet fully supported in OpenAPI
                        _ => {}
                    }
                }

                Ok(schema)
            }
        }
    }

    fn generate_property_schema(
        &self,
        entry: &CsilGroupEntry,
        type_definitions: &HashMap<String, CsilTypeExpression>,
    ) -> Result<Value> {
        let mut schema = self.type_expression_to_schema(&entry.value_type, type_definitions)?;

        for metadata in &entry.metadata {
            match metadata {
                CsilFieldMetadata::Visibility(visibility) => match visibility {
                    CsilFieldVisibility::SendOnly => {
                        schema["description"] =
                            json!("Send-only field (not included in responses)");
                    }
                    CsilFieldVisibility::ReceiveOnly => {
                        schema["description"] =
                            json!("Receive-only field (not included in requests)");
                    }
                    CsilFieldVisibility::Bidirectional => {}
                },
                CsilFieldMetadata::Constraint(constraint) => {
                    self.add_validation_constraint(&mut schema, constraint);
                }
                CsilFieldMetadata::Description(desc) => {
                    schema["description"] = json!(desc);
                }
                CsilFieldMetadata::DependsOn { field, value } => {
                    let dependency_desc = if let Some(val) = value {
                        format!("Depends on field '{field}' having value '{val:?}'")
                    } else {
                        format!("Depends on field '{field}'")
                    };

                    if let Some(existing_desc) = schema.get("description") {
                        let combined = format!(
                            "{}. {}",
                            existing_desc.as_str().unwrap_or(""),
                            dependency_desc
                        );
                        schema["description"] = json!(combined);
                    } else {
                        schema["description"] = json!(dependency_desc);
                    }
                }
                CsilFieldMetadata::Custom {
                    name,
                    parameters: _,
                } => {
                    if let Some(existing_desc) = schema.get("description") {
                        let combined = format!(
                            "{}. Custom hint: {}",
                            existing_desc.as_str().unwrap_or(""),
                            name
                        );
                        schema["description"] = json!(combined);
                    } else {
                        schema["description"] = json!(format!("Custom hint: {}", name));
                    }
                }
            }
        }

        if let Some(occ) = &entry.occurrence {
            self.add_occurrence_constraints(&mut schema, occ);
        }

        Ok(schema)
    }

    fn builtin_type_to_schema(&self, builtin_type: &str) -> Value {
        match builtin_type {
            "int" | "uint" | "nint" | "pint" => json!({
                "type": "integer"
            }),
            "float" | "float16" | "float32" | "float64" => json!({
                "type": "number"
            }),
            "text" | "tstr" => json!({
                "type": "string"
            }),
            "bytes" | "bstr" => json!({
                "type": "string",
                "format": "byte"
            }),
            "bool" => json!({
                "type": "boolean"
            }),
            "null" | "nil" => json!({
                "type": "null"
            }),
            "any" => json!({}),
            _ => json!({
                "type": "string",
                "description": format!("Custom type: {}", builtin_type)
            }),
        }
    }

    fn literal_to_schema(&self, literal: &CsilLiteralValue) -> Value {
        match literal {
            CsilLiteralValue::Integer(val) => json!({
                "type": "integer",
                "enum": [val]
            }),
            CsilLiteralValue::Float(val) => json!({
                "type": "number",
                "enum": [val]
            }),
            CsilLiteralValue::Text(val) => json!({
                "type": "string",
                "enum": [val]
            }),
            CsilLiteralValue::Bool(val) => json!({
                "type": "boolean",
                "enum": [val]
            }),
            CsilLiteralValue::Null => json!({
                "type": "null"
            }),
            CsilLiteralValue::Bytes(val) => json!({
                "type": "string",
                "format": "byte",
                "description": format!("Byte array of length {}", val.len())
            }),
            CsilLiteralValue::Array(elements) => {
                let json_elements: Vec<Value> = elements
                    .iter()
                    .map(|e| self.literal_to_json_value(e))
                    .collect();
                json!({
                    "type": "array",
                    "enum": [json_elements]
                })
            }
        }
    }

    fn literal_to_json_value(&self, literal: &CsilLiteralValue) -> Value {
        match literal {
            CsilLiteralValue::Integer(val) => json!(val),
            CsilLiteralValue::Float(val) => json!(val),
            CsilLiteralValue::Text(val) => json!(val),
            CsilLiteralValue::Bool(val) => json!(val),
            CsilLiteralValue::Null => json!(null),
            CsilLiteralValue::Bytes(val) => json!(val),
            CsilLiteralValue::Array(elements) => {
                let json_elements: Vec<Value> = elements
                    .iter()
                    .map(|e| self.literal_to_json_value(e))
                    .collect();
                Value::Array(json_elements)
            }
        }
    }

    fn group_key_to_string(&self, key: &CsilGroupKey) -> String {
        match key {
            CsilGroupKey::Bare(name) => name.clone(),
            CsilGroupKey::Type(_) => "typed_key".to_string(),
            CsilGroupKey::Literal(literal) => match literal {
                CsilLiteralValue::Text(s) => s.clone(),
                CsilLiteralValue::Integer(i) => i.to_string(),
                _ => "literal_key".to_string(),
            },
        }
    }

    fn add_occurrence_constraints(&self, schema: &mut Value, occurrence: &CsilOccurrence) {
        match occurrence {
            CsilOccurrence::Optional => {}
            CsilOccurrence::ZeroOrMore => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(0);
                }
            }
            CsilOccurrence::OneOrMore => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(1);
                }
            }
            CsilOccurrence::Exact(count) => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(count);
                    schema["maxItems"] = json!(count);
                }
            }
            CsilOccurrence::Range { min, max } => {
                if schema["type"] == "array" {
                    if let Some(min_val) = min {
                        schema["minItems"] = json!(min_val);
                    }
                    if let Some(max_val) = max {
                        schema["maxItems"] = json!(max_val);
                    }
                }
            }
        }
    }

    fn add_validation_constraint(&self, schema: &mut Value, constraint: &CsilValidationConstraint) {
        match constraint {
            CsilValidationConstraint::MinLength(min) => {
                schema["minLength"] = json!(min);
            }
            CsilValidationConstraint::MaxLength(max) => {
                schema["maxLength"] = json!(max);
            }
            CsilValidationConstraint::MinItems(min) => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(min);
                } else if schema["type"] == "object" {
                    schema["minProperties"] = json!(min);
                }
            }
            CsilValidationConstraint::MaxItems(max) => {
                if schema["type"] == "array" {
                    schema["maxItems"] = json!(max);
                } else if schema["type"] == "object" {
                    schema["maxProperties"] = json!(max);
                }
            }
            CsilValidationConstraint::MinValue(value) => {
                schema["minimum"] = match value {
                    CsilLiteralValue::Integer(v) => json!(v),
                    CsilLiteralValue::Float(v) => json!(v),
                    _ => json!(null),
                };
            }
            CsilValidationConstraint::MaxValue(value) => {
                schema["maximum"] = match value {
                    CsilLiteralValue::Integer(v) => json!(v),
                    CsilLiteralValue::Float(v) => json!(v),
                    _ => json!(null),
                };
            }
            CsilValidationConstraint::Custom { name, value } => {
                let constraint_desc = format!("Custom constraint '{name}': {value:?}");
                if let Some(existing_desc) = schema.get("description") {
                    let combined = format!(
                        "{}. {}",
                        existing_desc.as_str().unwrap_or(""),
                        constraint_desc
                    );
                    schema["description"] = json!(combined);
                } else {
                    schema["description"] = json!(constraint_desc);
                }
            }
        }
    }

    fn is_optional_occurrence(&self, occurrence: &Option<CsilOccurrence>) -> bool {
        matches!(
            occurrence,
            Some(CsilOccurrence::Optional) | Some(CsilOccurrence::ZeroOrMore)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::{
        CsilFieldMetadata, CsilFieldVisibility, CsilGroupEntry, CsilGroupExpression, CsilGroupKey,
        CsilLiteralValue, CsilOccurrence, CsilPosition, CsilRule, CsilRuleType,
        CsilServiceDefinition, CsilServiceDirection, CsilServiceOperation, CsilSpecSerialized,
        CsilTypeExpression, CsilValidationConstraint,
    };
    use std::collections::HashMap;

    fn create_test_config() -> GeneratorConfig {
        GeneratorConfig {
            target: "openapi".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        }
    }

    fn create_test_position() -> CsilPosition {
        CsilPosition {
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    /// Build a `CsilSpecSerialized` from rules, hiding the boilerplate of the
    /// non-rule fields (`source_content`, the counts) so test fixtures stay
    /// focused on what they're actually asserting.
    fn spec_from_rules(rules: Vec<CsilRule>) -> CsilSpecSerialized {
        let service_count = rules
            .iter()
            .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
            .count();
        CsilSpecSerialized {
            rules,
            source_content: None,
            service_count,
            fields_with_metadata_count: 0,
        }
    }

    #[test]
    fn test_generate_openapi_spec_with_basic_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "User".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("age".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, "openapi.json");

        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();
        assert_eq!(openapi_json["openapi"], "3.0.3");
        assert_eq!(openapi_json["info"]["title"], "Generated API");

        let user_schema = &openapi_json["components"]["schemas"]["User"];
        assert_eq!(user_schema["type"], "object");
        assert_eq!(user_schema["properties"]["name"]["type"], "string");
        assert_eq!(user_schema["properties"]["age"]["type"], "integer");

        let required = user_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("name")));
        assert!(!required.contains(&json!("age")));
    }

    #[test]
    fn test_generate_openapi_spec_with_service_operations() {
        let spec = spec_from_rules(vec![
            CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    }],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            },
            CsilRule {
                name: "UserService".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![
                        CsilServiceOperation {
                            name: "create_user".to_string(),
                            input_type: CsilTypeExpression::Reference("User".to_string()),
                            output_type: CsilTypeExpression::Reference("User".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                        },
                        CsilServiceOperation {
                            name: "get_user".to_string(),
                            input_type: CsilTypeExpression::Builtin("text".to_string()),
                            output_type: CsilTypeExpression::Reference("User".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: create_test_position(),
                            doc_comments: Vec::new(),
                        },
                    ],
                }),
                position: create_test_position(),
                doc_comments: Vec::new(),
            },
        ]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let paths = &openapi_json["paths"];
        assert!(paths["/userservice/create_user"]["post"].is_object());
        assert!(paths["/userservice/get_user"]["post"].is_object());

        let create_user_op = &paths["/userservice/create_user"]["post"];
        assert_eq!(create_user_op["summary"], "create_user operation");
        assert_eq!(create_user_op["operationId"], "userservice_create_user");

        let request_schema =
            &create_user_op["requestBody"]["content"]["application/json"]["schema"];
        assert_eq!(request_schema["$ref"], "#/components/schemas/User");

        let response_schema =
            &create_user_op["responses"]["200"]["content"]["application/json"]["schema"];
        assert_eq!(response_schema["$ref"], "#/components/schemas/User");
    }

    #[test]
    fn test_generate_openapi_spec_with_field_metadata() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "User".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            CsilFieldMetadata::Description("User's full name".to_string()),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(2)),
                        ],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("email".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![
                            CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly),
                            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(100)),
                        ],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("secret".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![CsilFieldMetadata::Visibility(
                            CsilFieldVisibility::ReceiveOnly,
                        )],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("age".to_string())),
                        value_type: CsilTypeExpression::Builtin("int".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![CsilFieldMetadata::DependsOn {
                            field: "status".to_string(),
                            value: Some(CsilLiteralValue::Text("verified".to_string())),
                        }],
                        doc_comments: Vec::new(),
                    },
                ],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let user_schema = &openapi_json["components"]["schemas"]["User"];

        let name_prop = &user_schema["properties"]["name"];
        assert_eq!(name_prop["description"], "User's full name");
        assert_eq!(name_prop["minLength"], 2);

        let email_prop = &user_schema["properties"]["email"];
        assert_eq!(
            email_prop["description"],
            "Send-only field (not included in responses)"
        );
        assert_eq!(email_prop["maxLength"], 100);

        let secret_prop = &user_schema["properties"]["secret"];
        assert_eq!(
            secret_prop["description"],
            "Receive-only field (not included in requests)"
        );

        let age_prop = &user_schema["properties"]["age"];
        assert!(
            age_prop["description"]
                .as_str()
                .unwrap()
                .contains("Depends on field 'status'")
        );
    }

    #[test]
    fn test_generate_openapi_spec_with_array_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "UserList".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                occurrence: Some(CsilOccurrence::Range {
                    min: Some(1),
                    max: Some(10),
                }),
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let user_list_schema = &openapi_json["components"]["schemas"]["UserList"];
        assert_eq!(user_list_schema["type"], "array");
        assert_eq!(user_list_schema["items"]["type"], "string");
        assert_eq!(user_list_schema["minItems"], 1);
        assert_eq!(user_list_schema["maxItems"], 10);
    }

    #[test]
    fn test_generate_openapi_spec_with_map_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "StringMap".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Builtin("int".to_string())),
                occurrence: None,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let string_map_schema = &openapi_json["components"]["schemas"]["StringMap"];
        assert_eq!(string_map_schema["type"], "object");
        assert_eq!(string_map_schema["additionalProperties"]["type"], "integer");
    }

    #[test]
    fn test_generate_openapi_spec_with_choice_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "StringOrInt".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                CsilTypeExpression::Builtin("text".to_string()),
                CsilTypeExpression::Builtin("int".to_string()),
            ]),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let choice_schema = &openapi_json["components"]["schemas"]["StringOrInt"];
        assert!(choice_schema["oneOf"].is_array());

        let choices = choice_schema["oneOf"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0]["type"], "string");
        assert_eq!(choices[1]["type"], "integer");
    }

    #[test]
    fn test_generate_openapi_spec_with_range_types() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Age".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Range {
                start: Some(0),
                end: Some(120),
                inclusive: true,
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let age_schema = &openapi_json["components"]["schemas"]["Age"];
        assert_eq!(age_schema["type"], "integer");
        assert_eq!(age_schema["minimum"], 0);
        assert_eq!(age_schema["maximum"], 120);
    }

    #[test]
    fn test_generate_openapi_spec_with_literal_values() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Status".to_string(),
            rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Literal(CsilLiteralValue::Text(
                "active".to_string(),
            ))),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let status_schema = &openapi_json["components"]["schemas"]["Status"];
        assert_eq!(status_schema["type"], "string");
        let enum_values = status_schema["enum"].as_array().unwrap();
        assert_eq!(enum_values[0], "active");
    }

    #[test]
    fn test_generate_openapi_spec_with_custom_config() {
        let mut config = create_test_config();
        config
            .options
            .insert("title".to_string(), json!("My Custom API"));
        config.options.insert("version".to_string(), json!("2.0.0"));
        config
            .options
            .insert("description".to_string(), json!("A custom API description"));

        let spec = spec_from_rules(vec![]);

        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        assert_eq!(openapi_json["info"]["title"], "My Custom API");
        assert_eq!(openapi_json["info"]["version"], "2.0.0");
        assert_eq!(
            openapi_json["info"]["description"],
            "A custom API description"
        );
    }

    #[test]
    fn test_generate_openapi_spec_with_bidirectional_service() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "NotificationService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "subscribe".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Bidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let subscribe_op = &openapi_json["paths"]["/notificationservice/subscribe"]["post"];
        assert!(subscribe_op.is_object());
        assert_eq!(subscribe_op["operationId"], "notificationservice_subscribe");
    }

    #[test]
    fn test_generate_openapi_spec_with_reverse_service() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "CallbackService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "webhook".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Reverse,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let webhook_op = &openapi_json["paths"]["/callbackservice/webhook"]["get"];
        assert!(webhook_op.is_object());
    }

    #[test]
    fn test_builtin_type_to_schema() {
        let generator = OpenApiGenerator::new(&create_test_config());

        assert_eq!(generator.builtin_type_to_schema("text")["type"], "string");
        assert_eq!(generator.builtin_type_to_schema("int")["type"], "integer");
        assert_eq!(generator.builtin_type_to_schema("float")["type"], "number");
        assert_eq!(generator.builtin_type_to_schema("bool")["type"], "boolean");
        assert_eq!(generator.builtin_type_to_schema("bytes")["type"], "string");
        assert_eq!(generator.builtin_type_to_schema("bytes")["format"], "byte");
    }

    #[test]
    fn test_generate_openapi_spec_validates_json() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "ComplexType".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![CsilGroupEntry {
                    key: Some(CsilGroupKey::Bare("nested".to_string())),
                    value_type: CsilTypeExpression::Array {
                        element_type: Box::new(CsilTypeExpression::Map {
                            key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                            value: Box::new(CsilTypeExpression::Choice(vec![
                                CsilTypeExpression::Builtin("int".to_string()),
                                CsilTypeExpression::Builtin("text".to_string()),
                            ])),
                            occurrence: None,
                        }),
                        occurrence: Some(CsilOccurrence::OneOrMore),
                    },
                    occurrence: None,
                    metadata: vec![],
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let config = create_test_config();
        let (result, _warnings) = generate_openapi_spec(&spec, &config).unwrap();

        let parsed_json: Value =
            serde_json::from_str(&result[0].content).expect("Generated JSON should be valid");
        assert_eq!(parsed_json["openapi"], "3.0.3");
    }

    #[test]
    fn bidirectional_op_emits_x_csil_direction_and_warning() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Chat".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "subscribe".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Bidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // Bidirectional maps to POST as a documentation placeholder, but the
        // operation carries `x-csil-direction: "bidirectional"` so consumers
        // can detect that HTTP is a stand-in.
        let op = &openapi["paths"]["/chat/subscribe"]["post"];
        assert_eq!(op["x-csil-direction"], "bidirectional");

        // A warning explains the limitation.
        assert_eq!(warnings.len(), 1);
        let msg = &warnings[0].message;
        assert!(msg.contains("Chat.subscribe"));
        assert!(msg.contains("bidirectional"));
        assert!(msg.contains("<->"));
    }

    #[test]
    fn reverse_op_emits_x_csil_direction_and_warning() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Notify".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "deleted".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Reverse,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // Reverse maps to GET (server-pushed semantics shoe-horned into HTTP).
        let op = &openapi["paths"]["/notify/deleted"]["get"];
        assert_eq!(op["x-csil-direction"], "reverse");

        assert_eq!(warnings.len(), 1);
        let msg = &warnings[0].message;
        assert!(msg.contains("Notify.deleted"));
        assert!(msg.contains("reverse"));
        assert!(msg.contains("<-"));
    }

    #[test]
    fn unidirectional_op_does_not_emit_x_csil_direction_or_warning() {
        let spec = spec_from_rules(vec![CsilRule {
            name: "Auth".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "login".to_string(),
                    input_type: CsilTypeExpression::Builtin("text".to_string()),
                    output_type: CsilTypeExpression::Builtin("text".to_string()),
                    direction: CsilServiceDirection::Unidirectional,
                    position: create_test_position(),
                    doc_comments: Vec::new(),
                }],
            }),
            position: create_test_position(),
            doc_comments: Vec::new(),
        }]);

        let (result, warnings) = generate_openapi_spec(&spec, &create_test_config()).unwrap();
        let openapi: Value = serde_json::from_str(&result[0].content).unwrap();

        // `->` is a faithful HTTP POST; no extension, no warning.
        let op = &openapi["paths"]["/auth/login"]["post"];
        assert!(op.get("x-csil-direction").is_none());
        assert!(warnings.is_empty());
    }
}
