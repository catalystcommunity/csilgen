//! OpenAPI specification generator for csilgen

use csilgen_common::{GeneratedFile, GeneratedFiles, GeneratorConfig, Result};
use csilgen_core::ast::{
    CsilSpec, FieldVisibility, GroupEntry, GroupKey, LiteralValue, Occurrence, RuleType,
    ServiceDefinition, ServiceDirection, ServiceOperation, TypeExpression, ValidationConstraint,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// Generate OpenAPI specification from CSIL specification
pub fn generate_openapi_spec(spec: &CsilSpec, config: &GeneratorConfig) -> Result<GeneratedFiles> {
    let mut generator = OpenApiGenerator::new(config);
    generator.generate(spec)
}

struct OpenApiGenerator {
    config: GeneratorConfig,
}

impl OpenApiGenerator {
    fn new(config: &GeneratorConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    fn generate(&mut self, spec: &CsilSpec) -> Result<GeneratedFiles> {
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
                RuleType::TypeDef(type_expr) => {
                    type_definitions.insert(rule.name.clone(), type_expr.clone());
                }
                RuleType::GroupDef(group_expr) => {
                    type_definitions
                        .insert(rule.name.clone(), TypeExpression::Group(group_expr.clone()));
                }
                RuleType::ServiceDef(service_def) => {
                    services.push((rule.name.clone(), service_def));
                }
                RuleType::TypeChoice(choices) => {
                    type_definitions
                        .insert(rule.name.clone(), TypeExpression::Choice(choices.clone()));
                }
                RuleType::GroupChoice(groups) => {
                    if let Some(first_group) = groups.first() {
                        type_definitions.insert(
                            rule.name.clone(),
                            TypeExpression::Group(first_group.clone()),
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
        type_definitions: &HashMap<String, TypeExpression>,
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
        &self,
        openapi_spec: &mut Value,
        services: &[(String, &ServiceDefinition)],
        type_definitions: &HashMap<String, TypeExpression>,
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
        &self,
        paths: &mut Value,
        service_name: &str,
        operation: &ServiceOperation,
        type_definitions: &HashMap<String, TypeExpression>,
    ) -> Result<()> {
        let path = format!("/{}/{}", service_name.to_lowercase(), operation.name);
        let method = match operation.direction {
            ServiceDirection::Unidirectional => "post",
            ServiceDirection::Bidirectional => "post",
            ServiceDirection::Reverse => "get",
        };

        let request_schema =
            self.type_expression_to_schema(&operation.input_type, type_definitions)?;
        let response_schema =
            self.type_expression_to_schema(&operation.output_type, type_definitions)?;

        let operation_spec = json!({
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

        if paths.get(&path).is_none() {
            paths[&path] = json!({});
        }

        paths[&path][method] = operation_spec;

        Ok(())
    }

    fn type_expression_to_schema(
        &self,
        type_expr: &TypeExpression,
        type_definitions: &HashMap<String, TypeExpression>,
    ) -> Result<Value> {
        match type_expr {
            TypeExpression::Builtin(builtin_type) => Ok(self.builtin_type_to_schema(builtin_type)),
            TypeExpression::Reference(ref_name) => Ok(json!({
                "$ref": format!("#/components/schemas/{}", ref_name)
            })),
            TypeExpression::Array {
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
            TypeExpression::Map {
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
            TypeExpression::Group(group_expr) => {
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
            TypeExpression::Choice(choices) => {
                let choice_schemas: Result<Vec<Value>> = choices
                    .iter()
                    .map(|choice| self.type_expression_to_schema(choice, type_definitions))
                    .collect();

                Ok(json!({
                    "oneOf": choice_schemas?
                }))
            }
            TypeExpression::Range {
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
            TypeExpression::Literal(literal) => Ok(self.literal_to_schema(literal)),
            TypeExpression::Socket(_) | TypeExpression::Plug(_) => Ok(json!({})),
            TypeExpression::Constrained {
                base_type,
                constraints,
            } => {
                let mut schema = self.type_expression_to_schema(base_type, type_definitions)?;
                
                // Apply constraints to the schema
                for constraint in constraints {
                    match constraint {
                        csilgen_core::ast::ControlOperator::Size(size_constraint) => {
                            match size_constraint {
                                csilgen_core::ast::SizeConstraint::Exact(val) => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(val);
                                        schema["maxLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(val);
                                        schema["maxItems"] = json!(val);
                                    }
                                }
                                csilgen_core::ast::SizeConstraint::Range { min, max } => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(min);
                                        schema["maxLength"] = json!(max);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(min);
                                        schema["maxItems"] = json!(max);
                                    }
                                }
                                csilgen_core::ast::SizeConstraint::Min(val) => {
                                    if schema["type"] == "string" {
                                        schema["minLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["minItems"] = json!(val);
                                    }
                                }
                                csilgen_core::ast::SizeConstraint::Max(val) => {
                                    if schema["type"] == "string" {
                                        schema["maxLength"] = json!(val);
                                    } else if schema["type"] == "array" {
                                        schema["maxItems"] = json!(val);
                                    }
                                }
                            }
                        }
                        csilgen_core::ast::ControlOperator::Regex(pattern) => {
                            if schema["type"] == "string" {
                                schema["pattern"] = json!(pattern);
                            }
                        }
                        csilgen_core::ast::ControlOperator::Default(value) => {
                            schema["default"] = self.literal_to_json_value(value);
                        }
                        csilgen_core::ast::ControlOperator::GreaterEqual(value) => {
                            if let csilgen_core::ast::LiteralValue::Integer(i) = value {
                                schema["minimum"] = json!(i);
                            } else if let csilgen_core::ast::LiteralValue::Float(f) = value {
                                schema["minimum"] = json!(f);
                            }
                        }
                        csilgen_core::ast::ControlOperator::LessEqual(value) => {
                            if let csilgen_core::ast::LiteralValue::Integer(i) = value {
                                schema["maximum"] = json!(i);
                            } else if let csilgen_core::ast::LiteralValue::Float(f) = value {
                                schema["maximum"] = json!(f);
                            }
                        }
                        csilgen_core::ast::ControlOperator::GreaterThan(value) => {
                            if let csilgen_core::ast::LiteralValue::Integer(i) = value {
                                schema["exclusiveMinimum"] = json!(true);
                                schema["minimum"] = json!(i);
                            } else if let csilgen_core::ast::LiteralValue::Float(f) = value {
                                schema["exclusiveMinimum"] = json!(true);
                                schema["minimum"] = json!(f);
                            }
                        }
                        csilgen_core::ast::ControlOperator::LessThan(value) => {
                            if let csilgen_core::ast::LiteralValue::Integer(i) = value {
                                schema["exclusiveMaximum"] = json!(true);
                                schema["maximum"] = json!(i);
                            } else if let csilgen_core::ast::LiteralValue::Float(f) = value {
                                schema["exclusiveMaximum"] = json!(true);
                                schema["maximum"] = json!(f);
                            }
                        }
                        csilgen_core::ast::ControlOperator::Json => {
                            // For .json constraint in OpenAPI, we can use format
                            if schema["type"] == "string" {
                                schema["format"] = json!("json");
                            }
                        }
                        csilgen_core::ast::ControlOperator::Cbor => {
                            // For .cbor constraint in OpenAPI, we can use format
                            if schema["type"] == "string" {
                                schema["format"] = json!("byte");
                                schema["contentMediaType"] = json!("application/cbor");
                            }
                        }
                        csilgen_core::ast::ControlOperator::Cborseq => {
                            // For .cborseq constraint in OpenAPI, we can use format
                            if schema["type"] == "string" {
                                schema["format"] = json!("byte");
                                schema["contentMediaType"] = json!("application/cbor-seq");
                            }
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
        entry: &GroupEntry,
        type_definitions: &HashMap<String, TypeExpression>,
    ) -> Result<Value> {
        let mut schema = self.type_expression_to_schema(&entry.value_type, type_definitions)?;

        for metadata in &entry.metadata {
            match metadata {
                csilgen_core::ast::FieldMetadata::Visibility(visibility) => match visibility {
                    FieldVisibility::SendOnly => {
                        schema["description"] =
                            json!("Send-only field (not included in responses)");
                    }
                    FieldVisibility::ReceiveOnly => {
                        schema["description"] =
                            json!("Receive-only field (not included in requests)");
                    }
                    FieldVisibility::Bidirectional => {}
                },
                csilgen_core::ast::FieldMetadata::Constraint(constraint) => {
                    self.add_validation_constraint(&mut schema, constraint);
                }
                csilgen_core::ast::FieldMetadata::Description(desc) => {
                    schema["description"] = json!(desc);
                }
                csilgen_core::ast::FieldMetadata::DependsOn { field, value } => {
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
                csilgen_core::ast::FieldMetadata::Custom {
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

    fn literal_to_schema(&self, literal: &LiteralValue) -> Value {
        match literal {
            LiteralValue::Integer(val) => json!({
                "type": "integer",
                "enum": [val]
            }),
            LiteralValue::Float(val) => json!({
                "type": "number",
                "enum": [val]
            }),
            LiteralValue::Text(val) => json!({
                "type": "string",
                "enum": [val]
            }),
            LiteralValue::Bool(val) => json!({
                "type": "boolean",
                "enum": [val]
            }),
            LiteralValue::Null => json!({
                "type": "null"
            }),
            LiteralValue::Bytes(val) => json!({
                "type": "string",
                "format": "byte",
                "description": format!("Byte array of length {}", val.len())
            }),
            LiteralValue::Array(elements) => {
                let json_elements: Vec<Value> = elements.iter().map(|e| self.literal_to_json_value(e)).collect();
                json!({
                    "type": "array",
                    "enum": [json_elements]
                })
            }
        }
    }

    fn literal_to_json_value(&self, literal: &LiteralValue) -> Value {
        match literal {
            LiteralValue::Integer(val) => json!(val),
            LiteralValue::Float(val) => json!(val),
            LiteralValue::Text(val) => json!(val),
            LiteralValue::Bool(val) => json!(val),
            LiteralValue::Null => json!(null),
            LiteralValue::Bytes(val) => json!(val),
            LiteralValue::Array(elements) => {
                let json_elements: Vec<Value> = elements.iter().map(|e| self.literal_to_json_value(e)).collect();
                Value::Array(json_elements)
            }
        }
    }

    fn group_key_to_string(&self, key: &GroupKey) -> String {
        match key {
            GroupKey::Bare(name) => name.clone(),
            GroupKey::Type(_) => "typed_key".to_string(),
            GroupKey::Literal(literal) => match literal {
                LiteralValue::Text(s) => s.clone(),
                LiteralValue::Integer(i) => i.to_string(),
                _ => "literal_key".to_string(),
            },
        }
    }

    fn add_occurrence_constraints(&self, schema: &mut Value, occurrence: &Occurrence) {
        match occurrence {
            Occurrence::Optional => {}
            Occurrence::ZeroOrMore => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(0);
                }
            }
            Occurrence::OneOrMore => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(1);
                }
            }
            Occurrence::Exact(count) => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(count);
                    schema["maxItems"] = json!(count);
                }
            }
            Occurrence::Range { min, max } => {
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

    fn add_validation_constraint(&self, schema: &mut Value, constraint: &ValidationConstraint) {
        match constraint {
            ValidationConstraint::MinLength(min) => {
                schema["minLength"] = json!(min);
            }
            ValidationConstraint::MaxLength(max) => {
                schema["maxLength"] = json!(max);
            }
            ValidationConstraint::MinItems(min) => {
                if schema["type"] == "array" {
                    schema["minItems"] = json!(min);
                } else if schema["type"] == "object" {
                    schema["minProperties"] = json!(min);
                }
            }
            ValidationConstraint::MaxItems(max) => {
                if schema["type"] == "array" {
                    schema["maxItems"] = json!(max);
                } else if schema["type"] == "object" {
                    schema["maxProperties"] = json!(max);
                }
            }
            ValidationConstraint::MinValue(value) => {
                schema["minimum"] = match value {
                    LiteralValue::Integer(v) => json!(v),
                    LiteralValue::Float(v) => json!(v),
                    _ => json!(null),
                };
            }
            ValidationConstraint::MaxValue(value) => {
                schema["maximum"] = match value {
                    LiteralValue::Integer(v) => json!(v),
                    LiteralValue::Float(v) => json!(v),
                    _ => json!(null),
                };
            }
            ValidationConstraint::Custom { name, value } => {
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

    fn is_optional_occurrence(&self, occurrence: &Option<Occurrence>) -> bool {
        matches!(
            occurrence,
            Some(Occurrence::Optional) | Some(Occurrence::ZeroOrMore)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_core::ast::{
        CsilSpec, FieldMetadata, FieldVisibility, GroupEntry, GroupExpression, GroupKey,
        LiteralValue, Occurrence, Rule, RuleType, ServiceDefinition, ServiceDirection,
        ServiceOperation, TypeExpression, ValidationConstraint,
    };
    use csilgen_core::lexer::Position;
    use std::collections::HashMap;

    fn create_test_config() -> GeneratorConfig {
        GeneratorConfig {
            target: "openapi".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        }
    }

    fn create_test_position() -> Position {
        Position {
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    #[test]
    fn test_generate_openapi_spec_with_basic_types() {
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "User".to_string(),
                rule_type: RuleType::GroupDef(GroupExpression {
                    entries: vec![
                        GroupEntry {
                            key: Some(GroupKey::Bare("name".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("age".to_string())),
                            value_type: TypeExpression::Builtin("int".to_string()),
                            occurrence: Some(Occurrence::Optional),
                            metadata: vec![],
                        },
                    ],
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();

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
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![
                Rule {
                    name: "User".to_string(),
                    rule_type: RuleType::GroupDef(GroupExpression {
                        entries: vec![GroupEntry {
                            key: Some(GroupKey::Bare("name".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![],
                        }],
                    }),
                    position: create_test_position(),
                },
                Rule {
                    name: "UserService".to_string(),
                    rule_type: RuleType::ServiceDef(ServiceDefinition {
                        operations: vec![
                            ServiceOperation {
                                name: "create_user".to_string(),
                                input_type: TypeExpression::Reference("User".to_string()),
                                output_type: TypeExpression::Reference("User".to_string()),
                                direction: ServiceDirection::Unidirectional,
                                position: create_test_position(),
                            },
                            ServiceOperation {
                                name: "get_user".to_string(),
                                input_type: TypeExpression::Builtin("text".to_string()),
                                output_type: TypeExpression::Reference("User".to_string()),
                                direction: ServiceDirection::Unidirectional,
                                position: create_test_position(),
                            },
                        ],
                    }),
                    position: create_test_position(),
                },
            ],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
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
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "User".to_string(),
                rule_type: RuleType::GroupDef(GroupExpression {
                    entries: vec![
                        GroupEntry {
                            key: Some(GroupKey::Bare("name".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![
                                FieldMetadata::Description("User's full name".to_string()),
                                FieldMetadata::Constraint(ValidationConstraint::MinLength(2)),
                            ],
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("email".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: Some(Occurrence::Optional),
                            metadata: vec![
                                FieldMetadata::Visibility(FieldVisibility::SendOnly),
                                FieldMetadata::Constraint(ValidationConstraint::MaxLength(100)),
                            ],
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("secret".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: Some(Occurrence::Optional),
                            metadata: vec![FieldMetadata::Visibility(FieldVisibility::ReceiveOnly)],
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("age".to_string())),
                            value_type: TypeExpression::Builtin("int".to_string()),
                            occurrence: Some(Occurrence::Optional),
                            metadata: vec![FieldMetadata::DependsOn {
                                field: "status".to_string(),
                                value: Some(LiteralValue::Text("verified".to_string())),
                            }],
                        },
                    ],
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
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
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "UserList".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Array {
                    element_type: Box::new(TypeExpression::Builtin("text".to_string())),
                    occurrence: Some(Occurrence::Range {
                        min: Some(1),
                        max: Some(10),
                    }),
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let user_list_schema = &openapi_json["components"]["schemas"]["UserList"];
        assert_eq!(user_list_schema["type"], "array");
        assert_eq!(user_list_schema["items"]["type"], "string");
        assert_eq!(user_list_schema["minItems"], 1);
        assert_eq!(user_list_schema["maxItems"], 10);
    }

    #[test]
    fn test_generate_openapi_spec_with_map_types() {
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "StringMap".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Map {
                    key: Box::new(TypeExpression::Builtin("text".to_string())),
                    value: Box::new(TypeExpression::Builtin("int".to_string())),
                    occurrence: None,
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let string_map_schema = &openapi_json["components"]["schemas"]["StringMap"];
        assert_eq!(string_map_schema["type"], "object");
        assert_eq!(string_map_schema["additionalProperties"]["type"], "integer");
    }

    #[test]
    fn test_generate_openapi_spec_with_choice_types() {
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "StringOrInt".to_string(),
                rule_type: RuleType::TypeChoice(vec![
                    TypeExpression::Builtin("text".to_string()),
                    TypeExpression::Builtin("int".to_string()),
                ]),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
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
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "Age".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Range {
                    start: Some(0),
                    end: Some(120),
                    inclusive: true,
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let age_schema = &openapi_json["components"]["schemas"]["Age"];
        assert_eq!(age_schema["type"], "integer");
        assert_eq!(age_schema["minimum"], 0);
        assert_eq!(age_schema["maximum"], 120);
    }

    #[test]
    fn test_generate_openapi_spec_with_literal_values() {
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "Status".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Literal(LiteralValue::Text(
                    "active".to_string(),
                ))),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
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

        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![],
        };

        let result = generate_openapi_spec(&spec, &config).unwrap();
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
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "NotificationService".to_string(),
                rule_type: RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "subscribe".to_string(),
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("text".to_string()),
                        direction: ServiceDirection::Bidirectional,
                        position: create_test_position(),
                    }],
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
        let openapi_json: Value = serde_json::from_str(&result[0].content).unwrap();

        let subscribe_op = &openapi_json["paths"]["/notificationservice/subscribe"]["post"];
        assert!(subscribe_op.is_object());
        assert_eq!(subscribe_op["operationId"], "notificationservice_subscribe");
    }

    #[test]
    fn test_generate_openapi_spec_with_reverse_service() {
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "CallbackService".to_string(),
                rule_type: RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "webhook".to_string(),
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("text".to_string()),
                        direction: ServiceDirection::Reverse,
                        position: create_test_position(),
                    }],
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();
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
        let spec = CsilSpec {
            imports: Vec::new(),
            options: None,
            rules: vec![Rule {
                name: "ComplexType".to_string(),
                rule_type: RuleType::GroupDef(GroupExpression {
                    entries: vec![GroupEntry {
                        key: Some(GroupKey::Bare("nested".to_string())),
                        value_type: TypeExpression::Array {
                            element_type: Box::new(TypeExpression::Map {
                                key: Box::new(TypeExpression::Builtin("text".to_string())),
                                value: Box::new(TypeExpression::Choice(vec![
                                    TypeExpression::Builtin("int".to_string()),
                                    TypeExpression::Builtin("text".to_string()),
                                ])),
                                occurrence: None,
                            }),
                            occurrence: Some(Occurrence::OneOrMore),
                        },
                        occurrence: None,
                        metadata: vec![],
                    }],
                }),
                position: create_test_position(),
            }],
        };

        let config = create_test_config();
        let result = generate_openapi_spec(&spec, &config).unwrap();

        let parsed_json: Value =
            serde_json::from_str(&result[0].content).expect("Generated JSON should be valid");
        assert_eq!(parsed_json["openapi"], "3.0.3");
    }
}
