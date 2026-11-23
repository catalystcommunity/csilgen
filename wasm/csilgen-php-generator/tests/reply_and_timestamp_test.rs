//! The typed-reply seam (`CsilReplyTransport::callReply` for multi-arm replies), its
//! package quickstart adapter, and the strict `timestamp` and `cddl_time` codecs. The PHP runs skip
//! cleanly when `php` is not on PATH.

use csilgen_common::*;
use csilgen_php_generator::generate_php_code_from_serialized;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

fn config(target: &str, package: bool) -> GeneratorConfig {
    let mut options = HashMap::new();
    if package {
        options.insert("emit_packages".to_string(), json!(["php"]));
        options.insert("package_name".to_string(), json!("echo-client"));
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

fn reference(name: &str) -> CsilTypeExpression {
    CsilTypeExpression::Reference(name.to_string())
}

fn group_rule(name: &str, field: &str, ty: &str) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
            entries: vec![CsilGroupEntry {
                key: Some(CsilGroupKey::Bare(field.to_string())),
                value_type: CsilTypeExpression::Builtin(ty.to_string()),
                occurrence: None,
                metadata: Vec::new(),
                doc_comments: Vec::new(),
            }],
        }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

/// `Pinger { ping: Ping -> Pong / Fault }` when `multi`, else `Ping -> Pong`.
fn echo_spec(multi: bool) -> CsilSpecSerialized {
    let output = if multi {
        CsilTypeExpression::Choice(vec![reference("Pong"), reference("Fault")])
    } else {
        reference("Pong")
    };
    CsilSpecSerialized {
        rules: vec![
            group_rule("Ping", "n", "int"),
            group_rule("Pong", "n", "int"),
            group_rule("Fault", "code", "int"),
            CsilRule {
                name: "Pinger".to_string(),
                rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                    operations: vec![CsilServiceOperation {
                        name: "ping".to_string(),
                        input_type: reference("Ping"),
                        output_type: output,
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
        ],
        source_content: None,
        service_count: 1,
        fields_with_metadata_count: 0,
    }
}

fn file(files: &[GeneratedFile], path: &str) -> String {
    files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} not generated"))
        .content
        .clone()
}

fn have_php() -> bool {
    Command::new("php").arg("--version").output().is_ok()
}

fn transport_src() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../transports/php/src")
}

#[test]
fn single_arm_spec_keeps_the_duck_typed_call_seam() {
    let files =
        generate_php_code_from_serialized(&echo_spec(false), &config("php-client", false)).unwrap();
    let client = file(&files, "client.php");
    assert!(!client.contains("CsilReplyTransport"));
    assert!(!client.contains("callReply"));
    assert!(client.contains("public function __construct($transport)"));
    assert!(client.contains("$this->transport->call('Pinger', 'ping', $payload)"));
}

#[test]
fn multi_arm_spec_requires_call_reply() {
    let files =
        generate_php_code_from_serialized(&echo_spec(true), &config("php-client", false)).unwrap();
    let client = file(&files, "client.php");
    assert!(client.contains("interface CsilReplyTransport"));
    assert!(client.contains("public function callReply($service, $op, $payload);"));
    assert!(client.contains("public function __construct(CsilReplyTransport $transport)"));
    assert!(client.contains("$this->transport->callReply('Pinger', 'ping', $payload)"));
    assert!(client.contains(
        "Codec::decodePong(Codec::resolveRpcReply('Pinger', 'ping', $reply['variant'], $reply['payload']))"
    ));
}

#[test]
fn quickstart_adapter_implements_call_reply_only_when_needed() {
    let multi = generate_php_code_from_serialized(&echo_spec(true), &config("php", true)).unwrap();
    let md = file(&multi, "genquickstart.md");
    assert!(
        md.contains("final class RpcAdapter implements \\Csilgen\\Generated\\CsilReplyTransport")
    );
    assert!(md.contains("public function callReply($service, $op, $payload)"));
    assert!(!md.contains("resolveRpcReply"));

    let single =
        generate_php_code_from_serialized(&echo_spec(false), &config("php", true)).unwrap();
    let md = file(&single, "genquickstart.md");
    assert!(md.contains("final class RpcAdapter\n"));
    assert!(!md.contains("callReply"));
}

/// Run the emitted quickstart adapter, verbatim except for its socket, against an
/// in-process carrier that answers each request through the generated router or
/// with a forced variant.
#[test]
fn quickstart_adapter_runs_under_php() {
    if !have_php() {
        eprintln!("skipping quickstart_adapter_runs_under_php: `php` not on PATH");
        return;
    }
    let files = generate_php_code_from_serialized(&echo_spec(true), &config("php", true)).unwrap();
    let md = file(&files, "genquickstart.md");
    let start = md.find("```php\n<?php\n").expect("php block") + "```php\n<?php\n".len();
    let end = start + md[start..].find("```").expect("block end");
    let block = md[start..end].replace(
        "new StreamCarrier(stream_socket_client('tcp://localhost:5080'))",
        "new LoopCarrier()",
    );
    assert!(block.contains("new LoopCarrier()"));

    let dir = std::env::temp_dir().join(format!("csilgen-php-quickstart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &f.content).unwrap();
    }
    let src = transport_src();
    let mut script = String::from("<?php\n");
    for lib in ["Errors", "CBOR", "Conventions", "Rpc", "Carrier"] {
        script.push_str(&format!(
            "require_once {:?};\n",
            src.join(format!("{lib}.php")).display().to_string()
        ));
    }
    for gen_file in ["types", "codec", "server", "client"] {
        script.push_str(&format!("require_once __DIR__ . '/src/{gen_file}.php';\n"));
    }
    script.push_str(
        r#"
use Csilgen\Generated\{Codec, PingerHandler, PingerRouter, Fault, FaultRefusal, Ping, Pong, CodecException};

final class Handler implements PingerHandler
{
    public function ping($request)
    {
        if ($request->n === 13) {
            throw new FaultRefusal(new Fault(['code' => 13]));
        }
        return new Pong(['n' => $request->n + 1]);
    }
}

final class LoopCarrier implements Csilgen\Transport\ByteCarrier
{
    private $reply;

    public function send($bytes)
    {
        $request = Csilgen\Transport\Rpc::decodeRequest($bytes);
        if (Codec::decodePing($request['payload'])->n === 14) {
            $this->reply = Csilgen\Transport\Rpc::encodeResponse(0, $request['payload'], null, 'Other');
            return;
        }
        $out = (new PingerRouter(new Handler()))->dispatch($request['op'], $request['payload']);
        $this->reply = Csilgen\Transport\Rpc::encodeResponse(0, $out['payload'], null, $out['variant']);
    }

    public function receive()
    {
        return $this->reply;
    }
}

$request = new Ping(['n' => 1]);
"#,
    );
    script.push_str(&block);
    script.push_str(
        r#"
if (!($response instanceof Pong) || $response->n !== 2) {
    throw new LogicException('success arm');
}
try {
    $client->ping(new Ping(['n' => 13]));
    throw new LogicException('missing refusal');
} catch (FaultRefusal $e) {
    if ($e->value->code !== 13) throw new LogicException('refusal value');
}
try {
    $client->ping(new Ping(['n' => 14]));
    throw new LogicException('accepted unknown variant');
} catch (CodecException $e) {
}
echo "RPC OK\n";
"#,
    );
    let path = dir.join("quickstart.php");
    std::fs::write(&path, &script).unwrap();
    let run = Command::new("php").arg(&path).output().expect("run php");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("RPC OK"),
        "quickstart adapter failed:\n{stdout}\n{}\n{script}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn timestamp_codec_is_tagged_and_strict() {
    let spec = CsilSpecSerialized {
        rules: vec![group_rule("Stamp", "at", "timestamp")],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    };
    let files = generate_php_code_from_serialized(&spec, &config("php", false)).unwrap();
    let codec = file(&files, "codec.php");
    assert!(codec.contains("$out['at'] = self::timestampToCbor($field);"));
    assert!(codec.contains("self::timestampFromCbor("));
    assert!(codec.contains("new \\Csilgen\\Transport\\Tag(0, "));
    // The shape is checked by hand, not by the lenient DateTime parser.
    assert!(codec.contains("[Tt]"));
    assert!(!codec.contains("new \\DateTimeImmutable($value"));
}

#[test]
fn cddl_time_codec_is_tag_one_epoch_seconds() {
    let spec = CsilSpecSerialized {
        rules: vec![group_rule("Moment", "at", "cddl_time")],
        source_content: None,
        service_count: 0,
        fields_with_metadata_count: 0,
    };
    let files = generate_php_code_from_serialized(&spec, &config("php", false)).unwrap();
    let codec = file(&files, "codec.php");
    assert!(codec.contains("$out['at'] = self::cddlTimeToCbor($field);"));
    assert!(codec.contains("self::cddlTimeFromCbor("));
    assert!(codec.contains("public static function decode_cddl_time($bytes)"));
    assert!(!codec.contains("timestampFromCbor"));
    if !have_php() {
        eprintln!("skipping the cddl_time PHP run: `php` not on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("csilgen-php-cddl-time-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for f in &files {
        std::fs::write(dir.join(&f.path), &f.content).unwrap();
    }
    let src = transport_src();
    let mut script = String::from("<?php\nnamespace Csilgen\\Generated;\n");
    for lib in ["Errors", "CBOR"] {
        script.push_str(&format!(
            "require_once {:?};\n",
            src.join(format!("{lib}.php")).display().to_string()
        ));
    }
    script.push_str(
        r#"require_once __DIR__ . '/types.php';
require_once __DIR__ . '/codec.php';

function roundTrip($hex)
{
    return bin2hex(Codec::encodeMoment(Codec::decodeMoment(hex2bin($hex))));
}

function at($text)
{
    return new \DateTimeImmutable($text);
}

$checks = array(
    // Whole seconds stay an integer; a float with no fraction becomes one.
    array(roundTrip('a1626174c11a38bbb4c0'), 'a1626174c11a38bbb4c0'),
    array(roundTrip('a1626174c1fb41cc5dda60000000'), 'a1626174c11a38bbb4c0'),
    // Half and single precision content widen to binary64.
    array(roundTrip('a1626174c1f93c00'), 'a1626174c101'),
    array(roundTrip('a1626174c1fa3fc00000'), 'a1626174c1fb3ff8000000000000'),
    array(roundTrip('a1626174c120'), 'a1626174c120'),
    array(bin2hex(Codec::encodeMoment(new Moment(['at' => at('1969-12-31T23:59:59.5Z')]))), 'a1626174c1fbbfe0000000000000'),
    array(Codec::decodeMoment(hex2bin('a1626174c1fbbfe0000000000000'))->at->format('Y-m-d\TH:i:s.u'), '1969-12-31T23:59:59.500000'),
    array(bin2hex(Codec::encodeMoment(new Moment(['at' => at('2000-02-29T13:00:00+01:00')]))), 'a1626174c11a38bbb4c0'),
);
foreach ($checks as $i => $check) {
    if ($check[0] !== $check[1]) {
        echo "check $i: got {$check[0]}, want {$check[1]}\n";
        exit(1);
    }
}
$bad = array(
    'a1626174c074323030302d30322d32395431323a30303a30305a', // tag 0 text
    'a16261741a38bbb4c0',                // no tag
    'a1626174c21a38bbb4c0',              // tag 2
    'a1626174c1f5',                      // true
    'a1626174c1f6',                      // null
    'a1626174c16131',                    // text
    'a1626174c14131',                    // bytes
    'a1626174c18101',                    // array
    'a1626174c1f97e00',                  // half NaN
    'a1626174c1fb7ff0000000000000',      // binary64 infinity
    'a1626174c11bffffffffffffffff',      // u64 max
    'a1626174c13bffffffffffffffff',      // most negative nint
    'a1626174c11b0000003afff44180',      // 253402300800, one past the end
    'a1626174c1fb7e37e43c8800759c',      // 1e300
);
foreach ($bad as $hex) {
    try {
        Codec::decodeMoment(hex2bin($hex));
    } catch (\RuntimeException $e) {
        continue;
    }
    echo "decoded $hex, but must fail\n";
    exit(1);
}
echo "ok\n";
"#,
    );
    let path = dir.join("check.php");
    std::fs::write(&path, &script).unwrap();
    let run = Command::new("php").arg(&path).output().expect("run php");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.trim() == "ok",
        "cddl_time PHP run failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
