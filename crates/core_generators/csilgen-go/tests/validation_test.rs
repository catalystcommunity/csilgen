//! Validation constraint tests for Go code generator

use csilgen_common::*;
use std::collections::HashMap;

/// Helper to create a test generator input with validation constraints
fn create_test_input_with_constraints(entries: Vec<CsilGroupEntry>) -> WasmGeneratorInput {
    let mut options = HashMap::new();
    options.insert(
        "package_name".to_string(),
        serde_json::Value::String("testpkg".to_string()),
    );
    options.insert(
        "generate_validation".to_string(),
        serde_json::Value::Bool(true),
    );

    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "TestStruct".to_string(),
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
fn test_min_length_constraint_structure() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("username".to_string())),
        value_type: CsilTypeExpression::Builtin("text".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinLength(5),
        )],
    }]);

    // Verify the constraint is properly structured
    assert_eq!(input.csil_spec.rules.len(), 1);
    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 1);
            assert_eq!(
                group.entries[0].metadata.len(),
                1,
                "Should have one constraint"
            );
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(len)) => {
                    assert_eq!(*len, 5);
                }
                _ => panic!("Expected MinLength constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_max_length_constraint_structure() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("username".to_string())),
        value_type: CsilTypeExpression::Builtin("text".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MaxLength(20),
        )],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(len)) => {
                    assert_eq!(*len, 20);
                }
                _ => panic!("Expected MaxLength constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_min_items_constraint_structure() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("tags".to_string())),
        value_type: CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
            occurrence: None,
        },
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinItems(1),
        )],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinItems(items)) => {
                    assert_eq!(*items, 1);
                }
                _ => panic!("Expected MinItems constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_max_items_constraint_structure() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("tags".to_string())),
        value_type: CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Builtin("text".to_string())),
            occurrence: None,
        },
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MaxItems(10),
        )],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxItems(items)) => {
                    assert_eq!(*items, 10);
                }
                _ => panic!("Expected MaxItems constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_min_value_integer_constraint_structure() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("age".to_string())),
        value_type: CsilTypeExpression::Builtin("int".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(18)),
        )],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinValue(
                    CsilLiteralValue::Integer(val),
                )) => {
                    assert_eq!(*val, 18);
                }
                _ => panic!("Expected MinValue constraint with Integer"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_max_value_integer_constraint_structure() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("age".to_string())),
        value_type: CsilTypeExpression::Builtin("int".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MaxValue(CsilLiteralValue::Integer(120)),
        )],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxValue(
                    CsilLiteralValue::Integer(val),
                )) => {
                    assert_eq!(*val, 120);
                }
                _ => panic!("Expected MaxValue constraint with Integer"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_min_max_value_float_constraint_structure() {
    let input = create_test_input_with_constraints(vec![
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("score".to_string())),
            value_type: CsilTypeExpression::Builtin("float".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinValue(CsilLiteralValue::Float(0.0)),
            )],
        },
        CsilGroupEntry {
            key: Some(CsilGroupKey::Bare("percentage".to_string())),
            value_type: CsilTypeExpression::Builtin("float".to_string()),
            occurrence: None,
            metadata: vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MaxValue(CsilLiteralValue::Float(100.0)),
            )],
        },
    ]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 2);

            // Check first field (score with MinValue)
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinValue(
                    CsilLiteralValue::Float(val),
                )) => {
                    assert_eq!(*val, 0.0);
                }
                _ => panic!("Expected MinValue constraint with Float for score"),
            }

            // Check second field (percentage with MaxValue)
            match &group.entries[1].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxValue(
                    CsilLiteralValue::Float(val),
                )) => {
                    assert_eq!(*val, 100.0);
                }
                _ => panic!("Expected MaxValue constraint with Float for percentage"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_regex_custom_constraint_structure() {
    let pattern = "^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,}$";
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("email".to_string())),
        value_type: CsilTypeExpression::Builtin("text".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::Custom {
                name: "regex".to_string(),
                value: CsilLiteralValue::Text(pattern.to_string()),
            },
        )],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            match &group.entries[0].metadata[0] {
                CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom {
                    name,
                    value,
                }) => {
                    assert_eq!(name, "regex");
                    match value {
                        CsilLiteralValue::Text(p) => assert_eq!(p, pattern),
                        _ => panic!("Expected Text literal value for regex pattern"),
                    }
                }
                _ => panic!("Expected Custom constraint"),
            }
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_multiple_constraints_on_field() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("username".to_string())),
        value_type: CsilTypeExpression::Builtin("text".to_string()),
        occurrence: None,
        metadata: vec![
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(3)),
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(20)),
            CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom {
                name: "regex".to_string(),
                value: CsilLiteralValue::Text("^[a-zA-Z0-9_]+$".to_string()),
            }),
        ],
    }]);

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 1);
            assert_eq!(
                group.entries[0].metadata.len(),
                3,
                "Should have three constraints"
            );

            // Verify all constraints are present
            assert!(matches!(
                &group.entries[0].metadata[0],
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(3))
            ));

            assert!(matches!(
                &group.entries[0].metadata[1],
                CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxLength(20))
            ));

            assert!(matches!(
                &group.entries[0].metadata[2],
                CsilFieldMetadata::Constraint(CsilValidationConstraint::Custom { .. })
            ));
        }
        _ => panic!("Expected GroupDef"),
    }
}

#[test]
fn test_validation_serialization() {
    let input = create_test_input_with_constraints(vec![CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("age".to_string())),
        value_type: CsilTypeExpression::Builtin("int".to_string()),
        occurrence: None,
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(18)),
        )],
    }]);

    // Verify the input can be serialized and deserialized
    let json = serde_json::to_string(&input).expect("Failed to serialize");
    let deserialized: WasmGeneratorInput =
        serde_json::from_str(&json).expect("Failed to deserialize");

    assert_eq!(deserialized.csil_spec.rules.len(), 1);
    assert_eq!(deserialized.csil_spec.rules[0].name, "TestStruct");

    // Verify config options are preserved
    assert!(deserialized.config.options.contains_key("generate_validation"));
    assert!(
        deserialized
            .config
            .options
            .get("generate_validation")
            .unwrap()
            .as_bool()
            .unwrap()
    );
}
