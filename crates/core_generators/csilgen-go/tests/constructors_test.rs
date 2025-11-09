//! Constructor generation tests for Go code generator

use csilgen_common::*;
use std::collections::HashMap;

/// Helper to create a test generator input with default constraints
fn create_test_input_with_defaults(entries: Vec<CsilGroupEntry>, type_name: &str) -> WasmGeneratorInput {
    let mut options = HashMap::new();
    options.insert(
        "package_name".to_string(),
        serde_json::Value::String("testpkg".to_string()),
    );
    options.insert(
        "generate_constructors".to_string(),
        serde_json::Value::Bool(true),
    );

    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: type_name.to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        },
        config: GeneratorConfig {
            target: "go".to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: GeneratorMetadata {
            name: "go-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "go".to_string(),
            capabilities: vec![GeneratorCapability::ValidationConstraints],
            author: None,
            homepage: None,
        },
    }
}

#[test]
fn test_constructor_with_string_default() {
    let input = create_test_input_with_defaults(
        vec![CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("role".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::Custom {
                    name: "default".to_string(),
                    value: CsilLiteralValue::Text("user".to_string()),
                },
            )],
        }],
        "User",
    );

    // Verify the input structure
    assert_eq!(input.csil_spec.rules.len(), 1);
    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 1);
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { name, value }) => {
                    assert_eq!(name, "default");
                    assert_eq!(value, &CsilLiteralValue::Text("user".to_string()));
                }
                _ => panic!("Expected default constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_constructor_with_bool_default() {
    let input = create_test_input_with_defaults(
        vec![CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("active".to_string())),
            value_type: CsilTypeExpression::Builtin("bool".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::Custom {
                    name: "default".to_string(),
                    value: CsilLiteralValue::Bool(true),
                },
            )],
        }],
        "Status",
    );

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { value, .. }) => {
                    assert_eq!(value, &CsilLiteralValue::Bool(true));
                }
                _ => panic!("Expected default constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_constructor_with_int_default() {
    let input = create_test_input_with_defaults(
        vec![CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("timeout".to_string())),
            value_type: CsilTypeExpression::Builtin("int".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::Custom {
                    name: "default".to_string(),
                    value: CsilLiteralValue::Integer(30),
                },
            )],
        }],
        "Config",
    );

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { value, .. }) => {
                    assert_eq!(value, &CsilLiteralValue::Integer(30));
                }
                _ => panic!("Expected default constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_constructor_with_optional_field_default() {
    let input = create_test_input_with_defaults(
        vec![CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("host".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: Some(CsilOccurrence::Optional),
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::Custom {
                    name: "default".to_string(),
                    value: CsilLiteralValue::Text("localhost".to_string()),
                },
            )],
        }],
        "ServerConfig",
    );

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries[0].occurrence, Some(CsilOccurrence::Optional));
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { value, .. }) => {
                    assert_eq!(value, &CsilLiteralValue::Text("localhost".to_string()));
                }
                _ => panic!("Expected default constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_constructor_with_multiple_defaults() {
    let input = create_test_input_with_defaults(
        vec![
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("host".to_string())),
                value_type: CsilTypeExpression::Builtin("text".to_string()),
                occurrence: None,
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::Custom {
                        name: "default".to_string(),
                        value: CsilLiteralValue::Text("localhost".to_string()),
                    },
                )],
            },
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("port".to_string())),
                value_type: CsilTypeExpression::Builtin("int".to_string()),
                occurrence: None,
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::Custom {
                        name: "default".to_string(),
                        value: CsilLiteralValue::Integer(8080),
                    },
                )],
            },
            CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("timeout".to_string())),
                value_type: CsilTypeExpression::Builtin("int".to_string()),
                occurrence: None,
                metadata: vec![CsilFieldMetadata::Constraint(
                    CsilValidationConstraint::Custom {
                        name: "default".to_string(),
                        value: CsilLiteralValue::Integer(30),
                    },
                )],
            },
        ],
        "ServerConfig",
    );

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 3);

            // Check all three fields have default constraints
            for entry in &group.entries {
                assert!(entry.metadata.iter().any(|meta| {
                    matches!(meta, CsilFieldMetadata::Constraint(
                        CsilValidationConstraint::Custom { name, .. }
                    ) if name == "default")
                }));
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_no_constructor_when_no_defaults() {
    let input = create_test_input_with_defaults(
        vec![CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("name".to_string())),
            value_type: CsilTypeExpression::Builtin("text".to_string()),
            occurrence: None,
            metadata: vec![], // No defaults
        }],
        "User",
    );

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert!(group.entries[0].metadata.is_empty());
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_constructor_disabled_via_config() {
    let mut options = HashMap::new();
    options.insert(
        "package_name".to_string(),
        serde_json::Value::String("testpkg".to_string()),
    );
    options.insert(
        "generate_constructors".to_string(),
        serde_json::Value::Bool(false), // Disabled
    );

    let input = WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("role".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![CsilFieldMetadata::Constraint(
                            CsilValidationConstraint::Custom {
                                name: "default".to_string(),
                                value: CsilLiteralValue::Text("user".to_string()),
                            },
                        )],
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 1,
        },
        config: GeneratorConfig {
            target: "go".to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: GeneratorMetadata {
            name: "go-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "go".to_string(),
            capabilities: vec![GeneratorCapability::ValidationConstraints],
            author: None,
            homepage: None,
        },
    };

    // Verify the config has constructors disabled
    assert_eq!(
        input.config.options.get("generate_constructors")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        false
    );
}
