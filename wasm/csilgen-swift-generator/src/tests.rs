//! Unit tests for the Swift generator's emitter functions.

use super::*;

#[test]
fn decoder_checks_declared_lengths_before_conversion_or_allocation() {
    assert_eq!(
        CODEC_RUNTIME_SWIFT
            .matches("guard arg <= UInt64(b.count - pos) else")
            .count(),
        4
    );
    assert!(CODEC_RUNTIME_SWIFT.contains("guard depth <= 64, pos < b.count"));
    assert!(CODEC_RUNTIME_SWIFT.contains("guard s.utf8.elementsEqual(bytes)"));
    assert!(CODEC_RUNTIME_SWIFT.contains("b.count - pos - 1 >= width"));
}
use csilgen_common::{
    CsilGroupExpression, CsilPosition, CsilRule, CsilServiceOperation, CsilSpecSerialized,
    GeneratorConfig,
};
use std::collections::HashMap;

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn input_from_rules(
    target: &str,
    rules: Vec<CsilRule>,
    service_count: usize,
) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules,
            source_content: None,
            service_count,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        },
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp".to_string(),
            options: HashMap::new(),
        },
        generator_metadata: GeneratorMetadata {
            name: "swift".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            target: "swift".to_string(),
            capabilities: vec![],
            author: None,
            homepage: None,
        },
    }
}

/// Run `input` through the same `csilgen_common::hoist_inline_composites` pass
/// `build_files` applies before calling any emitter — a test exercising a hoisted
/// inline-choice field/array-element/map-value/tuple-element position must replicate
/// that step itself, since `generate_types`/`generate_codec` no longer hoist on their
/// own (hoisting now happens once, at the `build_files` entry point). Mirrors how the
/// ocaml generator's tests call `csilgen_common::hoist_inline_composites` directly for
/// the same reason.
fn hoisted(input: WasmGeneratorInput) -> WasmGeneratorInput {
    let mut hoisted = input.clone();
    hoisted.csil_spec = csilgen_common::hoist_inline_composites(
        &input.csil_spec,
        csilgen_common::HoistOptions {
            hoist_all_literal_choices: true,
        },
    );
    hoisted
}

fn bare_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

fn opt_entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    let mut e = bare_entry(name, value_type);
    e.occurrence = Some(CsilOccurrence::Optional);
    e
}

fn group_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn make_op(
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

fn service_rule(name: &str, ops: Vec<CsilServiceOperation>, wire_id: Option<u64>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: ops,
            wire_id,
        }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

#[test]
fn type_mapping_covers_builtins() {
    assert_eq!(map_type(&builtin("int"), false), "Int64");
    assert_eq!(map_type(&builtin("uint"), false), "UInt64");
    assert_eq!(map_type(&builtin("float"), false), "Double");
    assert_eq!(map_type(&builtin("text"), false), "String");
    assert_eq!(map_type(&builtin("tstr"), false), "String");
    assert_eq!(map_type(&builtin("bytes"), false), "[UInt8]");
    assert_eq!(map_type(&builtin("bstr"), false), "[UInt8]");
    assert_eq!(map_type(&builtin("bool"), false), "Bool");
    assert_eq!(map_type(&reference("User"), false), "User");
    assert_eq!(map_type(&builtin("text"), true), "String?");
}

#[test]
fn array_and_map_mapping() {
    let arr = CsilTypeExpression::Array {
        element_type: Box::new(builtin("int")),
        occurrence: None,
    };
    assert_eq!(map_type(&arr, false), "[Int64]");
    let map = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("bytes")),
        occurrence: None,
    };
    assert_eq!(map_type(&map, false), "[String: [UInt8]]");
}

/// An optional `bytes` field carries three distinct states — absent, present-and-empty,
/// present-and-non-empty — and the codec must decide presence by whether the value is
/// set, never by whether it is non-empty (cbor-wire-contract.md "Optional fields"). An
/// `isEmpty` guard would collapse present-empty into absent and silently lose a caller's
/// "replace this with nothing".
///
/// Swift is asserted at the source level rather than executed: there is no `swiftc` in
/// the toolchain set, and unlike the other 13 languages Swift has no `tests/interop`
/// harness to run it end to end.
#[test]
fn optional_bytes_encodes_on_presence_not_emptiness() {
    let input = input_from_rules(
        "swift",
        vec![group_rule(
            "UpdateRequest",
            vec![
                bare_entry("id", builtin("text")),
                opt_entry("payload", builtin("bytes")),
            ],
        )],
        0,
    );
    let types = generate_types(&input).expect("types emitted");
    let codec = generate_codec(&input).expect("codec emitted");

    // The field type must be able to hold all three states: `Optional` distinguishes
    // nil (absent) from `.some([])` (present-and-empty).
    assert!(
        types.contains("public let payload: [UInt8]?"),
        "optional bytes needs a presence-carrying type:\n{types}"
    );
    // Encode gates on presence (`if let`), not on emptiness.
    assert!(
        codec.contains(
            "if let csilV = self.payload { csilEntries.append((.text(\"payload\"), .bytes(csilV))) }"
        ),
        "encode must gate on presence, not emptiness:\n{codec}"
    );
    assert!(
        !codec.contains("payload.isEmpty"),
        "encode must not gate on emptiness:\n{codec}"
    );
    // Decode gates on the key being present in the map; a present empty byte string
    // therefore stays `.some([])` rather than collapsing to nil.
    assert!(
        codec.contains("if let csilV = CsilCbor.mapGet(cborValue, \"payload\")"),
        "decode must gate on key presence:\n{codec}"
    );
}

#[test]
fn snake_case_field_becomes_camel_with_verbatim_wire_key() {
    let rule = group_rule(
        "Task",
        vec![
            bare_entry("current_state", builtin("text")),
            opt_entry("note", builtin("text")),
        ],
    );
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public struct Task: Equatable, Sendable"));
    // snake_case -> camelCase identifier...
    assert!(types.contains("public let currentState: String"));
    assert!(types.contains("public let note: String?"));
    // ...but the wire key stays the original snake_case verbatim.
    assert!(types.contains("\"currentState\": \"current_state\""));
    // Optional defaults to nil in the memberwise init.
    assert!(types.contains("note: String? = nil"));
}

#[test]
fn swift_keyword_field_is_backtick_escaped() {
    let rule = group_rule("Config", vec![bare_entry("protocol", builtin("text"))]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let `protocol`: String"));
}

#[test]
fn type_choice_becomes_enum_with_associated_values() {
    let rule = CsilRule {
        name: "DepositResult".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            reference("DepositOk"),
            reference("ServiceError"),
        ]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum DepositResult: Equatable, Sendable"));
    assert!(types.contains("case depositOk(DepositOk)"));
    assert!(types.contains("case serviceError(ServiceError)"));
}

fn text_literal(s: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
}

fn int_literal(n: i64) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Integer(n))
}

fn bool_literal(b: bool) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Bool(b))
}

#[test]
fn closed_string_literal_choice_becomes_string_backed_enum() {
    let rule = CsilRule {
        name: "Color".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            text_literal("red"),
            text_literal("forest_green"),
            // a label that collides with a Swift keyword must still backtick-escape.
            text_literal("default"),
        ]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum Color: String, Equatable, Sendable, CaseIterable"));
    assert!(types.contains("case red = \"red\""));
    // camelCased case name, verbatim wire raw value.
    assert!(types.contains("case forestGreen = \"forest_green\""));
    assert!(types.contains("case `default` = \"default\""));
    // No opaque-byte fallback cases.
    assert!(!types.contains("AnyCsilValue"));
    assert!(!types.contains("case case1"));
}

