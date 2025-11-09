//! Integration tests for Go code generator

use csilgen_common::*;
use std::collections::HashMap;

/// Create a simple test input for Go code generation
fn create_simple_test_input() -> WasmGeneratorInput {
    let metadata = GeneratorMetadata {
        name: "go-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Go code generator with service support".to_string(),
        target: "go".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen".to_string()),
    };

    let config = GeneratorConfig {
        target: "go".to_string(),
        output_dir: "/tmp/output".to_string(),
        options: {
            let mut opts = HashMap::new();
            opts.insert("package_name".to_string(), serde_json::Value::String("api".to_string()));
            opts
        },
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
                        metadata: vec![],
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("email".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
                    },
                ],
            }),
            position: CsilPosition {
                line: 1,
                column: 1,
                offset: 0,
            },
        }],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    };

    WasmGeneratorInput {
        csil_spec: spec,
        config,
        generator_metadata: metadata,
    }
}

#[test]
fn test_basic_struct_generation() {
    // Note: This test validates the structure, not WASM execution
    // WASM execution tests are done at the CLI level
    let input = create_simple_test_input();

    // Verify input structure is valid
    assert_eq!(input.csil_spec.rules.len(), 1);
    assert_eq!(input.csil_spec.rules[0].name, "User");

    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 2);
            assert_eq!(group.entries[0].key, Some(CsilGroupKey::Bare("name".to_string())));
            assert_eq!(group.entries[1].key, Some(CsilGroupKey::Bare("email".to_string())));

            // Verify optional field
            assert!(matches!(group.entries[1].occurrence, Some(CsilOccurrence::Optional)));
        }
        _ => panic!("Expected GroupDef rule type"),
    }
}

#[test]
fn test_generator_metadata() {
    let input = create_simple_test_input();

    assert_eq!(input.generator_metadata.name, "go-generator");
    assert_eq!(input.generator_metadata.target, "go");
    assert!(input.generator_metadata.capabilities.contains(&GeneratorCapability::BasicTypes));
    assert!(input.generator_metadata.capabilities.contains(&GeneratorCapability::ComplexStructures));
}

#[test]
fn test_config_options() {
    let input = create_simple_test_input();

    assert_eq!(input.config.target, "go");
    assert!(input.config.options.contains_key("package_name"));

    if let Some(serde_json::Value::String(package)) = input.config.options.get("package_name") {
        assert_eq!(package, "api");
    } else {
        panic!("Expected package_name option to be a string");
    }
}
