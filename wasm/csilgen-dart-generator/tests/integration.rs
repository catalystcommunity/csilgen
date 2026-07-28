//! Integration tests for the Dart code generator. Each test feeds a serialized
//! CSIL spec to `generate_dart_code` and asserts on the emitted Dart shapes —
//! `final class` records, `sealed class` choices, clients, routers, wire-ids.

use csilgen_common::*;
use csilgen_dart_generator::generate_dart_code;
use std::collections::HashMap;

fn config(target: &str) -> GeneratorConfig {
    GeneratorConfig {
        target: target.to_string(),
        output_dir: "/tmp".to_string(),
        options: HashMap::new(),
    }
}

fn config_with(target: &str, opts: &[(&str, serde_json::Value)]) -> GeneratorConfig {
    let mut options = HashMap::new();
    for (k, v) in opts {
        options.insert((*k).to_string(), v.clone());
    }
    GeneratorConfig {
        target: target.to_string(),
        output_dir: "/tmp".to_string(),
        options,
    }
}

fn pos() -> CsilPosition {
    CsilPosition {
        line: 1,
        column: 1,
        offset: 0,
    }
}

fn builtin(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Builtin(name.to_string())
}

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

fn entry(key: &str, value_type: CsilTypeExpression, optional: bool) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(key.to_string())),
        value_type,
        occurrence: optional.then_some(CsilOccurrence::Optional),
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

fn record_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

/// A transparent type alias (`Name = <target>`), the `TypeDef` shape a named map /
/// list / scalar alias parses to.
fn type_rule(name: &str, target: CsilTypeExpression) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::TypeDef(target),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn spec(rules: Vec<CsilRule>, service_count: usize) -> CsilSpecSerialized {
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count,
        fields_with_metadata_count: 0,
    }
}

fn types_file(files: &[GeneratedFile]) -> &str {
    &files
        .iter()
        .find(|f| f.path == "types.gen.dart")
        .expect("types.gen.dart should be generated")
        .content
}

fn services_file(files: &[GeneratedFile]) -> &str {
    &files
        .iter()
        .find(|f| f.path == "services.gen.dart")
        .expect("services.gen.dart should be generated")
        .content
}

/// An optional `bytes` field carries three distinct states — absent, present-and-empty,
/// present-and-non-empty — and the codec must decide presence by whether the value is
/// set, never by whether it is non-empty (cbor-wire-contract.md "Optional fields"). An
/// `isNotEmpty` guard would collapse present-empty into absent and silently lose a
/// caller's "replace this with nothing".
#[test]
fn optional_bytes_encodes_on_presence_not_emptiness() {
    let rules = vec![record_rule(
        "UpdateRequest",
        vec![
            entry("id", builtin("text"), false),
            entry("payload", builtin("bytes"), true),
        ],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);

    // A nullable Uint8List distinguishes null (absent) from an empty list
    // (present-and-empty).
    assert!(
        code.contains("final Uint8List? payload;"),
        "optional bytes needs a presence-carrying type: {code}"
    );
    // Encode gates on `!= null` (presence), not on emptiness.
    assert!(
        code.contains("if (payload != null) map['payload'] = payload"),
        "encode must gate on presence, not emptiness: {code}"
    );
    assert!(
        !code.contains("payload!.isNotEmpty"),
        "encode must not gate on emptiness: {code}"
    );
    // Decode maps a missing key to null but keeps a present zero-length byte string,
    // so the three states stay distinct.
    assert!(
        code.contains("payload: map['payload'] == null ? null : map['payload'] as Uint8List"),
        "decode must gate on key presence: {code}"
    );
}

#[test]
fn record_emits_final_class_with_const_named_constructor() {
    let rules = vec![record_rule(
        "User",
        vec![
            entry("user_name", builtin("text"), false),
            entry("age", builtin("int"), true),
        ],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);

    assert!(
        code.contains("final class User {"),
        "missing final class: {code}"
    );
    // snake_case wire field -> lowerCamelCase Dart member.
    assert!(
        code.contains("final String userName;"),
        "field mapping wrong: {code}"
    );
    assert!(
        code.contains("final int? age;"),
        "optional should be nullable: {code}"
    );
    // A short parameter list collapses onto one line, as `dart format` leaves it.
    assert!(
        code.contains("const User({required this.userName, this.age});"),
        "missing collapsed const constructor: {code}"
    );
    // Wire key stays verbatim snake_case in (de)serialization.
    assert!(
        code.contains("map['user_name'] = userName;"),
        "verbatim wire key: {code}"
    );
    assert!(
        code.contains("if (age != null) map['age'] = age;"),
        "absent optional dropped from wire: {code}"
    );
    assert!(
        code.contains("factory User.fromMap("),
        "missing fromMap: {code}"
    );
    assert!(
        code.contains("bool operator =="),
        "missing value equality: {code}"
    );
}

#[test]
fn zero_field_record_emits_unnamed_const_constructor() {
    // Dart rejects an empty named-parameter list, so a fieldless record must use a
    // plain `const X();` constructor rather than `const X({ ... });`.
    let rules = vec![record_rule("GetQueuesRequest", vec![])];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);

    assert!(
        code.contains("const GetQueuesRequest();"),
        "fieldless record should use an unnamed const constructor: {code}"
    );
    assert!(
        !code.contains("const GetQueuesRequest({"),
        "fieldless record must not emit an empty named-parameter list: {code}"
    );
}

#[test]
fn type_choice_emits_sealed_hierarchy() {
    let rules = vec![CsilRule {
        name: "Result".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![reference("Ok"), reference("Failure")]),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);

    assert!(
        code.contains("sealed class Result {"),
        "missing sealed base: {code}"
    );
    assert!(
        code.contains("final class ResultOk extends Result {"),
        "missing Ok arm: {code}"
    );
    assert!(
        code.contains("final class ResultFailure extends Result {"),
        "missing Failure arm: {code}"
    );
    assert!(
        code.contains("final Ok value;"),
        "arm carries its type: {code}"
    );
}

#[test]
fn client_subtarget_emits_typed_client_with_canonical_wire_names() {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "deposit-claim".to_string(),
            input_type: reference("DepositClaimRequest"),
            output_type: reference("DepositClaimResponse"),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    let rules = vec![
        record_rule(
            "DepositClaimRequest",
            vec![entry("subject", builtin("text"), false)],
        ),
        record_rule(
            "DepositClaimResponse",
            vec![entry("ok", builtin("bool"), false)],
        ),
        CsilRule {
            name: "AttestationService".to_string(),
            rule_type: CsilRuleType::ServiceDef(service),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ];
    let files = generate_dart_code(&spec(rules, 1), &config("dart-client")).unwrap();
    let code = services_file(&files);

    assert!(
        code.contains("final class AttestationClient {"),
        "client class: {code}"
    );
    // kebab-case op -> lowerCamelCase Dart method.
    assert!(
        code.contains("DepositClaimResponse depositClaim(DepositClaimRequest request)"),
        "method signature: {code}"
    );
    // Typed seam + canonical wire strings: verbatim CSIL service and op names
    // (csil-rpc-transport.md §1.1), matching the Go/Python/TS peers. The call
    // exceeds the 80-column page width, so it renders in the formatter's split
    // shape (one argument per line, trailing comma).
    assert!(
        code.contains(
            "    final csilResp = transport.call(\n      'AttestationService',\n      'deposit-claim',\n      request.toCbor(),\n    );"
        ),
        "canonical wire strings in split call: {code}"
    );
    assert!(
        code.contains("DepositClaimResponse.fromCborValue(CsilCbor.decode(csilResp))"),
        "typed response decode: {code}"
    );
}

#[test]
fn server_subtarget_emits_handler_interface() {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "get-user".to_string(),
            input_type: builtin("text"),
            output_type: reference("User"),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    let rules = vec![CsilRule {
        name: "UserService".to_string(),
        rule_type: CsilRuleType::ServiceDef(service),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 1), &config("dart")).unwrap();
    let code = services_file(&files);

    assert!(
        code.contains("abstract interface class UserServiceHandler {"),
        "handler interface: {code}"
    );
    assert!(
        code.contains("User getUser(String request);"),
        "handler method: {code}"
    );
}

#[test]
fn channel_service_with_wire_ids_emits_both_routers_and_ordinals() {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "chat".to_string(),
            input_type: reference("ChatMessage"),
            output_type: builtin("null"),
            direction: CsilServiceDirection::Bidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: Some(2),
        }],
        wire_id: Some(1),
    };
    let rules = vec![CsilRule {
        name: "RoomService".to_string(),
        rule_type: CsilRuleType::ServiceDef(service),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 1), &config("dart")).unwrap();
    let code = services_file(&files);

    assert!(
        code.contains("void routeRoomServiceChannel("),
        "verbose router: {code}"
    );
    assert!(
        code.contains("void routeRoomServiceChannelCompact("),
        "compact router: {code}"
    );
    assert!(
        code.contains("case 'chat':"),
        "verbose dispatch by verbatim wire op: {code}"
    );
    assert!(
        code.contains("case 2:"),
        "compact dispatch by ordinal: {code}"
    );
    assert!(
        code.contains("const int roomServiceServiceWireId = 1;"),
        "service ordinal: {code}"
    );
    assert!(
        code.contains("const int roomServiceOpChatWireId = 2;"),
        "op ordinal: {code}"
    );
}

#[test]
fn no_wire_ids_means_no_compact_router() {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "chat".to_string(),
            input_type: reference("ChatMessage"),
            output_type: builtin("null"),
            direction: CsilServiceDirection::Bidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    let rules = vec![CsilRule {
        name: "RoomService".to_string(),
        rule_type: CsilRuleType::ServiceDef(service),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 1), &config("dart")).unwrap();
    let code = services_file(&files);
    assert!(
        code.contains("routeRoomServiceChannel("),
        "verbose router present: {code}"
    );
    assert!(
        !code.contains("Compact"),
        "no compact router without wire-ids: {code}"
    );
    assert!(!code.contains("WireId"), "no wire-id constants: {code}");
}

