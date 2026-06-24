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
    assert!(
        code.contains("const User({"),
        "missing const constructor: {code}"
    );
    assert!(
        code.contains("required this.userName,"),
        "required named param: {code}"
    );
    assert!(
        code.contains("this.age,"),
        "optional param not required: {code}"
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
fn client_subtarget_emits_typed_client_with_verbatim_wire_names() {
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
    let rules = vec![CsilRule {
        name: "AttestationService".to_string(),
        rule_type: CsilRuleType::ServiceDef(service),
        position: pos(),
        doc_comments: Vec::new(),
    }];
    let files = generate_dart_code(&spec(rules, 1), &config("dart-client")).unwrap();
    let code = services_file(&files);

    assert!(
        code.contains("final class AttestationClient {"),
        "client class: {code}"
    );
    // kebab-case op -> lowerCamelCase Dart method; wire op string stays verbatim.
    assert!(
        code.contains("DepositClaimResponse depositClaim(DepositClaimRequest request)"),
        "method signature: {code}"
    );
    assert!(
        code.contains("transport.call('Attestation', 'deposit-claim', request)"),
        "verbatim wire service+op: {code}"
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
        "verbose dispatch by wire name: {code}"
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

#[test]
fn string_literal_choice_becomes_string_typedef_not_sealed() {
    // `Status = text / "open" / "closed"` is a closed string set: idiomatic Dart is
    // a `String` alias, and the bare string round-trips — a sealed wrapper wouldn't.
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
        code.contains("typedef Status = String;"),
        "string choice -> typedef: {code}"
    );
    assert!(
        !code.contains("sealed class Status"),
        "no sealed wrapper for a string set: {code}"
    );
}

#[test]
fn inline_string_choice_field_maps_to_string() {
    let mut e = entry("note", builtin("text"), true);
    e.value_type = CsilTypeExpression::Choice(vec![builtin("text"), text_lit("vip")]);
    let files = generate_dart_code(
        &spec(vec![record_rule("Acct", vec![e])], 0),
        &config("dart"),
    )
    .unwrap();
    let code = types_file(&files);
    assert!(
        code.contains("final String? note;"),
        "inline string choice -> String?: {code}"
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
        code.contains("details: map['details'],"),
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
        code.contains("id: map['id'] as String,"),
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
