//! Renames only for real clashes, service identifier clashes, names the hoister
//! synthesizes, map key order, duplicate keys, and the arm a plain PHP array
//! encodes as. The PHP checks run the generated code through a real `php` and
//! skip when it is not on PATH.

use csilgen_common::*;
use csilgen_php_generator::run_generation;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

fn config(target: &str, package: bool) -> GeneratorConfig {
    let mut options = HashMap::new();
    if package {
        options.insert("emit_packages".to_string(), serde_json::json!(["php"]));
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

fn entry(name: &str, value_type: CsilTypeExpression) -> CsilGroupEntry {
    CsilGroupEntry {
        key: Some(CsilGroupKey::Bare(name.to_string())),
        value_type,
        occurrence: None,
        metadata: Vec::new(),
        doc_comments: Vec::new(),
    }
}

fn group(entries: Vec<CsilGroupEntry>) -> CsilGroupExpression {
    CsilGroupExpression { entries }
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
        CsilRuleType::TypeDef(CsilTypeExpression::Group(group(entries))),
    )
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
        .filter(|rule| matches!(rule.rule_type, CsilRuleType::ServiceDef(_)))
        .count();
    CsilSpecSerialized {
        rules,
        source_content: None,
        service_count,
        fields_with_metadata_count: 0,
        declared_names: Default::default(),
    }
}

fn generate(spec: CsilSpecSerialized, config: GeneratorConfig) -> WasmGeneratorOutput {
    run_generation(&WasmGeneratorInput {
        csil_spec: spec,
        config,
        generator_metadata: GeneratorMetadata {
            name: "php-generator".to_string(),
            version: "0.1.0".to_string(),
            description: String::new(),
            target: "php".to_string(),
            capabilities: Vec::new(),
            author: None,
            homepage: None,
        },
    })
    .expect("generation succeeded")
}

fn file<'a>(output: &'a WasmGeneratorOutput, path: &str) -> &'a str {
    &output
        .files
        .iter()
        .find(|f| f.path == path || f.path == format!("src/{path}"))
        .unwrap_or_else(|| panic!("missing generated file {path}"))
        .content
}

fn all_code(output: &WasmGeneratorOutput) -> String {
    output
        .files
        .iter()
        .filter(|f| f.path.ends_with(".php"))
        .map(|f| f.content.as_str())
        .collect()
}

fn have_php() -> bool {
    Command::new("php").arg("--version").output().is_ok()
}