#[test]
fn int_literal_choice_becomes_a_raw_int64_enum_not_a_tagged_sum() {
    // Regression: `Priority = 1 / 2 / 3` (interop.csil's own int-literal enum, pinned
    // bare `1`/`2`/`3` on the wire — every other of the 14 generators agrees, see
    // `docs/csilgen-requests/...`) used to fall into the tagged-sum union path, wire-
    // encoding as `[index, value]` (e.g. `[0, 1]`) instead of the bare literal `1`.
    let rule = CsilRule {
        name: "Priority".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![int_literal(1), int_literal(2), int_literal(3)]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum Priority: Int64, Equatable, Sendable, CaseIterable {"));
    assert!(types.contains("case v1 = 1"));
    assert!(types.contains("case v2 = 2"));
    assert!(types.contains("case v3 = 3"));
    // Not the tagged-sum shape.
    assert!(!types.contains("case case0(Int64)"));

    let codec = generate_codec(&input).expect("codec emitted");
    let enum_body = codec
        .split("extension Priority {")
        .nth(1)
        .expect("Priority codec extension emitted");
    // Bare literal on the wire: encode returns the rawValue itself, not an
    // `[index, payload]` array.
    assert!(enum_body.contains("let csilV = self.rawValue"));
    assert!(enum_body.contains("return csilV >= 0 ? .uint(UInt64(csilV)) : .int(csilV)"));
    assert!(!enum_body.contains(".array([.uint("));
    // Decode reads the raw scalar directly (not `asArray`/`asU64` index dispatch) and
    // maps it back through the failable `init(rawValue:)`.
    assert!(enum_body.contains("let csilS = try CsilCbor.asI64(cborValue)"));
    assert!(enum_body.contains("guard let csilV = Priority(rawValue: csilS)"));
    // Out-of-set value throws the same error the string-enum/tagged-union codecs use.
    assert!(enum_body.contains("else { throw CsilCborError.typeMismatch }"));
}

#[test]
fn negative_int_literal_choice_sanitizes_the_case_name() {
    let rule = CsilRule {
        name: "Delta".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![int_literal(-1), int_literal(0), int_literal(1)]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("case vNeg1 = -1"));
    assert!(types.contains("case v0 = 0"));
    assert!(types.contains("case v1 = 1"));
}

#[test]
fn int_literal_choice_referenced_by_a_struct_field_gets_a_working_codec() {
    // Same "field routes through the named choice's own codec" contract
    // `named_pure_literal_choice_referenced_by_a_struct_field_gets_a_working_codec`
    // proves for a string enum, but for the int-literal (Priority) shape: the field
    // must not fall through the union/`.null` catch-all.
    let rules = vec![
        CsilRule {
            name: "Priority".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                int_literal(1),
                int_literal(2),
                int_literal(3),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        group_rule(
            "Task",
            vec![
                bare_entry("id", builtin("text")),
                bare_entry("prio", reference("Priority")),
            ],
        ),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    assert!(codec.contains("extension Priority {"));
    let task_body = codec
        .split("extension Task {")
        .nth(1)
        .expect("Task codec extension emitted");
    assert!(!task_body.contains("\"prio\", .null"));
    assert!(task_body.contains("self.prio.toCborValue()"));
    assert!(
        task_body.contains(
            "let prio = try Priority(cborValue: try CsilCbor.require(cborValue, \"prio\"))"
        )
    );
}

#[test]
fn bool_literal_choice_becomes_a_manual_enum_with_bare_literal_codec() {
    // Bool can't be a native Swift raw-value enum literal (`case x = true` isn't valid
    // raw-value syntax the way `case x = 1` is), so this kind gets the manual
    // (no-raw-value) codec path — but it must still be bare-literal on the wire, not
    // the tagged-sum `[index, value]` shape.
    let rule = CsilRule {
        name: "Flag".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![bool_literal(true), bool_literal(false)]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum Flag: Equatable, Sendable, CaseIterable {"));
    assert!(types.contains("case vTrue"));
    assert!(types.contains("case vFalse"));
    // No raw value and no associated-value tagged-sum case.
    assert!(!types.contains("case vTrue("));

    let codec = generate_codec(&input).expect("codec emitted");
    let enum_body = codec
        .split("extension Flag {")
        .nth(1)
        .expect("Flag codec extension emitted");
    assert!(enum_body.contains("case .vTrue: return .bool(true)"));
    assert!(enum_body.contains("case .vFalse: return .bool(false)"));
    assert!(!enum_body.contains(".array([.uint("));
    assert!(enum_body.contains("let csilS = try CsilCbor.asBool(cborValue)"));
    assert!(enum_body.contains("case true: self = .vTrue"));
    assert!(enum_body.contains("case false: self = .vFalse"));
    assert!(enum_body.contains("default: throw CsilCborError.typeMismatch"));
}

#[test]
fn mixed_kind_literal_choice_becomes_a_manual_enum_with_bare_literal_codec() {
    // `"a" / 1`: two literal arms of DIFFERENT kinds (text and int). Neither
    // `all_text_literals` nor `all_scalar_literals` (each requires uniform kind)
    // matches this, so before this fix it fell to the tagged-sum union path
    // (`case case0(String)` / `case case1(Int64)`, `[index, value]` on the wire).
    // The CSIL wire contract says an ALL-literal choice always rides bare
    // regardless of whether the kinds match — matches the Go/PHP/Python/
    // TypeScript generators' shared contract decision.
    let rule = CsilRule {
        name: "MixedLit".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![text_literal("a"), int_literal(1)]),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    // A manual (no-raw-value) enum — no single Swift raw-value type spans both
    // a String and an Int64 case the way `emit_scalar_enum`'s native path can.
    assert!(types.contains("public enum MixedLit: Equatable, Sendable, CaseIterable {"));
    assert!(types.contains("case a"));
    assert!(types.contains("case v1"));
    // Never the tagged-sum shape.
    assert!(!types.contains("case case0("));
    assert!(!types.contains("case a("));

    let codec = generate_codec(&input).expect("codec emitted");
    let enum_body = codec
        .split("extension MixedLit {")
        .nth(1)
        .expect("MixedLit codec extension emitted");
    // Encode: each case returns its own literal's CBOR value directly (bare), no
    // `[index, payload]` wrapping array.
    assert!(enum_body.contains("case .a: return .text(\"a\")"));
    assert!(enum_body.contains("case .v1: return .uint(UInt64(1))"));
    assert!(!enum_body.contains(".array([.uint("));
    // Decode: mixed kinds mean no single `CsilCbor.as*` accessor spans every arm,
    // so it switches on the decoded `CsilCborValue` tree node's own case directly
    // (not `CsilCbor.asText`/`asI64` unwrapped-then-switched, the uniform-kind
    // enums' shape) — and still rejects a value outside the declared closed set.
    assert!(enum_body.contains("case .text(\"a\"): self = .a"));
    assert!(enum_body.contains("case .uint(1): self = .v1"));
    assert!(enum_body.contains("default: throw CsilCborError.typeMismatch"));
}

#[test]
fn mixed_kind_literal_choice_referenced_by_a_struct_field_gets_a_working_codec() {
    // Same "field routes through the named choice's own codec" contract
    // `int_literal_choice_referenced_by_a_struct_field_gets_a_working_codec` proves
    // for a uniform int-literal enum, but for the mixed-kind (`"a" / 1`) shape.
    let rules = vec![
        CsilRule {
            name: "MixedLit".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![text_literal("a"), int_literal(1)]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        group_rule("Holder", vec![bare_entry("value", reference("MixedLit"))]),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    assert!(codec.contains("extension MixedLit {"));
    let holder_body = codec
        .split("extension Holder {")
        .nth(1)
        .expect("Holder codec extension emitted");
    assert!(!holder_body.contains("\"value\", .null"));
    assert!(holder_body.contains("self.value.toCborValue()"));
    assert!(holder_body.contains(
        "let value = try MixedLit(cborValue: try CsilCbor.require(cborValue, \"value\"))"
    ));
}

#[test]
fn int_literal_inline_choice_field_hoists_to_a_scalar_enum_with_membership_validation() {
    // Same hoisting contract `pure_literal_inline_choice_field_hoists_to_a_string_enum_with_membership_validation`
    // proves for an inline all-text choice, but for an inline all-int choice: it must
    // hoist to the scalar (`Int64`-raw-value) enum shape, not the tagged-sum fallback.
    let rule = group_rule(
        "Ticket",
        vec![bare_entry(
            "prio_inline",
            CsilTypeExpression::Choice(vec![int_literal(1), int_literal(2), int_literal(3)]),
        )],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let prioInline: TicketPrioInline"));
    assert!(
        types.contains("public enum TicketPrioInline: Int64, Equatable, Sendable, CaseIterable {")
    );
    assert!(types.contains("case v1 = 1"));

    let codec = generate_codec(&input).expect("codec emitted");
    let enum_body = codec
        .split("extension TicketPrioInline {")
        .nth(1)
        .expect("hoisted scalar enum codec emitted");
    assert!(enum_body.contains("let csilV = self.rawValue"));
    assert!(enum_body.contains("guard let csilV = TicketPrioInline(rawValue: csilS)"));
    assert!(enum_body.contains("throw CsilCborError.typeMismatch"));

    let struct_body = codec
        .split("extension Ticket {")
        .nth(1)
        .expect("Ticket codec extension emitted");
    assert!(struct_body.contains("self.prioInline.toCborValue()"));
    assert!(struct_body.contains("try TicketPrioInline(cborValue:"));
}

#[test]
fn open_text_choice_becomes_a_tagged_union_enum() {
    // Regression: a mixed choice (the open `text` builtin plus string literals) used to
    // collapse to `typealias OrderStatus = String`, which threw away the ability to
    // encode/decode it as a proper wire union. It's now a tagged `enum`, matching every
    // other non-pure-literal choice — see `order_status_union_gets_a_full_codec` for the
    // CBOR side of this.
    let rule = CsilRule {
        name: "OrderStatus".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            builtin("text"),
            text_literal("pending"),
            text_literal("shipped"),
        ])),
        position: pos(),
        doc_comments: Vec::new(),
    };
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(!types.contains("public typealias OrderStatus = String"));
    assert!(types.contains("public enum OrderStatus: Equatable, Sendable {"));
    assert!(types.contains("case text(String)"));
    assert!(types.contains("case case1(String)"));
    assert!(types.contains("case case2(String)"));
}

/// The exact `OrderStatus` shape from `examples/real-world-api/e-commerce-api.csil`
/// line 138: `text / "pending" / "confirmed" / "processing" / "shipped" / "delivered" /
/// "cancelled" / "refunded"` — one open `text` arm plus seven literal arms.
fn order_status_rule() -> CsilRule {
    CsilRule {
        name: "OrderStatus".to_string(),
        rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
            builtin("text"),
            text_literal("pending"),
            text_literal("confirmed"),
            text_literal("processing"),
            text_literal("shipped"),
            text_literal("delivered"),
            text_literal("cancelled"),
            text_literal("refunded"),
        ])),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

#[test]
fn order_status_union_type_declaration_has_all_eight_cases() {
    let input = input_from_rules("swift", vec![order_status_rule()], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public enum OrderStatus: Equatable, Sendable {"));
    assert!(!types.contains("public typealias OrderStatus = String"));
    let names = [
        "text", "case1", "case2", "case3", "case4", "case5", "case6", "case7",
    ];
    for name in names {
        assert!(
            types.contains(&format!("case {name}(String)")),
            "missing case {name} in {types}"
        );
    }
}

#[test]
fn order_status_union_gets_a_full_codec() {
    // A spec with only a union rule (no struct rows) must still get a Codec.swift —
    // `generate_codec`'s old `records.is_empty()` early-return would have silently
    // skipped this codec entirely.
    let input = input_from_rules("swift", vec![order_status_rule()], 0);
    let codec = generate_codec(&input).expect("codec emitted for a union-only spec");
    let body = codec
        .split("public extension OrderStatus {")
        .nth(1)
        .expect("OrderStatus codec extension emitted");

    // Encode: each of the 8 cases tags its own declared 0-based index. The general
    // `text` arm carries its wrapped value through; each literal arm's payload is the
    // literal constant itself (the enum case is already the discriminant, so the
    // wrapped associated value is irrelevant on encode).
    // The general `text` arm checks the literal arms first (literal wins).
    assert!(body.contains("case .text(let csilV):\n            if csilV == "));
    assert!(body.contains("            return .array([.uint(0), .text(csilV)])"));
    assert!(
        body.contains("case .case1:\n            return .array([.uint(1), .text(\"pending\")])")
    );
    assert!(body.contains(
        "            if csilV == \"refunded\" { return .array([.uint(7), .text(\"refunded\")]) }\n"
    ));
    assert!(
        body.contains("case .case2:\n            return .array([.uint(2), .text(\"confirmed\")])")
    );
    assert!(
        body.contains("case .case3:\n            return .array([.uint(3), .text(\"processing\")])")
    );
    assert!(
        body.contains("case .case4:\n            return .array([.uint(4), .text(\"shipped\")])")
    );
    assert!(
        body.contains("case .case5:\n            return .array([.uint(5), .text(\"delivered\")])")
    );
    assert!(
        body.contains("case .case6:\n            return .array([.uint(6), .text(\"cancelled\")])")
    );
    assert!(
        body.contains("case .case7:\n            return .array([.uint(7), .text(\"refunded\")])")
    );

    // Decode: dispatches on the 2-element array's index; the `text` arm reads through
    // as plain text, each literal arm validates the payload against its declared
    // literal via `CsilCbor.expectLiteral`, erroring (not silently coercing) on a
    // wire/spec mismatch.
    assert!(body.contains("case 0: self = .text(try CsilCbor.asText(csilPayload))"));
    assert!(body.contains(
        "case 1: self = .case1(try CsilCbor.expectLiteral(csilPayload, .text(\"pending\"), \"pending\"))"
    ));
    assert!(body.contains(
        "case 7: self = .case7(try CsilCbor.expectLiteral(csilPayload, .text(\"refunded\"), \"refunded\"))"
    ));
    assert!(body.contains("default: throw CsilCborError.typeMismatch"));
    assert!(
        body.contains("func toCbor() throws -> [UInt8] { CsilCbor.encode(try toCborValue()) }")
    );
    assert!(body.contains(
        "static func fromCbor(_ bytes: [UInt8]) throws -> OrderStatus { try OrderStatus(cborValue: CsilCbor.decode(bytes)) }"
    ));
}

#[test]
fn struct_field_referencing_a_union_round_trips_through_its_own_codec() {
    // Regression: a struct field typed as a named union (e.g. `Order.status:
    // OrderStatus`) used to fall through to the `.null` / `asText` catch-all — the
    // field's data was silently dropped on encode and misread on decode. It must now
    // call the union's own generated `toCborValue()`/`init(cborValue:)`.
    let rules = vec![
        order_status_rule(),
        group_rule(
            "Order",
            vec![
                bare_entry("id", builtin("text")),
                bare_entry("status", reference("OrderStatus")),
            ],
        ),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    let body = codec
        .split("extension Order {")
        .nth(1)
        .expect("Order codec extension emitted");
    // No more `.null` stub or `asText` misread for this field.
    assert!(!body.contains("\"status\", .null"));
    assert!(!body.contains("CsilCbor.asText(try CsilCbor.require(cborValue, \"status\")"));
    // Encode/decode delegate to OrderStatus's own codec.
    assert!(
        body.contains("csilEntries.append((.text(\"status\"), try self.status.toCborValue()))")
    );
    assert!(body.contains(
        "let status = try OrderStatus(cborValue: try CsilCbor.require(cborValue, \"status\"))"
    ));
}

/// An inline choice arm wrapped in a `.default` control operator, matching how CSIL's
/// grammar actually parses `text / "low" / "high" .default "normal"`: the operator
/// binds to the immediately preceding arm (`"high"`), not the choice as a whole, so
/// the arm is `Constrained { base_type: Literal("high"), .. }` rather than a bare
/// `Literal`.
fn default_constrained_literal(lit: &str, default: &str) -> CsilTypeExpression {
    CsilTypeExpression::Constrained {
        base_type: Box::new(text_literal(lit)),
        constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
            default.to_string(),
        ))],
    }
}

#[test]
fn default_suffixed_literal_arm_does_not_fall_out_of_the_union_shape() {
    // Regression: `constrained_arm_inline: text / "low" / "high" .default "normal"` —
    // the trailing `.default` wraps only the last arm (`choice_arm_literal` strips it),
    // so this must still classify as a 3-arm tagged union (one open `text` arm plus two
    // literals), matching the ocaml ground-truth bytes: Low -> [1,"low"], High ->
    // [2,"high"], an arbitrary string -> [0, <string>].
    let rule = group_rule(
        "Torture",
        vec![bare_entry(
            "constrained_arm_inline",
            CsilTypeExpression::Choice(vec![
                builtin("text"),
                text_literal("low"),
                default_constrained_literal("high", "normal"),
            ]),
        )],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    // Hoisted to its own named union type, not collapsed to a bare String (which
    // would have happened if the `.default` wrapper defeated the literal check).
    assert!(types.contains("public let constrainedArmInline: TortureConstrainedArmInline"));
    assert!(
        types.contains("public enum TortureConstrainedArmInline: Equatable, Sendable {"),
        "expected a tagged union, not a string-backed enum, in {types}"
    );
    assert!(!types.contains("String, Equatable, Sendable, CaseIterable"));

    let codec = generate_codec(&input).expect("codec emitted");
    let union_body = codec
        .split("extension TortureConstrainedArmInline {")
        .nth(1)
        .expect("hoisted union codec emitted");
    assert!(
        union_body.contains("case .case1:\n            return .array([.uint(1), .text(\"low\")])")
    );
    assert!(
        union_body.contains("case .case2:\n            return .array([.uint(2), .text(\"high\")])")
    );
    assert!(union_body.contains(
        "case 1: self = .case1(try CsilCbor.expectLiteral(csilPayload, .text(\"low\"), \"low\"))"
    ));
    assert!(union_body.contains(
        "case 2: self = .case2(try CsilCbor.expectLiteral(csilPayload, .text(\"high\"), \"high\"))"
    ));

    // The struct field routes through the hoisted type's own codec.
    let struct_body = codec
        .split("extension Torture {")
        .nth(1)
        .expect("Torture codec extension emitted");
    assert!(struct_body.contains("self.constrainedArmInline.toCborValue()"));
    assert!(struct_body.contains("try TortureConstrainedArmInline(cborValue:"));
}

#[test]
fn pure_literal_inline_choice_field_hoists_to_a_string_enum_with_membership_validation() {
    // `pure_literal_inline: "active" / "inactive" / "pending"` — every arm a literal,
    // no open arm — must behave exactly like a field referencing a named all-literal
    // choice rule: bare wire text (ocaml ground truth: `pure_literal_inline(Active)` is
    // just `"active"`, not `[idx, "active"]`), plus decode-time membership validation.
    let rule = group_rule(
        "Torture",
        vec![bare_entry(
            "pure_literal_inline",
            CsilTypeExpression::Choice(vec![
                text_literal("active"),
                text_literal("inactive"),
                text_literal("pending"),
            ]),
        )],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let pureLiteralInline: TorturePureLiteralInline"));
    assert!(types.contains(
        "public enum TorturePureLiteralInline: String, Equatable, Sendable, CaseIterable {"
    ));
    assert!(types.contains("case active = \"active\""));
    assert!(types.contains("case pending = \"pending\""));

    let codec = generate_codec(&input).expect("codec emitted");
    let enum_body = codec
        .split("extension TorturePureLiteralInline {")
        .nth(1)
        .expect("hoisted string enum codec emitted");
    // Bare wire text, not an `[index, value]` tagged array.
    assert!(
        enum_body.contains("func toCborValue() throws -> CsilCborValue { .text(self.rawValue) }")
    );
    // Decode rejects a string outside the declared closed set instead of accepting it.
    assert!(enum_body.contains("guard let csilV = TorturePureLiteralInline(rawValue: csilS)"));
    assert!(enum_body.contains("throw CsilCborError.typeMismatch"));

    let struct_body = codec
        .split("extension Torture {")
        .nth(1)
        .expect("Torture codec extension emitted");
    assert!(struct_body.contains("self.pureLiteralInline.toCborValue()"));
    assert!(struct_body.contains("try TorturePureLiteralInline(cborValue:"));
}

#[test]
fn mixed_inline_choice_field_hoists_to_a_tagged_union_with_literal_first_precedence() {
    // `mixed_inline: text / "not_found" / "permission_denied" / "invalid_input"` — the
    // exact shape of `examples/real-world-api/e-commerce-api.csil`'s `APIError.error_type`
    // — used to collapse to a bare `String` (dropping the ability to round-trip it and
    // silently accepting any string). Ocaml ground truth: `not_found` encodes to
    // `[1, "not_found"]` (index 1, the literal arm), not `[0, "not_found"]` (index 0,
    // the open `text` arm) — literal-first precedence is a type-system given here since
    // the enum case IS the discriminant, but the index assignment itself must match.
    let rule = group_rule(
        "Torture",
        vec![bare_entry(
            "mixed_inline",
            CsilTypeExpression::Choice(vec![
                builtin("text"),
                text_literal("not_found"),
                text_literal("permission_denied"),
                text_literal("invalid_input"),
            ]),
        )],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let mixedInline: TortureMixedInline"));
    assert!(types.contains("public enum TortureMixedInline: Equatable, Sendable {"));
    assert!(types.contains("case text(String)"));
    assert!(types.contains("case case1(String)"));

    let codec = generate_codec(&input).expect("codec emitted");
    let body = codec
        .split("extension TortureMixedInline {")
        .nth(1)
        .expect("hoisted union codec emitted");
    // The general `text` arm checks the literal arms first (literal wins).
    assert!(body.contains("case .text(let csilV):\n            if csilV == "));
    assert!(body.contains("            return .array([.uint(0), .text(csilV)])"));
    assert!(
        body.contains("case .case1:\n            return .array([.uint(1), .text(\"not_found\")])")
    );
    assert!(body.contains(
        "case .case2:\n            return .array([.uint(2), .text(\"permission_denied\")])"
    ));
    assert!(
        body.contains(
            "case .case3:\n            return .array([.uint(3), .text(\"invalid_input\")])"
        )
    );
    assert!(body.contains("case 0: self = .text(try CsilCbor.asText(csilPayload))"));
    assert!(body.contains(
        "case 1: self = .case1(try CsilCbor.expectLiteral(csilPayload, .text(\"not_found\"), \"not_found\"))"
    ));
}

#[test]
fn inline_choice_as_array_element_hoists_to_a_synthesized_type() {
    // `tag_list: [* (text / "red" / "green" / "blue")]` — the array-element position;
    // ocaml's own reference codec has a known gap here (`failwith` at runtime), but the
    // Swift codec must actually work: the array element type routes through the same
    // hoisted-union machinery a direct field would.
    let rule = group_rule(
        "Torture",
        vec![bare_entry(
            "tag_list",
            CsilTypeExpression::Array {
                element_type: Box::new(CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    text_literal("red"),
                    text_literal("green"),
                    text_literal("blue"),
                ])),
                occurrence: None,
            },
        )],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let tagList: [TortureTagListItem]"));
    assert!(types.contains("public enum TortureTagListItem: Equatable, Sendable {"));

    let codec = generate_codec(&input).expect("codec emitted");
    // The array element codec delegates to the hoisted type per element, not a bare
    // `.text($0)` (which would be the pre-fix bare-string collapse).
    let struct_body = codec
        .split("extension Torture {")
        .nth(1)
        .expect("Torture codec extension emitted");
    assert!(struct_body.contains("try self.tagList.map { try $0.toCborValue() }"));
    assert!(struct_body.contains("try TortureTagListItem(cborValue: $0)"));
    assert!(codec.contains("extension TortureTagListItem {"));
}

#[test]
fn inline_choice_as_map_value_hoists_to_a_synthesized_type() {
    // `label_map: { * text => (text / "urgent" / "normal") }` — the map-value position.
    let rule = group_rule(
        "Torture",
        vec![bare_entry(
            "label_map",
            CsilTypeExpression::Map {
                key: Box::new(builtin("text")),
                value: Box::new(CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    text_literal("urgent"),
                    text_literal("normal"),
                ])),
                occurrence: None,
            },
        )],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public let labelMap: [String: TortureLabelMapValue]"));
    assert!(types.contains("public enum TortureLabelMapValue: Equatable, Sendable {"));

    let codec = generate_codec(&input).expect("codec emitted");
    let struct_body = codec
        .split("extension Torture {")
        .nth(1)
        .expect("Torture codec extension emitted");
    assert!(struct_body.contains("try $0.value.toCborValue()"));
    assert!(struct_body.contains("try TortureLabelMapValue(cborValue: $1.1)"));
}

#[test]
fn inline_choice_as_tuple_element_hoists_to_a_synthesized_type() {
    // `coord: [text / "lat" / "lon", int]` — the tuple-element position. Tuples had no
    // codec route at all before this fix (every tuple field encoded as `.null` and
    // decoded via a type-mismatched `asText`), so this also exercises the new general
    // tuple codec, not just the hoisting.
    let tuple_group = CsilGroupExpression {
        entries: vec![
            bare_entry(
                "lat_or_lon",
                CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    text_literal("lat"),
                    text_literal("lon"),
                ]),
            ),
            bare_entry("value", builtin("int")),
        ],
    };
    let rule = group_rule(
        "Torture",
        vec![bare_entry("coord", CsilTypeExpression::Tuple(tuple_group))],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    // `csilgen_common::hoist_inline_composites` names a hoisted tuple element from the
    // element's OWN key when it has one (`lat_or_lon` here) rather than its positional
    // index — this generator's pre-migration hand-rolled hoister ignored a tuple
    // element's own key entirely and always used the index (`TortureCoord0`); the
    // shared hoister's naming is more readable and still collision-free, and doesn't
    // change any wire byte (it's a Swift-side identifier only).
    assert!(types.contains("public let coord: (latOrLon: TortureCoordLatOrLon, value: Int64)"));
    assert!(types.contains("public enum TortureCoordLatOrLon: Equatable, Sendable {"));

    let codec = generate_codec(&input).expect("codec emitted");
    let struct_body = codec
        .split("extension Torture {")
        .nth(1)
        .expect("Torture codec extension emitted");
    // Encode: a 2-element CBOR array, first element via the hoisted type's own codec.
    assert!(struct_body.contains(
        "CsilCborValue.array([try self.coord.latOrLon.toCborValue(), .int(self.coord.value)])"
    ));
    // Decode: positional array access, first element through the hoisted type's
    // `init(cborValue:)`, second through the plain int decoder.
    assert!(struct_body.contains("try TortureCoordLatOrLon(cborValue:"));
    assert!(struct_body.contains("try CsilCbor.asI64("));
    assert!(struct_body.contains("latOrLon: try TortureCoordLatOrLon"));
}

#[test]
fn single_element_tuple_with_an_inline_choice_collapses_to_the_bare_hoisted_type() {
    // A one-element CSIL tuple `[text / "a" / "b"]` maps to a bare Swift value (no
    // tuple wrapper, per `map_tuple`'s existing collapse) but must still be a 1-element
    // CBOR array on the wire, not the bare element itself.
    let tuple_group = CsilGroupExpression {
        entries: vec![bare_entry(
            "only",
            CsilTypeExpression::Choice(vec![builtin("text"), text_literal("a"), text_literal("b")]),
        )],
    };
    let rule = group_rule(
        "Torture",
        vec![bare_entry("solo", CsilTypeExpression::Tuple(tuple_group))],
    );
    let input = hoisted(input_from_rules("swift", vec![rule], 0));
    let types = generate_types(&input).expect("types emitted");
    // Bare hoisted type, no Swift tuple wrapper.
    assert!(types.contains("public let solo: TortureSoloOnly"));
    assert!(!types.contains("solo: (only:"));

    let codec = generate_codec(&input).expect("codec emitted");
    let struct_body = codec
        .split("extension Torture {")
        .nth(1)
        .expect("Torture codec extension emitted");
    assert!(struct_body.contains("CsilCborValue.array([try self.solo.toCborValue()])"));
    assert!(struct_body.contains("try TortureSoloOnly(cborValue: try CsilCbor.item("));
}

#[test]
fn named_pure_literal_choice_referenced_by_a_struct_field_gets_a_working_codec() {
    // Regression: before `swift_string_enum_choices` existed, a struct field typed as a
    // named ALL-literal choice rule (e.g. `Color = "red" / "green" / "blue"`) fell
    // through the same `.null` / `asText` catch-all a named union field used to hit —
    // silently dropping the field on encode and producing a decode that doesn't even
    // type-check (`asText` returns `String`, not the enum). This is the "behaves
    // exactly like a field referencing a named choice rule" half of the contract: the
    // named-rule case must actually work, since the hoisted inline case routes through
    // identical machinery.
    let rules = vec![
        CsilRule {
            name: "Color".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                text_literal("red"),
                text_literal("green"),
                text_literal("blue"),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        group_rule(
            "Widget",
            vec![
                bare_entry("id", builtin("text")),
                bare_entry("color", reference("Color")),
            ],
        ),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    assert!(codec.contains("extension Color {"));
    let widget_body = codec
        .split("extension Widget {")
        .nth(1)
        .expect("Widget codec extension emitted");
    assert!(!widget_body.contains("\"color\", .null"));
    assert!(!widget_body.contains("CsilCbor.asText(try CsilCbor.require(cborValue, \"color\")"));
    assert!(widget_body.contains("self.color.toCborValue()"));
    assert!(
        widget_body.contains(
            "let color = try Color(cborValue: try CsilCbor.require(cborValue, \"color\"))"
        )
    );
}

#[test]
fn error_suffixed_arms_are_stripped_from_the_returned_success_type() {
    // `create-user: CreateUserRequest -> User / UserError` returns just User; the error
    // half is thrown, never collapsed to opaque AnyCsilValue.
    let ops = vec![make_op(
        "create-user",
        reference("CreateUserRequest"),
        CsilTypeExpression::Choice(vec![reference("User"), reference("UserError")]),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule(
                "CreateUserRequest",
                vec![bare_entry("name", builtin("text"))],
            ),
            group_rule("User", vec![bare_entry("id", builtin("int"))]),
            service_rule("UserService", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("client");
    assert!(
        client
            .content
            .contains("public func createUser(_ request: CreateUserRequest) throws -> User")
    );
    assert!(!client.content.contains("AnyCsilValue"));
}

#[test]
fn zero_minimum_length_check_is_not_emitted() {
    // `.size (0..200)` must not produce a dead `count < 0` guard.
    let entry = bare_entry(
        "description",
        CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Range {
                min: 0,
                max: 200,
            })],
        },
    );
    let rule = group_rule("Doc", vec![entry]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(!types.contains("< 0"));
    assert!(types.contains("self.description.count > 200"));
}

#[test]
fn any_value_is_the_cbor_value_and_round_trips_unchanged() {
    // A field of core type `any` with no constraints still needs AnyCsilValue defined.
    let rule = group_rule("Envelope", vec![bare_entry("body", builtin("any"))]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    // The codec runtime declares `AnyCsilValue` as the decoded CBOR value, so the
    // codec ships with any spec that has a type.
    assert!(!types.contains("public typealias AnyCsilValue"));
    assert!(types.contains("public let body: AnyCsilValue"));
    let codec = generate_codec(&input).expect("codec emitted");
    assert!(codec.contains("public typealias AnyCsilValue = CsilCborValue"));
    assert!(codec.contains("(.text(\"body\"), self.body)"), "{codec}");
    assert!(
        codec.contains("let body = try CsilCbor.require(cborValue, \"body\")"),
        "{codec}"
    );
    // No validation machinery when there are no constraints.
    assert!(!types.contains("CsilValidationError"));
}

#[test]
fn validation_emits_throwing_checks() {
    let entry = bare_entry(
        "username",
        CsilTypeExpression::Constrained {
            base_type: Box::new(builtin("text")),
            constraints: vec![CsilControlOperator::Size(CsilSizeConstraint::Min(3))],
        },
    );
    let rule = group_rule("Account", vec![entry]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public func validate() throws"));
    assert!(types.contains("self.username.count < 3"));
    assert!(types.contains("throw CsilValidationError("));
    assert!(types.contains("struct CsilValidationError"));
}

#[test]
fn numeric_min_value_compares_as_scalar() {
    let mut entry = bare_entry("age", builtin("int"));
    entry.metadata.push(CsilFieldMetadata::Constraint(
        CsilValidationConstraint::MinValue(CsilLiteralValue::Integer(18)),
    ));
    let rule = group_rule("Person", vec![entry]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("if self.age < 18"));
}

#[test]
fn server_target_emits_protocol_and_routers() {
    let ops = vec![
        make_op(
            "submit-task",
            reference("SubmitTaskRequest"),
            CsilTypeExpression::Choice(vec![
                reference("SubmitTaskResponse"),
                reference("ServiceError"),
            ]),
            CsilServiceDirection::Unidirectional,
        ),
        make_op(
            "play",
            reference("Move"),
            reference("Move"),
            CsilServiceDirection::Bidirectional,
        ),
    ];
    let input = input_from_rules("swift", vec![service_rule("Match", ops, Some(7))], 1);
    let services = generate_services(&input).expect("services emitted");
    // Handler protocol with verbatim-derived method names; ServiceError stripped from output.
    assert!(services.contains("public protocol Match {"));
    assert!(
        services
            .contains("func submitTask(_ request: SubmitTaskRequest) throws -> SubmitTaskResponse")
    );
    assert!(services.contains("func play(_ msg: Move) throws"));
    // Verbose router keyed by verbatim op name.
    assert!(services.contains("public func routeMatchChannel(_ handler: Match, codec: CsilCodec, op: String, data: [UInt8]) throws"));
    assert!(services.contains("case \"play\":"));
    assert!(services.contains("try handler.play(msg)"));
    // Compact twin keyed by ordinal (service has a wire-id).
    assert!(services.contains("public func routeMatchChannelCompact"));
    // Wire-id ordinals.
    assert!(services.contains("public enum MatchWireID"));
    assert!(services.contains("public static let service: UInt64 = 7"));
    // Codec seam present because there is a channel op.
    assert!(services.contains("public protocol CsilCodec"));
}

#[test]
fn client_target_emits_typed_client_with_canonical_wire_strings() {
    let ops = vec![make_op(
        "deposit-claim",
        reference("DepositClaimRequest"),
        CsilTypeExpression::Choice(vec![
            reference("DepositClaimResponse"),
            reference("ServiceError"),
        ]),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule(
                "DepositClaimRequest",
                vec![bare_entry("subject", builtin("text"))],
            ),
            group_rule(
                "DepositClaimResponse",
                vec![bare_entry("ok", builtin("bool"))],
            ),
            service_rule("Attestation", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("client emitted");
    assert!(client.content.contains("public protocol CsilTransport"));
    assert!(client.content.contains("public struct AttestationClient"));
    assert!(client.content.contains(
        "public func depositClaim(_ request: DepositClaimRequest) throws -> DepositClaimResponse"
    ));
    // Typed seam: request serializes itself, the carrier moves bytes.
    assert!(
        client
            .content
            .contains("let csilRequest = try request.toCbor()\n")
    );
    assert!(
        client
            .content
            .contains("DepositClaimResponse.fromCbor(csilResp)")
    );
    // Canonical wire strings: verbatim CSIL service and op names
    // (csil-rpc-transport.md §1.1) — so a Swift client reaches the same endpoint
    // as its peers.
    assert!(client.content.contains("service: \"Attestation\""));
    assert!(client.content.contains("op: \"deposit-claim\""));
    // No server protocol for the client sub-target.
    assert!(!files.iter().any(|f| f.path == "Services.swift"));
}

#[test]
fn null_input_op_takes_no_request() {
    let ops = vec![make_op(
        "room-delta",
        builtin("null"),
        reference("RoomDelta"),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule("RoomDelta", vec![bare_entry("seq", builtin("int"))]),
            service_rule("World", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("client emitted");
    assert!(
        client
            .content
            .contains("public func roomDelta() throws -> RoomDelta")
    );
    // A null-input op sends an empty request body.
    assert!(client.content.contains("request: [])"));
}

#[test]
fn typesonly_target_skips_services() {
    let ops = vec![make_op(
        "ping",
        builtin("null"),
        reference("Pong"),
        CsilServiceDirection::Unidirectional,
    )];
    let rules = vec![
        group_rule("Pong", vec![bare_entry("ok", builtin("bool"))]),
        service_rule("Health", ops, None),
    ];
    let input = input_from_rules("swift-typesonly", rules, 1);
    let files = build_files(&input).expect("ok");
    assert!(files.iter().any(|f| f.path == "Types.swift"));
    assert!(!files.iter().any(|f| f.path == "Services.swift"));
    assert!(!files.iter().any(|f| f.path == "Client.swift"));
}

#[test]
fn unknown_subtarget_is_an_error() {
    let input = input_from_rules("swift-bogus", vec![], 0);
    assert!(build_files(&input).is_err());
}

#[test]
fn wire_id_free_service_omits_compact_router() {
    let ops = vec![make_op(
        "play",
        reference("Move"),
        reference("Move"),
        CsilServiceDirection::Bidirectional,
    )];
    let input = input_from_rules("swift", vec![service_rule("Match", ops, None)], 1);
    let services = generate_services(&input).expect("services emitted");
    assert!(services.contains("public func routeMatchChannel"));
    assert!(!services.contains("routeMatchChannelCompact"));
    assert!(!services.contains("MatchWireID"));
}

// --- codec ------------------------------------------------------------------

/// A corndogs-shaped spec: text, bytes, an optional int, a map, a list, a nested
/// record, and a service whose output is a `Res / ServiceError` choice.
fn corndogs_rules() -> Vec<CsilRule> {
    let map_ty = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("int")),
        occurrence: None,
    };
    let list_ty = CsilTypeExpression::Array {
        element_type: Box::new(builtin("text")),
        occurrence: None,
    };
    vec![
        group_rule(
            "Task",
            vec![
                bare_entry("uuid", builtin("text")),
                bare_entry("current_state", builtin("text")),
                bare_entry("payload", builtin("bytes")),
                opt_entry("priority", builtin("int")),
                bare_entry("labels", map_ty),
                bare_entry("tags", list_ty),
            ],
        ),
        group_rule(
            "SubmitTaskRequest",
            vec![
                bare_entry("task", reference("Task")),
                bare_entry("queue", builtin("text")),
            ],
        ),
        group_rule("ServiceError", vec![bare_entry("code", builtin("int"))]),
        service_rule(
            "CorndogsService",
            vec![make_op(
                "submit-task",
                reference("SubmitTaskRequest"),
                CsilTypeExpression::Choice(vec![reference("Task"), reference("ServiceError")]),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]
}

#[test]
fn codec_emitted_with_typed_client() {
    let input = input_from_rules("swift-client", corndogs_rules(), 1);
    let files = build_files(&input).expect("ok");
    let codec = files
        .iter()
        .find(|f| f.path == "Codec.swift")
        .expect("Codec.swift emitted");
    assert!(codec.content.contains("public enum CsilCbor"));
    assert!(codec.content.contains("extension Task {"));
    assert!(codec.content.contains("func toCbor() throws -> [UInt8]"));
    assert!(
        codec
            .content
            .contains("static func fromCbor(_ bytes: [UInt8]) throws -> SubmitTaskRequest")
    );
    // bytes -> CBOR byte string (.bytes, major type 2); text -> .text; nested record
    // recurses via toCborValue.
    assert!(codec.content.contains(".bytes(self.payload)"));
    assert!(codec.content.contains(".text(self.uuid)"));
    assert!(codec.content.contains("self.task.toCborValue()"));
    // Canonical key order within Task: `tags`/`uuid` (len 4) precede longer keys,
    // `current_state` (len 13) is last.
    let body = codec.content.split("extension Task").nth(1).unwrap();
    let pos_tags = body.find("\"tags\"").unwrap();
    let pos_uuid = body.find("\"uuid\"").unwrap();
    let pos_state = body.find("\"current_state\"").unwrap();
    assert!(pos_tags < pos_uuid && pos_uuid < pos_state);

    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("Client.swift emitted");
    assert!(
        client
            .content
            .contains("public func submitTask(_ request: SubmitTaskRequest) throws -> Task")
    );
    assert!(
        client
            .content
            .contains("let csilRequest = try request.toCbor()\n")
    );
    assert!(client.content.contains("Task.fromCbor(csilResp)"));
    // The carrier seam is raw bytes.
    assert!(
        client
            .content
            .contains("func call(service: String, op: String, request: [UInt8]) throws -> [UInt8]")
    );
}

#[test]
fn non_record_op_boundaries_get_client_methods_and_per_op_codecs() {
    // Mirrors tests/fixtures/services/nonrecord-ops.csil: scalar-id requests, bare-array
    // and scalar responses, and map responses that the old record-only filter dropped.
    let array_of = |elem: CsilTypeExpression| CsilTypeExpression::Array {
        element_type: Box::new(elem),
        occurrence: None,
    };
    let text_map = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("text")),
        occurrence: None,
    };
    let ops = vec![
        make_op(
            "create-member",
            reference("Member"),
            reference("Member"),
            CsilServiceDirection::Unidirectional,
        ),
        make_op(
            "get-member",
            reference("MemberID"),
            reference("Member"),
            CsilServiceDirection::Unidirectional,
        ),
        make_op(
            "list-members",
            reference("ListMembersRequest"),
            array_of(reference("Member")),
            CsilServiceDirection::Unidirectional,
        ),
        make_op(
            "delete-task",
            reference("TaskID"),
            builtin("bool"),
            CsilServiceDirection::Unidirectional,
        ),
        make_op(
            "member-names",
            reference("ListMembersRequest"),
            text_map,
            CsilServiceDirection::Unidirectional,
        ),
    ];
    let input = input_from_rules(
        "swift-client",
        vec![
            typedef_rule("MemberID", builtin("text")),
            typedef_rule("TaskID", builtin("text")),
            group_rule(
                "Member",
                vec![
                    bare_entry("id", reference("MemberID")),
                    bare_entry("name", builtin("text")),
                ],
            ),
            group_rule(
                "ListMembersRequest",
                vec![opt_entry("limit", builtin("uint"))],
            ),
            service_rule("MemberService", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("Client.swift emitted");

    // Every op gets a method — scalar-id request, bare-array, scalar, and map responses
    // included. No op is dropped with a note anymore.
    assert!(!client.content.contains("handle it manually"));
    assert!(
        client
            .content
            .contains("public func getMember(_ request: MemberId) throws -> Member")
    );
    assert!(
        client
            .content
            .contains("public func listMembers(_ request: ListMembersRequest) throws -> [Member]")
    );
    assert!(
        client
            .content
            .contains("public func deleteTask(_ request: TaskId) throws -> Bool")
    );
    assert!(client.content.contains(
        "public func memberNames(_ request: ListMembersRequest) throws -> [String: String]"
    ));
    // Record boundary keeps its `toCbor`/`fromCbor`; non-record rides the per-op helpers.
    assert!(
        client
            .content
            .contains("let csilRequest = try request.toCbor()\n")
    );
    assert!(
        client
            .content
            .contains("encodeMemberGetMemberRequest(request)")
    );
    assert!(
        client
            .content
            .contains("decodeMemberListMembersResponse(csilResp)")
    );
    assert!(
        client
            .content
            .contains("decodeMemberDeleteTaskResponse(csilResp)")
    );

    let codec = files
        .iter()
        .find(|f| f.path == "Codec.swift")
        .expect("Codec.swift emitted");
    // Per-op helpers for non-record shapes are exported, so a server in another module can
    // compose decode(request)/encode(response) for every op — not just record↔record.
    assert!(
        codec.content.contains(
            "public func decodeMemberGetMemberRequest(_ bytes: [UInt8]) throws -> MemberId"
        )
    );
    assert!(codec.content.contains(
        "public func encodeMemberListMembersResponse(_ value: [Member]) throws -> [UInt8]"
    ));
    assert!(
        codec.content.contains(
            "public func encodeMemberDeleteTaskResponse(_ value: Bool) throws -> [UInt8]"
        )
    );
    // The array response decodes through the Member record codec, not a `.null` stub.
    assert!(
        codec
            .content
            .contains("try CsilCbor.asArray(csilRoot).map { try Member(cborValue: $0) }")
    );
    // The map response round-trips as a real CBOR map.
    assert!(
        codec
            .content
            .contains("public func encodeMemberMemberNamesResponse")
    );
}

#[test]
fn async_twin_emitted_by_default_with_marked_symbols() {
    // Default client_style is `both`: an `async` twin at ClientAsync.swift whose symbols
    // carry an `Async` marker so it coexists with the blocking client in one module.
    let input = input_from_rules("swift-client", corndogs_rules(), 1);
    let files = build_files(&input).expect("ok");
    let twin = files
        .iter()
        .find(|f| f.path == "ClientAsync.swift")
        .expect("ClientAsync.swift emitted");

    // `async` transport seam, marked protocol name.
    assert!(
        twin.content
            .contains("public protocol AsyncCsilTransport {")
    );
    assert!(twin.content.contains(
        "func call(service: String, op: String, request: [UInt8]) async throws -> [UInt8]"
    ));
    // Marked per-service client over the marked seam.
    assert!(twin.content.contains("public struct CorndogsAsyncClient {"));
    assert!(
        twin.content
            .contains("public let transport: AsyncCsilTransport")
    );
    // Methods are `async throws` and `await` the byte seam.
    assert!(
        twin.content
            .contains("public func submitTask(_ request: SubmitTaskRequest) async throws -> Task")
    );
    // submit-task has a ServiceError arm, so it needs the reply variant.
    assert!(twin.content.contains("try await transport.callReply("));
    assert!(twin.content.contains("Task.fromCbor(csilResp)"));
    // The twin reuses the module's CsilClientError (declared in Client.swift).
    assert!(!twin.content.contains("public struct CsilClientError"));

    // The sync client is untouched: blocking seam + canonical names, no `async`.
    let sync = files
        .iter()
        .find(|f| f.path == "Client.swift")
        .expect("Client.swift emitted");
    assert!(sync.content.contains("public struct CorndogsClient {"));
    assert!(
        sync.content
            .contains("public func submitTask(_ request: SubmitTaskRequest) throws -> Task")
    );
    assert!(!sync.content.contains("async"));
    assert!(!sync.content.contains("await"));
    assert!(sync.content.contains("public struct CsilClientError"));
}

#[test]
fn client_style_async_is_drop_in_at_canonical_path() {
    // `client_style: async` yields a single `async` client at the canonical path with
    // the canonical symbol names — a drop-in for a blocking consumer.
    let mut input = input_from_rules("swift-client", corndogs_rules(), 1);
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("async"));
    let files = build_files(&input).expect("ok");
    assert!(files.iter().any(|f| f.path == "Client.swift"));
    assert!(
        !files.iter().any(|f| f.path == "ClientAsync.swift"),
        "async drop-in emits no separate twin"
    );

    let client = files.iter().find(|f| f.path == "Client.swift").unwrap();
    // Canonical (unmarked) names, but `async`.
    assert!(client.content.contains("public protocol CsilTransport {"));
    assert!(client.content.contains(
        "func call(service: String, op: String, request: [UInt8]) async throws -> [UInt8]"
    ));
    assert!(client.content.contains("public struct CorndogsClient {"));
    assert!(
        client
            .content
            .contains("public let transport: CsilTransport")
    );
    assert!(
        client
            .content
            .contains("public func submitTask(_ request: SubmitTaskRequest) async throws -> Task")
    );
    assert!(client.content.contains("try await transport.callReply("));
    // The drop-in is the primary file, so it still declares the shared error.
    assert!(client.content.contains("public struct CsilClientError"));
}

#[test]
fn client_style_sync_suppresses_the_twin() {
    let mut input = input_from_rules("swift-client", corndogs_rules(), 1);
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("sync"));
    let files = build_files(&input).expect("ok");
    assert!(files.iter().any(|f| f.path == "Client.swift"));
    assert!(!files.iter().any(|f| f.path == "ClientAsync.swift"));
    let client = files.iter().find(|f| f.path == "Client.swift").unwrap();
    assert!(!client.content.contains("async"));
    assert!(!client.content.contains("await"));
    assert!(!client.content.contains("AsyncCsilTransport"));
}

#[test]
fn client_style_invalid_value_is_rejected() {
    // A bad value fails the whole run regardless of surface.
    let mut input = input_from_rules("swift-client", corndogs_rules(), 1);
    input
        .config
        .options
        .insert("client_style".to_string(), serde_json::json!("blocking"));
    assert!(build_files(&input).is_err());

    // The validator names the offending option so the failure is actionable.
    let mut opts = HashMap::new();
    opts.insert("client_style".to_string(), serde_json::json!("blocking"));
    let err = client_style(&opts).expect_err("invalid client_style must be rejected");
    assert!(
        err.contains("client_style"),
        "error should name the option: {err}"
    );
}

fn typedef_rule(name: &str, ty: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(ty),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

#[test]
fn named_map_alias_field_roundtrips_as_a_map_not_a_stub() {
    // Regression: a field typed as a transparent named map alias
    // (`StringInt64Map = {* text => int}`) used to fall through the record check to
    // the `.null` / `asText` stub, dropping the entire dictionary on the wire.
    let map_ty = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("int")),
        occurrence: None,
    };
    let rules = vec![
        typedef_rule("StringInt64Map", map_ty),
        group_rule(
            "Holder",
            vec![bare_entry("counts", reference("StringInt64Map"))],
        ),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    // Encode must build a CBOR map; decode must read one back via asCanonicalMap, which checks the key order.
    assert!(codec.contains("CsilCborValue.map(self.counts.map {"));
    assert!(codec.contains("try CsilCbor.asCanonicalMap"));
    assert!(codec.contains("reduce(into: [String: Int64]()"));
    // The stub must be gone for this field.
    let body = codec.split("extension Holder").nth(1).unwrap();
    assert!(!body.contains("\"counts\", .null"));
    assert!(!body.contains("CsilCbor.asText(try CsilCbor.require(cborValue, \"counts\")"));
}

#[test]
fn named_map_of_record_alias_recurses_into_the_record_codec() {
    // `M = {* text => SomeRecord}`: the alias resolves to a map whose values are a
    // record, so the value handler must recurse to the record's
    // `toCborValue` / `init(cborValue:)` rather than stubbing.
    let map_of_record = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(reference("SomeRecord")),
        occurrence: None,
    };
    let rules = vec![
        typedef_rule("M", map_of_record),
        group_rule("SomeRecord", vec![bare_entry("id", builtin("text"))]),
        group_rule("Holder", vec![bare_entry("items", reference("M"))]),
    ];
    let input = input_from_rules("swift", rules, 0);
    let codec = generate_codec(&input).expect("codec emitted");
    let body = codec.split("extension Holder").nth(1).unwrap();
    // Encode recurses to the record value's codec inside a map builder.
    assert!(body.contains("CsilCborValue.map(try self.items.map {"));
    assert!(body.contains("try $0.value.toCborValue()"));
    // Decode reads a map and reconstructs each record value.
    assert!(body.contains("try CsilCbor.asCanonicalMap"));
    assert!(body.contains("try SomeRecord(cborValue: $1.1)"));
    assert!(!body.contains("\"items\", .null"));
}

#[test]
fn wire_strings_are_verbatim_csil_names() {
    // `service CorndogsService` hits the wire as "CorndogsService" — no `Service`
    // suffix stripping, no lowercasing — and `submit-task` stays "submit-task",
    // verbatim per csil-rpc-transport.md §1.1.
    let ops = vec![make_op(
        "submit-task",
        reference("SubmitTaskRequest"),
        reference("Task"),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule(
                "SubmitTaskRequest",
                vec![bare_entry("queue", builtin("text"))],
            ),
            group_rule("Task", vec![bare_entry("uuid", builtin("text"))]),
            service_rule("CorndogsService", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let client = files.iter().find(|f| f.path == "Client.swift").unwrap();
    assert!(client.content.contains("service: \"CorndogsService\""));
    assert!(client.content.contains("op: \"submit-task\""));
    assert!(!client.content.contains("\"corndogs\""));
    assert!(!client.content.contains("\"SubmitTask\""));
}

// ---------------------------------------------------------------------------
// Self-contained SwiftPM package mode
// ---------------------------------------------------------------------------

/// Build an input whose generator options are pre-populated, so package-mode
/// triggers (`emit_packages`, `package_name`, `package_version`) can be exercised.
fn input_with_options(
    target: &str,
    rules: Vec<CsilRule>,
    service_count: usize,
    options: Vec<(&str, serde_json::Value)>,
) -> WasmGeneratorInput {
    let mut input = input_from_rules(target, rules, service_count);
    input.config.options = options
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    input
}

fn one_record_rules() -> Vec<CsilRule> {
    vec![group_rule(
        "Task",
        vec![bare_entry("uuid", builtin("text"))],
    )]
}

/// Swift `Regex` needs macOS 13 / iOS 16, above SwiftPM's default floor, so a
/// package with a `.regex` field must declare it or it fails to build on macOS.
/// A package without one keeps the default floor for its users.
#[test]
fn package_manifest_declares_a_platform_floor_only_for_regex() {
    let manifest = |rules: Vec<CsilRule>| {
        let input = input_with_options(
            "swift",
            rules,
            0,
            vec![("emit_packages", serde_json::json!(["swift"]))],
        );
        build_files(&input)
            .expect("ok")
            .into_iter()
            .find(|f| f.path == "Package.swift")
            .expect("Package.swift emitted")
            .content
    };
    let plain = manifest(one_record_rules());
    assert!(!plain.contains("platforms:"), "{plain}");
    let regex = manifest(vec![group_rule(
        "Slug",
        vec![bare_entry(
            "value",
            CsilTypeExpression::Constrained {
                base_type: Box::new(builtin("text")),
                constraints: vec![CsilControlOperator::Regex("^[a-z]+$".to_string())],
            },
        )],
    )]);
    assert!(
        regex.contains("  platforms: [.macOS(.v13), .iOS(.v16), .tvOS(.v16), .watchOS(.v9)],\n"),
        "{regex}"
    );
    assert!(
        regex.contains("\n  products: [\n"),
        "the manifest keeps its indentation"
    );
}

#[test]
fn package_mode_emits_manifest_and_relocates_sources() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![("emit_packages", serde_json::json!(["swift"]))],
    );
    let files = build_files(&input).expect("ok");

    let manifest = files
        .iter()
        .find(|f| f.path == "Package.swift")
        .expect("Package.swift emitted in package mode");
    assert!(manifest.content.contains("// swift-tools-version:5.9"));
    assert!(manifest.content.contains("import PackageDescription"));
    // Default coordinates: package + product + target all "CsilgenClient".
    assert!(manifest.content.contains("name: \"CsilgenClient\""));
    assert!(
        manifest
            .content
            .contains(".library(name: \"CsilgenClient\", targets: [\"CsilgenClient\"])")
    );
    assert!(
        manifest
            .content
            .contains(".target(name: \"CsilgenClient\")")
    );

    // The package README rides at the root, beside the manifest (never under Sources/).
    assert!(files.iter().any(|f| f.path == "genquickstart.md"));

    // Generated sources live under SwiftPM's required Sources/<Target>/ layout, and
    // none remain at the package root.
    assert!(
        files
            .iter()
            .any(|f| f.path == "Sources/CsilgenClient/Types.swift")
    );
    assert!(
        !files
            .iter()
            .any(|f| f.path == "Types.swift" || f.path == "Codec.swift")
    );
}

#[test]
fn package_name_and_version_drive_manifest_coordinates() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            // A kebab package name keeps its raw spelling for the package/product, but
            // the target identifier is PascalCased so it is a valid Swift identifier.
            ("package_name", serde_json::json!("my-client")),
            ("package_version", serde_json::json!("2.4.0")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let manifest = files
        .iter()
        .find(|f| f.path == "Package.swift")
        .expect("Package.swift emitted");
    assert!(manifest.content.contains("name: \"my-client\""));
    assert!(
        manifest
            .content
            .contains(".library(name: \"my-client\", targets: [\"MyClient\"])")
    );
    assert!(manifest.content.contains(".target(name: \"MyClient\")"));
    // Version informs the manifest comment only (SwiftPM uses git tags).
    assert!(manifest.content.contains("my-client 2.4.0"));
    // Sources relocate under the PascalCased target directory.
    assert!(
        files
            .iter()
            .any(|f| f.path == "Sources/MyClient/Types.swift")
    );
}

#[test]
fn non_package_mode_leaves_output_unchanged() {
    let input = input_from_rules("swift", one_record_rules(), 0);
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Types.swift"));
    assert!(!files.iter().any(|f| f.path.starts_with("Sources/")));
}

#[test]
fn emit_packages_without_swift_stays_off() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        // Other languages requested, but not Swift.
        vec![("emit_packages", serde_json::json!(["go", "rust"]))],
    );
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Types.swift"));
}

#[test]
fn emit_packages_non_array_is_parsed_defensively() {
    // A malformed (non-array) value must not error and must not enable package mode.
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![("emit_packages", serde_json::json!("swift"))],
    );
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Types.swift"));
}

// --- package README: 3-transport Quickstart ---------------------------------
//
// No Swift toolchain is available here (no swiftc), so the emitted sections cannot be
// compiled or run; these tests assert the structural contract instead (assert-only).

/// A minimal `ping: Ping -> Pong` unary service over two single-field records — used for
/// the RPC + Datagrams sections and the no-channel Events note.
fn pingpong_rules() -> Vec<CsilRule> {
    vec![
        group_rule("Ping", vec![bare_entry("msg", builtin("text"))]),
        group_rule("Pong", vec![bare_entry("msg", builtin("text"))]),
        service_rule(
            "Echo",
            vec![make_op(
                "ping",
                reference("Ping"),
                reference("Pong"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]
}

/// The verification spec: a `->` unary op AND a record-typed `<->` channel op on one
/// service, so every section has a generated surface to wire to (per the contract).
fn transports_rules() -> Vec<CsilRule> {
    vec![
        group_rule("Ping", vec![bare_entry("msg", builtin("text"))]),
        group_rule("Pong", vec![bare_entry("msg", builtin("text"))]),
        group_rule("ChatMsg", vec![bare_entry("body", builtin("text"))]),
        group_rule("ChatEvent", vec![bare_entry("body", builtin("text"))]),
        service_rule(
            "Echo",
            vec![
                make_op(
                    "ping",
                    reference("Ping"),
                    reference("Pong"),
                    CsilServiceDirection::Unidirectional,
                ),
                make_op(
                    "chat",
                    reference("ChatMsg"),
                    reference("ChatEvent"),
                    CsilServiceDirection::Bidirectional,
                ),
            ],
            Some(7),
        ),
    ]
}

fn render_transports_readme(options: Vec<(&str, serde_json::Value)>) -> String {
    let mut opts = vec![
        ("emit_packages", serde_json::json!(["swift"])),
        ("package_name", serde_json::json!("Echo")),
    ];
    opts.extend(options);
    let input = input_with_options("swift-client", transports_rules(), 1, opts);
    let files = build_files(&input).expect("ok");
    files
        .into_iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md emitted at the package root")
        .content
}

#[test]
fn readme_intro_credits_the_transport_library() {
    let c = render_transports_readme(vec![]);
    // The intro credits the lib for the envelope/framing/lifecycle and the carrier model.
    assert!(c.contains("`CsilgenTransport` library owns the envelope"));
    assert!(c.contains("supply only a *carrier* that moves bytes"));
    // The Install section adds the transport-lib dependency with the not-yet-published note.
    assert!(c.contains(".package(path: \"../csilgen/transports/swift\")"));
    assert!(c.contains("not yet published"));
}

#[test]
fn readme_rpc_section_uses_lib_envelope_and_example_call() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-RPC (HTTP)"));
    assert!(c.contains("import CsilgenTransport"));
    // The carrier implements the generated byte seam and uses the lib's RpcRequest envelope.
    assert!(c.contains("struct HttpRpcCarrier: CsilTransport"));
    assert!(
        c.contains("func call(service: String, op: String, request: [UInt8]) throws -> [UInt8]")
    );
    assert!(c.contains("RpcRequest(service: service, op: op, payload: request).encode()"));
    // It POSTs the canonical mount over URLSession.
    assert!(c.contains("/csil/v1/rpc"));
    assert!(c.contains("httpMethod = \"POST\""));
    assert!(c.contains("URLSession.shared.dataTask"));
    // Decode + both error arms via the lib: non-zero transport status and the ServiceError arm.
    assert!(c.contains("RpcResponse.decode(try outcome.get())"));
    assert!(c.contains("resp.asTransportError()"));
    // The generated client resolves reply variants; the carrier never does.
    assert!(!c.contains("resolveRpcReply("));
    assert!(!c.contains("func callReply("));
    assert!(c.contains("try roundTrip(service: service, op: op, request: request).payload"));
    // The typed client is built over the carrier and the example calls the first `->` op.
    assert!(
        c.contains("EchoClient(transport: HttpRpcCarrier(baseURL: \"http://localhost:5080\"))")
    );
    assert!(c.contains("try client.ping(Ping(msg: \"example\"))"));
    assert!(c.contains("import Echo"));
}

#[test]
fn readme_events_section_handshake_heartbeat_and_router_dispatch() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-Events (TLS)"));
    // A frame carrier over a TLS byte stream via the lib's StreamCarrier framing, built
    // with an explicit max-frame limit so an operator can see and change the guard
    // without editing generated source (conventions doc §5).
    assert!(c.contains(
        "stream: TlsByteStream(host: \"localhost\", port: 7443), maxFrame: maxFrameDefault"
    ));
    assert!(c.contains("let carrier = try StreamCarrier("));
    assert!(c.contains("final class TlsByteStream: ByteStream"));
    // The $hello / $hello-ack handshake with the lib Hello/HelloAck.
    assert!(
        c.contains("Hello(versions: [csilVersion], profiles: [\"verbose\"], service: \"Echo\")")
    );
    assert!(c.contains("HelloAck.decode(ackFrame)"));
    assert!(c.contains("Profile.parse(ack.profile)"));
    // The $ping / $pong heartbeat via the lib Heartbeat + control names.
    assert!(c.contains("ev.event == Control.pingName"));
    assert!(c.contains("Control.pongName"));
    assert!(c.contains("Heartbeat(nonce: ping.nonce)"));
    // One outbound event via the generated encoder, dispatch into the generated router.
    assert!(c.contains("struct Handler: Echo"));
    // A frame from the peer must not trap the sample session.
    assert!(!c.contains("as! T") && !c.contains("ev.event!"), "{c}");
    assert!(c.contains("if let v = value as? ChatEvent { return try v.toCbor() }"));
    assert!(c.contains("encodeEchoChat(codec: codec, msg: ChatEvent(body: \"example\"))"));
    assert!(
        c.contains("guard let op = ev.event else { continue }\n        try routeEchoChannel(handler, codec: codec, op: op, data: ev.payload)")
    );
}

#[test]
fn readme_datagrams_section_send_and_late_arrival_note() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-Datagrams (UDP)"));
    assert!(c.contains("final class UdpDatagramCarrier: DatagramCarrier"));
    // Encode a `->` request via the generated codec, wrap in the lib Datagram, fire-and-forget.
    assert!(c.contains("let req: Ping = Ping(msg: \"example\")"));
    assert!(c.contains("let payload = try req.toCbor()\n    try carrier.sendDatagram(Datagram(opOrd: opOrd, seq: 0, payload: payload).encode())"));
    // The op's datagram ordinal (ping op has no @wire-id → placeholder 1).
    assert!(c.contains("let opOrd: UInt64 = 1"));
    // Recv path decodes an inbound Datagram into the RESPONSE type, with the loss note.
    assert!(c.contains("Datagram.decode(inbound)"));
    assert!(c.contains("Pong.fromCbor(dg.payload)"));
    assert!(c.contains("MAY arrive later — or never"));
}

#[test]
fn readme_no_channel_op_emits_events_note() {
    // pingpong has only a `->` op: Events still shows handshake + heartbeat, but notes the
    // absence of a generated channel router rather than wiring dispatch.
    let input = input_with_options(
        "swift-client",
        pingpong_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let c = &files
        .iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md")
        .content;
    assert!(c.contains("## CSIL-Events (TLS)"));
    assert!(c.contains("Hello(versions: [csilVersion], profiles: [\"verbose\"]).encode()"));
    assert!(c.contains("no generated channel router"));
    assert!(!c.contains("routeEchoChannel"));
}

#[test]
fn readme_transport_subset_renders_only_listed_sections() {
    // `genquickstart_transports: ["rpc"]` renders only the RPC section.
    let c = render_transports_readme(vec![(
        "genquickstart_transports",
        serde_json::json!(["rpc"]),
    )]);
    assert!(c.contains("## CSIL-RPC (HTTP)"));
    assert!(!c.contains("## CSIL-Events (TLS)"));
    assert!(!c.contains("## CSIL-Datagrams (UDP)"));

    // An absent option renders all three.
    let all = render_transports_readme(vec![]);
    assert!(all.contains("## CSIL-RPC (HTTP)"));
    assert!(all.contains("## CSIL-Events (TLS)"));
    assert!(all.contains("## CSIL-Datagrams (UDP)"));

    // An all-unknown array falls back to all three rather than an empty doc.
    let unknown = render_transports_readme(vec![(
        "genquickstart_transports",
        serde_json::json!(["bogus"]),
    )]);
    assert!(unknown.contains("## CSIL-RPC (HTTP)"));
    assert!(unknown.contains("## CSIL-Events (TLS)"));
    assert!(unknown.contains("## CSIL-Datagrams (UDP)"));
}

#[test]
fn package_readme_absent_without_package_mode() {
    let input = input_from_rules("swift-client", pingpong_rules(), 1);
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "genquickstart.md"));
}

#[test]
fn package_readme_opt_out_suppresses_only_readme() {
    // By default the README rides at the package root in package mode.
    let default_input = input_with_options(
        "swift-client",
        pingpong_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
        ],
    );
    let default_files = build_files(&default_input).expect("ok");
    assert!(default_files.iter().any(|f| f.path == "genquickstart.md"));

    // An explicit `emit_readme: false` suppresses only the README; the manifest and the
    // relocated sources are unchanged.
    let input = input_with_options(
        "swift-client",
        pingpong_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
            ("emit_readme", serde_json::json!(false)),
        ],
    );
    let files = build_files(&input).expect("ok");
    assert!(!files.iter().any(|f| f.path == "genquickstart.md"));
    assert!(files.iter().any(|f| f.path == "Package.swift"));
    assert!(files.iter().any(|f| f.path == "Sources/Echo/Types.swift"));
}

#[test]
fn empty_package_name_falls_back_to_default() {
    let input = input_with_options(
        "swift",
        one_record_rules(),
        0,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            // A blank name must not yield an empty target; fall back to the default.
            ("package_name", serde_json::json!("   ")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let manifest = files
        .iter()
        .find(|f| f.path == "Package.swift")
        .expect("Package.swift emitted");
    assert!(
        manifest
            .content
            .contains(".target(name: \"CsilgenClient\")")
    );
}

/// A package must be SELF-CONTAINED: its genquickstart references the typed client
/// (CSIL-RPC/Datagrams), the channel router + handler protocol (CSIL-Events), and the
/// codec (all sections), so the emitted file set must carry ALL THREE surfaces together —
/// Client.swift (client), Services.swift (router/protocol), and Codec.swift (codec) — even
/// from a single-surface (sub-)target like `swift-client`, which flat-mode would emit
/// without the router. No swiftc here, so this is assert-only: the file set proves the
/// quickstart's symbols all resolve against the single package. Mirrors the OCaml generator.
#[test]
fn package_mode_is_self_contained_with_client_router_and_codec() {
    let input = input_with_options(
        "swift-client",
        transports_rules(),
        1,
        vec![
            ("emit_packages", serde_json::json!(["swift"])),
            ("package_name", serde_json::json!("Echo")),
        ],
    );
    let files = build_files(&input).expect("ok");
    let has = |name: &str| files.iter().any(|f| f.path.ends_with(name));

    // The package carries the RPC client AND the channel router AND the codec together.
    assert!(
        has("/Client.swift"),
        "package missing the RPC client surface"
    );
    assert!(
        has("/Services.swift"),
        "package missing the channel-router surface"
    );
    assert!(has("/Codec.swift"), "package missing the codec");

    // Services.swift actually defines the router + handler protocol the Events section calls.
    let services = files
        .iter()
        .find(|f| f.path.ends_with("/Services.swift"))
        .unwrap();
    assert!(services.content.contains("public protocol Echo"));
    assert!(services.content.contains(
        "public func routeEchoChannel(_ handler: Echo, codec: CsilCodec, op: String, data: [UInt8]) throws"
    ));
    // Client.swift defines the typed client the RPC/Datagrams sections call.
    let client = files
        .iter()
        .find(|f| f.path.ends_with("/Client.swift"))
        .unwrap();
    assert!(client.content.contains("EchoClient"));

    // The Events section dispatches via the generated router + encoder (not codec-direct).
    let quickstart = files.iter().find(|f| f.path == "genquickstart.md").unwrap();
    assert!(
        quickstart.content.contains(
            "guard let op = ev.event else { continue }\n        try routeEchoChannel(handler, codec: codec, op: op, data: ev.payload)"
        )
    );
    assert!(
        quickstart
            .content
            .contains("encodeEchoChat(codec: codec, msg: ChatEvent(body: \"example\"))")
    );

    // A flat (non-package) client build stays surface-only: Client.swift but no Services.swift.
    let flat = build_files(&input_from_rules("swift-client", transports_rules(), 1)).expect("ok");
    assert!(flat.iter().any(|f| f.path == "Client.swift"));
    assert!(
        !flat.iter().any(|f| f.path == "Services.swift"),
        "flat client build must stay surface-only"
    );
}

/// Regression: an existing rule `UserData` and a field `User.data` typed as an inline
/// MIXED-kind literal choice (`"x" / 1` — not a uniform text/scalar vocabulary, so it
/// exercises the `all_mixed_literals` bare-enum shape too) — the field's naive
/// synthesized name is `User_data` (owner `User`, field `data`), which
/// pascal-collides with the existing `UserData` (`swift_type_name` maps both to
/// `"UserData"`). Before this generator's hoister migrated to
/// `csilgen_common::hoist_inline_composites` — which reserves every existing rule
/// name's case-insensitive canonical key up front (see `crates/csilgen-common/src/
/// hoist.rs`'s `canonical_key`/`reserve`) — swift's own hand-rolled hoister
/// (`format!("{owner}_{wire}")`, no reservation set at all) would have silently
/// synthesized the exact colliding name `User_data` here, emitting two Swift
/// declarations named `UserData`: a non-compiling duplicate. This is a real latent
/// bug the migration fixes as a side effect, not a hypothetical.
#[test]
fn case_insensitive_collision_between_existing_rule_and_hoisted_inline_choice_is_disambiguated() {
    let user_data = group_rule("UserData", vec![bare_entry("value", builtin("text"))]);
    let user = group_rule(
        "User",
        vec![bare_entry(
            "data",
            CsilTypeExpression::Choice(vec![text_literal("x"), int_literal(1)]),
        )],
    );
    let input = input_from_rules("swift", vec![user_data, user], 0);
    let files = build_files(&input).expect("ok");
    let types = files
        .iter()
        .find(|f| f.path == "Types.swift")
        .expect("Types.swift emitted")
        .content
        .clone();

    // The original UserData record survives unchanged.
    assert!(types.contains("public struct UserData: Equatable, Sendable {"));
    // The field routes through a DISAMBIGUATED synthesized type, not the raw
    // colliding "UserData" a naive `format!("{owner}_{wire}")` scheme would produce.
    assert!(types.contains("public let data: UserData2"));
    assert!(types.contains(
        "public enum UserData2: Equatable, Sendable, CaseIterable {\n  case x\n  case v1\n"
    ));

    // Exactly one declaration for each name — not two conflicting `UserData`s.
    assert_eq!(
        types.matches("public struct UserData:").count(),
        1,
        "expected exactly one UserData declaration in {types}"
    );
    assert_eq!(
        types.matches("public enum UserData2:").count(),
        1,
        "expected exactly one UserData2 declaration in {types}"
    );
    assert!(
        !types.contains("public enum UserData:"),
        "the synthesized mixed-choice type must not collide with the UserData struct: {types}"
    );
}

fn compile_regression_input() -> WasmGeneratorInput {
    let event = opt_entry(
        "event",
        CsilTypeExpression::Constrained {
            base_type: Box::new(reference("NodeEvent")),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "state".into(),
            ))],
        },
    );
    input_from_rules(
        "swift-client",
        vec![
            CsilRule {
                name: "NodeEvent".into(),
                rule_type: CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                    text_literal("ready"),
                    text_literal("state"),
                    text_literal("default"),
                ])),
                position: pos(),
                doc_comments: vec![],
            },
            group_rule("Deadline", vec![bare_entry("due", builtin("date"))]),
            group_rule("Stamp", vec![bare_entry("at", builtin("timestamp"))]),
            group_rule("Moment", vec![bare_entry("seen", builtin("cddl_time"))]),
            group_rule(
                "NodeReport",
                vec![bare_entry("query", builtin("text")), event],
            ),
            service_rule(
                "NodeService",
                vec![make_op(
                    "report",
                    reference("NodeReport"),
                    reference("NodeReport"),
                    CsilServiceDirection::Unidirectional,
                )],
                None,
            ),
        ],
        1,
    )
}

#[test]
fn enum_defaults_use_the_declared_case_names() {
    let mut input = compile_regression_input();
    input.csil_spec.rules.extend([
        typedef_rule("EventAlias", reference("NodeEvent")),
        typedef_rule(
            "Priority",
            CsilTypeExpression::Choice(vec![int_literal(-1), int_literal(2)]),
        ),
        typedef_rule(
            "Flag",
            CsilTypeExpression::Choice(vec![bool_literal(true), bool_literal(false)]),
        ),
        typedef_rule(
            "Mixed",
            CsilTypeExpression::Choice(vec![text_literal("ready"), int_literal(2)]),
        ),
    ]);
    let cases = [
        (
            reference("NodeEvent"),
            CsilLiteralValue::Text("default".into()),
            ".`default`",
        ),
        (
            reference("EventAlias"),
            CsilLiteralValue::Text("state".into()),
            ".state",
        ),
        (
            reference("Priority"),
            CsilLiteralValue::Integer(-1),
            ".vNeg1",
        ),
        (reference("Flag"), CsilLiteralValue::Bool(false), ".vFalse"),
        (reference("Mixed"), CsilLiteralValue::Integer(2), ".v2"),
        (
            builtin("text"),
            CsilLiteralValue::Text("state".into()),
            "\"state\"",
        ),
    ];
    for (ty, value, expected) in cases {
        let mut field = bare_entry("value", ty);
        field.metadata.push(CsilFieldMetadata::Constraint(
            CsilValidationConstraint::Custom {
                name: "default".into(),
                value,
            },
        ));
        let mut case_input = input.clone();
        case_input
            .csil_spec
            .rules
            .push(group_rule("Defaults", vec![field]));
        let files = build_files(&case_input).unwrap();
        let types = &files
            .iter()
            .find(|file| file.path == "Types.swift")
            .unwrap()
            .content;
        assert!(types.contains(&format!(" = {expected})")), "{types}");
    }
    assert_eq!(
        enum_default_swift(
            &CsilLiteralValue::Text("missing".into()),
            &reference("NodeEvent"),
            &input
        ),
        None
    );
    input
        .csil_spec
        .rules
        .push(typedef_rule("Cycle", reference("Cycle")));
    assert_eq!(
        enum_default_swift(&CsilLiteralValue::Integer(1), &reference("Cycle"), &input),
        None
    );
}

/// The Swift compiler from `SWIFTC` (default `swiftc`), or `None` to skip when it
/// is absent and `CSILGEN_REQUIRE_SWIFTC` is unset.
fn swift_compiler() -> Option<String> {
    let swiftc = std::env::var("SWIFTC").unwrap_or_else(|_| "swiftc".into());
    match std::process::Command::new(&swiftc)
        .arg("--version")
        .output()
    {
        Ok(output) => assert!(output.status.success(), "Swift compiler is not usable"),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && std::env::var_os("CSILGEN_REQUIRE_SWIFTC").is_none() =>
        {
            eprintln!("skipping Swift compiler test: install swiftc or set SWIFTC");
            return None;
        }
        Err(error) => panic!("Swift compiler is required: {error}"),
    }
    Some(swiftc)
}

/// Compile the client and check the CBOR wire format with a Swift compiler.
#[test]
fn generated_client_compiles_and_preserves_cbor() {
    let Some(swiftc) = swift_compiler() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let files = build_files(&compile_driver_input()).unwrap();
    let mut sources = Vec::new();
    for file in files {
        if file.path.ends_with(".swift") {
            std::fs::write(dir.path().join(&file.path), file.content).unwrap();
            sources.push(file.path);
        }
    }
    std::fs::write(dir.path().join("main.swift"), SWIFT_COMPILE_DRIVER).unwrap();
    sources.push("main.swift".into());
    let output = std::process::Command::new(&swiftc)
        .current_dir(dir.path())
        .args(["-swift-version", "5"])
        .args(&sources)
        .args(["-o", "regression"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Swift compile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(dir.path().join("regression"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "Swift runtime check failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // Apple SDK checks must use the oldest supported deployment target.
    #[cfg(target_os = "macos")]
    {
        let sdk = std::process::Command::new("xcrun")
            .args(["--sdk", "iphoneos", "--show-sdk-path"])
            .output()
            .unwrap();
        assert!(sdk.status.success(), "iOS SDK is required");
        let output = std::process::Command::new(&swiftc)
            .current_dir(dir.path())
            .args([
                "-swift-version",
                "5",
                "-typecheck",
                "-target",
                "arm64-apple-ios16.0",
                "-sdk",
            ])
            .arg(String::from_utf8(sdk.stdout).unwrap().trim())
            .args(&sources)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "iOS 16 compile failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The regression spec plus a multi-arm operation whose request and reply hold a
/// timestamp, so the driver reaches the client and reply-helper encode failures.
fn compile_driver_input() -> WasmGeneratorInput {
    let mut input = compile_regression_input();
    input.csil_spec.rules.extend([
        group_rule("StampError", vec![bare_entry("code", builtin("int"))]),
        service_rule(
            "Clock",
            vec![make_op(
                "stamp",
                reference("Stamp"),
                CsilTypeExpression::Choice(vec![reference("Stamp"), reference("StampError")]),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ]);
    input
}

const SWIFT_COMPILE_DRIVER: &str = r#"
import Foundation

let deadline = Deadline(due: "2000-02-29")
let dateBytes: [UInt8] = [0xa1, 0x63, 0x64, 0x75, 0x65, 0xd9, 0x03, 0xec, 0x6a]
    + Array("2000-02-29".utf8)
let deadlineBytes = try deadline.toCbor()
precondition(deadlineBytes == dateBytes)
let decodedDate = try Deadline.fromCbor(dateBytes)
precondition(decodedDate == deadline)
for text in ["2026-02-30", "1900-02-29", "2026-1-01"] {
    do {
        _ = try CsilCbor.decodeDate(.tag(1004, .text(text)))
        fatalError("invalid date was accepted")
    } catch CsilCborError.typeMismatch {}
}
let stamp = try Stamp(cborValue: .map([(.text("at"), .tag(0, .text("2000-02-29t13:30:00.5+01:30")))]))
precondition(stamp.at == "2000-02-29T12:00:00.500Z")
let stampBytes = try stamp.toCbor()
precondition(stampBytes
    == CsilCbor.encode(.map([(.text("at"), .tag(0, .text("2000-02-29T12:00:00.500Z")))])))
let nanos = try CsilCbor.decodeTimestamp(.tag(0, .text("2000-02-29T12:00:00.000000500Z")))
precondition(nanos == "2000-02-29T12:00:00.000000500Z")
for text in ["2000-02-29 12:00:00Z", "2000-02-29T23:59:60Z", "2000-02-29T12:00:00",
             "0001-01-01T00:30:00+01:00", "9999-12-31T23:30:00-01:00", "2000-02-29T12:00:00.Z"] {
    do {
        _ = try CsilCbor.decodeTimestamp(.tag(0, .text(text)))
        fatalError("invalid timestamp was accepted")
    } catch CsilCborError.typeMismatch {}
}
let epochHalf: CsilCborValue = .map([(.text("seen"), .tag(1, .double(951825600.5)))])
let moment = try Moment(cborValue: epochHalf)
precondition(moment.seen == "2000-02-29T12:00:00.500Z")
let momentBytes = try moment.toCbor()
precondition(momentBytes == CsilCbor.encode(epochHalf))
let epochBytes = CsilCbor.encode(try CsilCbor.encodeEpoch("2000-02-29T13:00:00+01:00"))
precondition(epochBytes == [0xc1, 0x1a, 0x38, 0xbb, 0xb4, 0xc0])
let epochCases: [(CsilCborValue, String)] = [
    (.tag(1, .int(-1)), "1969-12-31T23:59:59Z"),
    (try CsilCbor.decode([0xc1, 0xf9, 0x3c, 0x00]), "1970-01-01T00:00:01Z"),
    (.tag(1, .double(0.9999999999)), "1970-01-01T00:00:01Z"),
    (.tag(1, .double(-0.5)), "1969-12-31T23:59:59.500Z"),
]
for (value, want) in epochCases {
    let got = try CsilCbor.decodeEpoch(value)
    precondition(got == want, "decoded \(got), want \(want)")
}
let refusedEpochs: [CsilCborValue] = [.tag(1, .uint(253402300800)), .tag(1, .int(-62135596801)),
    .tag(1, .double(.nan)), .tag(1, .double(.infinity)), .tag(1, .text("0")), .tag(0, .int(0)), .int(0)]
for value in refusedEpochs {
    do {
        _ = try CsilCbor.decodeEpoch(value)
        fatalError("invalid cddl_time was accepted")
    } catch CsilCborError.typeMismatch {}
}
let report = NodeReport(query: "ok")
precondition(report.event == .state)
// {"event": "state", "query": "ok"}, in canonical CBOR key order.
let expected: [UInt8] = [0xa2, 0x65, 0x65, 0x76, 0x65, 0x6e, 0x74,
    0x65, 0x73, 0x74, 0x61, 0x74, 0x65, 0x65, 0x71, 0x75, 0x65, 0x72, 0x79,
    0x62, 0x6f, 0x6b]
let reportBytes = try report.toCbor()
precondition(reportBytes == expected)
let decoded = try NodeReport(cborValue: CsilCbor.decode(expected))
precondition(decoded == report)
let absent = NodeReport(query: "ok", event: nil)
let absentBytes = try absent.toCbor()
precondition(absentBytes == [0xa1, 0x65, 0x71, 0x75, 0x65, 0x72, 0x79, 0x62, 0x6f, 0x6b])
let decodedAbsent = try NodeReport(cborValue: CsilCbor.decode(absentBytes))
precondition(decodedAbsent == absent)
precondition(CsilCbor.valueEquals(.double(1.5), .double(1.5)))
precondition(!CsilCbor.valueEquals(.double(1.5), .double(2.5)))
let unicode = "\u{0}é🎵"
let decodedUnicode = try CsilCbor.asText(CsilCbor.decode(CsilCbor.encode(.text(unicode))))
precondition(decodedUnicode == unicode)
for invalid: [UInt8] in [[0x61, 0xff], [0x62, 0xc0, 0xaf], [0x63, 0xed, 0xa0, 0x80], [0x61, 0xc2]] {
    do {
        _ = try CsilCbor.decode(invalid)
        fatalError("invalid UTF-8 was accepted")
    } catch CsilCborError.malformed {}
}
// Bad text and an instant outside years 0001 to 9999 make encode throw, not trap.
for text in ["2000-02-30T00:00:00Z", "0001-01-01T00:30:00+01:00", "9999-12-31T23:30:00-01:00",
             "10000-01-01T00:00:00Z", "not a time", ""] {
    do {
        _ = try Stamp(at: text).toCbor()
        fatalError("timestamp encode accepted \(text)")
    } catch CsilCborError.typeMismatch {}
    do {
        _ = try Moment(seen: text).toCbor()
        fatalError("cddl_time encode accepted \(text)")
    } catch CsilCborError.typeMismatch {}
}
do {
    _ = try Deadline(due: "2026-02-30").toCbor()
    fatalError("date encode accepted 2026-02-30")
} catch CsilCborError.typeMismatch {}
// A map key with a long-form head, a repeated key, or (for a dynamic map) keys out
// of encoded-byte order is refused; a record takes its keys in any order.
for bad in [[0xa1, 0x18, 0x01, 0x60], [0xa1, 0x78, 0x01, 0x61, 0x60], [0xa1, 0x19, 0x00, 0x05, 0x60]] as [[UInt8]] {
    do {
        _ = try CsilCbor.decode(bad)
        fatalError("a long-form key was read")
    } catch CsilCborError.longFormKey {}
}
do {
    _ = try CsilCbor.decode([0xa2, 0x61, 0x61, 0x01, 0x61, 0x61, 0x02])
    fatalError("a repeated key was read")
} catch CsilCborError.duplicateKey(let key) { precondition(key == "\"a\"") }
precondition(CsilCbor.shortestHead([0x18, 0x18]) && !CsilCbor.shortestHead([0x18, 0x17]))
precondition(CsilCbor.shortestHead([0x1a, 0x00, 0x01, 0x00, 0x00]))
precondition(!CsilCbor.shortestHead([0x1b, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff]))
precondition(CsilCbor.shortestHead([0xf9, 0x00, 0x00]))
do {
    _ = try CsilCbor.asCanonicalMap(CsilCbor.decode([0xa2, 0x62, 0x61, 0x61, 0x01, 0x61, 0x62, 0x02]))
    fatalError("unsorted dynamic map keys were read")
} catch CsilCborError.mapKeyOrder {}
_ = try CsilCbor.asCanonicalMap(CsilCbor.decode([0xa2, 0x61, 0x62, 0x01, 0x62, 0x61, 0x61, 0x02]))
// Float keys are ordered on the bytes that arrived: a half float sorts before a double.
_ = try CsilCbor.asCanonicalMap(CsilCbor.decode([0xa2, 0xf9, 0x3c, 0x00, 0x01, 0xfb, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0, 0x02]))
do {
    _ = try CsilCbor.decode([0xa2, 0xfb, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0, 0x01, 0xf9, 0x40, 0x00, 0x02])
    fatalError("unsorted float keys were read")
} catch CsilCborError.mapKeyOrder {}
// A short tuple array from a peer throws instead of indexing out of range.
let second = try CsilCbor.item(.array([.text("a"), .int(1)]), 1)
precondition(CsilCbor.valueEquals(second, .int(1)))
for index in [1, -1] {
    do {
        _ = try CsilCbor.item(.array([.text("a")]), index)
        fatalError("tuple position \(index) was read")
    } catch CsilCborError.malformed {}
}

final class CountingCarrier: CsilTransport {
    var calls = 0
    func call(service: String, op: String, request: [UInt8]) throws -> [UInt8] {
        calls += 1
        return request
    }
    func callReply(service: String, op: String, request: [UInt8]) throws -> (variant: String?, payload: [UInt8]) {
        calls += 1
        return ("Stamp", request)
    }
}
let carrier = CountingCarrier()
let clock = ClockClient(transport: carrier)
let echoed = try clock.stamp(Stamp(at: "2000-02-29T12:00:00Z"))
precondition(echoed.at == "2000-02-29T12:00:00Z" && carrier.calls == 1)
do {
    _ = try clock.stamp(Stamp(at: "2000-02-29T24:00:00Z"))
    fatalError("client sent an unencodable request")
} catch CsilCborError.typeMismatch {}
precondition(carrier.calls == 1, "the transport saw a request that failed to encode")
// The reply helper throws for a reply it cannot encode, so the host answers status 6.
let reply = try encodeClockStampReply { Stamp(at: "2000-02-29T12:00:00Z") }
precondition(reply.variant == "Stamp")
do {
    _ = try encodeClockStampReply { Stamp(at: "2000-02-29T12:00:60Z") }
    fatalError("reply helper encoded an invalid timestamp")
} catch CsilCborError.typeMismatch {}
do {
    _ = try encodeClockStampReply { throw StampErrorRefusal(value: StampError(code: 1)) }
} catch {
    fatalError("a declared refusal must encode as its arm")
}
"#;

/// `Date` is a Double and `ISO8601DateFormatter` is lenient, so a timestamp stays
/// canonical UTC text that the codec checks and rewrites by hand.
#[test]
fn timestamp_codec_is_strict_and_canonical() {
    let files = build_files(&compile_regression_input()).unwrap();
    let codec = &files
        .iter()
        .find(|file| file.path == "Codec.swift")
        .unwrap()
        .content;
    assert!(
        codec.contains("CsilCbor.encodeTimestamp(self.at)"),
        "{codec}"
    );
    assert!(codec.contains("try CsilCbor.decodeTimestamp("), "{codec}");
    assert!(codec.contains("static func canonicalTimestamp(_ text: String) -> String?"));
    assert!(codec.contains("guard (epochFirst...epochLast).contains(total) else { return nil }"));
    assert!(!codec.contains("ISO8601DateFormatter()"));
}

/// `call` returns bytes alone, so a multi-arm operation goes through `callReply`
/// and the generated resolver; the seam declares `callReply` only for such a spec.
#[test]
fn multi_arm_operation_uses_call_reply_in_both_client_shapes() {
    let rules = vec![
        group_rule("Req", vec![bare_entry("a", builtin("text"))]),
        group_rule("Resp", vec![bare_entry("b", builtin("text"))]),
        group_rule("Err", vec![bare_entry("code", builtin("int"))]),
        service_rule(
            "Signer",
            vec![make_op(
                "sign",
                reference("Req"),
                CsilTypeExpression::Choice(vec![reference("Resp"), reference("Err")]),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ];
    let files = build_files(&input_from_rules("swift-client", rules, 1)).unwrap();
    let content = |path: &str| {
        files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
            .content
            .clone()
    };
    let sync = content("Client.swift");
    assert!(sync.contains(
        "  func callReply(\n    service: String,\n    op: String,\n    request: [UInt8]\n  ) throws -> (variant: String?, payload: [UInt8])\n"
    ), "{sync}");
    assert!(sync.contains(
        "    let csilReply = try transport.callReply(service: \"Signer\", op: \"sign\", request: csilRequest)\n    let csilResp = try resolveRpcReply(\n      service: \"Signer\",\n      op: \"sign\",\n      variant: csilReply.variant,\n      payload: csilReply.payload\n    )\n"
    ), "{sync}");
    let twin = content("ClientAsync.swift");
    assert!(
        twin.contains("async throws -> (variant: String?, payload: [UInt8])"),
        "{twin}"
    );
    assert!(
        twin.contains("let csilReply = try await transport.callReply("),
        "{twin}"
    );

    let plain = build_files(&compile_regression_input()).unwrap();
    assert!(plain.iter().all(|file| !file.content.contains("callReply")));
}

/// `cddl_time` shares the canonical `String` of `timestamp` and only swaps the
/// wire form, so it goes through the epoch codec and not the tag-0 one.
#[test]
fn cddl_time_is_a_string_with_the_epoch_codec() {
    let files = build_files(&compile_regression_input()).unwrap();
    let file = |name: &str| {
        &files
            .iter()
            .find(|file| file.path == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .content
    };
    assert!(
        file("Types.swift").contains("public let seen: String"),
        "{}",
        file("Types.swift")
    );
    let codec = file("Codec.swift");
    assert!(codec.contains("CsilCbor.encodeEpoch(self.seen)"), "{codec}");
    assert!(codec.contains("try CsilCbor.decodeEpoch("), "{codec}");
    assert!(
        codec.contains("public static func decodeEpoch(_ value: CsilCborValue) throws -> String")
    );
    // The epoch form may arrive as a half-precision float.
    assert!(
        codec.contains("Double(sign: sign, exponent: exponent - 25, significand: mantissa + 1024)")
    );
}

/// A bad value from a payload or from the caller must reach the caller as a thrown
/// error. A trap ends the whole process, so no generated source may hold one.
#[test]
fn encode_paths_throw_instead_of_trapping() {
    let files = build_files(&compile_driver_input()).unwrap();
    let file = |name: &str| {
        files
            .iter()
            .find(|file| file.path == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .content
            .clone()
    };
    let codec = file("Codec.swift");
    for signature in [
        "public static func encodeDate(_ text: String) throws -> CsilCborValue",
        "public static func encodeTimestamp(_ text: String) throws -> CsilCborValue",
        "public static func encodeEpoch(_ text: String) throws -> CsilCborValue",
        "func toCborValue() throws -> CsilCborValue",
        "func toCbor() throws -> [UInt8] { CsilCbor.encode(try toCborValue()) }",
        "public static func item(_ v: CsilCborValue, _ index: Int) throws -> CsilCborValue",
    ] {
        assert!(codec.contains(signature), "missing {signature}");
    }
    assert!(
        codec
            .contains("csilEntries.append((.text(\"at\"), try CsilCbor.encodeTimestamp(self.at)))")
    );
    assert!(
        codec
            .contains("csilEntries.append((.text(\"seen\"), try CsilCbor.encodeEpoch(self.seen)))")
    );
    assert!(codec.contains("    return (\"Stamp\", CsilCbor.encode(try value.toCborValue()))\n"));
    assert!(codec.contains("answer with transport status 6 (internal)"));
    let client = file("Client.swift");
    assert!(client.contains(
        "    let csilRequest = try request.toCbor()\n    let csilReply = try transport.callReply(service: \"Clock\", op: \"stamp\", request: csilRequest)\n"
    ), "{client}");
    let twin = file("ClientAsync.swift");
    assert!(twin.contains(
        "    let csilRequest = try request.toCbor()\n    let csilReply = try await transport.callReply("
    ), "{twin}");
    for file in files.iter().filter(|file| file.path.ends_with(".swift")) {
        for trap in [
            "precondition(",
            "preconditionFailure(",
            "fatalError(",
            "try!",
            "as!",
            "!)",
        ] {
            assert!(
                !file.content.contains(trap),
                "{} holds the trap {trap}",
                file.path
            );
        }
    }
}

fn positional(value_type: CsilTypeExpression, optional: bool) -> CsilGroupEntry {
    CsilGroupEntry {
        key: None,
        value_type,
        occurrence: optional.then_some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

fn tuple(entries: Vec<CsilGroupEntry>) -> CsilTypeExpression {
    CsilTypeExpression::Tuple(CsilGroupExpression { entries })
}

/// Every shape that once made generated Swift fail to compile or lose data: tuple
/// fields and tuple union arms (no synthesized `Equatable`), `nint`, `decimal`,
/// narrow floats, a `null` union arm, a union map key (not `Hashable`), `any`, a
/// constant array element, a spec type named `Error`, and a package that ships the
/// client and the services together (wire ids declared twice).
fn shapes_input() -> WasmGeneratorInput {
    let shapes = group_rule(
        "Shapes",
        vec![
            bare_entry("n", builtin("nint")),
            bare_entry("amount", builtin("decimal")),
            bare_entry("single", builtin("float32")),
            bare_entry(
                "pair",
                tuple(vec![
                    positional(builtin("text"), false),
                    positional(builtin("int"), false),
                ]),
            ),
            bare_entry(
                "triple",
                tuple(vec![
                    positional(builtin("text"), false),
                    positional(builtin("int"), true),
                    positional(builtin("bool"), true),
                ]),
            ),
            bare_entry("outcome", reference("Outcome")),
            bare_entry(
                "by_id",
                CsilTypeExpression::Map {
                    key: Box::new(reference("IdOrName")),
                    value: Box::new(builtin("int")),
                    occurrence: None,
                },
            ),
            bare_entry(
                "extra",
                CsilTypeExpression::Map {
                    key: Box::new(builtin("text")),
                    value: Box::new(builtin("any")),
                    occurrence: None,
                },
            ),
            bare_entry("status", reference("MixedStatus")),
            bare_entry(
                "nulls",
                CsilTypeExpression::Array {
                    element_type: Box::new(builtin("null")),
                    occurrence: None,
                },
            ),
            opt_entry(
                "maybe_pair",
                tuple(vec![
                    positional(builtin("text"), false),
                    positional(builtin("int"), false),
                ]),
            ),
        ],
    );
    let mut success = tuple(vec![
        positional(builtin("text"), false),
        positional(builtin("any"), false),
    ]);
    if let CsilTypeExpression::Tuple(group) = &mut success {
        group.entries[0].key = Some(CsilGroupKey::Bare("tag".into()));
        group.entries[1].key = Some(CsilGroupKey::Bare("data".into()));
    }
    let mut op = make_op(
        "reshape",
        reference("Shapes"),
        CsilTypeExpression::Choice(vec![reference("Shapes"), reference("Error")]),
        CsilServiceDirection::Unidirectional,
    );
    op.wire_id = Some(0);
    let mut input = input_from_rules(
        "swift",
        vec![
            typedef_rule(
                "MixedStatus",
                CsilTypeExpression::Choice(vec![
                    builtin("text"),
                    text_literal("pending"),
                    text_literal("confirmed"),
                ]),
            ),
            typedef_rule(
                "IdOrName",
                CsilTypeExpression::Choice(vec![builtin("uint"), builtin("text")]),
            ),
            typedef_rule("Success", success),
            typedef_rule(
                "Outcome",
                CsilTypeExpression::Choice(vec![reference("Success"), builtin("null")]),
            ),
            group_rule(
                "Error",
                vec![
                    bare_entry("code", builtin("int")),
                    bare_entry("message", builtin("text")),
                ],
            ),
            shapes,
            service_rule("Shaper", vec![op], Some(3)),
        ],
        1,
    );
    input
        .config
        .options
        .insert("emit_packages".into(), serde_json::json!(["swift"]));
    input
        .config
        .options
        .insert("package_name".into(), serde_json::json!("shapes"));
    input
        .config
        .options
        .insert("package_version".into(), serde_json::json!("0.1.0"));
    input
}

fn shapes_file(name: &str) -> String {
    build_files(&shapes_input())
        .unwrap()
        .into_iter()
        .find(|file| file.path.ends_with(name))
        .unwrap_or_else(|| panic!("missing {name}"))
        .content
}

#[test]
fn nint_is_int64_with_a_sign_check_both_ways() {
    assert!(shapes_file("Types.swift").contains("public let n: Int64"));
    let codec = shapes_file("Codec.swift");
    assert!(codec.contains("try CsilCbor.encodeNint(self.n)"), "{codec}");
    assert!(codec.contains("try CsilCbor.asNint(try CsilCbor.require(cborValue, \"n\"))"));
}

#[test]
fn decimal_travels_as_tag_4_not_text() {
    let codec = shapes_file("Codec.swift");
    assert!(
        codec.contains("try CsilCbor.encodeDecimal(self.amount)"),
        "{codec}"
    );
    assert!(codec.contains("try CsilCbor.decodeDecimal("), "{codec}");
    assert!(codec.contains("return .tag(4, .array([.int(-Int64(scale)), .int(mantissa)]))"));
}

#[test]
fn narrow_floats_are_doubles() {
    assert!(shapes_file("Types.swift").contains("public let single: Double"));
    assert!(shapes_file("Codec.swift").contains("(.text(\"single\"), .double(self.single))"));
}

#[test]
fn records_and_unions_with_tuples_spell_out_equality() {
    let types = shapes_file("Types.swift");
    assert!(types.contains("public enum CsilEq {"), "{types}");
    assert!(
        types.contains("public static func == (lhs: Shapes, rhs: Shapes) -> Bool {"),
        "{types}"
    );
    assert!(types.contains(
        "      && lhs.triple.field0 == rhs.triple.field0\n      && lhs.triple.field1 == rhs.triple.field1\n      && lhs.triple.field2 == rhs.triple.field2\n"
    ), "{types}");
    assert!(types.contains(
        "CsilEq.optional(lhs.maybePair, rhs.maybePair) {\n        (csilL0, csilR0) in csilL0.field0"
    ));
    assert!(
        types.contains("public static func == (lhs: Outcome, rhs: Outcome) -> Bool {"),
        "{types}"
    );
    assert!(
        types.contains("case (.null, .null): return true"),
        "{types}"
    );
    // A record without a tuple keeps the synthesized conformance.
    assert!(!types.contains("lhs: Error, rhs: Error"), "{types}");
}

#[test]
fn optional_tuple_positions_travel_as_null_and_keep_the_length() {
    let codec = shapes_file("Codec.swift");
    assert!(codec.contains(
        "self.triple.field1.map {\n              (csilSome) -> CsilCborValue in .int(csilSome)\n            } ?? CsilCborValue.null,"
    ), "{codec}");
    assert!(codec.contains(
        "field1: try CsilCbor.orNull(\n        try CsilCbor.item(try CsilCbor.require(cborValue, \"triple\"), 1, of: 3)\n      ) {\n        (csilSome) in try CsilCbor.asI64(csilSome)\n      },"
    ), "{codec}");
    assert!(codec.contains(", 2, of: 3)\n"), "{codec}");
}

#[test]
fn a_null_union_arm_has_no_payload() {
    let types = shapes_file("Types.swift");
    assert!(types.contains("  case null\n"), "{types}");
    let codec = shapes_file("Codec.swift");
    assert!(codec.contains("case .null:\n      return .array([.uint(1), .null])"));
    assert!(codec.contains("guard csilPayload == .null else { throw CsilCborError.typeMismatch }"));
}

#[test]
fn a_map_keyed_by_a_union_is_keyed_by_the_cbor_value() {
    let types = shapes_file("Types.swift");
    assert!(
        types.contains("public let byId: [AnyCsilValue: Int64]"),
        "{types}"
    );
    let codec = shapes_file("Codec.swift");
    assert!(codec.contains("public indirect enum CsilCborValue: Hashable, Sendable {"));
    assert!(codec.contains(".sorted { $0.0.lexicographicallyPrecedes($1.0) }"));
}

#[test]
fn constant_array_elements_ignore_the_closure_argument() {
    let codec = shapes_file("Codec.swift");
    assert!(
        codec.contains("CsilCborValue.array(self.nulls.map { _ in .null })"),
        "{codec}"
    );
}

#[test]
fn a_spec_type_named_error_does_not_shadow_swift_error() {
    let files = build_files(&shapes_input()).unwrap();
    for file in files.iter().filter(|file| file.path.ends_with(".swift")) {
        assert!(
            !file.content.contains(": Error {") && !file.content.contains(": Error,"),
            "{} conforms to the spec's Error type:\n{}",
            file.path,
            file.content
        );
    }
    assert!(shapes_file("Codec.swift").contains("public struct ErrorRefusal: Swift.Error {"));
}

#[test]
fn wire_ids_are_declared_once_when_client_and_services_ship_together() {
    let files = build_files(&shapes_input()).unwrap();
    let count: usize = files
        .iter()
        .map(|file| file.content.matches("public enum ShaperWireID {").count())
        .sum();
    assert_eq!(count, 1);
    // The client target alone still declares them.
    let mut client = shapes_input();
    client.config.target = "swift-client".into();
    client.config.options.clear();
    let files = build_files(&client).unwrap();
    let client_file = files
        .iter()
        .find(|file| file.path == "Client.swift")
        .unwrap();
    assert!(client_file.content.contains("public enum ShaperWireID {"));
}

/// Compiles the shapes package with Swift 6 language mode and warnings as errors,
/// then runs a driver that checks the wire bytes.
#[test]
fn regression_shapes_compile_warning_free_and_round_trip() {
    let Some(swiftc) = swift_compiler() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut sources = Vec::new();
    for file in build_files(&shapes_input()).unwrap() {
        if file.path.ends_with(".swift") && file.path.starts_with("Sources/") {
            let name = file.path.rsplit('/').next().unwrap().to_string();
            std::fs::write(dir.path().join(&name), file.content).unwrap();
            sources.push(name);
        }
    }
    let typecheck = std::process::Command::new(&swiftc)
        .current_dir(dir.path())
        .args(["-typecheck", "-swift-version", "6", "-warnings-as-errors"])
        .args(&sources)
        .output()
        .unwrap();
    assert!(
        typecheck.status.success(),
        "Swift 6 typecheck failed:\n{}",
        String::from_utf8_lossy(&typecheck.stderr)
    );
    std::fs::write(dir.path().join("main.swift"), SHAPES_DRIVER).unwrap();
    sources.push("main.swift".into());
    let output = std::process::Command::new(&swiftc)
        .current_dir(dir.path())
        .args(["-swift-version", "5"])
        .args(&sources)
        .args(["-o", "shapes"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Swift compile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(dir.path().join("shapes"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "Swift runtime check failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

const SHAPES_DRIVER: &str = r#"
// `precondition` takes an autoclosure, which cannot hold `try`.
func expect(_ condition: Bool, _ message: String = "check failed") {
    if !condition { fatalError(message) }
}

func shapes(pair: Int64 = 5) -> Shapes {
    Shapes(
        n: -7, amount: "123.45", single: 1.5,
        pair: (field0: "p", field1: pair), triple: (field0: "t", field1: nil, field2: true),
        outcome: .success((tag: "ok", data: .text("v"))),
        byId: [.array([.uint(0), .uint(7)]): 1],
        extra: ["k": .text("v"), "a": .map([(.text("z"), .uint(1)), (.text("b"), .null)])],
        status: .text("free"), nulls: [.null, .null], maybePair: (field0: "m", field1: 1))
}

let value = shapes()
let back = try Shapes.fromCbor(try value.toCbor())
expect(back == value, "round trip changed the value")
expect(shapes(pair: 6) != value, "tuple equality ignored a position")
let tree = try value.toCborValue()
expect(CsilCbor.mapGet(tree, "triple") == .array([.text("t"), .null, .bool(true)]))
expect(CsilCbor.encode(CsilCbor.mapGet(tree, "single")!) == [0xfb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0])
expect(try CsilCbor.asDouble(CsilCbor.decode([0xfa, 0x3f, 0xc0, 0x00, 0x00])) == 1.5)

// nint: negative only, both ways.
expect(CsilCbor.encode(CsilCbor.mapGet(tree, "n")!) == [0x26])
for bad: Int64 in [0, 1] {
    do {
        _ = try CsilCbor.encodeNint(bad)
        fatalError("nint encode accepted \(bad)")
    } catch CsilCborError.typeMismatch {}
}
do {
    _ = try CsilCbor.asNint(.uint(1))
    fatalError("nint decode accepted 1")
} catch CsilCborError.typeMismatch {}

// decimal: tag 4 [exponent, mantissa], exact both ways.
expect(CsilCbor.encode(try CsilCbor.encodeDecimal("123.45")) == [0xc4, 0x82, 0x21, 0x19, 0x30, 0x39])
expect(CsilCbor.encode(try CsilCbor.encodeDecimal("-0.5")) == [0xc4, 0x82, 0x20, 0x24])
expect(CsilCbor.encode(try CsilCbor.encodeDecimal("-9223372036854775808"))
    == [0xc4, 0x82, 0x00, 0x3b, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff])
let decimals: [(CsilCborValue, String)] = [
    (.tag(4, .array([.uint(3), .uint(7)])), "7000"),
    (.tag(4, .array([.int(-3), .uint(5)])), "0.005"),
    (.tag(4, .array([.int(-1), .uint(0)])), "0.0"),
    (.tag(4, .array([.int(-2), .int(-12345)])), "-123.45"),
]
for (wire, text) in decimals {
    let got = try CsilCbor.decodeDecimal(wire)
    expect(got == text, "decoded \(got), want \(text)")
}
for text in ["", "-", "1.", ".5", "1.2.3", "1e5", "abc", "9223372036854775808"] {
    do {
        _ = try CsilCbor.encodeDecimal(text)
        fatalError("decimal encode accepted \(text)")
    } catch CsilCborError.typeMismatch {}
}
for wire: CsilCborValue in [.tag(4, .array([.uint(5000), .uint(1)])), .tag(5, .array([.uint(0), .uint(1)])),
                            .tag(4, .array([.uint(0)])), .text("1.5")] {
    do {
        _ = try CsilCbor.decodeDecimal(wire)
        fatalError("decimal decode accepted \(wire)")
    } catch CsilCborError.typeMismatch {}
}

// Tuples keep their declared length.
do {
    _ = try CsilCbor.item(.array([.text("t")]), 0, of: 3)
    fatalError("a short tuple was read")
} catch CsilCborError.malformed {}

// A literal arm wins over the general arm by value; a null arm has no payload.
expect(try MixedStatus.text("pending").toCborValue() == .array([.uint(1), .text("pending")]))
expect(try MixedStatus.text("free").toCborValue() == .array([.uint(0), .text("free")]))
expect(CsilCbor.encode(try Outcome.null.toCborValue()) == [0x82, 0x01, 0xf6])
expect(try Outcome(cborValue: .array([.uint(1), .null])) == .null)
expect(Outcome.success((tag: "ok", data: .uint(1))) != .success((tag: "ok", data: .uint(2))))

// Maps encode in canonical key order whatever the entry order, and compare that way.
expect(CsilCbor.encode(.map([(.text("b"), .uint(1)), (.text("a"), .uint(2))]))
    == [0xa2, 0x61, 0x61, 0x02, 0x61, 0x62, 0x01])
expect(CsilCborValue.map([(.text("a"), .uint(1)), (.text("b"), .uint(2))])
    == .map([(.text("b"), .uint(2)), (.text("a"), .uint(1))]))

// A spec type named `Error` rides as the declared error arm.
let refused = try encodeShaperReshapeReply { throw ErrorRefusal(value: Error(code: 7, message: "no")) }
expect(refused.variant == "Error")
expect(ShaperWireID.service == 3 && ShaperWireID.reshape == 0)
"#;

#[test]
fn reserved_rule_name_is_renamed_but_keeps_its_wire_variant() {
    let ops = vec![make_op(
        "op",
        reference("Req"),
        CsilTypeExpression::Choice(vec![reference("Req"), reference("CsilTransport")]),
        CsilServiceDirection::Unidirectional,
    )];
    let input = input_from_rules(
        "swift-client",
        vec![
            group_rule("Req", vec![bare_entry("id", builtin("int"))]),
            group_rule("CsilTransport", vec![bare_entry("code", builtin("uint"))]),
            service_rule("Svc", ops, None),
        ],
        1,
    );
    let files = build_files(&input).expect("ok");
    let file = |path: &str| {
        files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.content.clone())
            .expect(path)
    };
    let types = file("Types.swift");
    assert!(types.contains("public struct CsilTransportType: Equatable, Sendable {"));
    assert!(!types.contains("public struct CsilTransport:"));
    let codec = file("Codec.swift");
    assert!(codec.contains("public struct CsilTransportTypeRefusal: Swift.Error {"));
    assert!(codec.contains("} catch let refusal as CsilTransportTypeRefusal {\n    return (\"CsilTransport\", CsilCbor.encode("));
    assert!(codec.contains("    case \"CsilTransport\":\n      throw CsilTransportTypeRefusal(\n"));
}

/// `Svc.get: Holder -> Holder / <error_arm>` over `Holder = { fN: <rule> }`, one
/// record per name in `records`.
fn clash_input(records: &[&str], error_arm: &str) -> WasmGeneratorInput {
    let ops = vec![make_op(
        "get",
        reference("Holder"),
        CsilTypeExpression::Choice(vec![reference("Holder"), reference(error_arm)]),
        CsilServiceDirection::Unidirectional,
    )];
    let fields = records
        .iter()
        .enumerate()
        .map(|(i, name)| bare_entry(&format!("f{i}"), reference(name)))
        .collect();
    let mut rules = vec![group_rule("Holder", fields)];
    for name in records {
        rules.push(group_rule(name, vec![bare_entry("code", builtin("uint"))]));
    }
    rules.push(service_rule("Svc", ops, None));
    input_from_rules("swift-client", rules, 1)
}

fn file_text(files: &[GeneratedFile], path: &str) -> String {
    files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.content.clone())
        .expect(path)
}

#[test]
fn rule_named_like_a_derived_type_is_renamed() {
    let files = build_files(&clash_input(&["Foo", "FooRefusal", "SvcClient"], "Foo")).expect("ok");
    let types = file_text(&files, "Types.swift");
    assert!(types.contains("public struct FooRefusalType: Equatable, Sendable {"));
    assert!(types.contains("public struct SvcClientType: Equatable, Sendable {"));
    let codec = file_text(&files, "Codec.swift");
    assert!(codec.contains("public struct FooRefusal: Swift.Error {"));
    assert!(codec.contains("    case \"Foo\": throw FooRefusal(value: "));
    assert!(file_text(&files, "Client.swift").contains("public struct SvcClient {"));
}

#[test]
fn rules_with_the_same_swift_name_are_told_apart() {
    let input = clash_input(&["Value", "value", "user_id", "UserId"], "value");
    let files = build_files(&input).expect("ok");
    let types = file_text(&files, "Types.swift");
    for ty in ["Value", "ValueType", "UserId", "UserIdType"] {
        assert!(
            types.contains(&format!("public struct {ty}: Equatable, Sendable {{")),
            "{ty}"
        );
    }
    let codec = file_text(&files, "Codec.swift");
    assert!(codec.contains(
        "} catch let refusal as ValueTypeRefusal {\n    return (\"value\", CsilCbor.encode("
    ));
    assert!(codec.contains("    case \"value\": throw ValueTypeRefusal(value: "));
}

#[test]
fn renamed_rules_are_reported_as_warnings() {
    let (_, warnings) =
        build_files_with_warnings(&clash_input(&["Value", "value", "Int"], "value")).expect("ok");
    let messages: Vec<&str> = warnings.iter().map(|w| w.message.as_str()).collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`value` is generated as `valueType` for swift-client"))
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`Int` is generated as `IntType` for swift-client"))
    );
}

/// Type-check the emitted Swift sources, or skip when no Swift compiler is present
/// (see `swift_compiler`). Only `-typecheck`, so a containerized `SWIFTC` works too.
fn typecheck(files: &[GeneratedFile]) {
    let Some(swiftc) = swift_compiler() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut sources = Vec::new();
    // The manifest is read by SwiftPM, not compiled with the module.
    for file in files
        .iter()
        .filter(|f| f.path.ends_with(".swift") && f.path != "Package.swift")
    {
        let name = file.path.rsplit('/').next().unwrap().to_string();
        std::fs::write(dir.path().join(&name), &file.content).unwrap();
        sources.push(name);
    }
    let output = std::process::Command::new(&swiftc)
        .current_dir(dir.path())
        .arg("-typecheck")
        .args(&sources)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Swift type-check failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The first schema of the linkkeys request: the op boundary is the record itself,
/// so nothing is derived from the op that could clash with the record.
#[test]
fn op_boundary_record_keeps_its_name() {
    let rules = vec![
        group_rule(
            "RpIssueAttestationRequest",
            vec![bare_entry("claim_type", builtin("text"))],
        ),
        group_rule(
            "RpIssueAttestationResponse",
            vec![bare_entry("deposited", builtin("bool"))],
        ),
        service_rule(
            "Rp",
            vec![make_op(
                "issue-attestation",
                reference("RpIssueAttestationRequest"),
                reference("RpIssueAttestationResponse"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ];
    for target in ["swift", "swift-client"] {
        let input = input_from_rules(target, rules.clone(), 1);
        let (files, warnings) = build_files_with_warnings(&input).expect("ok");
        assert!(warnings.is_empty(), "{target}: {warnings:?}");
        let types = file_text(&files, "Types.swift");
        assert_eq!(
            count(&types, "public struct RpIssueAttestationRequest:"),
            1,
            "{target}"
        );
        assert!(!types.contains("RpIssueAttestationRequestType"));
        typecheck(&files);
    }
}

/// The second schema of the request. Swift emits `encodeMemberGetMemberRequest` for
/// the `MemberID` boundary, but that is a free function with a lower-case name, so it
/// does not clash with the type `MemberGetMemberRequest`, and no rename is needed.
#[test]
fn per_op_codec_function_does_not_clash_with_a_type() {
    let rules = vec![
        typedef_rule("MemberID", builtin("text")),
        group_rule(
            "MemberGetMemberRequest",
            vec![bare_entry("id", builtin("text"))],
        ),
        service_rule(
            "Member",
            vec![make_op(
                "get-member",
                reference("MemberID"),
                reference("MemberID"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ];
    let input = input_from_rules("swift-client", rules, 1);
    let (files, warnings) = build_files_with_warnings(&input).expect("ok");
    assert!(warnings.is_empty(), "{warnings:?}");
    let codec = file_text(&files, "Codec.swift");
    assert_eq!(
        count(&codec, "public func encodeMemberGetMemberRequest("),
        1
    );
    assert_eq!(count(&codec, "\nextension MemberGetMemberRequest {"), 1);
    typecheck(&files);
}

/// `<Service>Client` and `<Service>AsyncClient` are reserved only when the run emits
/// that client struct.
#[test]
fn client_names_are_reserved_only_when_the_client_is_emitted() {
    let names = |target: &str, style: Option<&str>| {
        let mut input = clash_input(&["SvcClient", "SvcAsyncClient"], "Holder");
        input.config.target = target.to_string();
        if let Some(style) = style {
            input
                .config
                .options
                .insert("client_style".into(), serde_json::json!(style));
        }
        let (files, warnings) = build_files_with_warnings(&input).expect("ok");
        typecheck(&files);
        let types = file_text(&files, "Types.swift");
        let renamed: Vec<&str> = ["SvcClientType", "SvcAsyncClientType"]
            .into_iter()
            .filter(|ty| types.contains(&format!("public struct {ty}:")))
            .collect();
        assert_eq!(warnings.len(), renamed.len(), "{target}: {warnings:?}");
        renamed
    };
    assert!(names("swift", None).is_empty());
    assert_eq!(
        names("swift-client", None),
        ["SvcClientType", "SvcAsyncClientType"]
    );
    assert_eq!(names("swift-client", Some("sync")), ["SvcClientType"]);
    assert_eq!(names("swift-client", Some("async")), ["SvcClientType"]);
}

/// Two services whose Swift names are equal: the later one is renamed, and the wire
/// keeps each declared name.
#[test]
fn services_with_the_same_swift_name_keep_their_wire_names() {
    // `Foo` and `FooService` share the client name `FooClient`, so the later one is
    // renamed too.
    let rules = vec![
        group_rule("Holder", vec![bare_entry("id", builtin("int"))]),
        service_rule(
            "Foo",
            vec![make_op(
                "get",
                reference("Holder"),
                reference("Holder"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
        service_rule(
            "FooService",
            vec![make_op(
                "put",
                reference("Holder"),
                reference("Holder"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ];
    let files = build_files(&input_from_rules("swift-client", rules, 2)).expect("ok");
    let client = file_text(&files, "Client.swift");
    assert!(client.contains("public struct FooClient {"));
    assert!(client.contains("public struct FooService2Client {"));
    assert!(client.contains("service: \"FooService\", op: \"put\""));
    typecheck(&files);

    let op = |name: &str| {
        make_op(
            name,
            reference("Holder"),
            reference("Holder"),
            CsilServiceDirection::Unidirectional,
        )
    };
    let rules = vec![
        group_rule("Holder", vec![bare_entry("id", builtin("int"))]),
        service_rule("UserSvc", vec![op("get")], None),
        service_rule("user-svc", vec![op("put")], None),
    ];
    let mut input = input_from_rules("swift", rules, 2);
    input
        .config
        .options
        .insert("emit_packages".into(), serde_json::json!(["swift"]));
    let (files, warnings) = build_files_with_warnings(&input).expect("ok");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .message
            .contains("service `user-svc` is generated as `user-svc2`")
    );
    typecheck(&files);
    let client = files
        .iter()
        .find(|f| f.path.ends_with("/Client.swift"))
        .expect("client")
        .content
        .clone();
    assert!(client.contains("public struct UserSvcClient {"));
    assert!(client.contains("public struct UserSvc2Client {"));
    assert!(client.contains("service: \"UserSvc\", op: \"get\""));
    assert!(client.contains("service: \"user-svc\", op: \"put\""));
    let services = files
        .iter()
        .find(|f| f.path.ends_with("/Services.swift"))
        .expect("services")
        .content
        .clone();
    assert!(services.contains("public protocol UserSvc {"));
    assert!(services.contains("public protocol UserSvc2 {"));
}

/// A name the inline-composite hoister invents is checked like a user rule: the
/// field `cbor` of `Csil` becomes `Csil_cbor`, whose Swift name `CsilCbor` is the
/// runtime's own type.
#[test]
fn hoisted_names_are_checked_for_clashes() {
    let inline = CsilTypeExpression::Group(CsilGroupExpression {
        entries: vec![bare_entry("a", builtin("int"))],
    });
    let rules = vec![group_rule("Csil", vec![bare_entry("cbor", inline)])];
    let input = input_from_rules("swift", rules, 0);
    let (files, warnings) = build_files_with_warnings(&input).expect("ok");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].message.contains("`Csil_cbor`"));
    let types = file_text(&files, "Types.swift");
    assert!(types.contains("public let cbor: CsilCborType\n"));
    assert!(types.contains("public struct CsilCborType: Equatable, Sendable {"));
    typecheck(&files);
}

/// A mixed op output is one named union with every arm, in the client and the server.
#[test]
fn mixed_op_output_is_a_named_union_with_every_arm() {
    let output = CsilTypeExpression::Choice(vec![
        CsilTypeExpression::Map {
            key: Box::new(builtin("text")),
            value: Box::new(builtin("int")),
            occurrence: None,
        },
        reference("Err"),
    ]);
    let rules = vec![
        group_rule("Holder", vec![bare_entry("id", builtin("int"))]),
        group_rule("Err", vec![bare_entry("message", builtin("text"))]),
        service_rule(
            "Lookup",
            vec![make_op(
                "read-counts",
                reference("Holder"),
                output,
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ];
    let mut input = input_from_rules("swift", rules, 1);
    input
        .config
        .options
        .insert("emit_packages".into(), serde_json::json!(["swift"]));
    let files = build_files(&input).expect("ok");
    let find = |suffix: &str| {
        files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .expect(suffix)
            .content
            .clone()
    };
    let types = find("/Types.swift");
    assert!(types.contains("public enum LookupReadCountsOutput: Equatable, Sendable {"));
    assert!(types.contains("case err(Err)"));
    assert!(
        find("/Client.swift")
            .contains("func readCounts(_ request: Holder) throws -> LookupReadCountsOutput {")
    );
    assert!(
        find("/Services.swift")
            .contains("func readCounts(_ request: Holder) throws -> LookupReadCountsOutput\n")
    );
    typecheck(&files);
}

/// The runtime rejects a dynamic map whose keys are out of canonical order and any
/// map with a key twice.
#[test]
fn runtime_checks_map_key_order_and_duplicates() {
    assert!(CODEC_RUNTIME_SWIFT.contains("public static func asCanonicalMap("));
    assert!(CODEC_RUNTIME_SWIFT.contains("!previous.lexicographicallyPrecedes(key)"));
    assert!(CODEC_RUNTIME_SWIFT.contains("guard seen.insert(raw).inserted else {"));
    assert!(CODEC_RUNTIME_SWIFT.contains("csil cbor: map keys are not in canonical order"));
    assert!(CODEC_RUNTIME_SWIFT.contains("csil cbor: duplicate key"));
    assert!(
        CODEC_RUNTIME_SWIFT
            .contains("guard shortestHead(raw) else { throw CsilCborError.longFormKey }")
    );
    assert!(CODEC_RUNTIME_SWIFT.contains("csil cbor: map key is not in shortest form"));
}

/// Every emitted Swift line fits the swift-format line length, except a line that
/// holds one token the formatter cannot break either (a long name or string).
/// `tests/interop/formatting` runs the real formatter on the same shapes.
#[test]
fn emitted_lines_fit_the_formatter_line_length() {
    let long_enum = "NotificationChannelKindWithAnUnusuallyLongDescriptiveTypeNameForTesting";
    let long_record = "ExtraordinarilyLongRecordNameForFormatterChecksAcrossEveryTargetLanguage";
    let service = "ServiceWithAnExceedinglyLongNameUsedOnlyForFormatterChecks";
    let rules = vec![
        typedef_rule(
            long_enum,
            CsilTypeExpression::Choice(vec![
                text_literal("electronic-mail-address-delivery"),
                text_literal("push-notification-delivery-to-registered-device"),
                text_literal("web-hook-callback-to-registered-endpoint"),
            ]),
        ),
        typedef_rule(
            "ClaimValueType",
            CsilTypeExpression::Choice(
                [
                    "text", "url", "email", "bool", "int", "float", "decimal", "date",
                ]
                .into_iter()
                .map(text_literal)
                .collect(),
            ),
        ),
        group_rule(
            long_record,
            vec![
                bare_entry(
                    "channel_kind_for_notifications_sent_to_the_account_holder",
                    reference(long_enum),
                ),
                opt_entry(
                    "optional_list_of_extraordinarily_long_record_references",
                    CsilTypeExpression::Array {
                        element_type: Box::new(reference("ShortRecord")),
                        occurrence: None,
                    },
                ),
                bare_entry(
                    "map_from_identifier_to_holder_with_a_very_long_field_name",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(reference("ShortRecord")),
                        occurrence: None,
                    },
                ),
                bare_entry(
                    "timestamp_of_the_last_modification_made_by_any_user",
                    builtin("timestamp"),
                ),
            ],
        ),
        group_rule("ShortRecord", vec![bare_entry("a", builtin("int"))]),
        group_rule(
            "ErrorForLongOperations",
            vec![bare_entry("message", builtin("text"))],
        ),
        service_rule(
            service,
            vec![
                make_op(
                    "operation-with-an-exceedingly-long-name-that-returns-a-choice",
                    reference(long_record),
                    CsilTypeExpression::Choice(vec![
                        reference(long_record),
                        reference("ErrorForLongOperations"),
                    ]),
                    CsilServiceDirection::Unidirectional,
                ),
                make_op(
                    "read-mixed-output-with-an-inline-map-arm",
                    reference("ShortRecord"),
                    CsilTypeExpression::Choice(vec![
                        CsilTypeExpression::Map {
                            key: Box::new(builtin("text")),
                            value: Box::new(builtin("int")),
                            occurrence: None,
                        },
                        reference("ErrorForLongOperations"),
                    ]),
                    CsilServiceDirection::Unidirectional,
                ),
            ],
            None,
        ),
    ];
    let mut input = input_from_rules("swift", rules, 1);
    input
        .config
        .options
        .insert("emit_packages".into(), serde_json::json!(["swift"]));
    let files = build_files(&input).expect("ok");
    let mut checked = 0;
    for file in files.iter().filter(|f| f.path.ends_with(".swift")) {
        for line in file.content.lines() {
            checked += 1;
            if line.chars().count() <= swiftfmt::LINE_LENGTH {
                continue;
            }
            let code = line.trim();
            let atom = code.trim_start_matches("-> ");
            let one_token = !atom.contains(' ') || (atom.starts_with('"') && atom.ends_with('"'));
            assert!(
                code.starts_with("//") || one_token,
                "{}: line over {} columns: {line}",
                file.path,
                swiftfmt::LINE_LENGTH
            );
        }
    }
    assert!(checked > 500, "{checked}");
    typecheck(&files);
}

/// Text enum values that are not Swift identifiers get legal case names, keep their
/// wire value, and round-trip through the codec.
#[test]
fn text_enum_values_get_legal_case_names_and_round_trip() {
    let values = ["calendars.read", "events.write", "a-b c", "2fa", "class"];
    let rules = vec![
        typedef_rule(
            "Scope",
            CsilTypeExpression::Choice(values.iter().map(|v| text_literal(v)).collect()),
        ),
        group_rule("Grant", vec![bare_entry("scope", reference("Scope"))]),
    ];
    let files = build_files(&input_from_rules("swift", rules, 0)).expect("ok");
    let types = file_text(&files, "Types.swift");
    for case in [
        "case calendarsRead = \"calendars.read\"",
        "case eventsWrite = \"events.write\"",
        "case aBC = \"a-b c\"",
        "case v2Fa = \"2fa\"",
        "case `class` = \"class\"",
    ] {
        assert!(types.contains(case), "{case}\n{types}");
    }
    assert_eq!(
        text_case_names(&["pending", "allow-once", "rawValue", "a.b", "a-b"]),
        ["pending", "allowOnce", "vRawValue", "aB", "aB2"]
    );
    let Some(swiftc) = swift_compiler() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut sources = Vec::new();
    for file in files.iter().filter(|f| f.path.ends_with(".swift")) {
        std::fs::write(dir.path().join(&file.path), &file.content).unwrap();
        sources.push(file.path.clone());
    }
    std::fs::write(
        dir.path().join("main.swift"),
        r#"for scope in Scope.allCases {
    let bytes = try Grant(scope: scope).toCbor()
    let wire = Array(scope.rawValue.utf8)
    precondition(bytes.suffix(wire.count) == wire[...], "wire value of \(scope)")
    let back = try Grant.fromCbor(bytes).scope
    precondition(back == scope, "round trip of \(scope)")
}
precondition(Scope.allCases.map(\.rawValue) == ["calendars.read", "events.write", "a-b c", "2fa", "class"])
"#,
    )
    .unwrap();
    sources.push("main.swift".into());
    let output = std::process::Command::new(&swiftc)
        .current_dir(dir.path())
        .args(&sources)
        .args(["-o", "enums"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Swift compile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run = std::process::Command::new(dir.path().join("enums"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
