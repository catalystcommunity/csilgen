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
    // Typed seam + canonical wire strings: service is lowercased (`attestation`) and
    // the op PascalCased (`DepositClaim`), matching the Go/Python/TS peers.
    assert!(
        code.contains("transport.call('attestation', 'DepositClaim', request.toCbor())"),
        "canonical wire strings: {code}"
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
        code.contains("case 'Chat':"),
        "verbose dispatch by PascalCased wire op: {code}"
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

#[test]
fn wire_service_is_lowercased_and_op_pascalcased() {
    // `CorndogsService` -> wire service "corndogs" (strip Service + lowercase);
    // `submit-task` -> wire op "SubmitTask" (the wire contract), so a Dart client
    // hits the same route as the Go/Python/TS peers.
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
    assert!(
        code.contains("transport.call('corndogs', 'SubmitTask',"),
        "wire service lowercased + op PascalCased: {code}"
    );
    assert!(
        !code.contains("'Corndogs'"),
        "service must be lowercased: {code}"
    );
    assert!(
        !code.contains("'submit-task'"),
        "op must be PascalCased: {code}"
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
        readme.contains(r"Hello([version], ['verbose'], service: 'demo')"),
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
        readme.contains("Hello([version], ['verbose'], service: 'demo')"),
        "events handshake names the service: {readme}"
    );
    assert!(
        readme.contains("Control.pingName") && readme.contains("Control.pongName"),
        "events answers the $ping/$pong heartbeat: {readme}"
    );
    assert!(
        readme.contains("Event.verbose('demo', 'Chat', outbound.toCbor())"),
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
