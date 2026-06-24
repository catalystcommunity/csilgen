//! Integration tests for the Java code generator input contract.
//!
//! The substantive emission tests live in the crate's `#[cfg(test)] mod tests`
//! (a cdylib can't be linked as a normal library from `tests/`, so the private
//! generation functions are exercised there). These tests guard the shape of the
//! `WasmGeneratorInput` the generator consumes, mirroring the Go generator's suite.

use csilgen_common::*;
use std::collections::HashMap;

fn create_simple_test_input() -> WasmGeneratorInput {
    let metadata = GeneratorMetadata {
        name: "java-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Java code generator".to_string(),
        target: "java".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: None,
    };

    let config = GeneratorConfig {
        target: "java".to_string(),
        output_dir: "/tmp/output".to_string(),
        options: {
            let mut opts = HashMap::new();
            opts.insert(
                "java_package".to_string(),
                serde_json::Value::String("community.catalyst.demo".to_string()),
            );
            opts
        },
    };

    let spec = CsilSpecSerialized {
        rules: vec![CsilRule {
            name: "User".to_string(),
            rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                entries: vec![
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("display_name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: Vec::new(),
                    },
                    CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("email".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: Some(CsilOccurrence::Optional),
                        metadata: vec![],
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
fn test_input_structure_is_valid() {
    let input = create_simple_test_input();
    assert_eq!(input.csil_spec.rules.len(), 1);
    assert_eq!(input.csil_spec.rules[0].name, "User");
    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 2);
            assert!(matches!(
                group.entries[1].occurrence,
                Some(CsilOccurrence::Optional)
            ));
        }
        _ => panic!("expected GroupDef"),
    }
}

#[test]
fn test_generator_metadata_targets_java() {
    let input = create_simple_test_input();
    assert_eq!(input.generator_metadata.target, "java");
    assert!(
        input
            .generator_metadata
            .capabilities
            .contains(&GeneratorCapability::Services)
    );
}

#[test]
fn test_config_carries_java_package_option() {
    let input = create_simple_test_input();
    assert_eq!(input.config.target, "java");
    if let Some(serde_json::Value::String(pkg)) = input.config.options.get("java_package") {
        assert_eq!(pkg, "community.catalyst.demo");
    } else {
        panic!("expected java_package option");
    }
}