/// Write the generated files next to a driver that loads them and the transport,
/// run it, and return its stdout. The driver must print `ok`.
fn run_php(output: &WasmGeneratorOutput, label: &str, body: &str) {
    let dir = std::env::temp_dir().join(format!("csilgen-php-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut requires = String::new();
    for f in &output.files {
        let name = Path::new(&f.path).file_name().unwrap().to_str().unwrap();
        std::fs::write(dir.join(name), &f.content).unwrap();
        if name.ends_with(".php") {
            requires.push_str(&format!("require __DIR__ . '/{name}';\n"));
        }
    }
    let transport = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/php/src");
    let driver = format!(
        "<?php\nnamespace Csilgen\\Generated;\nrequire '{t}/Errors.php';\nrequire '{t}/CBOR.php';\n{requires}{body}\necho \"ok\\n\";\n",
        t = transport.display()
    );
    std::fs::write(dir.join("driver.php"), driver).unwrap();
    let run = Command::new("php")
        .arg(dir.join("driver.php"))
        .output()
        .unwrap();
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).trim() == "ok",
        "php {label} failed:\n{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The linkkeys request's first schema: the op's own records are its boundary
/// codecs, so nothing clashes.
#[test]
fn op_boundary_records_keep_their_names() {
    let s = spec(vec![
        record(
            "RpIssueAttestationRequest",
            vec![
                entry("claim_type", builtin("text")),
                entry("claim_value", builtin("bytes")),
            ],
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
    ]);
    for target in ["php", "php-client", "php-typesonly"] {
        let out = generate(s.clone(), config(target, true));
        assert!(out.warnings.is_empty(), "{target}: {:?}", out.warnings);
        let code = all_code(&out);
        assert_eq!(count(&code, "class RpIssueAttestationRequest\n"), 1);
        assert_eq!(count(&code, "function encodeRpIssueAttestationRequest("), 1);
        if have_php() {
            run_php(&out, "rp", "");
        }
    }
}

/// The request's second schema. PHP gives a non-record boundary the generic
/// `encode_value` codec, not a per-operation one, so `MemberGetMemberRequest`
/// clashes with nothing in PHP and keeps its name.
#[test]
fn php_has_no_per_operation_codec_to_clash_with() {
    let s = spec(vec![
        rule("MemberID", CsilRuleType::TypeDef(builtin("text"))),
        record("MemberGetMemberRequest", vec![entry("id", builtin("text"))]),
        service(
            "Member",
            vec![("get-member", reference("MemberID"), reference("MemberID"))],
        ),
    ]);
    let out = generate(s, config("php", true));
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(count(&all_code(&out), "class MemberGetMemberRequest\n"), 1);
}

/// A rule that equals a derived class is renamed only when the target emits
/// that class.
#[test]
fn derived_class_names_are_reserved_only_where_emitted() {
    let s = spec(vec![
        record("MemberClient", vec![entry("id", builtin("text"))]),
        record("MemberHandler", vec![entry("id", builtin("text"))]),
        service(
            "Member",
            vec![(
                "get-member",
                reference("MemberClient"),
                reference("MemberHandler"),
            )],
        ),
    ]);

    let client = generate(s.clone(), config("php-client", false));
    let code = all_code(&client);
    assert_eq!(client.warnings.len(), 1, "{:?}", client.warnings);
    assert!(
        client.warnings[0]
            .message
            .contains("rule `MemberClient` is generated as `MemberClientType`")
    );
    assert!(code.contains("class MemberClientType\n"));
    assert!(code.contains("class MemberClient\n"));
    assert!(code.contains("class MemberHandler\n"));

    let server = generate(s.clone(), config("php-server", false));
    assert_eq!(server.warnings.len(), 1, "{:?}", server.warnings);
    assert!(server.warnings[0].message.contains("`MemberHandlerType`"));

    let types = generate(s.clone(), config("php-typesonly", false));
    assert!(types.warnings.is_empty(), "{:?}", types.warnings);

    let both = generate(s, config("php", true));
    assert_eq!(both.warnings.len(), 2, "{:?}", both.warnings);
    if have_php() {
        run_php(&both, "derived", "");
    }
}

/// A rule whose class equals the stem of an RPC reply codec is renamed, because
/// both would be `Codec::encode<Stem>`.
#[test]
fn reply_codec_stem_clash_is_renamed() {
    let s = spec(vec![
        record("Ok", vec![entry("id", builtin("text"))]),
        record("Bad", vec![entry("why", builtin("text"))]),
        rule("PingGoReply", CsilRuleType::TypeDef(builtin("text"))),
        service(
            "Ping",
            vec![(
                "go",
                reference("PingGoReply"),
                CsilTypeExpression::Choice(vec![reference("Ok"), reference("Bad")]),
            )],
        ),
    ]);
    let out = generate(s, config("php", true));
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(out.warnings[0].message.contains("`PingGoReplyType`"));
    if have_php() {
        run_php(&out, "stem", "");
    }
}

/// Two services whose PHP names are equal get distinct classes, and each keeps
/// its declared name on the wire.
#[test]
fn service_identifier_clash_keeps_wire_names() {
    let s = spec(vec![
        record("Msg", vec![entry("id", builtin("text"))]),
        service("UserSvc", vec![("get", reference("Msg"), reference("Msg"))]),
        service(
            "user-svc",
            vec![("put", reference("Msg"), reference("Msg"))],
        ),
    ]);
    let out = generate(s, config("php", true));
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(
        out.warnings[0]
            .message
            .contains("service `user-svc` is generated as `user-svc2`")
    );
    let client = file(&out, "client.php");
    assert!(client.contains("class UserSvcClient\n"));
    assert!(client.contains("class UserSvc2Client\n"));
    assert!(client.contains("$this->transport->call('UserSvc', 'get', $payload)"));
    assert!(client.contains("$this->transport->call('user-svc', 'put', $payload)"));
    let server = file(&out, "server.php");
    assert!(server.contains("class UserSvcRouter\n"));
    assert!(server.contains("class UserSvc2Router\n"));
    if have_php() {
        run_php(
            &out,
            "services",
            r#"
final class Wire
{
    public $calls = array();
    public function call($service, $op, $payload)
    {
        $this->calls[] = "$service/$op";
        return $payload;
    }
}
$wire = new Wire();
(new UserSvcClient($wire))->get(new Msg(array('id' => 'a')));
(new UserSvc2Client($wire))->put(new Msg(array('id' => 'b')));
if ($wire->calls !== array('UserSvc/get', 'user-svc/put')) {
    throw new \LogicException(implode(',', $wire->calls));
}
"#,
        );
    }
}

/// A record and a service with the same stem keep their names: a service has no
/// class of its own.
#[test]
fn record_and_service_with_one_stem_do_not_clash() {
    let s = spec(vec![
        record("Order", vec![entry("id", builtin("text"))]),
        service(
            "OrderService",
            vec![("get", reference("Order"), reference("Order"))],
        ),
    ]);
    let out = generate(s, config("php", true));
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
}

/// The hoister names `Shop`'s inline `client` record `Shop_client`, whose class
/// `ShopClient` equals the service's client class. The synthesized rule is the
/// one renamed.
#[test]
fn synthesized_name_that_clashes_is_renamed() {
    let s = spec(vec![
        record(
            "Shop",
            vec![entry(
                "client",
                CsilTypeExpression::Group(group(vec![entry("x", builtin("int"))])),
            )],
        ),
        service(
            "ShopService",
            vec![("get", reference("Shop"), reference("Shop"))],
        ),
    ]);
    let out = generate(s, config("php", true));
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(
        out.warnings[0]
            .message
            .contains("rule `Shop_client` is generated as `Shop_clientType`")
    );
    let code = all_code(&out);
    assert!(code.contains("class Shop\n"));
    assert!(code.contains("class ShopClientType\n"));
    assert!(code.contains("class ShopClient\n"));
    if have_php() {
        run_php(&out, "synth", "");
    }
}

/// `Contact = {id, name} // {id} // {email}` with `Counts = {* text => int}`.
fn union_spec() -> CsilSpecSerialized {
    let arm = |entries| CsilTypeExpression::Group(group(entries));
    spec(vec![
        rule(
            "Contact",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                arm(vec![
                    entry("id", builtin("int")),
                    entry("name", builtin("text")),
                ]),
                arm(vec![entry("id", builtin("int"))]),
                arm(vec![entry("email", builtin("text"))]),
            ])),
        ),
        rule(
            "Counts",
            CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(builtin("text")),
                value: Box::new(builtin("int")),
                occurrence: None,
            }),
        ),
        rule(
            "Labels",
            CsilRuleType::TypeDef(CsilTypeExpression::Map {
                key: Box::new(builtin("int")),
                value: Box::new(builtin("text")),
                occurrence: None,
            }),
        ),
    ])
}

