//! Integration tests for the Swift generator: these exercise the WASM-boundary
//! input/output plumbing (serde round-trips, metadata) the way the Go generator's
//! integration tests do. The codegen logic itself is covered by the in-crate unit
//! tests in `src/tests.rs`.

use csilgen_common::*;
use std::collections::HashMap;

fn simple_input() -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![
                        CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("user_name".to_string())),
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
        },
        config: GeneratorConfig {
            target: "swift".to_string(),
            output_dir: "/tmp/out".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: GeneratorMetadata {
            name: "swift-generator".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            target: "swift".to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: None,
            homepage: None,
        },
    }
}

#[test]
fn input_serializes_and_round_trips() {
    let input = simple_input();
    let json = serde_json::to_string(&input).expect("serialize");
    let back: WasmGeneratorInput = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.config.target, "swift");
    assert_eq!(back.csil_spec.rules.len(), 1);
    assert_eq!(back.csil_spec.rules[0].name, "User");
}

#[test]
fn group_def_shape_is_preserved() {
    let input = simple_input();
    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 2);
            assert!(matches!(
                group.entries[0].key,
                Some(CsilGroupKey::Bare(ref n)) if n == "user_name"
            ));
        }
        other => panic!("expected GroupDef, got {other:?}"),
    }
}
