//! The generated Go package must build when a record has the name of a codec helper,
//! and a generated client must give one set of error types for every operation: with
//! and without a declared error arm. These tests run real Go against the generated
//! package with a fake transport. They skip when `go` is not on PATH.

use csilgen_common::*;
use csilgen_go_generator::{generate_go_files, generate_go_output};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Record names that the generator's own codec helpers once used, so a record with
/// one of these names declared the same Go function twice.
const HELPER_NAMES: [&str; 7] = [
    "Date",
    "Decimal",
    "BigInt",
    "Timestamp",
    "CddlTime",
    "Err",
    "DepthLimit",
];

/// Record names equal to a name the generated package declares itself. The
/// generator renames them to `<Name>Type`; the wire keeps the declared name.
const RESERVED_NAMES: [&str; 8] = [
    "Transport",
    "WireReply",
    "CsilDecimal",
    "ParseCsilDecimal",
    "ResolveRPCReply",
    "RPCDecodeError",
    "RPCEncodeError",
    "RPCApplicationError",
];

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

fn text(value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Literal(CsilLiteralValue::Text(value.to_string()))
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

fn service(name: &str, op: &str, output: CsilTypeExpression) -> CsilRule {
    rule(
        name,
        CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations: vec![CsilServiceOperation {
                name: op.to_string(),
                input_type: reference("Req"),
                output_type: output,
                direction: CsilServiceDirection::Unidirectional,
                position: pos(),
                doc_comments: Vec::new(),
                wire_id: None,
            }],
            wire_id: None,
        }),
    )
}

/// The schema from both consumer requests: an enum inside a record named `Member`,
/// records named after the other helpers, `Rp.sign-request` with an error arm, and
/// `Ops.healthcheck` without one. `Req.at` is a timestamp, so a year past 9999 gives
/// a request with no wire form.
fn rules() -> Vec<CsilRule> {
    let mut rules = vec![
        rule(
            "State",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(vec![
                text("active"),
                text("inactive"),
            ])),
        ),
        record(
            "Member",
            vec![
                entry("state", reference("State")),
                entry("day", builtin("date")),
                entry("price", builtin("decimal")),
                entry("seen", builtin("cddl_time")),
                entry("at", builtin("timestamp")),
            ],
        ),
        record(
            "RpError",
            vec![
                entry("code", builtin("uint")),
                entry("message", builtin("text")),
            ],
        ),
        record("Ok", vec![entry("value", builtin("text"))]),
        record(
            "Req",
            vec![
                entry("id", builtin("text")),
                entry("at", builtin("timestamp")),
            ],
        ),
        service(
            "Rp",
            "sign-request",
            CsilTypeExpression::Choice(vec![reference("Ok"), reference("RpError")]),
        ),
        service("Ops", "healthcheck", reference("Ok")),
        service(
            "Gate",
            "refuse",
            CsilTypeExpression::Choice(vec![reference("Ok"), reference("Transport")]),
        ),
    ];
    for name in RESERVED_NAMES {
        rules.push(record(name, vec![entry("code", builtin("uint"))]));
    }
    for name in HELPER_NAMES {
        rules.push(record(name, vec![entry("state", reference("State"))]));
    }
    rules
}

fn generate(target: &str) -> Vec<GeneratedFile> {
    let mut options = HashMap::new();
    options.insert("package_name".to_string(), serde_json::json!("api"));
    let input = WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: rules(),
            source_content: None,
            service_count: 2,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        },
        config: GeneratorConfig {
            target: target.to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: GeneratorMetadata {
            name: "go-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "go".to_string(),
            capabilities: vec![GeneratorCapability::Services],
            author: None,
            homepage: None,
        },
    };
    generate_go_files(input).expect("generation ok")
}