#[test]
fn plain_array_takes_the_first_record_arm_that_fits() {
    let out = generate(union_spec(), config("php-typesonly", false));
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    if !have_php() {
        return;
    }
    run_php(
        &out,
        "first-fit",
        r#"
function check($got, $want, $label)
{
    if ($got !== $want) {
        throw new \LogicException("$label: got " . bin2hex($got) . ', want ' . bin2hex($want));
    }
}
function rejects($fn, $message, $label)
{
    try {
        $fn();
    } catch (CodecException $e) {
        if (strpos($e->getMessage(), $message) === false) {
            throw new \LogicException("$label: " . $e->getMessage());
        }
        return;
    }
    throw new \LogicException("$label: decoded");
}
// Fits arms 0 and 1: index 0.
check(Codec::encodeContact(array('id' => 1, 'name' => 'x')), hex2bin('8200a262696401646e616d656178'), 'arm0');
// Fits only arm 1: index 1.
check(Codec::encodeContact(array('id' => 1)), hex2bin('8201a162696401'), 'arm1');
check(Codec::encodeContact(array('email' => 'e')), hex2bin('8202a165656d61696c6165'), 'arm2');
// A class instance names its arm.
check(Codec::encodeContact(new Contact1(array('id' => 1))), hex2bin('8201a162696401'), 'class');
rejects(function () { Codec::encodeContact(array('zz' => 1)); }, 'does not match any Contact variant', 'no arm');

// Dynamic maps encode in the bytewise order of their encoded keys.
check(Codec::encodeCounts(array('aa' => 2, 'b' => 1)), hex2bin('a261620162616102'), 'text order');
check(Codec::encodeLabels(array(-1 => 'minus', 10 => 'ten', 1 => 'one')), hex2bin('a301636f6e650a6374656e20656d696e7573'), 'int order');
check(Codec::encodeCounts(array()), hex2bin('a0'), 'empty map');
check(Codec::encodeCounts(array('0' => 1)), hex2bin('a1613001'), 'numeric text key');
rejects(function () { Codec::decodeCounts(hex2bin('a262616102616201')); }, 'map keys are not in canonical order', 'unsorted');
rejects(function () { Codec::decodeCounts(hex2bin('a2616101616102')); }, 'duplicate key "a"', 'duplicate');
rejects(function () { Codec::decodeContact(hex2bin('8201a26269640162696402')); }, 'duplicate key "id"', 'record duplicate');
rejects(function () { Codec::decodeContact(hex2bin('8200a162696401')); }, 'missing field "name"', 'missing');
rejects(function () { Codec::decodeContact(hex2bin('8203a162696401')); }, 'unknown Contact variant', 'range');
rejects(function () { Codec::decodeContact(hex2bin('a162696401')); }, 'expects a 2-element array', 'untagged');
// A map with keys 0 and 1 is not the array a union needs.
rejects(function () { Codec::decodeContact(hex2bin('a200a001a162696401')); }, 'expects a 2-element array', 'map as array');
// Key heads must be in shortest form: int 1 as 18 01, text "a" as 78 01 61.
rejects(function () { Codec::decodeLabels(hex2bin('a11801636f6e65')); }, 'map key is not in shortest form', 'int key width');
rejects(function () { Codec::decodeCounts(hex2bin('a178016101')); }, 'map key is not in shortest form', 'text key width');
rejects(function () { Codec::decodeContact(hex2bin('8201a17802696401')); }, 'map key is not in shortest form', 'record key width');
// A map is not an array, and an array is not a map.
rejects(function () { Codec::decodeCounts(hex2bin('80')); }, 'expected a map', 'array as map');
check(Codec::encodeLabels(Codec::decodeLabels(hex2bin('a301636f6e650a6374656e20656d696e7573'))), hex2bin('a301636f6e650a6374656e20656d696e7573'), 'int keys round trip');
"#,
    );
}

