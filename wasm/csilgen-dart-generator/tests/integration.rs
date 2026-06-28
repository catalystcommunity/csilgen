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
        with_readme.iter().any(|f| f.path == "README.md"),
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
        !without_readme.iter().any(|f| f.path == "README.md"),
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

    // Drop the README's Quickstart carrier (the real shipped artifact) into lib/ so
    // `dart analyze` proves it — and its sample example call — compile against the
    // generated codec + client. The `package:` self-import is rewritten to a relative
    // one so analysis resolves it even on the no-cache fallback path below.
    let readme = files.iter().find(|f| f.path == "README.md").unwrap();
    let carrier = extract_dart_block(&readme.content)
        .replace("package:csil_sample/csil_sample.dart", "csil_sample.dart");
    std::fs::write(dir.join("lib/quickstart_carrier.dart"), carrier).unwrap();

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

// --- README Quickstart carrier ---------------------------------------------

/// The body of the README's first fenced ```dart block — the shipped Quickstart.
fn extract_dart_block(readme: &str) -> String {
    const FENCE: &str = "```dart\n";
    let start = readme.find(FENCE).expect("README has a dart code block") + FENCE.len();
    let rest = &readme[start..];
    let end = rest.find("```").expect("dart code block is closed");
    rest[..end].to_string()
}

/// A ping/pong spec whose op echoes its request type as its response type, so the
/// echo server's tag-24 payload decodes cleanly back into the typed result.
fn pingpong_rules() -> Vec<CsilRule> {
    vec![
        record_rule(
            "Ping",
            vec![
                entry("message", builtin("text"), false),
                entry("nonce", builtin("int"), false),
            ],
        ),
        CsilRule {
            name: "EchoService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![CsilServiceOperation {
                    name: "ping".to_string(),
                    input_type: reference("Ping"),
                    output_type: reference("Ping"),
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

#[test]
fn package_readme_has_quickstart_carrier_and_example() {
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
    let readme = &files
        .iter()
        .find(|f| f.path == "README.md")
        .expect("README.md emitted in package mode")
        .content;

    // The carrier implements the generated async transport seam.
    assert!(
        readme.contains("class CsilRpcTransport implements AsyncCsilTransport {"),
        "carrier implements the transport seam: {readme}"
    );
    // It POSTs the CSIL-RPC envelope to the canonical path.
    assert!(
        readme.contains("/csil/v1/rpc"),
        "carrier posts to /csil/v1/rpc: {readme}"
    );
    // The payload is wrapped in CBOR tag 24 (0xd8 0x18) — the embedded-CBOR head.
    assert!(
        readme.contains("0xd8") && readme.contains("0x18"),
        "payload wrapped in CBOR tag 24: {readme}"
    );
    // Transport-status and typed ServiceError arms are both handled.
    assert!(
        readme.contains("transport status") && readme.contains("ServiceError"),
        "status / ServiceError handling: {readme}"
    );
    // It reuses the package's own codec for the envelope — no third-party dep.
    assert!(
        readme.contains("CsilCbor.encodeValue") && readme.contains("CsilCbor.decode"),
        "envelope reuses the generated CsilCbor codec: {readme}"
    );
    // The example call constructs the async client and calls the first op with a
    // generated sample literal.
    assert!(
        readme.contains("final client = CorndogsAsyncClient(CsilRpcTransport("),
        "example constructs the typed client: {readme}"
    );
    assert!(
        readme.contains("await client.submitTask(SubmitTaskRequest("),
        "example calls the first unary op with a sample literal: {readme}"
    );
    assert!(
        readme.contains("import 'package:csil_sample/csil_sample.dart';"),
        "example imports the package barrel: {readme}"
    );
}

/// Hermetically round-trips the shipped README carrier: it injects an in-process
/// echo `sender` (no socket — the sandbox kills cross-process loopback), so the real
/// carrier's envelope encode/decode and the typed client are exercised end to end.
/// Skips when `dart` is not on PATH so the suite stays portable.
#[test]
fn readme_quickstart_carrier_round_trips_through_an_injected_echo() {
    let have = std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok();
    if !have {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let files = generate_dart_code(
        &spec(pingpong_rules(), 1),
        &config_with(
            "dart-client",
            &[
                ("emit_packages", serde_json::json!(["dart"])),
                ("package_name", serde_json::json!("csil_echo")),
            ],
        ),
    )
    .unwrap();

    let dir = std::env::temp_dir().join(format!("csilgen-dart-readme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &f.content).unwrap();
    }

    // The carrier only (the README block up to its example `main`), with the package
    // self-import rewritten relative so the driver resolves it without pub.
    let readme = files.iter().find(|f| f.path == "README.md").unwrap();
    let block = extract_dart_block(&readme.content);
    let carrier = block[..block.find("Future<void> main").unwrap()]
        .replace("package:csil_echo/csil_echo.dart", "csil_echo.dart");
    std::fs::write(dir.join("lib/quickstart_carrier.dart"), carrier).unwrap();
    std::fs::write(dir.join("driver.dart"), README_ECHO_DRIVER_DART).unwrap();

    let run = std::process::Command::new("dart")
        .arg(dir.join("driver.dart"))
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "readme carrier round-trip failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "ok");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Drives the shipped carrier with an injected sender that echoes the tag-24 inner
/// payload back in a `status: 0` CsilRpcResponse — the same contract the shared echo
/// mock implements, but in-process so no socket is opened.
const README_ECHO_DRIVER_DART: &str = r#"import 'dart:typed_data';

import 'lib/csil_echo.dart';
import 'lib/quickstart_carrier.dart';

Future<Uint8List> echo(Uri uri, Uint8List body) async {
  // Unwrap the request envelope's tag-24 payload (CsilCbor.decode does this for us).
  final reqEnv = CsilCbor.decode(body) as Map;
  final inner = reqEnv['payload'] as Uint8List;
  // Build CsilRpcResponse = { v: 1, status: 0, payload: #6.24(inner) }.
  final b = BytesBuilder();
  b.addByte(0xa3);
  b.add(CsilCbor.encodeValue('v'));
  b.add(CsilCbor.encodeValue(1));
  b.add(CsilCbor.encodeValue('status'));
  b.add(CsilCbor.encodeValue(0));
  b.add(CsilCbor.encodeValue('payload'));
  b.addByte(0xd8);
  b.addByte(0x18);
  b.add(CsilCbor.encodeValue(inner));
  return b.toBytes();
}

Future<void> main() async {
  final client = EchoAsyncClient(CsilRpcTransport('http://unused', sender: echo));
  final resp = await client.ping(Ping(message: 'hi', nonce: 7));
  if (resp.message != 'hi' || resp.nonce != 7) {
    throw StateError('round-trip mismatch: ${resp.message}/${resp.nonce}');
  }
  print('ok');
}
"#;