fn have_go() -> bool {
    std::process::Command::new("go")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn stage(target: &str, test_go: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("csilgen-go-errs-{target}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in generate(target) {
        std::fs::write(dir.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(dir.join("go.mod"), "module example.com/api\n\ngo 1.22\n").unwrap();
    std::fs::write(dir.join("errors_test.go"), test_go).unwrap();
    dir
}

fn assert_go_test_passes(dir: &Path) {
    let out = std::process::Command::new("go")
        .args(["test", "./..."])
        .current_dir(dir)
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "-mod=mod")
        .env("GOPROXY", "off")
        .env("GO111MODULE", "on")
        .env("GOCACHE", dir.join(".gocache"))
        .output()
        .expect("failed to spawn go test");
    let ok = out.status.success();
    if ok {
        let _ = std::fs::remove_dir_all(dir);
    }
    assert!(
        ok,
        "go test failed in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Both the server and the client package can encode and decode a `Member`, and
/// refuse a state outside the enum in each direction.
const MEMBER_TEST_GO: &str = r#"
func TestMemberRoundTrip(t *testing.T) {
	in := Member{State: StateActive, Day: "2026-11-23", At: time.Date(2026, 11, 23, 0, 0, 0, 0, time.UTC), Seen: time.Unix(1700000000, 0)}
	b, err := EncodeMember(in)
	if err != nil {
		t.Fatalf("encode: %v", err)
	}
	out, err := DecodeMember(b)
	if err != nil {
		t.Fatalf("decode: %v", err)
	}
	if out.State != StateActive || out.Day != in.Day || !out.At.Equal(in.At) {
		t.Fatalf("round trip changed the value: %+v", out)
	}
	in.State = "bogus"
	if _, err := EncodeMember(in); err == nil {
		t.Fatal("encode accepted a state outside the enum")
	}
	// {"state": "bogus"}
	bad := []byte{0xa1, 0x65, 's', 't', 'a', 't', 'e', 0x65, 'b', 'o', 'g', 'u', 's'}
	if _, err := DecodeMember(bad); err == nil {
		t.Fatal("decode accepted a state outside the enum")
	}
	for _, enc := range []func() ([]byte, error){
		func() ([]byte, error) { return EncodeDate(Date{State: StateInactive}) },
		func() ([]byte, error) { return EncodeDecimal(Decimal{State: StateInactive}) },
		func() ([]byte, error) { return EncodeBigInt(BigInt{State: StateInactive}) },
		func() ([]byte, error) { return EncodeTimestamp(Timestamp{State: StateInactive}) },
		func() ([]byte, error) { return EncodeCddlTime(CddlTime{State: StateInactive}) },
	} {
		if _, err := enc(); err != nil {
			t.Fatalf("encode a record named after a helper: %v", err)
		}
	}
}
"#;

const CLIENT_TEST_GO: &str = r#"
type fakeTransport struct {
	resp  []byte
	reply WireReply
	err   error
	calls int
}

func (f *fakeTransport) Call(ctx context.Context, service, op string, req []byte) ([]byte, error) {
	f.calls++
	return f.resp, f.err
}

func (f *fakeTransport) CallReply(ctx context.Context, service, op string, req []byte) (WireReply, error) {
	f.calls++
	return f.reply, f.err
}

func variant(v string) *string { return &v }

func both(tr *fakeTransport, req Req) []error {
	_, rpErr := NewRpClient(tr).SignRequest(context.Background(), req)
	_, opsErr := NewOpsClient(tr).Healthcheck(context.Background(), req)
	return []error{rpErr, opsErr}
}

func TestMalformedReplyIsDecodeError(t *testing.T) {
	notCbor := []byte{0xff}
	tr := &fakeTransport{resp: notCbor, reply: WireReply{Variant: variant("Ok"), Payload: notCbor}}
	for i, err := range both(tr, Req{Id: "a"}) {
		var de *RPCDecodeError
		if !errors.As(err, &de) {
			t.Fatalf("client %d: want *RPCDecodeError, got %T %v", i, err, err)
		}
	}
	for _, v := range []*string{nil, variant("Nope")} {
		_, err := NewRpClient(&fakeTransport{reply: WireReply{Variant: v}}).SignRequest(context.Background(), Req{})
		var de *RPCDecodeError
		if !errors.As(err, &de) {
			t.Fatalf("reply variant %v: want *RPCDecodeError, got %T %v", v, err, err)
		}
	}
}

func TestRequestWithNoWireFormIsEncodeError(t *testing.T) {
	tr := &fakeTransport{}
	for i, err := range both(tr, Req{Id: "a", At: time.Date(10000, 1, 1, 0, 0, 0, 0, time.UTC)}) {
		var ee *RPCEncodeError
		if !errors.As(err, &ee) {
			t.Fatalf("client %d: want *RPCEncodeError, got %T %v", i, err, err)
		}
	}
	if tr.calls != 0 {
		t.Fatalf("a request with no wire form reached the transport %d times", tr.calls)
	}
}

func TestTransportErrorIsNotWrapped(t *testing.T) {
	for i, err := range both(&fakeTransport{err: context.Canceled}, Req{Id: "a"}) {
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("client %d: want context.Canceled, got %T %v", i, err, err)
		}
	}
}

func TestRenamedErrorArmKeepsItsWireName(t *testing.T) {
	payload, err := EncodeTransportType(TransportType{Code: 3})
	if err != nil {
		t.Fatal(err)
	}
	_, err = NewGateClient(&fakeTransport{reply: WireReply{Variant: variant("Transport"), Payload: payload}}).Refuse(context.Background(), Req{})
	var ae *RPCApplicationError[TransportType]
	if !errors.As(err, &ae) || ae.Value.Code != 3 {
		t.Fatalf("want *RPCApplicationError[TransportType], got %T %v", err, err)
	}
	name, _, err := EncodeGateRefuseReply(Ok{}, &RPCApplicationError[TransportType]{Value: TransportType{Code: 3}})
	if err != nil || name != "Transport" {
		t.Fatalf("reply variant: got %q, %v; want \"Transport\"", name, err)
	}
}

func TestDeclaredRefusalIsApplicationError(t *testing.T) {
	payload, err := EncodeRpError(RpError{Code: 7, Message: "no"})
	if err != nil {
		t.Fatal(err)
	}
	_, err = NewRpClient(&fakeTransport{reply: WireReply{Variant: variant("RpError"), Payload: payload}}).SignRequest(context.Background(), Req{})
	var ae *RPCApplicationError[RpError]
	if !errors.As(err, &ae) || ae.Value.Code != 7 {
		t.Fatalf("want *RPCApplicationError[RpError], got %T %v", err, err)
	}
}
"#;

#[test]
fn no_codec_function_is_declared_twice() {
    for target in ["go", "go-client", "go-server"] {
        let files = generate(target);
        let codec = files.iter().find(|f| f.path == "codec.gen.go").unwrap();
        let mut seen = std::collections::HashSet::new();
        for line in codec.content.lines() {
            if let Some(rest) = line.strip_prefix("func ") {
                if rest.starts_with('(') {
                    continue;
                }
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                assert!(
                    seen.insert(name.clone()),
                    "{target}: func {name} declared twice"
                );
            }
        }
    }
}

#[test]
fn client_has_no_unused_client_error_type() {
    let files = generate("go-client");
    let client = files.iter().find(|f| f.path == "client.gen.go").unwrap();
    assert!(
        !client.content.contains("ClientError"),
        "{}",
        client.content
    );
}

#[test]
fn server_package_round_trips_member() {
    if !have_go() {
        eprintln!("skipping server_package_round_trips_member: no go on PATH");
        return;
    }
    let test = format!("package api\n\nimport (\n\t\"testing\"\n\t\"time\"\n)\n{MEMBER_TEST_GO}");
    assert_go_test_passes(&stage("go-server", &test));
}

#[test]
fn client_errors_are_the_same_for_every_operation() {
    if !have_go() {
        eprintln!("skipping client_errors_are_the_same_for_every_operation: no go on PATH");
        return;
    }
    let test = format!(
        "package api\n\nimport (\n\t\"context\"\n\t\"errors\"\n\t\"testing\"\n\t\"time\"\n)\n{MEMBER_TEST_GO}{CLIENT_TEST_GO}"
    );
    assert_go_test_passes(&stage("go-client", &test));
}

/// A rule that takes a name the package derives from a service or op, and two
/// rules that become one Go identifier, are renamed with a warning each. The
/// package builds, and the reply variant keeps the declared name.
#[test]
fn derived_names_and_case_collisions_are_renamed_with_a_warning() {
    let mut options = HashMap::new();
    options.insert("package_name".to_string(), serde_json::json!("api"));
    let rules = vec![
        record("Req", vec![entry("id", builtin("text"))]),
        record("Ok", vec![entry("value", builtin("text"))]),
        record("Value", vec![entry("a", builtin("text"))]),
        record("value", vec![entry("b", builtin("int"))]),
        record("GateClient", vec![entry("c", builtin("text"))]),
        record("GateRefuseReply", vec![entry("d", builtin("text"))]),
        record(
            "Holder",
            vec![
                entry("v", reference("Value")),
                entry("w", reference("value")),
            ],
        ),
        service(
            "Gate",
            "refuse",
            CsilTypeExpression::Choice(vec![reference("Ok"), reference("value")]),
        ),
    ];
    let input = WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules,
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
            declared_names: Default::default(),
        },
        config: GeneratorConfig {
            target: "go-client".to_string(),
            output_dir: "/tmp".to_string(),
            options,
        },
        generator_metadata: GeneratorMetadata {
            name: "go-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "go".to_string(),
            capabilities: vec![GeneratorCapability::Services],
            author: None,
            homepage: None,
        },
    };
    let output = generate_go_output(input).expect("generation ok");
    let renamed: Vec<&str> = output.warnings.iter().map(|w| w.message.as_str()).collect();
    for (from, to) in [
        ("value", "valueType"),
        ("GateClient", "GateClientType"),
        ("GateRefuseReply", "GateRefuseReplyType"),
    ] {
        assert!(
            renamed
                .iter()
                .any(|m| m.contains(&format!("`{from}` is generated as `{to}`"))),
            "{renamed:?}"
        );
    }
    assert_eq!(renamed.len(), 3, "{renamed:?}");
    let codec = output
        .files
        .iter()
        .find(|f| f.path == "codec.gen.go")
        .unwrap();
    assert!(codec.content.contains("case \"value\":"));
    assert!(codec.content.contains("RPCApplicationError[valueType]"));
    if !have_go() {
        eprintln!("skipping the go build: no go on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("csilgen-go-derived-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &output.files {
        std::fs::write(dir.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(dir.join("go.mod"), "module example.com/api\n\ngo 1.22\n").unwrap();
    std::fs::write(dir.join("build_test.go"), "package api\n").unwrap();
    assert_go_test_passes(&dir);
}
