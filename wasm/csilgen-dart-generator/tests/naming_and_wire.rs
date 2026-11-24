//! Derived-name renames, op-boundary unions, service name clashes, canonical map
//! order and the formatter page width. Each test that runs Dart skips when `dart`
//! is not on PATH.

use csilgen_common::*;
use csilgen_dart_generator::generate_dart_output;
use std::collections::HashMap;

fn config(target: &str) -> GeneratorConfig {
    config_with(target, &[])
}

fn config_with(target: &str, opts: &[(&str, serde_json::Value)]) -> GeneratorConfig {
    GeneratorConfig {
        target: target.to_string(),
        output_dir: "/tmp".to_string(),
        options: opts
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect::<HashMap<_, _>>(),
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

fn entry(key: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(key.to_string())),
        value_type,
        occurrence: None,
        metadata: vec![],
        doc_comments: Vec::new(),
    }
}

fn rule(name: &str, rule_type: CsilRuleType) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type,
        position: pos(),
        doc_comments: Vec::new(),
    }
}

fn record(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    rule(
        name,
        CsilRuleType::GroupDef(CsilGroupExpression { entries }),
    )
}

fn alias(name: &str, target: CsilTypeExpression) -> CsilRule {
    rule(name, CsilRuleType::TypeDef(target))
}

fn service(name: &str, ops: Vec<(&str, CsilTypeExpression, CsilTypeExpression)>) -> CsilRule {
    rule(
        name,
        CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: ops
                .into_iter()
                .map(|(op, input_type, output_type)| CsilServiceOperation {
                    name: op.to_string(),
                    input_type,
                    output_type,
                    direction: CsilServiceDirection::Unidirectional,
                    position: pos(),
                    doc_comments: Vec::new(),
                    wire_id: None,
                })
                .collect(),
            wire_id: None,
        }),
    )
}

fn spec(rules: Vec<CsilRule>) -> CsilSpecSerialized {
    let service_count = rules
        .iter()
        .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
        .count();
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count,
        fields_with_metadata_count: 0,
        declared_names: Default::default(),
    }
}

fn text_map() -> CsilTypeExpression {
    CsilTypeExpression::Map {
        key: Box::new(builtin("text")),
        value: Box::new(builtin("int")),
        occurrence: None,
    }
}

fn file<'a>(files: &'a [GeneratedFile], path: &str) -> &'a str {
    &files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} should be generated"))
        .content
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

fn have_dart() -> bool {
    std::process::Command::new("dart")
        .arg("--version")
        .output()
        .is_ok()
}