#[test]
fn validation_emitted_for_constrained_field() {
    let mut e = entry("name", builtin("text"), false);
    e.metadata = vec![CsilFieldMetadata::Constraint(
        CsilValidationConstraint::MinLength(3),
    )];
    let files = generate_dart_code(
        &spec(vec![record_rule("User", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("void validate() {"),
        "validate method: {code}"
    );
    assert!(code.contains("name.length < 3"), "min-length guard: {code}");
    assert!(
        code.contains("throw ArgumentError("),
        "guard throws: {code}"
    );
}

#[test]
fn reserved_word_field_is_escaped() {
    let files = generate_dart_code(
        &spec(
            vec![record_rule(
                "Thing",
                vec![entry("class", builtin("text"), false)],
            )],
            0,
        ),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    // The Dart member is escaped, but the wire key stays the reserved word verbatim.
    assert!(
        code.contains("final String class_;"),
        "reserved escaped: {code}"
    );
    assert!(
        code.contains("map['class'] = class_;"),
        "verbatim wire key: {code}"
    );
}

#[test]
fn decimal_field_emits_helper_under_csil_mapping() {
    let files = generate_dart_code(
        &spec(
            vec![record_rule(
                "Money",
                vec![entry("amount", builtin("decimal"), false)],
            )],
            0,
        ),
        &config("dart"),
    )
    .unwrap();
    assert!(
        files.iter().any(|f| f.path == "csil_decimal.gen.dart"),
        "decimal helper file emitted"
    );
    let code = types_file(&files);
    assert!(
        code.contains("final CsilDecimal amount;"),
        "decimal type mapping: {code}"
    );
}

#[test]
fn unknown_subtarget_is_an_error() {
    let err = generate_dart_code(&spec(vec![], 0), &config("dart-bogus"));
    assert!(err.is_err(), "unknown sub-target should error");
}

#[test]
fn barrel_reexports_generated_files() {
    let files = generate_dart_code(
        &spec(
            vec![record_rule(
                "User",
                vec![entry("name", builtin("text"), false)],
            )],
            0,
        ),
        &config("dart"),
    )
    .unwrap();
    let barrel = files
        .iter()
        .find(|f| f.path == "models.gen.dart")
        .expect("barrel file");
    assert!(
        barrel.content.contains("export 'types.gen.dart';"),
        "barrel exports types"
    );
}

#[test]
fn timestamp_and_bytes_map_to_dart_types() {
    let rules = vec![record_rule(
        "Event",
        vec![
            entry("at", builtin("timestamp"), false),
            entry("blob", builtin("bytes"), false),
        ],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final DateTime at;"),
        "timestamp -> DateTime: {code}"
    );
    assert!(
        code.contains("final Uint8List blob;"),
        "bytes -> Uint8List: {code}"
    );
    assert!(
        code.contains("import 'dart:typed_data';"),
        "typed_data import: {code}"
    );
    assert!(
        code.contains("_bytesEqual(blob, other.blob)"),
        "byte content equality: {code}"
    );
}

fn text_lit(s: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string()))
}

fn int_lit(n: i64) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Integer(n))
}

#[test]
fn all_literal_string_choice_becomes_string_typedef_not_sealed() {
    // `"open" / "closed"` (no general `text`/`tstr` arm) is a closed string set:
    // idiomatic Dart is a `String` alias, and the bare string round-trips — a
    // sealed wrapper wouldn't.
    let rules = vec![CsilRule {
        name: "Status".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![text_lit("open"), text_lit("closed")]),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("typedef Status = String;"),
        "all-literal string choice -> typedef: {code}"
    );
    assert!(
        !code.contains("sealed class Status"),
        "no sealed wrapper for a string set: {code}"
    );
}

#[test]
fn mixed_text_choice_becomes_sealed_union_not_typedef() {
    // `Status = text / "open" / "closed"` has a general `text` arm alongside literal
    // arms: any string satisfies `text`, so the wire needs the tagged-sum
    // `[variant_index, value]` to disambiguate the literal "open" from some other
    // string that merely equals "open" — a bare `String` typedef would drop that tag.
    let rules = vec![CsilRule {
        name: "Status".to_string(),
        rule_type: CsilRuleType::TypeChoice(vec![
            builtin("text"),
            text_lit("open"),
            text_lit("closed"),
        ]),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        !code.contains("typedef Status = String;"),
        "mixed text choice must not collapse to a bare typedef: {code}"
    );
    assert!(
        code.contains("sealed class Status {"),
        "mixed text choice -> sealed union: {code}"
    );
    assert!(
        code.contains("final class StatusVariant0 extends Status {"),
        "index 0 is the general text arm: {code}"
    );
    assert!(
        code.contains("final class StatusVariant1 extends Status {"),
        "index 1 is the \"open\" literal arm: {code}"
    );
    assert!(
        code.contains("final class StatusVariant2 extends Status {"),
        "index 2 is the \"closed\" literal arm: {code}"
    );
}

#[test]
fn inline_all_literal_string_choice_field_maps_to_string() {
    let mut e = entry("note", builtin("text"), true);
    e.value_type = CsilTypeExpression::Choice(vec![text_lit("vip"), text_lit("regular")]);
    let files = generate_dart_code(
        &spec(vec![record_rule("Acct", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final String? note;"),
        "inline all-literal string choice -> String?: {code}"
    );
}

#[test]
fn inline_mixed_text_choice_field_hoists_to_sealed_union() {
    // An inline (non-named) mixed choice has no nameable Dart type of its own — a
    // sealed class must be nominal — so it must behave EXACTLY like a field that
    // instead referenced a named choice rule with the same arms: hoisted to a
    // synthesized `<Owner>_<field>` type with a real sealed-class/tagged-sum codec,
    // not the wire-incorrect `Object?` passthrough it used to collapse to.
    let mut e = entry("note", builtin("text"), true);
    e.value_type = CsilTypeExpression::Choice(vec![builtin("text"), text_lit("vip")]);
    let files = generate_dart_code(
        &spec(vec![record_rule("Acct", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final AcctNote? note;"),
        "inline mixed text choice hoists to a synthesized <Owner>_<field> sealed union: {code}"
    );
    assert!(
        code.contains("sealed class AcctNote {"),
        "hoisted type gets a real sealed-class declaration: {code}"
    );
    assert!(
        code.contains("final class AcctNoteVariant0 extends AcctNote {"),
        "the open text arm keeps its declaration-order variant name: {code}"
    );
    assert!(
        code.contains("final class AcctNoteVariant1 extends AcctNote {"),
        "the literal arm keeps its declaration-order variant name: {code}"
    );
}

/// `OrderStatus = text / "pending" / "confirmed" / "processing" / "shipped" /
/// "delivered" / "cancelled" / "refunded"` — the real-world shape from
/// examples/real-world-api/e-commerce-api.csil line 138 (8 arms: index 0 the
/// general `text` arm, indices 1-7 the 7 literals in declared order) — must emit a
/// real `sealed class`, not the `String` typedef it used to collapse to.
fn order_status_rules() -> Vec<CsilRule> {
    vec![
        CsilRule {
            name: "OrderStatus".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                builtin("text"),
                text_lit("pending"),
                text_lit("confirmed"),
                text_lit("processing"),
                text_lit("shipped"),
                text_lit("delivered"),
                text_lit("cancelled"),
                text_lit("refunded"),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        record_rule(
            "OrderStatusHolder",
            vec![entry("status", reference("OrderStatus"), false)],
        ),
    ]
}

#[test]
fn doc_commented_field_after_another_member_gets_a_blank_line() {
    // dart format inserts a blank line before a doc-commented member that follows
    // another member (but not after the opening brace), so the generator must emit
    // exactly that shape to stay `dart format --set-exit-if-changed`-clean.
    let mut first = entry("amount", builtin("int"), false);
    first.metadata = vec![CsilFieldMetadata::Description(
        "Amount in smallest currency unit".to_string(),
    )];
    let mut second = entry("currency", builtin("text"), false);
    second.metadata = vec![CsilFieldMetadata::Description(
        "ISO 4217 currency code".to_string(),
    )];
    let rules = vec![record_rule("Money", vec![first, second])];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains(
            "final class Money {\n  /// Amount in smallest currency unit\n  final int amount;\n"
        ),
        "no blank line after the opening brace: {code}"
    );
    assert!(
        code.contains(
            "final int amount;\n\n  /// ISO 4217 currency code\n  final String currency;\n"
        ),
        "blank line before a doc-commented follower field: {code}"
    );
}

#[test]
fn order_status_emits_sealed_class_not_typedef() {
    let files = generate_dart_code(&spec(order_status_rules(), 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("sealed class OrderStatus {"),
        "OrderStatus must be a real sealed union: {code}"
    );
    assert!(
        !code.contains("typedef OrderStatus = String;"),
        "OrderStatus must not collapse to a bare String typedef: {code}"
    );
}

#[test]
fn any_field_maps_to_object_without_double_nullable() {
    let rules = vec![record_rule(
        "Bag",
        vec![entry("details", builtin("any"), true)],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final Object? details;"),
        "any -> Object?: {code}"
    );
    assert!(!code.contains("Object??"), "no double nullable: {code}");
    // `Object?` matches the map's own value type, so no redundant cast is emitted.
    assert!(
        code.contains("details: map['details']"),
        "no unnecessary cast on Object?: {code}"
    );
}

#[test]
fn services_file_imports_the_types_library() {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "get-user".to_string(),
            input_type: reference("GetUser"),
            output_type: reference("User"),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    let rules = vec![
        record_rule("User", vec![entry("name", builtin("text"), false)]),
        CsilRule {
            name: "UserService".to_string(),
            rule_type: CsilRuleType::ServiceDef(service),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ];
    let files = generate_dart_code(&spec(rules, 1), &config("dart")).unwrap();
    let code = services_file(&files);
    assert!(
        code.contains("import 'types.gen.dart';"),
        "services must import the types it names: {code}"
    );
}

#[test]
fn required_send_only_field_is_still_read_in_from_map() {
    // A required `@send-only` field must still be populated by `fromMap`: dropping
    // it would leave the const constructor's `required` param unsatisfied — a hard
    // compile error in the emitted Dart.
    let mut e = entry("id", builtin("text"), false);
    e.metadata = vec![CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly)];
    let files = generate_dart_code(
        &spec(vec![record_rule("GetReq", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("id: map['id'] as String"),
        "send-only required field read back: {code}"
    );
}

#[test]
fn min_length_one_uses_is_empty_and_min_zero_is_dropped() {
    let mut e_one = entry("name", builtin("text"), false);
    e_one.metadata = vec![CsilFieldMetadata::Constraint(
        CsilValidationConstraint::MinLength(1),
    )];
    let mut e_zero = entry("bio", builtin("text"), false);
    e_zero.metadata = vec![CsilFieldMetadata::Constraint(
        CsilValidationConstraint::MinLength(0),
    )];
    let files = generate_dart_code(
        &spec(vec![record_rule("P", vec![e_one, e_zero])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("name.isEmpty"),
        "min-length 1 -> isEmpty (prefer_is_empty): {code}"
    );
    assert!(
        !code.contains("bio.length < 0"),
        "vacuous min-length 0 guard dropped: {code}"
    );
}

// --- codec round-trip -------------------------------------------------------

/// A corndogs-shaped spec exercising the codec: text, bytes, an optional int, a
/// map, a list, a nested record, and a service whose output is a `Res / Error`
/// choice.
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
    // A named map alias of records (`RecordMap = {* text => SomeRecord}`): the
    // regression dropped these because the bare reference fell through the codec.
    let record_map_ty = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(reference("SomeRecord")),
        occurrence: None,
    };
    vec![
        record_rule(
            "Task",
            vec![
                entry("uuid", builtin("text"), false),
                entry("current_state", builtin("text"), false),
                entry("payload", builtin("bytes"), false),
                entry("priority", builtin("int"), true),
                entry("labels", map_ty.clone(), false),
                entry("tags", list_ty, false),
            ],
        ),
        record_rule("SomeRecord", vec![entry("n", builtin("int"), false)]),
        // A zero-field request record (corndogs' `GetQueuesRequest = {}`): it must
        // emit a plain unnamed const constructor, not an empty named-parameter list
        // (`const X({});`), or the whole library fails to compile.
        record_rule("GetQueuesRequest", vec![]),
        // Named map aliases: a scalar-valued one and a record-valued one. A field
        // typed as either must round-trip its entries, not stub to null.
        type_rule("StringInt64Map", map_ty),
        type_rule("RecordMap", record_map_ty),
        record_rule(
            "SubmitTaskRequest",
            vec![
                entry("task", reference("Task"), false),
                entry("queue", builtin("text"), false),
                entry("counts", reference("StringInt64Map"), false),
                entry("things", reference("RecordMap"), false),
            ],
        ),
        record_rule(
            "ServiceError",
            vec![
                entry("code", builtin("int"), false),
                entry("message", builtin("text"), false),
            ],
        ),
        CsilRule {
            name: "CorndogsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "submit-task".to_string(),
                    input_type: reference("SubmitTaskRequest"),
                    output_type: CsilTypeExpression::Choice(vec![
                        reference("Task"),
                        reference("ServiceError"),
                    ]),
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                }],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ]
}

/// Compile and run the generated Dart, round-tripping a typed request/response.
/// Skips when `dart` is not on PATH so the suite stays portable.
#[test]
fn codec_round_trips_through_dart() {
    let have = std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let files = generate_dart_code(&spec(corndogs_rules(), 1), &config("dart-client")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-codec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.dart"), CODEC_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dart round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const CODEC_DRIVER_DART: &str = r#"import 'dart:typed_data';
import 'models.gen.dart';

class LoopbackTransport implements CsilTransport {
  @override
  List<int> call(String service, String op, List<int> request) {
    // The client must route with the verbatim CSIL service and op names.
    if (service != 'CorndogsService' || op != 'submit-task') {
      throw StateError('unexpected route $service/$op');
    }
    // Decode the typed request, then encode its task as the typed response.
    final req = SubmitTaskRequest.fromCbor(request);
    return req.task.toCbor();
  }
}

void _check(bool ok, String what) {
  if (!ok) throw StateError('check failed: $what');
}

bool _bytesEq(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

void main() {
  final payload = Uint8List.fromList([0xde, 0xad, 0xbe]);
  final task = Task(
    uuid: 'u-123',
    currentState: 'PENDING',
    payload: payload,
    priority: 7,
    labels: {'a': 1, 'b': 2},
    tags: ['x', 'y'],
  );
  final req = SubmitTaskRequest(
    task: task,
    queue: 'default',
    counts: {'one': 1, 'two': 2},
    things: {'r1': SomeRecord(n: 11), 'r2': SomeRecord(n: 22)},
  );

  // direct codec round-trip through the nested record
  final back = SubmitTaskRequest.fromCbor(req.toCbor());
  _check(back.task.uuid == 'u-123', 'uuid');
  _check(back.task.currentState == 'PENDING', 'current_state');
  _check(_bytesEq(back.task.payload, payload), 'payload');
  _check(back.task.priority == 7, 'priority');
  _check(back.task.labels['a'] == 1 && back.task.labels['b'] == 2, 'labels');
  _check(back.task.tags.length == 2 && back.task.tags[1] == 'y', 'tags');
  _check(back.queue == 'default', 'queue');

  // a named map alias of scalars must survive, not stub to null/empty
  _check(back.counts.length == 2, 'counts length');
  _check(back.counts['one'] == 1 && back.counts['two'] == 2, 'counts entries');

  // a named map alias of records must survive with reconstructed records
  _check(back.things.length == 2, 'things length');
  _check(back.things['r1']!.n == 11 && back.things['r2']!.n == 22, 'things entries');

  // an absent optional must round-trip to null
  final task2 = Task(
    uuid: 'u',
    currentState: 'S',
    payload: Uint8List(0),
    labels: const {},
    tags: const [],
  );
  final back2 = SubmitTaskRequest.fromCbor(
    SubmitTaskRequest(task: task2, queue: 'q', counts: const {}, things: const {}).toCbor(),
  );
  _check(back2.task.priority == null, 'absent optional');

  // a zero-field record constructs, encodes, and round-trips through CBOR
  const empty = GetQueuesRequest();
  final emptyBack = GetQueuesRequest.fromCbor(empty.toCbor());
  _check(emptyBack == empty, 'empty record round-trip');

  // typed client over the loopback carrier
  final client = CorndogsClient(LoopbackTransport());
  final resp = client.submitTask(req);
  _check(resp.uuid == 'u-123', 'resp uuid');
  _check(_bytesEq(resp.payload, payload), 'resp payload');
  _check(resp.priority == 7, 'resp priority');

  print('ok');
}
"#;

/// A record referencing two NAMED (hoisted) all-literal choices — `Color = "red" /
/// "green" / "blue"` and `Level = 1 / 2 / 3` — declared as top-level rules and used
/// by *reference*, not inlined on the field. Distinct from the inline-choice shape
/// `all_literal_string_choice_becomes_string_typedef_not_sealed` already covers.
fn enum_reference_rules() -> Vec<CsilRule> {
    vec![
        CsilRule {
            name: "Color".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                text_lit("red"),
                text_lit("green"),
                text_lit("blue"),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        CsilRule {
            name: "Level".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![
                CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
                CsilTypeExpression::Literal(CsilLiteralValue::Integer(2)),
                CsilTypeExpression::Literal(CsilLiteralValue::Integer(3)),
            ]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        record_rule(
            "Item",
            vec![
                entry("color", reference("Color"), false),
                entry("level", reference("Level"), false),
            ],
        ),
    ]
}

/// Regression: a field referencing a NAMED all-literal choice (`color: Color` where
/// `Color = "red" / "green" / "blue"` is declared separately, not inlined) used to
/// fall through every codec lookup (`records`/`unions`/`aliases` all missed it,
/// since `union_choices` deliberately excludes closed string/int sets and
/// `codec_aliases` used to exclude ALL choices) straight to the raw-passthrough
/// default, so `fromCborValue` never called `CsilCbor.expectOneOf` and any string —
/// not just a declared member — decoded successfully. `codec_aliases` now folds a
/// closed choice in as a transparent alias, so decode recurses into the same
/// `Choice(...)` arm the inline-field case already uses.
#[test]
fn hoisted_enum_reference_field_gets_membership_check_on_decode() {
    let files = generate_dart_code(&spec(enum_reference_rules(), 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("CsilCbor.expectOneOf<String>(map['color']"),
        "a field referencing a named string choice must decode through expectOneOf: {code}"
    );
    assert!(
        code.contains("CsilCbor.expectOneOf<int>(map['level']"),
        "a field referencing a named int choice must decode through expectOneOf: {code}"
    );
}

/// The same regression, but empirically probed with the Dart VM: decode a
/// hand-built CBOR-decoded-map carrying an out-of-set string (`"purple"` against
/// `Color`) and an out-of-set int (`99` against `Level`) and confirm both raise the
/// codec's standard `ArgumentError('CsilCbor: value not a member of the closed
/// set')` — matching the membership contract python/ocaml/php/ruby/elixir already
/// enforce — while a valid member still round-trips byte-identical. Skips when
/// `dart` is not on PATH.
#[test]
fn enum_field_decode_rejects_out_of_set_value_through_dart() {
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let files = generate_dart_code(&spec(enum_reference_rules(), 0), &config("dart")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-enum-decode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.dart"), ENUM_DECODE_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dart enum-decode probe failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const ENUM_DECODE_DRIVER_DART: &str = r#"import 'models.gen.dart';

void _check(bool ok, String what) {
  if (!ok) throw StateError('check failed: $what');
}

void main() {
  // Valid member round-trips byte-identical.
  final item = Item(color: 'red', level: 2);
  final back = Item.fromCbor(item.toCbor());
  _check(back.color == 'red' && back.level == 2, 'valid round-trip');

  // An out-of-set string must be rejected with the codec's standard error.
  try {
    Item.fromCborValue({'color': 'purple', 'level': 2});
    throw StateError('out-of-set color was accepted');
  } on ArgumentError catch (e) {
    _check(
      e.toString().contains('not a member of the closed set'),
      'color error shape: $e',
    );
  }

  // An out-of-set int must be rejected with the codec's standard error.
  try {
    Item.fromCborValue({'color': 'red', 'level': 99});
    throw StateError('out-of-set level was accepted');
  } on ArgumentError catch (e) {
    _check(
      e.toString().contains('not a member of the closed set'),
      'level error shape: $e',
    );
  }

  print('ok');
}
"#;

/// Drives the real Dart VM over the `OrderStatus` mixed-union shape (see
/// `order_status_rules`): proves the tagged-sum wire contract end to end — a
/// literal arm's own index wins on encode, decode dispatches by index, a
/// literal-typed arm validates its payload against the declared literal, and every
/// declared index round-trips. Skips when `dart` is not on PATH.
#[test]
fn order_status_union_round_trips_through_dart() {
    let have = std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let files = generate_dart_code(&spec(order_status_rules(), 0), &config("dart")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-orderstatus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.dart"), ORDER_STATUS_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dart OrderStatus round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const ORDER_STATUS_DRIVER_DART: &str = r#"import 'models.gen.dart';

void _check(bool ok, String what) {
  if (!ok) throw StateError('check failed: $what');
}

void main() {
  // All 8 declared arms: index 0 is the general `text` arm, 1-7 the literals in
  // declaration order (matches examples/real-world-api/e-commerce-api.csil:138).
  final arms = <(OrderStatus, int, String)>[
    (OrderStatusVariant0('on-hold'), 0, 'on-hold'),
    (OrderStatusVariant1('pending'), 1, 'pending'),
    (OrderStatusVariant2('confirmed'), 2, 'confirmed'),
    (OrderStatusVariant3('processing'), 3, 'processing'),
    (OrderStatusVariant4('shipped'), 4, 'shipped'),
    (OrderStatusVariant5('delivered'), 5, 'delivered'),
    (OrderStatusVariant6('cancelled'), 6, 'cancelled'),
    (OrderStatusVariant7('refunded'), 7, 'refunded'),
  ];

  for (final (status, index, value) in arms) {
    final holder = OrderStatusHolder(status: status);
    final bytes = holder.toCbor();
    final wire = (CsilCbor.decode(bytes) as Map)['status'] as List;
    _check(wire.length == 2, 'tagged-sum shape for index $index');
    _check(wire[0] == index, 'wire index for "$value": got ${wire[0]}');
    _check(wire[1] == value, 'wire value for "$value": got ${wire[1]}');

    final back = OrderStatusHolder.fromCbor(bytes);
    _check(
      back.status.runtimeType == status.runtimeType,
      'round-trip arm type for index $index',
    );
  }

  // Named spot checks from the ticket: the literal arm's own declared index wins
  // on encode (the general arm is only the fallback for values no literal claims).
  final pendingBytes = OrderStatusHolder(status: OrderStatusVariant1('pending')).toCbor();
  final pendingWire = (CsilCbor.decode(pendingBytes) as Map)['status'] as List;
  _check(
    pendingWire[0] == 1 && pendingWire[1] == 'pending',
    'literal arm "pending" -> [1, pending]',
  );

  final onHoldBytes = OrderStatusHolder(status: OrderStatusVariant0('on-hold')).toCbor();
  final onHoldWire = (CsilCbor.decode(onHoldBytes) as Map)['status'] as List;
  _check(
    onHoldWire[0] == 0 && onHoldWire[1] == 'on-hold',
    'general arm "on-hold" -> [0, on-hold]',
  );

  // Decode must validate a literal arm's payload against its declared literal:
  // index 1 claims "pending" but the payload says "confirmed" -> literal mismatch.
  final malformed = CsilCbor.encodeValue(<String, Object?>{
    'status': <Object?>[1, 'confirmed'],
  });
  var threw = false;
  try {
    OrderStatusHolder.fromCbor(malformed);
  } catch (_) {
    threw = true;
  }
  _check(threw, 'decoding [1, confirmed] (index 1 claims pending) must throw');

  print('ok');
}
"#;

#[test]
fn wire_strings_are_verbatim_csil_names() {
    // `service CorndogsService` -> wire service "CorndogsService" (no suffix
    // stripping, no lowercasing); `submit-task` -> wire op "submit-task", verbatim
    // per csil-rpc-transport.md §1.1, so a Dart client hits the same route as the
    // Go/Python/TS peers.
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "submit-task".to_string(),
            input_type: reference("SubmitTaskRequest"),
            output_type: reference("Task"),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    let rules = vec![
        record_rule(
            "SubmitTaskRequest",
            vec![entry("queue", builtin("text"), false)],
        ),
        record_rule("Task", vec![entry("uuid", builtin("text"), false)]),
        CsilRule {
            name: "CorndogsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(service),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ];
    let files = generate_dart_code(&spec(rules, 1), &config("dart-client")).unwrap();
    let code = services_file(&files);
    // The call exceeds the page width, so it renders split — the wire strings
    // stay verbatim on their own lines.
    assert!(
        code.contains("transport.call(\n      'CorndogsService',\n      'submit-task',"),
        "wire service and op verbatim: {code}"
    );
    assert!(
        !code.contains("'corndogs'"),
        "service must not be lowercased or stripped: {code}"
    );
    assert!(
        !code.contains("'SubmitTask'"),
        "op must not be PascalCased: {code}"
    );
}

// --- async client style ----------------------------------------------------

fn async_services_file(files: &[GeneratedFile]) -> &str {
    &files
        .iter()
        .find(|f| f.path == "services.async.gen.dart")
        .expect("services.async.gen.dart should be generated")
        .content
}

#[test]
fn async_twin_emitted_by_default_with_marked_symbols() {
    // Absent `client_style` defaults to `both`: the canonical sync client is
    // unchanged AND an async twin lands beside it, every public symbol marked so the
    // two coexist in one package/barrel without collisions.
    let files = generate_dart_code(&spec(corndogs_rules(), 1), &config("dart-client")).unwrap();

    // Sync client at the canonical path, with the canonical (unmarked) symbols.
    let sync = services_file(&files);
    assert!(
        sync.contains("final class CorndogsClient {"),
        "canonical sync client: {sync}"
    );
    assert!(
        sync.contains("abstract interface class CsilTransport {"),
        "canonical sync transport: {sync}"
    );
    assert!(
        sync.contains("Task submitTask(SubmitTaskRequest request) {"),
        "sync method stays blocking: {sync}"
    );

    // Async twin in a separate file, every symbol carrying the `Async` marker.
    let twin = async_services_file(&files);
    assert!(
        twin.contains("final class CorndogsAsyncClient {"),
        "marked async client class: {twin}"
    );
    assert!(
        twin.contains("abstract interface class AsyncCsilTransport {"),
        "marked async transport type: {twin}"
    );
    assert!(
        twin.contains("Future<Uint8List> call(String service, String op, List<int> request);"),
        "async transport seam returns Future<Uint8List>: {twin}"
    );
    assert!(
        twin.contains("Future<Task> submitTask(SubmitTaskRequest request) async {"),
        "async method returns a Future and is async: {twin}"
    );
    assert!(
        twin.contains("final csilResp = await transport.call("),
        "async method awaits the seam: {twin}"
    );
    assert!(
        twin.contains("final AsyncCsilTransport transport;"),
        "twin holds the marked transport: {twin}"
    );

    // The barrel registers the twin so a single import surfaces both clients.
    let barrel = files
        .iter()
        .find(|f| f.path == "models.gen.dart")
        .expect("barrel file");
    assert!(
        barrel.content.contains("export 'services.async.gen.dart';"),
        "barrel re-exports the async twin: {}",
        barrel.content
    );
}

#[test]
fn client_style_async_is_drop_in_at_canonical_path() {
    // `async` is a drop-in: the async client sits at the SAME canonical filename with
    // the SAME canonical symbol names (just async), so swapping sync for async changes
    // nothing but the await. No marked twin is emitted.
    let files = generate_dart_code(
        &spec(corndogs_rules(), 1),
        &config_with(
            "dart-client",
            &[("client_style", serde_json::json!("async"))],
        ),
    )
    .unwrap();
    let code = services_file(&files);
    assert!(
        code.contains("final class CorndogsClient {"),
        "canonical class name preserved: {code}"
    );
    assert!(
        code.contains("abstract interface class CsilTransport {"),
        "canonical transport name preserved: {code}"
    );
    assert!(
        code.contains("Future<Uint8List> call(String service, String op, List<int> request);"),
        "drop-in seam turns async: {code}"
    );
    assert!(
        code.contains("Future<Task> submitTask(SubmitTaskRequest request) async {"),
        "drop-in method turns async: {code}"
    );
    assert!(
        code.contains("final csilResp = await transport.call("),
        "drop-in awaits the seam: {code}"
    );
    assert!(
        !files.iter().any(|f| f.path == "services.async.gen.dart"),
        "drop-in mode emits no separate twin file"
    );
    assert!(
        !code.contains("Async"),
        "drop-in carries no Async marker: {code}"
    );
}

#[test]
fn client_style_sync_suppresses_the_twin() {
    // `sync` is today's output verbatim: blocking client at the canonical path, and
    // crucially NO async twin file.
    let files = generate_dart_code(
        &spec(corndogs_rules(), 1),
        &config_with(
            "dart-client",
            &[("client_style", serde_json::json!("sync"))],
        ),
    )
    .unwrap();
    let code = services_file(&files);
    assert!(
        code.contains("Task submitTask(SubmitTaskRequest request) {"),
        "sync method stays blocking: {code}"
    );
    assert!(
        code.contains("final csilResp = transport.call("),
        "sync method calls the seam directly (no await): {code}"
    );
    assert!(
        !code.contains("Future<"),
        "sync client returns no futures: {code}"
    );
    assert!(
        !files.iter().any(|f| f.path == "services.async.gen.dart"),
        "sync style emits no async twin"
    );
}

#[test]
fn client_style_invalid_value_is_rejected() {
    let err = generate_dart_code(
        &spec(corndogs_rules(), 1),
        &config_with(
            "dart-client",
            &[("client_style", serde_json::json!("eventually"))],
        ),
    );
    let msg = format!("{:?}", err.expect_err("invalid client_style must fail"));
    assert!(
        msg.contains("client_style"),
        "error must name the offending option: {msg}"
    );
}

/// Compile and run the generated async client, awaiting a typed response through an
/// async loopback transport. Skips when `dart` is not on PATH so the suite stays
/// portable. Mirrors `codec_round_trips_through_dart` but exercises the `both`-mode
/// async twin over `AsyncCsilTransport`.
#[test]
fn async_client_round_trips_through_dart() {
    let have = std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    // Default `both` mode: the barrel exports both the sync client and the async twin.
    let files = generate_dart_code(&spec(corndogs_rules(), 1), &config("dart-client")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-async-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.dart"), ASYNC_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dart async round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const ASYNC_DRIVER_DART: &str = r#"import 'dart:typed_data';
import 'models.gen.dart';

/// An async carrier: the seam returns a Future the generated client awaits.
class AsyncLoopback implements AsyncCsilTransport {
  @override
  Future<Uint8List> call(String service, String op, List<int> request) async {
    // The client must route with the verbatim CSIL service and op names.
    if (service != 'CorndogsService' || op != 'submit-task') {
      throw StateError('unexpected route $service/$op');
    }
    // Decode the typed request, then encode its task as the typed response.
    final req = SubmitTaskRequest.fromCbor(request);
    return req.task.toCbor();
  }
}

void _check(bool ok, String what) {
  if (!ok) throw StateError('check failed: $what');
}

bool _bytesEq(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

Future<void> main() async {
  final payload = Uint8List.fromList([0xde, 0xad, 0xbe]);
  final task = Task(
    uuid: 'u-123',
    currentState: 'PENDING',
    payload: payload,
    priority: 7,
    labels: {'a': 1, 'b': 2},
    tags: ['x', 'y'],
  );
  final req = SubmitTaskRequest(
    task: task,
    queue: 'default',
    counts: {'one': 1, 'two': 2},
    things: {'r1': SomeRecord(n: 11), 'r2': SomeRecord(n: 22)},
  );

  // typed async client over the async loopback carrier: the response survives the
  // await with its decoded fields intact.
  final client = CorndogsAsyncClient(AsyncLoopback());
  final resp = await client.submitTask(req);
  _check(resp.uuid == 'u-123', 'resp uuid');
  _check(resp.currentState == 'PENDING', 'resp current_state');
  _check(_bytesEq(resp.payload, payload), 'resp payload');
  _check(resp.priority == 7, 'resp priority');
  _check(resp.labels['a'] == 1 && resp.labels['b'] == 2, 'resp labels');
  _check(resp.tags.length == 2 && resp.tags[1] == 'y', 'resp tags');

  print('ok');
}
"#;

// --- publishable pub package mode ------------------------------------------

#[test]
fn pubspec_emitted_iff_emit_packages_includes_dart() {
    let rules = || {
        vec![record_rule(
            "User",
            vec![entry("name", builtin("text"), false)],
        )]
    };

    // No emit_packages: default flat layout, no pubspec.
    let plain = generate_dart_code(&spec(rules(), 0), &config("dart")).unwrap();
    assert!(
        !plain.iter().any(|f| f.path == "pubspec.yaml"),
        "no pubspec without emit_packages"
    );
    assert!(
        plain.iter().any(|f| f.path == "types.gen.dart"),
        "flat layout keeps types at root"
    );

    // emit_packages present but without "dart": still no package files.
    let other = generate_dart_code(
        &spec(rules(), 0),
        &config_with(
            "dart",
            &[("emit_packages", serde_json::json!(["go", "python"]))],
        ),
    )
    .unwrap();
    assert!(
        !other.iter().any(|f| f.path == "pubspec.yaml"),
        "emit_packages without 'dart' must not emit a pubspec"
    );
    assert!(
        other.iter().any(|f| f.path == "types.gen.dart"),
        "unchanged when 'dart' is absent"
    );

    // emit_packages includes "dart": pubspec + lib/ layout + lib/<name>.dart barrel.
    let pkg = generate_dart_code(
        &spec(rules(), 0),
        &config_with(
            "dart",
            &[
                ("emit_packages", serde_json::json!(["dart", "go"])),
                ("package_name", serde_json::json!("my_models")),
                ("package_version", serde_json::json!("2.3.4")),
            ],
        ),
    )
    .unwrap();
    let pubspec = pkg
        .iter()
        .find(|f| f.path == "pubspec.yaml")
        .expect("pubspec.yaml emitted in package mode");
    assert!(
        pubspec.content.contains("name: my_models"),
        "pubspec name: {}",
        pubspec.content
    );
    assert!(
        pubspec.content.contains("version: 2.3.4"),
        "pubspec version: {}",
        pubspec.content
    );
    assert!(
        pubspec.content.contains("sdk: '>=3.0.0 <4.0.0'"),
        "pubspec sdk constraint: {}",
        pubspec.content
    );
    assert!(
        !pubspec.content.contains("dependencies:"),
        "no dependency block — generated code uses only dart: libs: {}",
        pubspec.content
    );
    assert!(
        pkg.iter().any(|f| f.path == "lib/types.gen.dart"),
        "generated dart moves under lib/: {:?}",
        pkg.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    assert!(
        pkg.iter().any(|f| f.path == "lib/my_models.dart"),
        "barrel is the lib/<name>.dart entrypoint: {:?}",
        pkg.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
}

#[test]
fn emit_readme_false_suppresses_only_the_readme() {
    let rules = || {
        vec![record_rule(
            "User",
            vec![entry("name", builtin("text"), false)],
        )]
    };

    // Default package mode: the README rides along.
    let with_readme = generate_dart_code(
        &spec(rules(), 0),
        &config_with("dart", &[("emit_packages", serde_json::json!(["dart"]))]),
    )
    .unwrap();
    assert!(
        with_readme.iter().any(|f| f.path == "genquickstart.md"),
        "README emitted by default in package mode"
    );

    // Only an explicit `emit_readme: false` drops it; the rest of the package stays.
    let without_readme = generate_dart_code(
        &spec(rules(), 0),
        &config_with(
            "dart",
            &[
                ("emit_packages", serde_json::json!(["dart"])),
                ("emit_readme", serde_json::json!(false)),
            ],
        ),
    )
    .unwrap();
    assert!(
        !without_readme.iter().any(|f| f.path == "genquickstart.md"),
        "emit_readme: false suppresses the README"
    );
    assert!(
        without_readme.iter().any(|f| f.path == "pubspec.yaml"),
        "the rest of the package is untouched"
    );
    assert!(
        without_readme
            .iter()
            .any(|f| f.path == "lib/types.gen.dart"),
        "generated dart still moves under lib/"
    );
}

#[test]
fn package_name_is_normalized_to_a_valid_pub_name() {
    // A PascalCase package_name is normalized to lowercase_with_underscores so the
    // emitted pubspec always names a publishable package.
    let pkg = generate_dart_code(
        &spec(
            vec![record_rule(
                "User",
                vec![entry("name", builtin("text"), false)],
            )],
            0,
        ),
        &config_with(
            "dart",
            &[
                ("emit_packages", serde_json::json!(["dart"])),
                ("package_name", serde_json::json!("MyCoolPackage")),
            ],
        ),
    )
    .unwrap();
    let pubspec = pkg.iter().find(|f| f.path == "pubspec.yaml").unwrap();
    assert!(
        pubspec.content.contains("name: my_cool_package"),
        "normalized pub name: {}",
        pubspec.content
    );
    assert!(
        pkg.iter().any(|f| f.path == "lib/my_cool_package.dart"),
        "barrel matches the normalized name"
    );
}

/// Generate a `dart-client` package into a temp dir and prove it's a real,
/// analyzable pub package: resolve deps offline, then `dart analyze`. Skips when
/// `dart` is absent; if offline resolution can't run (bare environment with no
/// cache), falls back to analyzing the `lib/` sources directly + validating the
/// pubspec, and notes it.
#[test]
fn generated_dart_package_passes_pub_get_and_analyze() {
    let have = std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dart on PATH");
        return;
    }

    let files = generate_dart_code(
        &spec(corndogs_rules(), 1),
        &config_with(
            "dart-client",
            &[
                ("emit_packages", serde_json::json!(["dart"])),
                ("package_name", serde_json::json!("csil_sample")),
            ],
        ),
    )
    .unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &f.content).unwrap();
    }

    // The generated lib/ is self-contained (only `dart:` + its own CsilCbor codec — no
    // `csilgen_transport`), so the package analyzes without any external dependency. The
    // README's lib-based Quickstart sections are proved separately by
    // `readme_three_transports_verify`, which supplies the transport-library dep.

    let get = std::process::Command::new("dart")
        .args(["pub", "get", "--offline"])
        .current_dir(&dir)
        .output()
        .unwrap();

    // A resolved package analyzes as a whole; an unresolved one (no package cache)
    // still has analyzable lib/ sources because the generated code uses only relative
    // and `dart:` imports — no `package:` self-import that needs the package config.
    let analyze_target = if get.status.success() {
        dir.clone()
    } else {
        eprintln!(
            "note: `dart pub get --offline` did not resolve; analyzing lib/ directly.\n{}",
            String::from_utf8_lossy(&get.stderr)
        );
        let pubspec = files.iter().find(|f| f.path == "pubspec.yaml").unwrap();
        assert!(
            pubspec.content.contains("name: csil_sample")
                && pubspec.content.contains("sdk: '>=3.0.0 <4.0.0'"),
            "pubspec is still well-formed: {}",
            pubspec.content
        );
        dir.join("lib")
    };

    let analyze = std::process::Command::new("dart")
        .arg("analyze")
        .arg(&analyze_target)
        .output()
        .unwrap();
    assert!(
        analyze.status.success(),
        "dart analyze failed:\n{}{}",
        String::from_utf8_lossy(&analyze.stdout),
        String::from_utf8_lossy(&analyze.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- 3-transport genquickstart (lib-based) ---------------------------------

/// The verification spec: a `->` op (`ping: Ping -> Ping`, record request and
/// response so the datagram codec round-trips) and a record-typed `<->` op
/// (`chat: ChatMsg <-> ChatReply`, both records so the channel router exists).
fn demo_rules() -> Vec<CsilRule> {
    vec![
        record_rule(
            "Ping",
            vec![
                entry("message", builtin("text"), false),
                entry("nonce", builtin("int"), false),
            ],
        ),
        record_rule("ChatMsg", vec![entry("body", builtin("text"), false)]),
        record_rule("ChatReply", vec![entry("ok", builtin("bool"), false)]),
        CsilRule {
            name: "DemoService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![
                    CsilServiceOperation {
                        name: "ping".to_string(),
                        input_type: reference("Ping"),
                        output_type: reference("Ping"),
                        direction: CsilServiceDirection::Unidirectional,
                        position: pos(),
                        doc_comments: Vec::new(),
                        wire_id: None,
                    },
                    CsilServiceOperation {
                        name: "chat".to_string(),
                        input_type: reference("ChatMsg"),
                        output_type: reference("ChatReply"),
                        direction: CsilServiceDirection::Bidirectional,
                        position: pos(),
                        doc_comments: Vec::new(),
                        wire_id: Some(2),
                    },
                ],
                wire_id: Some(1),
            }),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ]
}

fn demo_pkg_opts(target: &str) -> GeneratorConfig {
    config_with(
        target,
        &[
            ("emit_packages", serde_json::json!(["dart"])),
            ("package_name", serde_json::json!("csil_demo")),
        ],
    )
}

fn readme_of(files: &[GeneratedFile]) -> &str {
    &files
        .iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md emitted in package mode")
        .content
}

/// The body of the first ```dart block under `heading` (bounded to that section, so a
/// note-only section returns `None` rather than the next section's block).
fn extract_dart_block_after(readme: &str, heading: &str) -> Option<String> {
    let h = readme.find(heading)?;
    let rest = &readme[h + heading.len()..];
    let section = &rest[..rest.find("\n## ").unwrap_or(rest.len())];
    const FENCE: &str = "```dart\n";
    let start = section.find(FENCE)? + FENCE.len();
    let after = &section[start..];
    let end = after.find("```")?;
    Some(after[..end].to_string())
}

#[test]
fn client_package_readme_has_rpc_and_datagram_sections() {
    let files = generate_dart_code(&spec(demo_rules(), 1), &demo_pkg_opts("dart-client")).unwrap();
    let readme = readme_of(&files);

    // All three headings render in a fixed order.
    assert!(
        readme.contains("## CSIL-RPC (HTTP)"),
        "rpc heading: {readme}"
    );
    assert!(
        readme.contains("## CSIL-Events (TLS)"),
        "events heading: {readme}"
    );
    assert!(
        readme.contains("## CSIL-Datagrams (UDP)"),
        "datagrams heading: {readme}"
    );

    // Install names the transport library, not yet published.
    assert!(
        readme.contains("csilgen_transport:") && readme.contains("transports/dart"),
        "install adds the transport lib: {readme}"
    );

    // RPC: the carrier implements the generated async seam and builds the envelope with
    // the LIBRARY's RpcRequest/RpcResponse (never hand-rolled), POSTing to the path.
    assert!(
        readme.contains("class HttpRpcCarrier implements AsyncCsilTransport {"),
        "carrier implements the async seam: {readme}"
    );
    assert!(
        readme.contains("RpcRequest(service, op, Uint8List.fromList(request)).encode()"),
        "request envelope via the lib: {readme}"
    );
    assert!(
        readme.contains("RpcResponse.decode(bytes).intoTransportError()"),
        "response decode via the lib: {readme}"
    );
    assert!(
        readme.contains("/csil/v1/rpc"),
        "posts to the path: {readme}"
    );
    assert!(
        readme.contains("resp.variant == 'ServiceError'"),
        "typed ServiceError arm handled: {readme}"
    );
    assert!(
        readme.contains("import 'package:csilgen_transport/csilgen_transport.dart';"),
        "imports the transport lib: {readme}"
    );
    // Example constructs the async client and calls the first `->` op with a sample.
    assert!(
        readme.contains("final client = DemoAsyncClient(HttpRpcCarrier("),
        "example constructs the typed client: {readme}"
    );
    assert!(
        readme.contains("await client.ping(Ping(message: 'example', nonce: 0))"),
        "example calls the first unary op with a sample: {readme}"
    );

    // Datagrams: encode via the generated codec, wrap in the lib Datagram, and note the
    // no-synchronous-response semantics.
    assert!(
        readme.contains("Datagram(opOrd, 0, request.toCbor()).encode()"),
        "datagram send via lib + generated codec: {readme}"
    );
    assert!(
        readme.contains("Ping.fromCbor(dg.payload)"),
        "inbound payload decoded into the response type: {readme}"
    );
    assert!(
        readme.contains("MAY arrive later — or never"),
        "datagram loss/late note: {readme}"
    );

    // A package carries BOTH surfaces (the server/router too), so even a `dart-client`
    // package's Events section dispatches into the generated router — not the note case.
    // The genquickstart is self-contained: RPC client + Events router + codec all resolve
    // from this one package.
    assert!(
        readme.contains("class _Handlers implements DemoServiceHandler {"),
        "events handler implements the generated interface: {readme}"
    );
    assert!(
        readme.contains("routeDemoServiceChannel(handlers, codec, ev.event!, ev.payload)"),
        "events dispatches to the generated router: {readme}"
    );
    assert!(
        readme.contains(r"Hello([version], ['verbose'], service: 'DemoService')"),
        "events shows the $hello handshake naming the service: {readme}"
    );
}

#[test]
fn server_package_readme_events_dispatches_to_the_generated_router() {
    let files = generate_dart_code(&spec(demo_rules(), 1), &demo_pkg_opts("dart")).unwrap();
    let readme = readme_of(&files);

    // A server surface emits the channel router, so Events dispatches into it.
    assert!(
        readme.contains("class _Handlers implements DemoServiceHandler {"),
        "events handler implements the generated interface: {readme}"
    );
    assert!(
        readme.contains("routeDemoServiceChannel(handlers, codec, ev.event!, ev.payload)"),
        "events dispatches to the generated router: {readme}"
    );
    assert!(
        readme.contains("Hello([version], ['verbose'], service: 'DemoService')"),
        "events handshake names the service: {readme}"
    );
    assert!(
        readme.contains("Control.pingName") && readme.contains("Control.pongName"),
        "events answers the $ping/$pong heartbeat: {readme}"
    );
    assert!(
        readme.contains("Event.verbose('DemoService', 'chat', outbound.toCbor())"),
        "events sends one outbound event via the generated codec: {readme}"
    );

    // A package carries BOTH surfaces (the client too), so even a `dart` (server) package's
    // RPC section shows the live typed client — not the note case.
    assert!(
        readme.contains("final client = DemoAsyncClient(HttpRpcCarrier("),
        "rpc shows the live typed client: {readme}"
    );
    assert!(
        readme.contains("await client.ping(Ping(message: 'example', nonce: 0))"),
        "rpc calls the first unary op with a sample: {readme}"
    );
}

#[test]
fn genquickstart_transports_selects_a_subset() {
    // Only "datagrams" requested: the other two sections are suppressed.
    let files = generate_dart_code(
        &spec(demo_rules(), 1),
        &config_with(
            "dart-client",
            &[
                ("emit_packages", serde_json::json!(["dart"])),
                ("package_name", serde_json::json!("csil_demo")),
                ("genquickstart_transports", serde_json::json!(["datagrams"])),
            ],
        ),
    )
    .unwrap();
    let readme = readme_of(&files);
    assert!(
        readme.contains("## CSIL-Datagrams (UDP)"),
        "datagrams kept: {readme}"
    );
    assert!(
        !readme.contains("## CSIL-RPC (HTTP)"),
        "rpc suppressed: {readme}"
    );
    assert!(
        !readme.contains("## CSIL-Events (TLS)"),
        "events suppressed: {readme}"
    );

    // An array naming no known transport falls back to all three.
    let all = generate_dart_code(
        &spec(demo_rules(), 1),
        &config_with(
            "dart-client",
            &[
                ("emit_packages", serde_json::json!(["dart"])),
                ("package_name", serde_json::json!("csil_demo")),
                ("genquickstart_transports", serde_json::json!(["bogus"])),
            ],
        ),
    )
    .unwrap();
    let readme = readme_of(&all);
    assert!(
        readme.contains("## CSIL-RPC (HTTP)")
            && readme.contains("## CSIL-Events (TLS)")
            && readme.contains("## CSIL-Datagrams (UDP)"),
        "unknown-only subset falls back to all three: {readme}"
    );
}

/// Path to the reference Dart transport library, consumed as a path dependency so the
/// hermetic verification resolves it offline (no network, no published artifact).
fn transport_lib_path() -> String {
    format!("{}/../../transports/dart", env!("CARGO_MANIFEST_DIR"))
}

/// A pubspec for the generated package that adds the transport-library path dep so the
/// lib-based Quickstart sections compile/run against it.
fn pubspec_with_transport() -> String {
    format!(
        "name: csil_demo\nversion: 0.1.0\npublish_to: none\n\
         environment:\n  sdk: '>=3.0.0 <4.0.0'\n\
         dependencies:\n  csilgen_transport:\n    path: {}\n",
        transport_lib_path()
    )
}

fn have_dart() -> bool {
    std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok()
}

/// Verify the shipped 3-transport Quickstart against the SINGLE generated package + the
/// transport library. Package mode now carries BOTH the client and the server/router
/// surfaces, so all three sections — the RPC client, the live Events router dispatch, and
/// the Datagrams codec — resolve from one package: this stages that one package and
/// compile-checks (`dart analyze`) all three sections together, then RUNs RPC and Datagrams
/// hermetically (an injected in-process echo for RPC, the library's loopback datagram
/// carrier for UDP — the sandbox kills cross-process sockets). The Events session is
/// compile-checked only (its handshake/recv-loop wants a live TLS peer). Skips cleanly when
/// `dart` is absent or the path dep cannot be resolved offline.
#[test]
fn readme_three_transports_verify() {
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }

    // The single package a user publishes (`--target dart`). In package mode it carries the
    // client surface AND the server/router surface, so the genquickstart is self-contained.
    let pkg = generate_dart_code(&spec(demo_rules(), 1), &demo_pkg_opts("dart")).unwrap();
    let dir = std::env::temp_dir().join(format!("csilgen-dart-3t-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &pkg {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &f.content).unwrap();
    }
    // Supply the transport-library dependency the Quickstart sections need.
    std::fs::write(dir.join("pubspec.yaml"), pubspec_with_transport()).unwrap();

    let readme = readme_of(&pkg).to_string();
    let rpc = extract_dart_block_after(&readme, "## CSIL-RPC (HTTP)").expect("rpc block");
    let events = extract_dart_block_after(&readme, "## CSIL-Events (TLS)").expect("events block");
    let datagrams =
        extract_dart_block_after(&readme, "## CSIL-Datagrams (UDP)").expect("datagrams block");
    std::fs::write(dir.join("lib/qs_rpc.dart"), &rpc).unwrap();
    std::fs::write(dir.join("lib/qs_events.dart"), &events).unwrap();
    std::fs::write(dir.join("lib/qs_datagrams.dart"), &datagrams).unwrap();
    std::fs::write(dir.join("driver_rpc.dart"), DRIVER_RPC_DART).unwrap();
    std::fs::write(dir.join("driver_datagrams.dart"), DRIVER_DATAGRAMS_DART).unwrap();

    let get = std::process::Command::new("dart")
        .args(["pub", "get", "--offline"])
        .current_dir(&dir)
        .output()
        .unwrap();
    if !get.status.success() {
        eprintln!(
            "skipping: `dart pub get --offline` could not resolve the path dep:\n{}",
            String::from_utf8_lossy(&get.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    // COMPILE-CHECK all three shipped sections together against the ONE package — the RPC
    // client, the live Events router dispatch, and the Datagrams codec all at once.
    let analyze = std::process::Command::new("dart")
        .arg("analyze")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        analyze.status.success(),
        "single-package analyze failed:\n{}{}",
        String::from_utf8_lossy(&analyze.stdout),
        String::from_utf8_lossy(&analyze.stderr)
    );

    // RUN the RPC carrier over an injected in-process echo.
    let run_rpc = std::process::Command::new("dart")
        .arg("run")
        .arg("driver_rpc.dart")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run_rpc.status.success(),
        "rpc run failed:\n{}{}",
        String::from_utf8_lossy(&run_rpc.stdout),
        String::from_utf8_lossy(&run_rpc.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run_rpc.stdout).trim(), "ok");

    // RUN the Datagram send/recv over the library's loopback datagram carrier.
    let run_dg = std::process::Command::new("dart")
        .arg("run")
        .arg("driver_datagrams.dart")
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        run_dg.status.success(),
        "datagram run failed:\n{}{}",
        String::from_utf8_lossy(&run_dg.stdout),
        String::from_utf8_lossy(&run_dg.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run_dg.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Drives the shipped RPC carrier with an injected sender that echoes the request
/// payload back in a `status: 0` RpcResponse — built with the library's own envelope,
/// in-process so no socket is opened.
const DRIVER_RPC_DART: &str = r#"import 'dart:typed_data';

import 'package:csilgen_transport/csilgen_transport.dart';
import 'package:csil_demo/csil_demo.dart';
import 'package:csil_demo/qs_rpc.dart';

Future<Uint8List> echo(Uri uri, Uint8List body) async {
  final req = RpcRequest.decode(body);
  return RpcResponse.okReply('Ping', req.payload).encode();
}

Future<void> main() async {
  final client = DemoAsyncClient(HttpRpcCarrier('http://unused', sender: echo));
  final resp = await client.ping(Ping(message: 'hi', nonce: 7));
  if (resp.message != 'hi' || resp.nonce != 7) {
    throw StateError('rpc mismatch: ${resp.message}/${resp.nonce}');
  }
  print('ok');
}
"#;

/// Drives the shipped Datagram send/recv over the library's loopback datagram carrier:
/// the sent datagram is looped back as if it arrived later, and its payload decodes
/// back into the typed response.
const DRIVER_DATAGRAMS_DART: &str = r#"import 'package:csilgen_transport/csilgen_transport.dart';
import 'package:csil_demo/csil_demo.dart';
import 'package:csil_demo/qs_datagrams.dart';

void main() {
  final carrier = LoopbackDatagramCarrier();
  sendRequest(carrier, Ping(message: 'hi', nonce: 7));
  final sent = carrier.takeOutbound();
  if (sent == null) throw StateError('no datagram sent');
  // A datagram of the response type "arrives later": loop the sent one back.
  carrier.pushInbound(sent);
  final resp = recvResponse(carrier);
  if (resp == null || resp.message != 'hi' || resp.nonce != 7) {
    throw StateError('datagram mismatch: $resp');
  }
  print('ok');
}
"#;

// ---------------------------------------------------------------------------
// dart format ("tall style") parity — the emitted bytes must already be what
// `dart format --set-exit-if-changed` accepts, so over-width constructs render
// in the formatter's split shapes and short ones collapse.
// ---------------------------------------------------------------------------

/// A one-op unary service whose request/response type names push the client
/// method signature and transport call past the 80-column page width.
fn wide_service_rules() -> Vec<CsilRule> {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "recompute-aggregations".to_string(),
            input_type: reference("RecomputeAggregationsRequest"),
            output_type: reference("RecomputeAggregationsResponse"),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    vec![
        record_rule(
            "RecomputeAggregationsRequest",
            vec![entry("window", builtin("text"), false)],
        ),
        record_rule(
            "RecomputeAggregationsResponse",
            vec![entry("ok", builtin("bool"), false)],
        ),
        CsilRule {
            name: "MetricsService".to_string(),
            rule_type: CsilRuleType::ServiceDef(service),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ]
}

#[test]
fn over_width_method_signature_splits_tall() {
    let files = generate_dart_code(&spec(wide_service_rules(), 1), &config("dart-client")).unwrap();
    let code = services_file(&files);
    // Each parameter on its own line with a trailing comma, `) {` back at the
    // method's indent — the tall-style signature split.
    assert!(
        code.contains(
            "  RecomputeAggregationsResponse recomputeAggregations(\n    RecomputeAggregationsRequest request,\n  ) {"
        ),
        "over-width signature must split tall: {code}"
    );
    // The over-width transport call splits one argument per line.
    assert!(
        code.contains(
            "    final csilResp = transport.call(\n      'MetricsService',\n      'recompute-aggregations',\n      request.toCbor(),\n    );"
        ),
        "over-width call must split tall: {code}"
    );
}

#[test]
fn over_width_handler_signature_splits_tall() {
    let files = generate_dart_code(&spec(wide_service_rules(), 1), &config("dart")).unwrap();
    let code = services_file(&files);
    assert!(
        code.contains(
            "  RecomputeAggregationsResponse recomputeAggregations(\n    RecomputeAggregationsRequest request,\n  );"
        ),
        "over-width handler signature must split tall: {code}"
    );
}

#[test]
fn client_class_has_no_blank_line_before_closing_brace_or_at_eof() {
    let files = generate_dart_code(&spec(wide_service_rules(), 1), &config("dart-client")).unwrap();
    let code = services_file(&files);
    assert!(
        !code.contains("\n\n}"),
        "no blank line before a closing brace: {code}"
    );
    assert!(
        code.ends_with(";\n") || code.ends_with("}\n"),
        "file must end with exactly one newline: {code:?}"
    );
    assert!(
        !code.ends_with("\n\n"),
        "no trailing blank line at EOF: {code:?}"
    );
}

#[test]
fn types_file_has_no_trailing_blank_line_at_eof() {
    let rules = vec![record_rule(
        "Tick",
        vec![entry("seq", builtin("int"), false)],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(code.ends_with("}\n"), "single trailing newline: {code:?}");
    assert!(!code.ends_with("\n\n"), "no blank line at EOF: {code:?}");
}

#[test]
fn short_constructor_call_collapses_to_one_line() {
    let rules = vec![record_rule(
        "Tick",
        vec![entry("seq", builtin("int"), false)],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("return Tick(seq: map['seq'] as int);"),
        "short constructor call collapses: {code}"
    );
    assert!(
        code.contains("const Tick({required this.seq});"),
        "short constructor declaration collapses: {code}"
    );
}

#[test]
fn over_width_constructor_declaration_splits_one_param_per_line() {
    let rules = vec![record_rule(
        "AggregationSettings",
        vec![
            entry("first_field_name", builtin("text"), false),
            entry("second_field_name", builtin("text"), false),
            entry("third_field_name", builtin("text"), true),
        ],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains(
            "  const AggregationSettings({\n    required this.firstFieldName,\n    required this.secondFieldName,\n    this.thirdFieldName,\n  });"
        ),
        "over-width constructor splits with trailing comma: {code}"
    );
}

#[test]
fn short_regex_guard_collapses_to_one_line() {
    let mut e = entry("code", builtin("text"), false);
    e.value_type = CsilTypeExpression::Constrained {
        base_type: Box::new(builtin("text")),
        constraints: vec![CsilControlOperator::Regex("^[a-z]+".to_string())],
    };
    let files = generate_dart_code(
        &spec(vec![record_rule("Item", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("if (!RegExp('^[a-z]+').hasMatch(code)) {"),
        "short regex guard collapses: {code}"
    );
    assert!(
        code.contains("throw ArgumentError('\\'code\\' must match pattern ^[a-z]+');"),
        "short throw message collapses: {code}"
    );
}

#[test]
fn over_width_regex_guard_splits_tall() {
    // Regression: a `.match()` pattern long enough to push `if (!RegExp('...')
    // .hasMatch(field))` past Dart's 80-column page width used to stay on one
    // line and fail `dart format --set-exit-if-changed` (examples/complex-metadata
    // /advanced-api.csil's `contentType` field pattern hit this in the wild). The
    // generator must pre-split the sole `RegExp(...)` argument, and the
    // `ArgumentError(...)` message, exactly the way `dart format` itself would.
    let pattern = "^[a-zA-Z0-9][a-zA-Z0-9!#and-hyphen-caret-underscore]*slash[a-zA-Z0-9][a-zA-Z0-9!#and-hyphen-caret-underscore-dot]*end";
    let mut e = entry("content_type", builtin("text"), false);
    e.value_type = CsilTypeExpression::Constrained {
        base_type: Box::new(builtin("text")),
        constraints: vec![CsilControlOperator::Regex(pattern.to_string())],
    };
    let files = generate_dart_code(
        &spec(vec![record_rule("Item", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("    if (!RegExp(\n      '"),
        "over-width regex guard splits its sole argument onto its own line: {code}"
    );
    assert!(
        code.contains("    ).hasMatch(contentType)) {"),
        "the closing paren + hasMatch call stay on the if-line's own indent: {code}"
    );
    assert!(
        code.contains("      throw ArgumentError(\n        '"),
        "over-width throw message splits its sole argument onto its own line: {code}"
    );
    // Every generated line except the two now-isolated string-literal lines (an
    // unsplittable token `dart format` itself leaves long) must fit the page width.
    for line in code.lines() {
        let is_isolated_literal =
            line.trim_start().starts_with('\'') && line.trim_end().ends_with(',');
        assert!(
            is_isolated_literal || line.chars().count() <= 80,
            "no non-literal generated line may exceed Dart's 80-column page width: {line:?}"
        );
    }
}

#[test]
fn hash_code_tiers_follow_the_formatter() {
    // Over the width flat but the body fits on the continuation line: split at
    // the arrow only.
    let mid = vec![record_rule(
        "MidWidth",
        vec![
            entry("alpha_one", builtin("int"), false),
            entry("beta_two", builtin("int"), false),
            entry("gamma_three", builtin("int"), false),
            entry("delta_four", builtin("int"), false),
        ],
    )];
    let files = generate_dart_code(&spec(mid, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains(
            "  int get hashCode =>\n      Object.hashAll([alphaOne, betaTwo, gammaThree, deltaFour]);"
        ),
        "mid-width hashCode splits at the arrow: {code}"
    );

    // Too wide even for the continuation line: keep the arrow, block-split the
    // list one element per line.
    let wide = vec![record_rule(
        "WideRecord",
        vec![
            entry("first_field_name", builtin("int"), false),
            entry("second_field_name", builtin("int"), false),
            entry("third_field_name", builtin("int"), false),
            entry("fourth_field_name", builtin("int"), false),
        ],
    )];
    let files = generate_dart_code(&spec(wide, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains(
            "  int get hashCode => Object.hashAll([\n    firstFieldName,\n    secondFieldName,\n    thirdFieldName,\n    fourthFieldName,\n  ]);"
        ),
        "over-width hashCode block-splits the list: {code}"
    );
}

#[test]
fn over_width_list_decode_splits_method_chain() {
    let list_of_records = CsilTypeExpression::Array {
        element_type: Box::new(reference("RevocationCertificate")),
        occurrence: None,
    };
    let rules = vec![
        record_rule(
            "RevocationCertificate",
            vec![entry("serial", builtin("text"), false)],
        ),
        record_rule(
            "CertificateBundle",
            vec![CsilGroupEntry {
                key: Some(CsilGroupKey::Bare("revocation_certificates".to_string())),
                value_type: list_of_records,
                occurrence: None,
                metadata: vec![],
                doc_comments: Vec::new(),
            }],
        ),
    ];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains(
            "      revocationCertificates: (map['revocation_certificates'] as List)\n          .map((csilE) => RevocationCertificate.fromCborValue(csilE))\n          .cast<RevocationCertificate>()\n          .toList(),"
        ),
        "over-width decode chain splits one call per line: {code}"
    );
}

#[test]
fn over_width_optional_decode_splits_conditional_and_encode_splits_braceless_if() {
    let rules = vec![record_rule(
        "KeyDescriptor",
        vec![entry("signed_by_key_identifier", builtin("text"), true)],
    )];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    // The optional read splits with `?`/`:` operands on their own lines at +4.
    assert!(
        code.contains(
            "      signedByKeyIdentifier: map['signed_by_key_identifier'] == null\n          ? null\n          : map['signed_by_key_identifier'] as String,"
        ),
        "over-width conditional splits: {code}"
    );
    // The optional encode keeps the braceless `if` and wraps only the body.
    assert!(
        code.contains(
            "    if (signedByKeyIdentifier != null)\n      map['signed_by_key_identifier'] = signedByKeyIdentifier!;"
        ),
        "over-width braceless if wraps the body: {code}"
    );
}

// ---------------------------------------------------------------------------
// Inline (anonymous) choice hoisting
// ---------------------------------------------------------------------------

#[test]
fn trailing_default_on_literal_arm_still_classifies_as_string_choice() {
    // CSIL's grammar attaches a trailing control operator to the immediately
    // preceding arm, not to the choice as a whole: `"low" / "high" .default
    // "normal"` parses the last arm as `Constrained { base_type: Literal("high"),
    // constraints: [Default(...)] }`, not a bare `Literal`. Every arm-literal check
    // must see through that wrapper (`choice_arm_literal`), or this all-literal
    // choice gets misclassified as a general union instead of collapsing to the
    // bare-string wire it should.
    let mut e = entry("level", builtin("text"), false);
    e.value_type = CsilTypeExpression::Choice(vec![
        text_lit("low"),
        CsilTypeExpression::Constrained {
            base_type: Box::new(text_lit("high")),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "normal".to_string(),
            ))],
        },
    ]);
    let files = generate_dart_code(
        &spec(vec![record_rule("Setting", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final String level;"),
        "all-literal choice with a trailing .default arm still collapses to String, not a hoisted union: {code}"
    );
    assert!(
        !code.contains("sealed class SettingLevel"),
        "must not hoist an all-literal choice into a union: {code}"
    );
}

#[test]
fn trailing_default_on_literal_arm_still_classifies_as_int_choice() {
    // The int-choice twin of the string-choice regression above.
    let mut e = entry("level", builtin("int"), false);
    e.value_type = CsilTypeExpression::Choice(vec![
        CsilTypeExpression::Literal(CsilLiteralValue::Integer(1)),
        CsilTypeExpression::Constrained {
            base_type: Box::new(CsilTypeExpression::Literal(CsilLiteralValue::Integer(2))),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Integer(1))],
        },
    ]);
    let files = generate_dart_code(
        &spec(vec![record_rule("Setting", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final int level;"),
        "all-literal int choice with a trailing .default arm still collapses to int, not a hoisted union: {code}"
    );
}

/// `TortureInlineChoice` — mirrors
/// `scratchpad/shared/torture-inline-choice.csil`: a direct-field mixed inline
/// choice, a direct-field all-literal inline choice, a direct-field mixed choice
/// whose last literal arm carries a trailing `.default` (the classification-bug
/// shape), and a mixed inline choice nested in an array element / map value /
/// tuple element.
fn torture_rules() -> Vec<CsilRule> {
    let mixed_inline = CsilTypeExpression::Choice(vec![
        builtin("text"),
        text_lit("not_found"),
        text_lit("permission_denied"),
        text_lit("invalid_input"),
    ]);
    let pure_literal_inline = CsilTypeExpression::Choice(vec![
        text_lit("active"),
        text_lit("inactive"),
        text_lit("pending"),
    ]);
    let constrained_arm_inline = CsilTypeExpression::Choice(vec![
        builtin("text"),
        text_lit("low"),
        CsilTypeExpression::Constrained {
            base_type: Box::new(text_lit("high")),
            constraints: vec![CsilControlOperator::Default(CsilLiteralValue::Text(
                "normal".to_string(),
            ))],
        },
    ]);
    let tag_choice = CsilTypeExpression::Choice(vec![
        builtin("text"),
        text_lit("red"),
        text_lit("green"),
        text_lit("blue"),
    ]);
    let tag_list = CsilTypeExpression::Array {
        element_type: Box::new(tag_choice),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    };
    let label_choice = CsilTypeExpression::Choice(vec![
        builtin("text"),
        text_lit("urgent"),
        text_lit("normal"),
    ]);
    let label_map = CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(label_choice),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    };
    let coord_choice =
        CsilTypeExpression::Choice(vec![builtin("text"), text_lit("lat"), text_lit("lon")]);
    let coord = CsilTypeExpression::Tuple(CsilGroupExpression {
        entries: vec![
            CsilGroupEntry {
                key: None,
                value_type: coord_choice,
                occurrence: None,
                metadata: vec![],
                doc_comments: Vec::new(),
            },
            CsilGroupEntry {
                key: None,
                value_type: builtin("int"),
                occurrence: None,
                metadata: vec![],
                doc_comments: Vec::new(),
            },
        ],
    });
    vec![record_rule(
        "TortureInlineChoice",
        vec![
            entry("mixed_inline", mixed_inline, false),
            entry("pure_literal_inline", pure_literal_inline, false),
            entry("constrained_arm_inline", constrained_arm_inline, false),
            entry("tag_list", tag_list, false),
            entry("label_map", label_map, false),
            entry("coord", coord, false),
        ],
    )]
}

#[test]
fn torture_inline_choice_hoists_all_four_positions() {
    let files = generate_dart_code(&spec(torture_rules(), 0), &config("dart")).unwrap();
    let code = types_file(&files);
    // Direct field: mixed choice hoists to a sealed union.
    assert!(
        code.contains("sealed class TortureInlineChoiceMixedInline {"),
        "direct-field mixed inline choice hoists: {code}"
    );
    // Direct field: all-literal choice stays a bare String (not hoisted).
    assert!(
        code.contains("final String pureLiteralInline;"),
        "direct-field all-literal inline choice stays String: {code}"
    );
    // Direct field: the trailing-.default arm doesn't break classification — this
    // choice has a general `text` arm too, so it's still a (correctly-shaped) union.
    assert!(
        code.contains("sealed class TortureInlineChoiceConstrainedArmInline {"),
        "direct-field mixed choice with a trailing .default arm still hoists as a union: {code}"
    );
    // Array element.
    assert!(
        code.contains("sealed class TortureInlineChoiceTagListItem {"),
        "array-element inline choice hoists: {code}"
    );
    assert!(
        code.contains("final List<TortureInlineChoiceTagListItem> tagList;"),
        "tagList field routes through the hoisted element type: {code}"
    );
    // Map value.
    assert!(
        code.contains("sealed class TortureInlineChoiceLabelMapValue {"),
        "map-value inline choice hoists: {code}"
    );
    assert!(
        code.contains("final Map<String, TortureInlineChoiceLabelMapValue> labelMap;"),
        "labelMap field routes through the hoisted value type: {code}"
    );
    // Tuple element.
    assert!(
        code.contains("sealed class TortureInlineChoiceCoord0 {"),
        "tuple-element inline choice hoists: {code}"
    );
    assert!(
        code.contains("final (TortureInlineChoiceCoord0, int) coord;"),
        "coord field routes through the hoisted tuple-element type: {code}"
    );
}

/// Drives the real Dart VM over the torture spec: byte-for-byte cross-checks the
/// three direct-field cases against `csilgen-ocaml-generator`'s ground truth (see
/// `scratchpad/shared/ocaml-ground-truth-bytes.txt`), proves literal-first encode
/// precedence and decode-time literal/membership validation, and self-consistency
/// round-trips (plus hand-derived expected bytes) the three nested positions
/// (array element / map value / tuple element) that have no OCaml codec to
/// cross-check against. Skips when `dart` is not on PATH.
#[test]
fn torture_inline_choice_byte_cross_check_and_round_trip_through_dart() {
    let have = std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let files = generate_dart_code(&spec(torture_rules(), 0), &config("dart")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-torture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.dart"), TORTURE_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dart torture-inline-choice run failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const TORTURE_DRIVER_DART: &str = r#"import 'models.gen.dart';

void _check(bool ok, String what) {
  if (!ok) throw StateError('check failed: $what');
}

String hex(List<int> bytes) =>
    bytes.map((b) => b.toRadixString(16).padLeft(2, '0')).join();

TortureInlineChoice rec({
  required TortureInlineChoiceMixedInline mixedInline,
  required String pureLiteralInline,
  required TortureInlineChoiceConstrainedArmInline constrainedArmInline,
  List<TortureInlineChoiceTagListItem>? tagList,
  Map<String, TortureInlineChoiceLabelMapValue>? labelMap,
  (TortureInlineChoiceCoord0, int)? coord,
}) {
  return TortureInlineChoice(
    mixedInline: mixedInline,
    pureLiteralInline: pureLiteralInline,
    constrainedArmInline: constrainedArmInline,
    tagList: tagList ?? [TortureInlineChoiceTagListItemVariant1('red')],
    labelMap:
        labelMap ?? {'k': TortureInlineChoiceLabelMapValueVariant1('urgent')},
    coord: coord ?? (TortureInlineChoiceCoord0Variant1('lat'), 42),
  );
}

/// The wire bytes for a single field, isolated by decoding the whole record and
/// re-encoding just that field's decoded sub-tree — the same technique the OCaml
/// ground truth used (it called the field's own `encode_*` function directly).
List<int> fieldBytes(TortureInlineChoice r, String field) {
  final map = CsilCbor.decode(r.toCbor()) as Map;
  return CsilCbor.encodeValue(map[field]);
}

void main() {
  // --- mixed_inline: byte-for-byte against the OCaml ground truth ---
  // (index 0 = open `text` arm, 1 = "not_found", 2 = "permission_denied",
  // 3 = "invalid_input" — declaration order.)
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'mixed_inline')) == '8201696e6f745f666f756e64',
    'mixed_inline(not_found) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant2('permission_denied'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'mixed_inline')) == '8202717065726d697373696f6e5f64656e696564',
    'mixed_inline(permission_denied) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant3('invalid_input'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'mixed_inline')) == '82036d696e76616c69645f696e707574',
    'mixed_inline(invalid_input) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant0('banana'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'mixed_inline')) == '82006662616e616e61',
    'mixed_inline(Other banana) byte cross-check',
  );
  // Literal-first encode precedence: a literal arm's OWN canonical value is what
  // hits the wire, regardless of whatever the constructor was (wrongly) handed.
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('this value is ignored'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'mixed_inline')) == '8201696e6f745f666f756e64',
    'literal-first encode precedence for mixed_inline',
  );

  // --- pure_literal_inline: byte-for-byte against the OCaml ground truth ---
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'pure_literal_inline')) == '66616374697665',
    'pure_literal_inline(active) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'inactive',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'pure_literal_inline')) == '68696e616374697665',
    'pure_literal_inline(inactive) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'pending',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'pure_literal_inline')) == '6770656e64696e67',
    'pure_literal_inline(pending) byte cross-check',
  );

  // --- constrained_arm_inline: byte-for-byte against the OCaml ground truth ---
  // Proves the trailing-.default classification bug is fixed: "high" still
  // codes as a proper literal arm (index 2), not a dropped/misclassified one.
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    ), 'constrained_arm_inline')) == '8201636c6f77',
    'constrained_arm_inline(Low) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant2('high'),
    ), 'constrained_arm_inline')) == '82026468696768',
    'constrained_arm_inline(High) byte cross-check',
  );
  _check(
    hex(fieldBytes(rec(
      mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
      pureLiteralInline: 'active',
      constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant0('orange'),
    ), 'constrained_arm_inline')) == '8200666f72616e6765',
    'constrained_arm_inline(Other orange) byte cross-check',
  );

  // --- nested positions: no OCaml codec to cross-check (see torture_rules'
  // doc comment / the task's ground-truth file) — hand-derived expected bytes
  // from the same wire contract (bare literal / [idx,value] tagged sum), plus a
  // full encode -> decode -> re-encode self-consistency round-trip. ---
  final full = TortureInlineChoice(
    mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
    pureLiteralInline: 'active',
    constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    tagList: [TortureInlineChoiceTagListItemVariant1('red')],
    labelMap: {'k': TortureInlineChoiceLabelMapValueVariant1('urgent')},
    coord: (TortureInlineChoiceCoord0Variant1('lat'), 42),
  );
  _check(
    hex(fieldBytes(full, 'tag_list')) == '81820163726564',
    'tag_list (array element, single literal arm) hand-derived bytes',
  );
  _check(
    hex(fieldBytes(full, 'label_map')) == 'a1616b820166757267656e74',
    'label_map (map value, single literal arm) hand-derived bytes',
  );
  _check(
    hex(fieldBytes(full, 'coord')) == '828201636c6174182a',
    'coord (tuple element, literal arm + int) hand-derived bytes',
  );

  final bytes1 = full.toCbor();
  final back = TortureInlineChoice.fromCbor(bytes1);
  final bytes2 = back.toCbor();
  _check(hex(bytes1) == hex(bytes2), 'full record encode -> decode -> re-encode is byte-identical');
  _check(back.tagList.length == 1 && back.tagList[0] is TortureInlineChoiceTagListItemVariant1,
      'tag_list round-trips its literal arm');
  _check(
      back.labelMap['k'] is TortureInlineChoiceLabelMapValueVariant1,
      'label_map round-trips its literal arm');
  _check(back.coord.$1 is TortureInlineChoiceCoord0Variant1 && back.coord.$2 == 42,
      'coord round-trips its literal arm and int slot');

  // A mix of literal AND open arms in the same array/map, round-tripped.
  final mixedList = TortureInlineChoice(
    mixedInline: TortureInlineChoiceMixedInlineVariant1('not_found'),
    pureLiteralInline: 'active',
    constrainedArmInline: TortureInlineChoiceConstrainedArmInlineVariant1('low'),
    tagList: [
      TortureInlineChoiceTagListItemVariant1('red'),
      TortureInlineChoiceTagListItemVariant0('purple'),
      TortureInlineChoiceTagListItemVariant3('blue'),
    ],
    labelMap: {
      'a': TortureInlineChoiceLabelMapValueVariant1('urgent'),
      'b': TortureInlineChoiceLabelMapValueVariant0('meh'),
    },
    coord: (TortureInlineChoiceCoord0Variant0('nowhere'), -3),
  );
  final mixedBack = TortureInlineChoice.fromCbor(mixedList.toCbor());
  _check(mixedBack.tagList.length == 3, 'mixed tag_list length round-trips');
  _check(
      mixedBack.tagList[1] is TortureInlineChoiceTagListItemVariant0 &&
          (mixedBack.tagList[1] as TortureInlineChoiceTagListItemVariant0).value ==
              'purple',
      'mixed tag_list open arm round-trips its value');
  _check(mixedBack.labelMap.length == 2, 'mixed label_map length round-trips');
  _check(
      (mixedBack.labelMap['b'] as TortureInlineChoiceLabelMapValueVariant0)
              .value ==
          'meh',
      'mixed label_map open arm round-trips its value');
  _check(
      (mixedBack.coord.$1 as TortureInlineChoiceCoord0Variant0).value ==
              'nowhere' &&
          mixedBack.coord.$2 == -3,
      'mixed coord open arm and negative int round-trip');

  // --- decode-time validation ---
  // Membership validation for the all-literal (bare-wire) shape: a value outside
  // the closed set must throw on decode.
  final badLiteral = CsilCbor.encodeValue(<String, Object?>{
    'mixed_inline': <Object?>[1, 'not_found'],
    'pure_literal_inline': 'bogus',
    'constrained_arm_inline': <Object?>[1, 'low'],
    'tag_list': <Object?>[],
    'label_map': <String, Object?>{},
    'coord': <Object?>[
      <Object?>[1, 'lat'],
      0,
    ],
  });
  var threwMembership = false;
  try {
    TortureInlineChoice.fromCbor(badLiteral);
  } catch (_) {
    threwMembership = true;
  }
  _check(threwMembership, 'unknown pure_literal_inline value must throw on decode');

  // Literal-arm payload validation for the tagged-sum shape: index 1 claims
  // "not_found" but the payload says something else.
  final badUnion = CsilCbor.encodeValue(<String, Object?>{
    'mixed_inline': <Object?>[1, 'permission_denied'],
    'pure_literal_inline': 'active',
    'constrained_arm_inline': <Object?>[1, 'low'],
    'tag_list': <Object?>[],
    'label_map': <String, Object?>{},
    'coord': <Object?>[
      <Object?>[1, 'lat'],
      0,
    ],
  });
  var threwUnion = false;
  try {
    TortureInlineChoice.fromCbor(badUnion);
  } catch (_) {
    threwUnion = true;
  }
  _check(threwUnion, 'literal-arm payload mismatch must throw on decode');

  print('ok');
}
"#;

// ---------------------------------------------------------------------------
// Mixed-kind all-literal choice (`"a" / 1`) — the shared csilgen_common
// choice/hoist migration. `csilgen_common::classify_choice` treats every arm
// being a literal, of ANY kind (even mixed), as an `Enum`; before the
// migration, `is_string_choice(cs) || is_int_choice(cs)` was used as the
// enum-vs-union PROXY, and both return `false` for a mixed-kind choice, so it
// fell through to the union path (hoisted + rendered as a `sealed class` with
// one wrapper arm per literal) — the wrong wire shape. These tests prove the
// fix end to end: classification, generation, and (via the Dart VM) the wire.
// ---------------------------------------------------------------------------

/// An inline mixed-kind choice field (`"urgent" / 1`) must stay inline — never
/// hoisted, since `hoist_all_literal_choices: false` covers ANY all-literal
/// choice regardless of kind uniformity — and map to `Object?`, the only static
/// Dart type that fits either a `String` or an `int` member.
#[test]
fn mixed_kind_inline_choice_field_stays_inline_as_object_type() {
    let mut e = entry("priority", builtin("text"), false);
    e.value_type = CsilTypeExpression::Choice(vec![text_lit("urgent"), int_lit(1)]);
    let files = generate_dart_code(
        &spec(vec![record_rule("Ticket", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final Object? priority;"),
        "mixed-kind inline choice field maps to Object?, not a hoisted sealed union: {code}"
    );
    assert!(
        !code.contains("sealed class"),
        "a mixed-kind all-literal choice must never hoist to a sealed union: {code}"
    );
    assert!(
        !code.contains("TicketPriority"),
        "a mixed-kind all-literal choice must stay inline, not synthesize a named type: {code}"
    );
}

/// The named-rule twin: a top-level `Status = "pending" / 42` (declared
/// separately, not inlined) must become a bare `Object?` alias — never a
/// `sealed class` — and a field referencing it must decode through
/// `CsilCbor.expectOneOf<Object?>`, folded in via `codec_aliases` exactly like
/// `hoisted_enum_reference_field_gets_membership_check_on_decode` already
/// proves for the uniform-kind case, so an out-of-set value is still rejected
/// on decode rather than silently passing through.
#[test]
fn mixed_kind_literal_choice_named_rule_becomes_object_typedef_with_membership_check() {
    let rules = vec![
        CsilRule {
            name: "Status".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![text_lit("pending"), int_lit(42)]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        record_rule("Job", vec![entry("status", reference("Status"), false)]),
    ];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("typedef Status = Object?;"),
        "a mixed-kind named choice becomes a bare Object? alias: {code}"
    );
    assert!(
        !code.contains("sealed class Status"),
        "a mixed-kind named choice must not become a sealed union: {code}"
    );
    assert!(
        code.contains("CsilCbor.expectOneOf<Object?>(map['status']"),
        "a field referencing a named mixed-kind choice must decode through expectOneOf<Object?>: {code}"
    );
}

/// Drives the real Dart VM over a mixed-kind all-literal choice (`"pending" /
/// 42`), both as (a) an inline field (`Job.tag`, proving it stays a bare
/// `Object?`, never a hoisted sealed union) and (b) a named top-level rule
/// referenced by a field (`Job.status: Status`, proving the `typedef ... =
/// Object?;` + `codec_aliases` + `expectOneOf<Object?>` decode path): both
/// member kinds round-trip byte-identical in both positions, and an
/// out-of-set value of either kind is rejected on decode with the codec's
/// standard `ArgumentError('CsilCbor: value not a member of the closed set')`
/// — the same error uniform-kind enums already raise (see
/// `enum_field_decode_rejects_out_of_set_value_through_dart`). Skips when
/// `dart` is not on PATH.
#[test]
fn mixed_kind_enum_round_trips_and_rejects_out_of_set_through_dart() {
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let mut tag_entry = entry("tag", builtin("text"), false);
    tag_entry.value_type = CsilTypeExpression::Choice(vec![text_lit("blue"), int_lit(7)]);
    let rules = vec![
        CsilRule {
            name: "Status".to_string(),
            rule_type: CsilRuleType::TypeChoice(vec![text_lit("pending"), int_lit(42)]),
            position: pos(),
            doc_comments: Vec::new(),
        },
        record_rule(
            "Job",
            vec![entry("status", reference("Status"), false), tag_entry],
        ),
    ];
    let files = generate_dart_code(&spec(rules, 0), &config("dart")).unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-mixedenum-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    std::fs::write(dir.join("driver.dart"), MIXED_ENUM_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "dart mixed-kind-enum probe failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

const MIXED_ENUM_DRIVER_DART: &str = r#"import 'models.gen.dart';

void _check(bool ok, String what) {
  if (!ok) throw StateError('check failed: $what');
}

void main() {
  // Both member kinds round-trip byte-identical, in both the named-reference
  // field (status) and the inline field (tag). tag's closed set is 'blue' / 7.
  final a = Job(status: 'pending', tag: 7);
  final aBack = Job.fromCbor(a.toCbor());
  _check(aBack.status == 'pending' && aBack.tag == 7, 'text status / int tag round-trip');

  final b = Job(status: 42, tag: 'blue');
  final bBack = Job.fromCbor(b.toCbor());
  _check(bBack.status == 42 && bBack.tag == 'blue', 'int status / text tag round-trip');

  // An out-of-set string must be rejected with the codec's standard error, for
  // both the named-reference position and the inline position. `tag` is given
  // a genuinely valid member (7) each time so the exception is unambiguously
  // attributable to the field under test, not a side effect of the other one.
  try {
    Job.fromCborValue({'status': 'bogus', 'tag': 7});
    throw StateError('out-of-set status string was accepted');
  } on ArgumentError catch (e) {
    _check(
      e.toString().contains('not a member of the closed set'),
      'status string error shape: $e',
    );
  }
  try {
    Job.fromCborValue({'status': 'pending', 'tag': 'bogus'});
    throw StateError('out-of-set tag string was accepted');
  } on ArgumentError catch (e) {
    _check(
      e.toString().contains('not a member of the closed set'),
      'tag string error shape: $e',
    );
  }

  // An out-of-set int must likewise be rejected, for both positions.
  try {
    Job.fromCborValue({'status': 99, 'tag': 7});
    throw StateError('out-of-set status int was accepted');
  } on ArgumentError catch (e) {
    _check(
      e.toString().contains('not a member of the closed set'),
      'status int error shape: $e',
    );
  }
  try {
    Job.fromCborValue({'status': 'pending', 'tag': 99});
    throw StateError('out-of-set tag int was accepted');
  } on ArgumentError catch (e) {
    _check(
      e.toString().contains('not a member of the closed set'),
      'tag int error shape: $e',
    );
  }

  print('ok');
}
"#;

// ---------------------------------------------------------------------------
// Case-insensitive hoist-name collision (shared `csilgen_common::hoist` module)
// ---------------------------------------------------------------------------

/// Mirrors `crates/csilgen-common/src/hoist.rs`'s
/// `case_insensitive_collision_between_existing_and_synthesized_rule_is_disambiguated`:
/// an existing `UserData` record and a `User` record whose `data` field is an
/// inline mixed-choice UNION (a text-literal arm plus a `Reference` arm — NOT
/// an all-literal enum, so it genuinely needs hoisting) synthesize `User_data`,
/// which pascal-collides with `UserData` (`dart_type_name` strips the
/// underscore, so both canonicalize to `UserData`). The shared hoister's
/// case-insensitive collision reservation must disambiguate the synthesized
/// name, or the generator would emit two Dart type declarations for the same
/// identifier — non-compiling output. Regression coverage at the Dart-generator
/// level for the same finding the shared module's own test guards.
#[test]
fn case_insensitive_collision_between_existing_and_synthesized_rule_is_disambiguated() {
    let user_data = record_rule("UserData", vec![entry("value", builtin("text"), false)]);
    let mut data_field = entry("data", builtin("text"), false);
    data_field.value_type = CsilTypeExpression::Choice(vec![text_lit("x"), reference("UserData")]);
    let user = record_rule("User", vec![data_field]);
    let files = generate_dart_code(&spec(vec![user_data, user], 0), &config("dart")).unwrap();
    let code = types_file(&files);

    // The original UserData record survives unchanged.
    assert!(
        code.contains("final class UserData {"),
        "the original UserData record must survive: {code}"
    );
    // Exactly one Dart type declaration for the identifier `UserData` — the
    // synthesized union's name must have been disambiguated away from it, not
    // collide and produce a second (non-compiling) declaration.
    let user_data_decls = code.matches("class UserData {").count();
    assert_eq!(
        user_data_decls, 1,
        "synthesized hoist name collided with UserData and was not disambiguated: {code}"
    );
}
