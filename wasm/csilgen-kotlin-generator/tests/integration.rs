//! Integration tests for the Kotlin code generator.
//!
//! The generator crate is a `cdylib`, so its Rust functions cannot be linked from an
//! integration test; the behavioral assertions live in the `#[cfg(test)]` unit module
//! inside `src/lib.rs` (which can reach `process_generation` directly). These tests
//! mirror the Go generator's integration style: they validate that the input model the
//! host feeds the generator round-trips through the WASM-boundary serialization the
//! runtime uses, since a malformed input shape is the most common integration break.

use csilgen_common::*;
use std::collections::HashMap;

fn sample_input() -> WasmGeneratorInput {
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
            target: "kotlin".to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: GeneratorMetadata {
            name: "kotlin".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            target: "kotlin".to_string(),
            capabilities: vec![GeneratorCapability::BasicTypes],
            author: None,
            homepage: None,
        },
    }
}

#[test]
fn input_round_trips_through_boundary_json() {
    let input = sample_input();
    let json = serde_json::to_string(&input).expect("serialize input");
    let back: WasmGeneratorInput = serde_json::from_str(&json).expect("deserialize input");
    assert_eq!(back.config.target, "kotlin");
    assert_eq!(back.csil_spec.rules.len(), 1);
    assert_eq!(back.csil_spec.rules[0].name, "User");
}

#[test]
fn input_shape_is_what_the_generator_expects() {
    let input = sample_input();
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
