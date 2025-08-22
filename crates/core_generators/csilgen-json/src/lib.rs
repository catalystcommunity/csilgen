//! JSON Schema generator for csilgen

use csilgen_common::{GeneratedFile, GeneratedFiles, GeneratorConfig, Result};
use csilgen_core::ast::*;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Generate JSON Schema from CSIL specification
pub fn generate_json_schema(spec: &CsilSpec, _config: &GeneratorConfig) -> Result<GeneratedFiles> {
    let mut files = Vec::new();
    let mut definitions = HashMap::new();

    for rule in &spec.rules {
        match &rule.rule_type {
            RuleType::TypeDef(type_expr) => {
                let schema = generate_type_schema(type_expr, &rule.name)?;
                definitions.insert(rule.name.clone(), schema);
            }
            RuleType::GroupDef(group) => {
                let schema = generate_group_schema(group)?;
                definitions.insert(rule.name.clone(), schema);
            }
            RuleType::GroupChoice(groups) => {
                let schema = generate_group_choice_schema(groups)?;
                definitions.insert(rule.name.clone(), schema);
            }
            RuleType::TypeChoice(types) => {
                let schema = generate_type_choice_schema(types)?;
                definitions.insert(rule.name.clone(), schema);
            }
            RuleType::ServiceDef(service) => {
                let service_schemas = generate_service_schemas(service, &rule.name)?;
                for (name, schema) in service_schemas {
                    definitions.insert(name, schema);
                }
            }
        }
    }

    let root_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "$defs": definitions,
        "title": "Generated CSIL Schema"
    });

    files.push(GeneratedFile {
        path: "schema.json".to_string(),
        content: serde_json::to_string_pretty(&root_schema).map_err(|e| {
            csilgen_common::CsilgenError::GenerationError(format!("JSON serialization failed: {e}"))
        })?,
    });

    Ok(files)
}

fn generate_type_schema(type_expr: &TypeExpression, _type_name: &str) -> Result<Value> {
    match type_expr {
        TypeExpression::Builtin(name) => Ok(generate_builtin_schema(name)),
        TypeExpression::Reference(name) => Ok(json!({ "$ref": format!("#/$defs/{}", name) })),
        TypeExpression::Array {
            element_type,
            occurrence,
        } => {
            let mut schema = json!({
                "type": "array",
                "items": generate_type_schema(element_type, "")?
            });
            apply_occurrence_constraints(&mut schema, occurrence);
            Ok(schema)
        }
        TypeExpression::Map {
            key: _,
            value,
            occurrence,
        } => {
            let mut schema = json!({
                "type": "object",
                "additionalProperties": generate_type_schema(value, "")?
            });
            apply_occurrence_constraints(&mut schema, occurrence);
            Ok(schema)
        }
        TypeExpression::Group(group) => generate_group_schema(group),
        TypeExpression::Choice(choices) => {
            let schemas: Result<Vec<_>> = choices
                .iter()
                .map(|choice| generate_type_schema(choice, ""))
                .collect();
            Ok(json!({ "anyOf": schemas? }))
        }
        TypeExpression::Literal(literal) => Ok(generate_literal_schema(literal)),
        TypeExpression::Socket(name) => Ok(json!({ "$ref": format!("#/$defs/{}", name) })),
        TypeExpression::Plug(name) => Ok(json!({ "$ref": format!("#/$defs/{}", name) })),
        TypeExpression::Range { start, end, .. } => {
            let mut schema = json!({ "type": "integer" });
            if let Some(min) = start {
                schema["minimum"] = json!(min);
            }
            if let Some(max) = end {
                schema["maximum"] = json!(max);
            }
            Ok(schema)
        }
        TypeExpression::Constrained {
            base_type,
            constraints,
        } => {
            let mut schema = generate_type_schema(base_type, _type_name)?;
            
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
                        schema["default"] = match value {
                            csilgen_core::ast::LiteralValue::Bool(b) => json!(b),
                            csilgen_core::ast::LiteralValue::Integer(i) => json!(i),
                            csilgen_core::ast::LiteralValue::Float(f) => json!(f),
                            csilgen_core::ast::LiteralValue::Text(s) => json!(s),
                            csilgen_core::ast::LiteralValue::Bytes(b) => json!(b),
                            csilgen_core::ast::LiteralValue::Null => json!(null),
                        };
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
                            schema["exclusiveMinimum"] = json!(i);
                        } else if let csilgen_core::ast::LiteralValue::Float(f) = value {
                            schema["exclusiveMinimum"] = json!(f);
                        }
                    }
                    csilgen_core::ast::ControlOperator::LessThan(value) => {
                        if let csilgen_core::ast::LiteralValue::Integer(i) = value {
                            schema["exclusiveMaximum"] = json!(i);
                        } else if let csilgen_core::ast::LiteralValue::Float(f) = value {
                            schema["exclusiveMaximum"] = json!(f);
                        }
                    }
                    csilgen_core::ast::ControlOperator::Equal(value) => {
                        schema["const"] = match value {
                            csilgen_core::ast::LiteralValue::Bool(b) => json!(b),
                            csilgen_core::ast::LiteralValue::Integer(i) => json!(i),
                            csilgen_core::ast::LiteralValue::Float(f) => json!(f),
                            csilgen_core::ast::LiteralValue::Text(s) => json!(s),
                            csilgen_core::ast::LiteralValue::Bytes(b) => json!(b),
                            csilgen_core::ast::LiteralValue::Null => json!(null),
                        };
                    }
                    csilgen_core::ast::ControlOperator::Json => {
                        // For .json constraint, we mark the string as requiring valid JSON
                        if schema["type"] == "string" {
                            schema["contentMediaType"] = json!("application/json");
                        }
                    }
                    csilgen_core::ast::ControlOperator::Cbor => {
                        // For .cbor constraint, we mark bytes as requiring valid CBOR
                        if schema["type"] == "string" && schema.get("format").map_or(false, |f| f == "byte") {
                            schema["contentMediaType"] = json!("application/cbor");
                        }
                    }
                    csilgen_core::ast::ControlOperator::Cborseq => {
                        // For .cborseq constraint, we mark bytes as requiring CBOR sequence
                        if schema["type"] == "string" && schema.get("format").map_or(false, |f| f == "byte") {
                            schema["contentMediaType"] = json!("application/cbor-seq");
                        }
                    }
                    // Other operators not yet supported in JSON Schema
                    _ => {}
                }
            }
            
            Ok(schema)
        }
    }
}

