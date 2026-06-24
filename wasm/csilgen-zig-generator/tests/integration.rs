//! Integration sanity checks for the Zig generator. Like the Go/C generator's
//! external tests, these only build a `WasmGeneratorInput` and assert on the AST
//! structure (the private codegen functions are exercised by the in-module tests
//! in `src/tests.rs`).

use csilgen_common::*;
use std::collections::HashMap;

fn simple_input() -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: vec![CsilRule {
                name: "User".to_string(),
                rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                    entries: vec![CsilGroupEntry {
                        key: Some(CsilGroupKey::Bare("name".to_string())),
                        value_type: CsilTypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![],
                        doc_comments: vec![],
                    }],
                }),
                position: CsilPosition {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
                doc_comments: vec![],
            }],
            source_content: None,
            service_count: 0,
            fields_with_metadata_count: 0,
        },
        config: GeneratorConfig {
            target: "zig".to_string(),
            output_dir: "/tmp/output".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: GeneratorMetadata {
            name: "zig-generator".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            target: "zig".to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: None,
            homepage: None,
        },
    }
}

#[test]
fn input_round_trips_through_serde() {
    let input = simple_input();
    let json = serde_json::to_string(&input).unwrap();
    let back: WasmGeneratorInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.csil_spec.rules.len(), 1);
    assert_eq!(back.csil_spec.rules[0].name, "User");
    assert_eq!(back.config.target, "zig");
}

#[test]
fn basic_struct_shape_is_a_group() {
    let input = simple_input();
    match &input.csil_spec.rules[0].rule_type {
        CsilRuleType::GroupDef(group) => {
            assert_eq!(group.entries.len(), 1);
            assert!(matches!(
                group.entries[0].key,
                Some(CsilGroupKey::Bare(ref n)) if n == "name"
            ));
        }
        other => panic!("expected GroupDef, got {other:?}"),
    }
}
