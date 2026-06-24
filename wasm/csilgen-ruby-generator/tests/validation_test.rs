//! Validation: both constraint systems (`@`-annotations and `.`-control-operators),
//! optional-field guards, ordered decimal/timestamp bounds, and encoding-only comments.

mod common;

use common::*;
use csilgen_common::*;

fn entry_with_meta(
    name: &str,
    value_type: CsilTypeExpression,
    metadata: Vec<CsilFieldMetadata>,
) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata,
        doc_comments: Vec::new(),
    }
}

#[test]
fn min_length_annotation_emits_guard() {
    let s = spec(vec![group_rule(
        "user",
        vec![entry_with_meta(
            "name",
            builtin("text"),
            vec![CsilFieldMetadata::Constraint(
                CsilValidationConstraint::MinLength(3),
            )],
        )],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("def validate"));
    assert!(types.contains(
        "raise ArgumentError, \"field 'name' must have at least 3 characters\" if name.length < 3"
    ));
    assert!(types.contains("    nil\n"));
}

#[test]
fn control_operator_comparisons_emit_guards() {
    let s = spec(vec![group_rule(
        "score",
        vec![constrained_entry(
            "value",
            "int",
            vec![
                CsilControlOperator::GreaterEqual(CsilLiteralValue::Integer(0)),
                CsilControlOperator::LessEqual(CsilLiteralValue::Integer(100)),
            ],
        )],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(
        types.contains("raise ArgumentError, \"field 'value' must be at least 0\" if value < 0")
    );
    assert!(
        types.contains("raise ArgumentError, \"field 'value' must be at most 100\" if value > 100")
    );
}

#[test]
fn optional_field_check_is_nil_guarded() {
    let entry = CsilGroupEntry {
        key: Some(CsilGroupKey::Bare("nickname".to_string())),
        value_type: builtin("text"),
        occurrence: Some(CsilOccurrence::Optional),
        metadata: vec![CsilFieldMetadata::Constraint(
            CsilValidationConstraint::MaxLength(8),
        )],
        doc_comments: Vec::new(),
    };
    let s = spec(vec![group_rule("user", vec![entry])]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("if !nickname.nil? && nickname.length > 8"));
}

#[test]
fn regex_uses_regexp_new_and_match_predicate() {
    let s = spec(vec![group_rule(
        "user",
        vec![constrained_entry(
            "handle",
            "text",
            vec![CsilControlOperator::Regex("^[a-z]+$".to_string())],
        )],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("!handle.match?(Regexp.new(\"^[a-z]+$\"))"));
}

#[test]
fn decimal_bound_parses_through_bigdecimal() {
    let s = spec(vec![group_rule(
        "money",
        vec![constrained_entry(
            "amount",
            "decimal",
            vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                "0.00".to_string(),
            ))],
        )],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("if amount < BigDecimal(\"0.00\")"));
}

#[test]
fn timestamp_bound_parses_through_time_iso8601() {
    let s = spec(vec![group_rule(
        "event",
        vec![constrained_entry(
            "at",
            "timestamp",
            vec![CsilControlOperator::GreaterEqual(CsilLiteralValue::Text(
                "2020-01-01T00:00:00Z".to_string(),
            ))],
        )],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("if at < Time.iso8601(\"2020-01-01T00:00:00Z\")"));
}

#[test]
fn encoding_only_operators_are_comments_not_checks() {
    let s = spec(vec![group_rule(
        "blob",
        vec![constrained_entry(
            "body",
            "bytes",
            vec![CsilControlOperator::Cbor],
        )],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    // An embedded-encoding operator alone yields no validate method.
    assert!(!types.contains("def validate"));
}

#[test]
fn no_checks_means_no_validate_method() {
    let s = spec(vec![group_rule(
        "plain",
        vec![bare_entry("name", builtin("text"))],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(!types.contains("def validate"));
}
