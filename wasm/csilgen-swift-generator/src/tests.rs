//! Unit tests for the Swift generator's emitter functions.

use super::*;
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
    assert!(task_body.contains(
        "let prio = try Priority(cborValue: (try CsilCbor.require(cborValue, \"prio\")))"
    ));
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
        "let value = try MixedLit(cborValue: (try CsilCbor.require(cborValue, \"value\")))"
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
    assert!(
        body.contains(
            "case .text(let csilV):\n            return .array([.uint(0), .text(csilV)])"
        )
    );
    assert!(body.contains(
        "case .case1(let csilV):\n            return .array([.uint(1), .text(\"pending\")])"
    ));
    assert!(body.contains(
        "case .case2(let csilV):\n            return .array([.uint(2), .text(\"confirmed\")])"
    ));
    assert!(body.contains(
        "case .case3(let csilV):\n            return .array([.uint(3), .text(\"processing\")])"
    ));
    assert!(body.contains(
        "case .case4(let csilV):\n            return .array([.uint(4), .text(\"shipped\")])"
    ));
    assert!(body.contains(
        "case .case5(let csilV):\n            return .array([.uint(5), .text(\"delivered\")])"
    ));
    assert!(body.contains(
        "case .case6(let csilV):\n            return .array([.uint(6), .text(\"cancelled\")])"
    ));
    assert!(body.contains(
        "case .case7(let csilV):\n            return .array([.uint(7), .text(\"refunded\")])"
    ));

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
    assert!(body.contains("func toCbor() -> [UInt8] { CsilCbor.encode(toCborValue()) }"));
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
    assert!(!body.contains("CsilCbor.asText((try CsilCbor.require(cborValue, \"status\"))"));
    // Encode/decode delegate to OrderStatus's own codec.
    assert!(body.contains("csilEntries.append((\"status\", self.status.toCborValue()))"));
    assert!(body.contains(
        "let status = try OrderStatus(cborValue: (try CsilCbor.require(cborValue, \"status\")))"
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
    assert!(union_body.contains(
        "case .case1(let csilV):\n            return .array([.uint(1), .text(\"low\")])"
    ));
    assert!(union_body.contains(
        "case .case2(let csilV):\n            return .array([.uint(2), .text(\"high\")])"
    ));
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
    assert!(enum_body.contains("func toCborValue() -> CsilCborValue { .text(self.rawValue) }"));
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
    assert!(
        body.contains(
            "case .text(let csilV):\n            return .array([.uint(0), .text(csilV)])"
        )
    );
    assert!(body.contains(
        "case .case1(let csilV):\n            return .array([.uint(1), .text(\"not_found\")])"
    ));
    assert!(body.contains(
        "case .case2(let csilV):\n            return .array([.uint(2), .text(\"permission_denied\")])"
    ));
    assert!(body.contains(
        "case .case3(let csilV):\n            return .array([.uint(3), .text(\"invalid_input\")])"
    ));
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
    assert!(struct_body.contains("self.tagList.map { $0.toCborValue() }"));
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
    assert!(struct_body.contains("$0.value.toCborValue()"));
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
        "CsilCborValue.array([self.coord.latOrLon.toCborValue(), .int(self.coord.value)])"
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
    assert!(struct_body.contains("CsilCborValue.array([self.solo.toCborValue()])"));
    assert!(struct_body.contains("try TortureSoloOnly(cborValue: (try CsilCbor.asArray("));
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
    assert!(!widget_body.contains("CsilCbor.asText((try CsilCbor.require(cborValue, \"color\"))"));
    assert!(widget_body.contains("self.color.toCborValue()"));
    assert!(widget_body.contains(
        "let color = try Color(cborValue: (try CsilCbor.require(cborValue, \"color\")))"
    ));
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
fn any_value_typealias_emitted_without_any_validation() {
    // A field of core type `any` with no constraints still needs AnyCsilValue defined.
    let rule = group_rule("Envelope", vec![bare_entry("body", builtin("any"))]);
    let input = input_from_rules("swift", vec![rule], 0);
    let types = generate_types(&input).expect("types emitted");
    assert!(types.contains("public typealias AnyCsilValue = [UInt8]"));
    assert!(types.contains("public let body: AnyCsilValue"));
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
    assert!(client.content.contains("request: request.toCbor()"));
    assert!(
        client
            .content
            .contains("DepositClaimResponse.fromCbor(csilResp)")
    );
    // Canonical wire strings: verbatim CSIL service and op names
    // (csil-rpc-transport.md §1.1) — so a Swift client reaches the same endpoint
    // as its peers.
    assert!(
        client
            .content
            .contains("service: \"Attestation\", op: \"deposit-claim\"")
    );
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
    assert!(codec.content.contains("public extension Task {"));
    assert!(codec.content.contains("func toCbor() -> [UInt8]"));
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
    assert!(client.content.contains("request: request.toCbor()"));
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
    assert!(client.content.contains("request: request.toCbor()"));
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
    assert!(
        codec
            .content
            .contains("public func encodeMemberListMembersResponse(_ value: [Member]) -> [UInt8]")
    );
    assert!(
        codec
            .content
            .contains("public func encodeMemberDeleteTaskResponse(_ value: Bool) -> [UInt8]")
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
    assert!(twin.content.contains("try await transport.call("));
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
    assert!(client.content.contains("try await transport.call("));
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
    // Encode must build a CBOR map; decode must read one back via asMap.
    assert!(codec.contains("CsilCborValue.map(self.counts.map {"));
    assert!(codec.contains("try CsilCbor.asMap"));
    assert!(codec.contains("reduce(into: [String: Int64]()"));
    // The stub must be gone for this field.
    let body = codec.split("extension Holder").nth(1).unwrap();
    assert!(!body.contains("\"counts\", .null"));
    assert!(!body.contains("CsilCbor.asText((try CsilCbor.require(cborValue, \"counts\"))"));
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
    assert!(body.contains("CsilCborValue.map(self.items.map {"));
    assert!(body.contains("$0.value.toCborValue()"));
    // Decode reads a map and reconstructs each record value.
    assert!(body.contains("try CsilCbor.asMap"));
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
    assert!(
        client
            .content
            .contains("service: \"CorndogsService\", op: \"submit-task\"")
    );
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
    assert!(c.contains("resp.variant == \"ServiceError\""));
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
    // A frame carrier over a TLS byte stream via the lib's StreamCarrier framing.
    assert!(c.contains("StreamCarrier(stream: TlsByteStream(host: \"localhost\", port: 7443))"));
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
    assert!(c.contains("encodeEchoChat(codec: codec, msg: ChatEvent(body: \"example\"))"));
    assert!(
        c.contains("try routeEchoChannel(handler, codec: codec, op: ev.event!, data: ev.payload)")
    );
}

#[test]
fn readme_datagrams_section_send_and_late_arrival_note() {
    let c = render_transports_readme(vec![]);
    assert!(c.contains("## CSIL-Datagrams (UDP)"));
    assert!(c.contains("final class UdpDatagramCarrier: DatagramCarrier"));
    // Encode a `->` request via the generated codec, wrap in the lib Datagram, fire-and-forget.
    assert!(c.contains("let req: Ping = Ping(msg: \"example\")"));
    assert!(c.contains("Datagram(opOrd: opOrd, seq: 0, payload: req.toCbor()).encode()"));
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
            "try routeEchoChannel(handler, codec: codec, op: ev.event!, data: ev.payload)"
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
        "public enum UserData2: Equatable, Sendable, CaseIterable {\n    case x\n    case v1\n"
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
