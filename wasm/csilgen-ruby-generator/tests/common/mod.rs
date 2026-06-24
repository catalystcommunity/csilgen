//! Shared builders for the Ruby generator's integration tests. These construct
//! `CsilSpecSerialized` + `GeneratorConfig` inputs and run the generator's public
//! entry so each test can assert on the emitted Ruby strings.

#![allow(dead_code)]

use csilgen_common::*;
pub use csilgen_ruby_generator::generate_ruby_code_from_serialized;
use std::collections::HashMap;

pub fn config(target: &str) -> GeneratorConfig {
    GeneratorConfig {
        target: target.to_string(),
        output_dir: "/tmp".to_string(),
        options: HashMap::new(),
    }
}

pub fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

pub fn bare_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

pub fn optional_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

pub fn constrained_entry(
    name: &str,
    base: &str,
    constraints: Vec<CsilControlOperator>,
) -> CsilGroupEntry {
    bare_entry(
        name,
        CsilTypeExpression::Constrained {
            base_type: Box::new(CsilTypeExpression::Builtin(base.to_string())),
            constraints,
        },
    )
}

pub fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

pub fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

pub fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

pub fn op(
    name: &str,
    input: CsilTypeExpression,
    output: CsilTypeExpression,
    direction: CsilServiceDirection,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: input,
        output_type: output,
        direction,
        position: pos(),
        doc_comments: Vec::new(),
        wire_id: None,
    }
}

pub fn op_wire(
    name: &str,
    input: CsilTypeExpression,
    output: CsilTypeExpression,
    direction: CsilServiceDirection,
    wire_id: u64,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: input,
        output_type: output,
        direction,
        position: pos(),
        doc_comments: Vec::new(),
        wire_id: Some(wire_id),
    }
}

pub fn service_rule(
    name: &str,
    operations: Vec<CsilServiceOperation>,
    wire_id: Option<u64>,
) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations,
            wire_id,
        }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

pub fn spec(rules: Vec<CsilRule>) -> CsilSpecSerialized {
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

/// Run the generator and return the file at `path`, panicking with context if absent.
pub fn file(spec: &CsilSpecSerialized, target: &str, path: &str) -> String {
    let files =
        generate_ruby_code_from_serialized(spec, &config(target)).expect("generation succeeded");
    files
        .into_iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("expected {path} to be emitted"))
        .content
}

/// Run the generator and return all emitted file paths.
pub fn paths(spec: &CsilSpecSerialized, target: &str) -> Vec<String> {
    generate_ruby_code_from_serialized(spec, &config(target))
        .expect("generation succeeded")
        .into_iter()
        .map(|f| f.path)
        .collect()
}
