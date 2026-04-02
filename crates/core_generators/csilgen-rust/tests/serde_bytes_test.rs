use csilgen_common::GeneratorConfig;
use csilgen_core::ast::*;
use csilgen_core::lexer::Position;
use csilgen_rust::generate_rust_code;
use std::collections::HashMap;

fn default_position() -> Position {
    Position {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn default_config() -> GeneratorConfig {
    GeneratorConfig {
        target: "rust".to_string(),
        output_dir: "/tmp/output".to_string(),
        options: HashMap::new(),
    }
}

fn make_spec(rules: Vec<Rule>) -> CsilSpec {
    CsilSpec {
        imports: vec![],
        options: None,
        rules,
    }
}

fn make_group_rule(name: &str, entries: Vec<GroupEntry>) -> Rule {
    Rule {
        name: name.to_string(),
        rule_type: RuleType::GroupDef(GroupExpression { entries }),
        position: default_position(),
    }
}

fn make_entry(name: &str, value_type: TypeExpression) -> GroupEntry {
    GroupEntry {
        key: Some(GroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
    }
}

fn make_optional_entry(name: &str, value_type: TypeExpression) -> GroupEntry {
    GroupEntry {
        key: Some(GroupKey::Bare(name.to_string())),
        value_type,
        occurrence: Some(Occurrence::Optional),
        metadata: vec![],
    }
}

#[test]
fn bytes_field_gets_serde_bytes_annotation() {
    let spec = make_spec(vec![make_group_rule(
        "MyStruct",
        vec![
            make_entry("data", TypeExpression::Builtin("bytes".to_string())),
            make_entry("name", TypeExpression::Builtin("text".to_string())),
        ],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("#[serde(with = \"serde_bytes\")]"),
        "bytes field should have serde_bytes annotation, got:\n{code}"
    );
    assert!(
        code.contains("use serde_bytes;"),
        "should include serde_bytes import, got:\n{code}"
    );

    let lines: Vec<&str> = code.lines().collect();
    let serde_bytes_line = lines
        .iter()
        .position(|l| l.contains("with = \"serde_bytes\""))
        .unwrap();
    let next_line = lines[serde_bytes_line + 1];
    assert!(
        next_line.contains("pub data: Vec<u8>"),
        "serde_bytes annotation should be on the bytes field, next line was: {next_line}"
    );
}

#[test]
fn bstr_field_gets_serde_bytes_annotation() {
    let spec = make_spec(vec![make_group_rule(
        "MyStruct",
        vec![make_entry(
            "payload",
            TypeExpression::Builtin("bstr".to_string()),
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("#[serde(with = \"serde_bytes\")]"),
        "bstr field should have serde_bytes annotation, got:\n{code}"
    );
    assert!(
        code.contains("pub payload: Vec<u8>"),
        "bstr should map to Vec<u8>, got:\n{code}"
    );
}

#[test]
fn optional_bytes_field_gets_both_annotations() {
    let spec = make_spec(vec![make_group_rule(
        "MyStruct",
        vec![make_optional_entry(
            "data",
            TypeExpression::Builtin("bytes".to_string()),
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("with = \"serde_bytes\""),
        "optional bytes should have serde_bytes, got:\n{code}"
    );
    assert!(
        code.contains("skip_serializing_if = \"Option::is_none\""),
        "optional bytes should have skip_serializing_if, got:\n{code}"
    );
    assert!(
        code.contains("pub data: Option<Vec<u8>>"),
        "optional bytes should be Option<Vec<u8>>, got:\n{code}"
    );
}

#[test]
fn non_bytes_field_does_not_get_serde_bytes() {
    let spec = make_spec(vec![make_group_rule(
        "MyStruct",
        vec![
            make_entry("name", TypeExpression::Builtin("text".to_string())),
            make_entry("age", TypeExpression::Builtin("uint".to_string())),
        ],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        !code.contains("serde_bytes"),
        "non-bytes fields should not have serde_bytes, got:\n{code}"
    );
}

#[test]
fn constrained_bytes_gets_serde_bytes_annotation() {
    let spec = make_spec(vec![make_group_rule(
        "MyStruct",
        vec![make_entry(
            "payload",
            TypeExpression::Constrained {
                base_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                constraints: vec![ControlOperator::Size(SizeConstraint::Max(1024))],
            },
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        code.contains("#[serde(with = \"serde_bytes\")]"),
        "constrained bytes should still get serde_bytes, got:\n{code}"
    );
}

#[test]
fn mixed_struct_annotates_only_bytes_fields() {
    let spec = make_spec(vec![make_group_rule(
        "SignedIdentityAssertion",
        vec![
            make_entry("assertion", TypeExpression::Builtin("bytes".to_string())),
            make_entry(
                "signing_key_id",
                TypeExpression::Builtin("text".to_string()),
            ),
            make_entry("signature", TypeExpression::Builtin("bytes".to_string())),
        ],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    let serde_bytes_count = code.matches("with = \"serde_bytes\"").count();
    assert_eq!(
        serde_bytes_count, 2,
        "should have exactly 2 serde_bytes annotations (for assertion and signature), got {serde_bytes_count} in:\n{code}"
    );

    assert!(
        !code.contains("signing_key_id")
            || {
                let lines: Vec<&str> = code.lines().collect();
                let key_id_line = lines
                    .iter()
                    .position(|l| l.contains("signing_key_id"))
                    .unwrap();
                !lines[key_id_line - 1].contains("serde_bytes")
            },
        "signing_key_id should not have serde_bytes annotation"
    );
}

#[test]
fn array_of_bytes_does_not_get_serde_bytes() {
    let spec = make_spec(vec![make_group_rule(
        "MyStruct",
        vec![make_entry(
            "chunks",
            TypeExpression::Array {
                element_type: Box::new(TypeExpression::Builtin("bytes".to_string())),
                occurrence: Some(Occurrence::ZeroOrMore),
            },
        )],
    )]);

    let files = generate_rust_code(&spec, &default_config()).unwrap();
    let code = &files[0].content;

    assert!(
        !code.contains("with = \"serde_bytes\""),
        "array of bytes (Vec<Vec<u8>>) should not get serde_bytes since the crate doesn't support nested vecs, got:\n{code}"
    );
}
