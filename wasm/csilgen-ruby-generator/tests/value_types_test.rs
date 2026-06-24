//! Value-type emission tests: `Data.define`, optional-field initialize override,
//! defaults, and doc headers.

mod common;

use common::*;
use csilgen_common::*;

#[test]
fn plain_record_is_a_bare_data_define() {
    let s = spec(vec![group_rule(
        "user_profile",
        vec![
            bare_entry("id", builtin("int")),
            bare_entry("name", builtin("text")),
        ],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    // snake_case type -> PascalCase class; snake_case fields stay verbatim symbols.
    assert!(types.contains("UserProfile = Data.define(:id, :name)"));
    // A record with no optionals/defaults/checks needs no reopened block.
    assert!(!types.contains("Data.define(:id, :name) do"));
}

#[test]
fn optional_field_forces_initialize_override() {
    let s = spec(vec![group_rule(
        "user",
        vec![
            bare_entry("name", builtin("text")),
            optional_entry("email", builtin("text")),
        ],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("User = Data.define(:name, :email) do"));
    // Required field stays mandatory; optional defaults to nil; super forwards.
    assert!(types.contains("def initialize(name:, email: nil)"));
    assert!(types.contains("    super"));
    assert!(types.contains("end\n"));
}

#[test]
fn default_operator_yields_keyword_default() {
    let entry = constrained_entry(
        "status",
        "text",
        vec![CsilControlOperator::Default(CsilLiteralValue::Text(
            "active".to_string(),
        ))],
    );
    let s = spec(vec![group_rule("task", vec![entry])]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("def initialize(status: \"active\")"));
}

#[test]
fn doc_comments_become_header_comments() {
    let mut rule = group_rule("user", vec![bare_entry("name", builtin("text"))]);
    rule.doc_comments = vec!["A registered user.".to_string()];
    let s = spec(vec![rule]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("# A registered user."));
    // Field type appears in the header summary.
    assert!(types.contains("# name [String]"));
}

#[test]
fn decimal_and_timestamp_pull_requires() {
    let s = spec(vec![group_rule(
        "money",
        vec![
            bare_entry("amount", builtin("decimal")),
            bare_entry("seen_at", builtin("timestamp")),
        ],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.contains("require \"bigdecimal\""));
    assert!(types.contains("require \"time\""));
}

#[test]
fn optional_keyword_params_follow_required_ones() {
    // Ruby rejects a required keyword parameter after an optional one, and standardrb's
    // Style/KeywordParametersOrder flags it; the optional field is reordered to the end
    // even though it precedes a required field in the source group.
    let s = spec(vec![group_rule(
        "thing",
        vec![
            optional_entry("note", builtin("text")),
            bare_entry("id", builtin("int")),
        ],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    // Members keep source order; only the constructor reorders the optionals last.
    assert!(types.contains("Thing = Data.define(:note, :id) do"));
    assert!(types.contains("def initialize(id:, note: nil)"));
}

#[test]
fn choice_of_literals_collapses_to_one_class_in_docs() {
    // A `text / "a" / "b"` enum-style choice should read as a clean `String`, not
    // `String | Object | Object` (literal arms map to their class and dedupe).
    let choice = CsilTypeExpression::Choice(vec![
        builtin("text"),
        CsilTypeExpression::Literal(CsilLiteralValue::Text("a".to_string())),
        CsilTypeExpression::Literal(CsilLiteralValue::Text("b".to_string())),
    ]);
    let rule = CsilRule {
        name: "status".to_string(),
        rule_type: CsilRuleType::TypeDef(choice),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let types = file(&spec(vec![rule]), "ruby-typesonly", "types.rb");
    assert!(types.contains("# Status is an alias for String.\n"));
    assert!(!types.contains("Object"));
}

#[test]
fn file_ends_with_exactly_one_newline() {
    let s = spec(vec![group_rule(
        "point",
        vec![bare_entry("x", builtin("int"))],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(types.ends_with("\n"));
    assert!(!types.ends_with("\n\n"));
}

#[test]
fn plain_scalars_pull_no_requires() {
    let s = spec(vec![group_rule(
        "point",
        vec![
            bare_entry("x", builtin("int")),
            bare_entry("y", builtin("int")),
        ],
    )]);
    let types = file(&s, "ruby-typesonly", "types.rb");
    assert!(!types.contains("require \"bigdecimal\""));
    assert!(!types.contains("require \"time\""));
}