/// Write `files` (plus `driver.dart` when given) to a fresh directory, run
/// `dart analyze`, and then run the driver. Returns the driver's stdout.
fn check_with_dart(name: &str, files: &[GeneratedFile], driver: Option<&str>) -> String {
    let dir = std::env::temp_dir().join(format!("csilgen-dart-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &f.content).unwrap();
    }
    if let Some(driver) = driver {
        std::fs::write(dir.join("driver.dart"), driver).unwrap();
    }
    let analyze = std::process::Command::new("dart")
        .args(["analyze", "--no-fatal-warnings"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        analyze.status.success(),
        "dart analyze failed:\n{}{}",
        String::from_utf8_lossy(&analyze.stdout),
        String::from_utf8_lossy(&analyze.stderr)
    );
    let mut stdout = String::new();
    if driver.is_some() {
        let run = std::process::Command::new("dart")
            .arg("--enable-asserts")
            .arg(dir.join("driver.dart"))
            .output()
            .unwrap();
        stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        assert!(
            run.status.success(),
            "dart run failed:\n{stdout}{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    stdout
}

/// linkkeys' `Rp` service: each record is the input or output of the operation
/// whose name it matches. Dart derives no per-operation names, so nothing moves.
#[test]
fn operation_records_keep_their_names() {
    let rules = vec![
        record(
            "RpIssueAttestationRequest",
            vec![entry("claim_type", builtin("text"))],
        ),
        record(
            "RpIssueAttestationResponse",
            vec![entry("deposited", builtin("bool"))],
        ),
        service(
            "Rp",
            vec![(
                "issue-attestation",
                reference("RpIssueAttestationRequest"),
                reference("RpIssueAttestationResponse"),
            )],
        ),
    ];
    for target in ["dart", "dart-client"] {
        let (files, warnings) =
            generate_dart_output(&spec(rules.clone()), &config(target)).unwrap();
        assert!(warnings.is_empty(), "{target}: {warnings:?}");
        let types = file(&files, "types.gen.dart");
        assert_eq!(
            count(types, "final class RpIssueAttestationRequest {"),
            1,
            "{types}"
        );
        assert_eq!(
            count(types, "final class RpIssueAttestationResponse {"),
            1,
            "{types}"
        );
    }
}

/// The request's second schema. The Rust generator emits
/// `encode_member_get_member_request` for the `MemberID` boundary; the Dart
/// generator emits no class named after the operation, so the record has no
/// clash in Dart and keeps its name.
#[test]
fn operation_named_rule_without_a_dart_clash_keeps_its_name() {
    let rules = vec![
        alias("MemberID", builtin("text")),
        record("MemberGetMemberRequest", vec![entry("id", builtin("text"))]),
        service(
            "Member",
            vec![("get-member", reference("MemberID"), reference("MemberID"))],
        ),
    ];
    let (files, warnings) = generate_dart_output(&spec(rules), &config("dart-client")).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(file(&files, "types.gen.dart").contains("final class MemberGetMemberRequest {"));
}

/// A client class is reserved only for a target that emits it, and a handler
/// interface only for a target that emits the server.
#[test]
fn derived_class_names_are_reserved_only_where_emitted() {
    let rules = |name: &str| {
        vec![
            record(name, vec![entry("a", builtin("text"))]),
            service("Signer", vec![("sign", builtin("text"), builtin("text"))]),
        ]
    };
    let renamed = |name: &str, target: &str, opts: &[(&str, serde_json::Value)]| {
        let (files, warnings) =
            generate_dart_output(&spec(rules(name)), &config_with(target, opts)).unwrap();
        let types = files
            .iter()
            .find(|f| f.path.ends_with("types.gen.dart"))
            .expect("types.gen.dart should be generated")
            .content
            .clone();
        let moved = types.contains(&format!("final class {name}Type {{"));
        assert_eq!(moved, !warnings.is_empty(), "{warnings:?}");
        moved
    };
    assert!(renamed("SignerClient", "dart-client", &[]));
    assert!(!renamed("SignerClient", "dart", &[]));
    assert!(renamed("SignerHandler", "dart", &[]));
    assert!(!renamed("SignerHandler", "dart-client", &[]));
    assert!(renamed("SignerAsyncClient", "dart-client", &[]));
    let sync = [("client_style", serde_json::json!("sync"))];
    assert!(!renamed("SignerAsyncClient", "dart-client", &sync));
    // A package carries both surfaces.
    let pkg = [("emit_packages", serde_json::json!(["dart"]))];
    assert!(renamed("SignerClient", "dart", &pkg));
}

/// A mixed op output (`{ * text => int } / Err`) is one named union with both arms,
/// in the handler and in the client.
#[test]
fn mixed_op_output_is_the_whole_union() {
    let rules = vec![
        record("Holder", vec![entry("counts", text_map())]),
        record("Err", vec![entry("message", builtin("text"))]),
        service(
            "Lookup",
            vec![(
                "read-counts",
                reference("Holder"),
                CsilTypeExpression::Choice(vec![text_map(), reference("Err")]),
            )],
        ),
    ];
    let pkg = config_with(
        "dart",
        &[
            ("emit_packages", serde_json::json!(["dart"])),
            ("emit_readme", serde_json::json!(false)),
        ],
    );
    let (files, warnings) = generate_dart_output(&spec(rules), &pkg).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let types = file(&files, "lib/types.gen.dart");
    assert!(
        types.contains("sealed class LookupReadCountsOutput {"),
        "{types}"
    );
    assert!(
        types.contains(
            "final class LookupReadCountsOutputVariant0 extends LookupReadCountsOutput {"
        ),
        "{types}"
    );
    assert!(
        types.contains("final class LookupReadCountsOutputErr extends LookupReadCountsOutput {"),
        "{types}"
    );
    let server = file(&files, "lib/services.gen.dart");
    assert!(
        server.contains("  LookupReadCountsOutput readCounts(Holder request);"),
        "{server}"
    );
    let client = file(&files, "lib/client.gen.dart");
    assert!(
        client.contains("  LookupReadCountsOutput readCounts(Holder request) {"),
        "{client}"
    );
    assert!(
        client.contains("LookupReadCountsOutput.fromCborValue(CsilCbor.decode(csilResp))"),
        "{client}"
    );
    if have_dart() {
        check_with_dart("mixed-output", &files, None);
    }
}

/// `UserSvc` and `user-svc` are both `UserSvc` in Dart. The later service gets
/// other class names, and each keeps its declared name on the wire.
#[test]
fn clashing_service_identifiers_keep_their_wire_names() {
    let rules = vec![
        service("UserSvc", vec![("get", builtin("text"), builtin("text"))]),
        service("user-svc", vec![("put", builtin("text"), builtin("text"))]),
    ];
    let pkg = config_with(
        "dart",
        &[
            ("emit_packages", serde_json::json!(["dart"])),
            ("emit_readme", serde_json::json!(false)),
        ],
    );
    let (files, warnings) = generate_dart_output(&spec(rules), &pkg).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .message
            .starts_with("service `user-svc` is generated as `user-svc2`"),
        "{warnings:?}"
    );
    let client = file(&files, "lib/client.gen.dart");
    assert!(client.contains("final class UserSvcClient {"), "{client}");
    assert!(client.contains("final class UserSvc2Client {"), "{client}");
    assert!(
        client.contains("transport.call(\n      'UserSvc',\n      'get',\n"),
        "{client}"
    );
    assert!(
        client.contains("transport.call(\n      'user-svc',\n      'put',\n"),
        "{client}"
    );
    assert!(!client.contains("'user-svc2'"), "{client}");
    let server = file(&files, "lib/services.gen.dart");
    assert!(server.contains("abstract interface class UserSvcHandler {"));
    assert!(server.contains("abstract interface class UserSvc2Handler {"));
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let driver = r#"import 'lib/models.dart';

final class Recorder implements CsilTransport {
  final calls = <String>[];

  @override
  List<int> call(String service, String op, List<int> request) {
    calls.add('$service/$op');
    return CsilCbor.encodeValue('ok');
  }
}

void main() {
  final t = Recorder();
  UserSvcClient(t).get_('a');
  UserSvc2Client(t).put('b');
  print(t.calls.join(' '));
}
"#;
    let out = check_with_dart("service-clash", &files, Some(driver));
    assert!(out.starts_with("UserSvc/get user-svc/put"), "{out}");
}

/// A name the hoister synthesizes (`FooRecord_client`, the Dart class
/// `FooRecordClient`) that equals a derived class (the client of service
/// `FooRecordService`) is renamed, with a warning; the service keeps its client.
#[test]
fn synthesized_rule_names_are_checked_against_derived_names() {
    let inline = CsilTypeExpression::Group(CsilGroupExpression {
        entries: vec![entry("x", builtin("int"))],
    });
    let rules = vec![
        record("FooRecord", vec![entry("client", inline)]),
        service(
            "FooRecordService",
            vec![("get", reference("FooRecord"), reference("FooRecord"))],
        ),
    ];
    let (files, warnings) = generate_dart_output(&spec(rules), &config("dart-client")).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .message
            .starts_with("rule `FooRecord_client` is generated as `FooRecord_clientType`"),
        "{warnings:?}"
    );
    let types = file(&files, "types.gen.dart");
    let services = file(&files, "services.gen.dart");
    assert!(!types.contains("final class FooRecordClient {"), "{types}");
    assert!(
        types.contains("final class FooRecordClientType {"),
        "{types}"
    );
    assert!(
        types.contains("final FooRecordClientType client;"),
        "{types}"
    );
    assert!(
        services.contains("final class FooRecordClient {"),
        "{services}"
    );
    if have_dart() {
        check_with_dart("hoist-clash", &files, None);
    }
}

/// A dynamic map is written in the bytewise order of its encoded keys, and the
/// decoder refuses one out of that order or with a key twice. A record accepts
/// its fields in any order but not a field twice.
#[test]
fn dynamic_maps_are_canonical_under_dart() {
    let rules = vec![
        alias("Counts", text_map()),
        record(
            "Holder",
            vec![
                entry("counts", reference("Counts")),
                entry("n", builtin("int")),
            ],
        ),
    ];
    let (files, _) = generate_dart_output(&spec(rules), &config("dart")).unwrap();
    let types = file(&files, "types.gen.dart");
    assert!(types.contains("CsilCbor.sortedMap(counts)"), "{types}");
    assert!(types.contains("CsilCbor.mapOf("), "{types}");
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let driver = r#"import 'types.gen.dart';

String hex(List<int> b) =>
    b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();

List<int> unhex(String s) => [
  for (var i = 0; i < s.length; i += 2)
    int.parse(s.substring(i, i + 2), radix: 16),
];

bool refused(String input) {
  try {
    Holder.fromCbor(unhex(input));
    return false;
  } on FormatException {
    return true;
  }
}

void main() {
  print(hex(Holder(counts: {'aa': 2, 'b': 1}, n: 0).toCbor()));
  print(refused('a266636f756e7473a262616102616201616e00'));
  print(refused('a266636f756e7473a2616101616102616e00'));
  print(refused('a3616e0066636f756e7473a0616e00'));
  print(hex(Holder.fromCbor(unhex('a2616e0066636f756e7473a0')).toCbor()));
}
"#;
    let out = check_with_dart("canonical-maps", &files, Some(driver));
    assert_eq!(
        out,
        "a266636f756e7473a26162016261610261 6e00\ntrue\ntrue\ntrue\na266636f756e7473a0616e00\n"
            .replace(' ', ""),
    );
}

/// The stress shapes of `tests/interop/formatting/stress.csil` that depend on name
/// length. The formatter check runs `dart format`; this test only measures widths.
#[test]
fn no_emitted_line_is_wider_than_the_page() {
    let long_record = "ExtraordinarilyLongRecordNameForFormatterChecksAcrossEveryTargetLanguage";
    let long_enum = "ConsentRowPolicyForApplicationsThatRequestVeryLongNamesEverywhere";
    let long_service = "ServiceWithAnExceedinglyLongNameUsedOnlyForFormatterChecks";
    let enum_rule = rule(
        long_enum,
        CsilRuleType::TypeDef(CsilTypeExpression::Choice(
            [
                "allow-once-for-this-session-only-and-ask-again-next-time",
                "deny-and-remember-this-decision-for-future-requests-from-this-origin",
            ]
            .iter()
            .map(|s| CsilTypeExpression::Literal(CsilLiteralValue::Text(s.to_string())))
            .collect(),
        )),
    );
    let rules = vec![
        enum_rule,
        record("ShortRecord", vec![entry("a", builtin("text"))]),
        record(
            "ErrorForLongOperations",
            vec![entry("message", builtin("text"))],
        ),
        record(
            long_record,
            vec![
                entry(
                    "this_field_name_is_long_enough_to_push_any_signature_past_the_line_limit",
                    reference(long_enum),
                ),
                entry(
                    "map_from_identifier_to_holder_with_a_very_long_field_name",
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(reference("ShortRecord")),
                        occurrence: None,
                    },
                ),
                entry(
                    "list_of_extraordinarily_long_record_references_in_a_field",
                    CsilTypeExpression::Array {
                        element_type: Box::new(reference("ShortRecord")),
                        occurrence: None,
                    },
                ),
                entry(
                    "timestamp_of_the_last_modification_made_by_any_user",
                    builtin("timestamp"),
                ),
            ],
        ),
        service(
            long_service,
            vec![
                (
                    "operation-with-an-exceedingly-long-name-that-returns-a-choice",
                    reference(long_record),
                    CsilTypeExpression::Choice(vec![
                        reference(long_record),
                        reference("ErrorForLongOperations"),
                    ]),
                ),
                (
                    "read-mixed-output-with-an-inline-map-arm",
                    reference("ShortRecord"),
                    CsilTypeExpression::Choice(vec![
                        text_map(),
                        reference("ErrorForLongOperations"),
                    ]),
                ),
            ],
        ),
    ];
    let pkg = config_with("dart", &[("emit_packages", serde_json::json!(["dart"]))]);
    let (files, _) = generate_dart_output(&spec(rules), &pkg).unwrap();
    for f in files.iter().filter(|f| f.path.ends_with(".dart")) {
        for line in f.content.lines() {
            // A line may be wider only when one word of it (an identifier or a
            // string literal, which cannot split) overflows the page by itself.
            let indent = line.len() - line.trim_start().len();
            let widest = line.split_whitespace().map(str::len).max().unwrap_or(0);
            let trimmed = line.trim_start();
            // The formatter never splits a lone string literal, a class header, or
            // the head before an opening parenthesis.
            let fixed = trimmed.starts_with('\'')
                || trimmed.ends_with('(')
                || trimmed.ends_with("({")
                || ["final class ", "sealed class ", "abstract interface class "]
                    .iter()
                    .any(|kw| trimmed.starts_with(kw));
            let unbreakable = fixed || indent + widest + 2 > 80;
            assert!(
                line.chars().count() <= 80 || unbreakable,
                "{}: line over 80 columns: {line}",
                f.path
            );
        }
    }
}

/// `Foo` and `FooService` are distinct identifiers, but stripping `Service` would
/// give both the client `FooClient`; the second keeps its suffix.
#[test]
fn service_suffix_stays_when_stripping_it_clashes() {
    let rules = vec![
        service("Foo", vec![("a", builtin("text"), builtin("text"))]),
        service("FooService", vec![("b", builtin("text"), builtin("text"))]),
    ];
    let (files, _) = generate_dart_output(&spec(rules), &config("dart-client")).unwrap();
    let client = file(&files, "services.gen.dart");
    assert!(client.contains("final class FooClient {"), "{client}");
    assert!(
        client.contains("final class FooServiceClient {"),
        "{client}"
    );
}

/// The prelude constants used as types go through the literal codec, and a field
/// named `map` does not read the local map of `toMap`/`toCborValue`.
#[test]
fn prelude_constants_and_a_map_field_compile() {
    let rules = vec![record(
        "Flags",
        vec![
            entry("yes", builtin("true")),
            entry("no", builtin("false")),
            entry("gone", builtin("undefined")),
            entry("map", text_map()),
        ],
    )];
    let (files, _) = generate_dart_output(&spec(rules), &config("dart")).unwrap();
    let types = file(&files, "types.gen.dart");
    assert!(types.contains("  final bool yes;"), "{types}");
    assert!(types.contains("map['yes'] = true;"), "{types}");
    assert!(types.contains("CsilCbor.sortedMap(this.map)"), "{types}");
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let driver = r#"import 'types.gen.dart';

String hex(List<int> b) =>
    b.map((x) => x.toRadixString(16).padLeft(2, '0')).join();

void main() {
  final bytes = Flags(yes: true, no: false, gone: null, map: {'b': 1}).toCbor();
  print(hex(bytes));
  print(Flags.fromCbor(bytes).map['b']);
}
"#;
    let out = check_with_dart("prelude", &files, Some(driver));
    // {yes: true, no: false, gone: null, map: {b: 1}}
    assert_eq!(out, "a463796573f5626e6ff464676f6e65f6636d6170a1616201\n1\n",);
}

/// A map key, in a dynamic map or a record, must use the shortest CBOR head for
/// its value or length.
#[test]
fn map_keys_must_use_the_shortest_head_under_dart() {
    let rules = vec![
        alias(
            "Labels",
            CsilTypeExpression::Map {
                key: Box::new(builtin("int")),
                value: Box::new(builtin("text")),
                occurrence: None,
            },
        ),
        record("Holder", vec![entry("labels", reference("Labels"))]),
    ];
    let (files, _) = generate_dart_output(&spec(rules), &config("dart")).unwrap();
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let driver = r#"import 'types.gen.dart';

List<int> unhex(String s) => [
  for (var i = 0; i < s.length; i += 2)
    int.parse(s.substring(i, i + 2), radix: 16),
];

String outcome(String input) {
  try {
    Holder.fromCbor(unhex(input));
    return 'ok';
  } on FormatException catch (e) {
    return e.message;
  }
}

void main() {
  // {labels: {1: "a"}}
  print(outcome('a1666c6162656c73a1016161'));
  // int key 1 as 18 01
  print(outcome('a1666c6162656c73a118016161'));
  // record key "labels" with a one-byte length head (78 06)
  print(outcome('a178066c6162656c73a1016161'));
  // int key 24 as 18 18 is the shortest form
  print(outcome('a1666c6162656c73a118186161'));
}
"#;
    let out = check_with_dart("shortest-keys", &files, Some(driver));
    assert_eq!(
        out,
        "ok\ncsil cbor: map key is not in shortest form\n\
         csil cbor: map key is not in shortest form\nok\n"
    );
}

/// tests/interop/repo-specs/dotted-enum-values.csil: text enum values that are not
/// identifiers get legal member names, and each value round-trips as written.
#[test]
fn dotted_enum_values_round_trip_under_dart() {
    let values = ["calendars.read", "events.write", "a-b c", "2fa", "class"];
    let rules = vec![
        alias(
            "Scope",
            CsilTypeExpression::Choice(
                values
                    .iter()
                    .map(|v| CsilTypeExpression::Literal(CsilLiteralValue::Text(v.to_string())))
                    .collect(),
            ),
        ),
        record(
            "Grant",
            vec![
                entry("scope", reference("Scope")),
                CsilGroupEntry {
                    occurrence: Some(CsilOccurrence::Optional),
                    ..entry(
                        "extra",
                        CsilTypeExpression::Array {
                            element_type: Box::new(reference("Scope")),
                            occurrence: None,
                        },
                    )
                },
            ],
        ),
    ];
    let (files, _) = generate_dart_output(&spec(rules), &config("dart")).unwrap();
    let types = file(&files, "types.gen.dart");
    for member in [
        "  calendarsRead('calendars.read'),",
        "  eventsWrite('events.write'),",
        "  aBC('a-b c'),",
        "  v2Fa('2fa'),",
        "  vClass('class');",
    ] {
        assert!(types.contains(member), "{member}: {types}");
    }
    if !have_dart() {
        eprintln!("skipping: no dart on PATH");
        return;
    }
    let driver = r#"import 'types.gen.dart';

void main() {
  for (final scope in Scope.values) {
    final grant = Grant(scope: scope, extra: [scope]);
    final again = Grant.fromCbor(grant.toCbor());
    if (again.scope != scope || again.extra!.single != scope) {
      throw StateError('round trip of ${scope.wireValue}');
    }
    print('${scope.name}=${again.scope.wireValue}');
  }
}
"#;
    let out = check_with_dart("dotted-enum", &files, Some(driver));
    assert_eq!(
        out,
        "calendarsRead=calendars.read\neventsWrite=events.write\naBC=a-b c\nv2Fa=2fa\nvClass=class\n"
    );
}