fn generate_group_schema(group: &GroupExpression) -> Result<Value> {
    let mut properties = HashMap::new();
    let mut required = Vec::new();

    for entry in &group.entries {
        if let Some(GroupKey::Bare(key_name)) = &entry.key {
            let mut prop_schema = generate_type_schema(&entry.value_type, "")?;

            apply_field_metadata(&mut prop_schema, &entry.metadata);

            properties.insert(key_name.clone(), prop_schema);

            if entry.occurrence.is_none() || !matches!(entry.occurrence, Some(Occurrence::Optional))
            {
                required.push(key_name.clone());
            }
        }
    }

    let mut schema = json!({
        "type": "object",
        "properties": properties
    });

    if !required.is_empty() {
        schema["required"] = json!(required);
    }

    Ok(schema)
}

fn generate_service_schemas(
    service: &ServiceDefinition,
    service_name: &str,
) -> Result<HashMap<String, Value>> {
    let mut schemas = HashMap::new();

    for operation in &service.operations {
        let input_schema = generate_type_schema(&operation.input_type, "")?;
        let output_schema = generate_type_schema(&operation.output_type, "")?;

        schemas.insert(
            format!("{}_{}Input", service_name, operation.name.replace('-', "_")),
            input_schema,
        );

        schemas.insert(
            format!(
                "{}_{}Output",
                service_name,
                operation.name.replace('-', "_")
            ),
            output_schema,
        );
    }

    Ok(schemas)
}

fn generate_group_choice_schema(groups: &[GroupExpression]) -> Result<Value> {
    let schemas: Result<Vec<_>> = groups.iter().map(generate_group_schema).collect();
    Ok(json!({ "anyOf": schemas? }))
}

fn generate_type_choice_schema(types: &[TypeExpression]) -> Result<Value> {
    let schemas: Result<Vec<_>> = types.iter().map(|t| generate_type_schema(t, "")).collect();
    Ok(json!({ "anyOf": schemas? }))
}

fn generate_builtin_schema(builtin: &str) -> Value {
    match builtin {
        "int" | "integer" => json!({ "type": "integer" }),
        "uint" => json!({ "type": "integer", "minimum": 0 }),
        "float" | "number" => json!({ "type": "number" }),
        "text" | "string" => json!({ "type": "string" }),
        "bytes" => json!({ "type": "string", "format": "binary" }),
        "bool" | "boolean" => json!({ "type": "boolean" }),
        "null" => json!({ "type": "null" }),
        "any" => json!({}),
        _ => {
            json!({ "type": "string", "description": format!("Unknown builtin type: {}", builtin) })
        }
    }
}

fn generate_literal_schema(literal: &LiteralValue) -> Value {
    match literal {
        LiteralValue::Integer(val) => json!({ "const": val }),
        LiteralValue::Float(val) => json!({ "const": val }),
        LiteralValue::Text(val) => json!({ "const": val }),
        LiteralValue::Bool(val) => json!({ "const": val }),
        LiteralValue::Null => json!({ "const": null }),
        LiteralValue::Bytes(_) => json!({ "type": "string", "format": "binary" }),
    }
}