/// `read-counts: Holder -> { * text => int } / Err` has an inline arm, so its
/// output is one named union with both arms, in client and server.
#[test]
fn mixed_op_output_is_the_whole_named_union() {
    let s = spec(vec![
        record("Holder", vec![entry("id", builtin("int"))]),
        record("Err", vec![entry("message", builtin("text"))]),
        service(
            "Lookup",
            vec![(
                "read-counts",
                reference("Holder"),
                CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Map {
                        key: Box::new(builtin("text")),
                        value: Box::new(builtin("int")),
                        occurrence: None,
                    },
                    reference("Err"),
                ]),
            )],
        ),
    ]);
    let out = generate(s, config("php", true));
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert!(
        file(&out, "client.php").contains("return Codec::decodeLookupReadCountsOutput($reply);")
    );
    assert!(file(&out, "server.php").contains(
        "return Codec::encodeLookupReadCountsOutput($this->handler->readCounts($request));"
    ));
    let codec = file(&out, "codec.php");
    assert!(codec.contains("return array(0, (function ($m)"));
    assert!(codec.contains("return array(1, self::toCborErr($value));"));
    assert!(codec.contains("return self::fromCborErr($csilVal);"));
}

/// tests/interop/repo-specs/dotted-enum-values.csil: each value gets a legal
/// constant, and each value round-trips through the codec as written.
#[test]
fn text_enum_values_that_are_not_identifiers_round_trip() {
    let text = |value: &str| CsilTypeExpression::Literal(CsilLiteralValue::Text(value.to_string()));
    let s = spec(vec![
        rule(
            "Scope",
            CsilRuleType::TypeChoice(vec![
                text("calendars.read"),
                text("events.write"),
                text("a-b c"),
                text("2fa"),
                text("class"),
            ]),
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
        service(
            "Auth",
            vec![("grant", reference("Grant"), reference("Grant"))],
        ),
    ]);
    let out = generate(s, config("php", true));
    let types = file(&out, "types.php");
    for constant in [
        "const CALENDARS_READ = 'calendars.read';",
        "const EVENTS_WRITE = 'events.write';",
        "const A_B_C = 'a-b c';",
        "const V_2FA = '2fa';",
        "const V_CLASS = 'class';",
    ] {
        assert!(types.contains(constant), "missing {constant}");
    }
    if !have_php() {
        return;
    }
    run_php(
        &out,
        "enum-values",
        r#"
$values = array(
    Scope::CALENDARS_READ => 'calendars.read',
    Scope::EVENTS_WRITE => 'events.write',
    Scope::A_B_C => 'a-b c',
    Scope::V_2FA => '2fa',
    Scope::V_CLASS => 'class',
);
foreach ($values as $member => $wire) {
    if ($member !== $wire) {
        throw new \LogicException("constant for $wire is $member");
    }
    $bytes = Codec::encodeScope($member);
    if ($bytes !== \Csilgen\Transport\CBOR::encode($wire) || Codec::decodeScope($bytes) !== $wire) {
        throw new \LogicException("$wire does not round-trip");
    }
    $grant = Codec::decodeGrant(Codec::encodeGrant(new Grant(array('scope' => $member, 'extra' => array($member)))));
    if ($grant->scope !== $wire || $grant->extra !== array($wire)) {
        throw new \LogicException("$wire does not round-trip in a record");
    }
}
"#,
    );
}