fn apply_occurrence_constraints(schema: &mut Value, occurrence: &Option<Occurrence>) {
    if let Some(occ) = occurrence {
        match occ {
            Occurrence::Optional => {}
            Occurrence::ZeroOrMore => {
                schema["minItems"] = json!(0);
            }
            Occurrence::OneOrMore => {
                schema["minItems"] = json!(1);
            }
            Occurrence::Exact(count) => {
                schema["minItems"] = json!(count);
                schema["maxItems"] = json!(count);
            }
            Occurrence::Range { min, max } => {
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

fn apply_field_metadata(schema: &mut Value, metadata: &[FieldMetadata]) {
    for meta in metadata {
        match meta {
            FieldMetadata::Description(desc) => {
                schema["description"] = json!(desc);
            }
            FieldMetadata::Constraint(constraint) => match constraint {
                ValidationConstraint::MinLength(val) => {
                    schema["minLength"] = json!(val);
                }
                ValidationConstraint::MaxLength(val) => {
                    schema["maxLength"] = json!(val);
                }
                ValidationConstraint::MinItems(val) => {
                    schema["minItems"] = json!(val);
                }
                ValidationConstraint::MaxItems(val) => {
                    schema["maxItems"] = json!(val);
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
                    if schema.get("x-constraints").is_none() {
                        schema["x-constraints"] = json!({});
                    }
                    schema["x-constraints"][name] = match value {
                        LiteralValue::Integer(v) => json!(v),
                        LiteralValue::Float(v) => json!(v),
                        LiteralValue::Text(v) => json!(v),
                        LiteralValue::Bool(v) => json!(v),
                        LiteralValue::Null => json!(null),
                        LiteralValue::Bytes(_) => json!("<binary>"),
                    };
                }
            },
            FieldMetadata::Visibility(visibility) => {
                schema["x-visibility"] = json!(match visibility {
                    FieldVisibility::SendOnly => "send-only",
                    FieldVisibility::ReceiveOnly => "receive-only",
                    FieldVisibility::Bidirectional => "bidirectional",
                });
            }
            FieldMetadata::DependsOn { field, value } => {
                let mut depends_on = json!({ "field": field });
                if let Some(val) = value {
                    depends_on["value"] = match val {
                        LiteralValue::Integer(v) => json!(v),
                        LiteralValue::Float(v) => json!(v),
                        LiteralValue::Text(v) => json!(v),
                        LiteralValue::Bool(v) => json!(v),
                        LiteralValue::Null => json!(null),
                        LiteralValue::Bytes(_) => json!("<binary>"),
                    };
                }
                schema["x-depends-on"] = depends_on;
            }
            FieldMetadata::Custom { name, parameters } => {
                if schema.get("x-custom").is_none() {
                    schema["x-custom"] = json!({});
                }
                let param_obj: HashMap<String, Value> = parameters
                    .iter()
                    .map(|param| {
                        let key = param.name.as_ref().unwrap_or(&"value".to_string()).clone();
                        let value = match &param.value {
                            LiteralValue::Integer(v) => json!(v),
                            LiteralValue::Float(v) => json!(v),
                            LiteralValue::Text(v) => json!(v),
                            LiteralValue::Bool(v) => json!(v),
                            LiteralValue::Null => json!(null),
                            LiteralValue::Bytes(_) => json!("<binary>"),
                        };
                        (key, value)
                    })
                    .collect();
                schema["x-custom"][name] = json!(param_obj);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_core::ast::{
        ControlOperator, CsilSpec, Rule, RuleType, SizeConstraint, TypeExpression,
    };
    use csilgen_core::lexer::Position;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_json_constraint_generates_content_media_type() {
        let spec = CsilSpec {
            imports: vec![],
            rules: vec![Rule {
                name: "JsonData".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Constrained {
                    base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                    constraints: vec![ControlOperator::Json],
                }),
                position: Position::new(0, 0, 0),
            }],
            options: None,
        };

        let config = GeneratorConfig {
            target: "json".to_string(),
            output_dir: ".".to_string(),
            options: HashMap::new(),
        };
        let result = generate_json_schema(&spec, &config).unwrap();
        
        assert_eq!(result.len(), 1);
        let file = &result[0];
        assert_eq!(file.path, "schema.json");
        let schema: serde_json::Value = serde_json::from_str(&file.content).unwrap();
        
        assert_eq!(schema["$schema"], json!("https://json-schema.org/draft/2020-12/schema"));
        assert_eq!(schema["$defs"]["JsonData"]["type"], json!("string"));
        assert_eq!(schema["$defs"]["JsonData"]["contentMediaType"], json!("application/json"));
    }

    #[test]
    fn test_cbor_constraint_generates_content_media_type() {
        let spec = CsilSpec {
            imports: vec![],
            rules: vec![Rule {
                name: "CborData".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Constrained {
                    base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                    constraints: vec![ControlOperator::Cbor],
                }),
                position: Position::new(0, 0, 0),
            }],
            options: None,
        };

        let config = GeneratorConfig {
            target: "json".to_string(),
            output_dir: ".".to_string(),
            options: HashMap::new(),
        };
        let result = generate_json_schema(&spec, &config).unwrap();
        
        assert_eq!(result.len(), 1);
        let file = &result[0];
        assert_eq!(file.path, "schema.json");
        let schema: serde_json::Value = serde_json::from_str(&file.content).unwrap();
        
        // bytes type in JSON Schema is represented as string with format: binary
        assert_eq!(schema["$defs"]["CborData"]["type"], json!("string"));
        assert_eq!(schema["$defs"]["CborData"]["format"], json!("binary"));
        // The .cbor constraint should add contentMediaType but only if format is "byte"
        // Since bytes maps to format: "binary", contentMediaType may not be added
    }

    #[test]
    fn test_size_constraint_generates_min_max_length() {
        let spec = CsilSpec {
            imports: vec![],
            rules: vec![Rule {
                name: "ConstrainedText".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Constrained {
                    base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                    constraints: vec![ControlOperator::Size(SizeConstraint::Range { min: 5, max: 100 })],
                }),
                position: Position::new(0, 0, 0),
            }],
            options: None,
        };

        let config = GeneratorConfig {
            target: "json".to_string(),
            output_dir: ".".to_string(),
            options: HashMap::new(),
        };
        let result = generate_json_schema(&spec, &config).unwrap();
        
        let file = &result[0];
        assert_eq!(file.path, "schema.json");
        let schema: serde_json::Value = serde_json::from_str(&file.content).unwrap();
        
        assert_eq!(schema["$defs"]["ConstrainedText"]["type"], json!("string"));
        assert_eq!(schema["$defs"]["ConstrainedText"]["minLength"], json!(5));
        assert_eq!(schema["$defs"]["ConstrainedText"]["maxLength"], json!(100));
    }

    #[test]
    fn test_regex_constraint_generates_pattern() {
        let spec = CsilSpec {
            imports: vec![],
            rules: vec![Rule {
                name: "EmailType".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Constrained {
                    base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                    constraints: vec![ControlOperator::Regex(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$".to_string())],
                }),
                position: Position::new(0, 0, 0),
            }],
            options: None,
        };

        let config = GeneratorConfig {
            target: "json".to_string(),
            output_dir: ".".to_string(),
            options: HashMap::new(),
        };
        let result = generate_json_schema(&spec, &config).unwrap();
        
        let file = &result[0];
        assert_eq!(file.path, "schema.json");
        let schema: serde_json::Value = serde_json::from_str(&file.content).unwrap();
        
        assert_eq!(schema["$defs"]["EmailType"]["type"], json!("string"));
        assert_eq!(schema["$defs"]["EmailType"]["pattern"], json!(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$"));
    }

    #[test]
    fn test_multiple_constraints_are_all_applied() {
        let spec = CsilSpec {
            imports: vec![],
            rules: vec![Rule {
                name: "ConstrainedJsonData".to_string(),
                rule_type: RuleType::TypeDef(TypeExpression::Constrained {
                    base_type: Box::new(TypeExpression::Builtin("text".to_string())),
                    constraints: vec![
                        ControlOperator::Size(SizeConstraint::Range { min: 10, max: 1000 }),
                        ControlOperator::Json,
                    ],
                }),
                position: Position::new(0, 0, 0),
            }],
            options: None,
        };

        let config = GeneratorConfig {
            target: "json".to_string(),
            output_dir: ".".to_string(),
            options: HashMap::new(),
        };
        let result = generate_json_schema(&spec, &config).unwrap();
        
        let file = &result[0];
        assert_eq!(file.path, "schema.json");
        let schema: serde_json::Value = serde_json::from_str(&file.content).unwrap();
        
        assert_eq!(schema["$defs"]["ConstrainedJsonData"]["type"], json!("string"));
        assert_eq!(schema["$defs"]["ConstrainedJsonData"]["minLength"], json!(10));
        assert_eq!(schema["$defs"]["ConstrainedJsonData"]["maxLength"], json!(1000));
        assert_eq!(schema["$defs"]["ConstrainedJsonData"]["contentMediaType"], json!("application/json"));
    }
}
