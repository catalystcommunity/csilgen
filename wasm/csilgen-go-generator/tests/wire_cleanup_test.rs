//! Derived names, service clashes, hoisted-name clashes, mixed op boundaries,
//! canonical map order and gofmt-stable output. Each test checks the generated text
//! and then builds (or runs `go test` on) the package. The Go steps skip when `go`
//! is not on PATH.

use csilgen_common::*;
use csilgen_go_generator::generate_go_output;
use std::collections::HashMap;
use std::path::Path;

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

fn map_of(key: &str, value: &str) -> CsilTypeExpression {
    CsilTypeExpression::Map {
        key: Box::new(builtin(key)),
        value: Box::new(builtin(value)),
        occurrence: Some(CsilOccurrence::ZeroOrMore),
    }
}

fn service(name: &str, ops: Vec<(&str, CsilTypeExpression, CsilTypeExpression)>) -> CsilRule {
    let operations = ops
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
        .collect();
    rule(
        name,
        CsilRuleType::ServiceDef(CsilServiceDefinition {
            operations,
            wire_id: None,
        }),
    )
}

fn generate(target: &str, rules: Vec<CsilRule>) -> WasmGeneratorOutput {
    let mut options = HashMap::new();
    options.insert("package_name".to_string(), serde_json::json!("api"));
    let service_count = rules
        .iter()
        .filter(|r| matches!(r.rule_type, CsilRuleType::ServiceDef(_)))
        .count();
    generate_go_output(WasmGeneratorInput {
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
    })
    .expect("generation ok")
}

fn all_code(output: &WasmGeneratorOutput) -> String {
    output
        .files
        .iter()
        .filter(|f| f.path.ends_with(".go"))
        .map(|f| f.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn have_go() -> bool {
    std::process::Command::new("go")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Write the package (plus an optional `_test.go`) and run `go vet` and `go test`.
fn go_test(name: &str, output: &WasmGeneratorOutput, test_go: &str) {
    if !have_go() {
        eprintln!("skipping the go build: no go on PATH");
        return;
    }
    let dir = std::env::temp_dir().join(format!("csilgen-go-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &output.files {
        std::fs::write(dir.join(&file.path), &file.content).unwrap();
    }
    std::fs::write(dir.join("go.mod"), "module example.com/api\n\ngo 1.22\n").unwrap();
    std::fs::write(dir.join("wire_test.go"), test_go).unwrap();
    run_go(&dir, &["vet", "./..."]);
    run_go(&dir, &["test", "./..."]);
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_go(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("go")
        .args(args)
        .current_dir(dir)
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "-mod=mod")
        .env("GOPROXY", "off")
        .env("GOCACHE", dir.join(".gocache"))
        .output()
        .expect("failed to spawn go");
    assert!(
        out.status.success(),
        "go {args:?} failed in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn count(code: &str, needle: &str) -> usize {
    code.matches(needle).count()
}

/// The linkkeys case: a record that IS its op's request or response keeps its name,
/// and the package declares its codec one time.
#[test]
fn op_boundary_record_keeps_its_name() {
    let rules = vec![
        record("SignedSigningRequest", vec![entry("x", builtin("int"))]),
        record("Claim", vec![entry("y", builtin("int"))]),
        record(
            "RpIssueAttestationRequest",
            vec![
                entry("signed_request", reference("SignedSigningRequest")),
                entry("claim_type", builtin("text")),
                entry("claim_value", builtin("bytes")),
            ],
        ),
        record(
            "RpIssueAttestationResponse",
            vec![
                entry("claim", reference("Claim")),
                entry("deposited", builtin("bool")),
            ],
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
    for target in ["go", "go-client"] {
        let output = generate(target, rules.clone());
        assert!(output.warnings.is_empty(), "{:?}", output.warnings);
        let code = all_code(&output);
        for needle in [
            "type RpIssueAttestationRequest struct",
            "func EncodeRpIssueAttestationRequest(",
            "func DecodeRpIssueAttestationResponse(",
        ] {
            assert_eq!(count(&code, needle), 1, "{target}: {needle}");
        }
        go_test(&format!("boundary-{target}"), &output, "package api\n");
    }
}

/// A scalar boundary gets per-op helpers, so a record named like them is renamed.
#[test]
fn record_named_like_a_scalar_boundary_helper_is_renamed() {
    let rules = vec![
        rule("MemberID", CsilRuleType::TypeDef(builtin("text"))),
        record("MemberGetMemberRequest", vec![entry("id", builtin("text"))]),
        service(
            "Member",
            vec![("get-member", reference("MemberID"), reference("MemberID"))],
        ),
    ];
    let output = generate("go-client", rules);
    let messages: Vec<&str> = output.warnings.iter().map(|w| w.message.as_str()).collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0]
            .contains("rule `MemberGetMemberRequest` is generated as `MemberGetMemberRequestType`")
    );
    let code = all_code(&output);
    assert_eq!(count(&code, "func EncodeMemberGetMemberRequest("), 1);
    assert_eq!(count(&code, "func EncodeMemberGetMemberRequestType("), 1);
    go_test("scalar-boundary", &output, "package api\n");
}

/// Two services with one identifier get two clients, and each keeps its declared
/// name on the wire.
#[test]
fn clashing_services_keep_their_wire_names() {
    let rules = vec![
        record("Ping", vec![entry("n", builtin("int"))]),
        service(
            "UserSvc",
            vec![("ping", reference("Ping"), reference("Ping"))],
        ),
        service(
            "user-svc",
            vec![("pong", reference("Ping"), reference("Ping"))],
        ),
    ];
    let output = generate("go-client", rules.clone());
    assert!(
        output
            .warnings
            .iter()
            .any(|w| w.message.contains("service `user-svc` is generated as")),
        "{:?}",
        output.warnings
    );
    let code = all_code(&output);
    assert!(code.contains("type UserSvcClient struct"));
    assert!(code.contains("type UserSvc2Client struct"));
    assert!(code.contains(r#"c.transport.Call(ctx, "UserSvc", "ping", csilReq)"#));
    assert!(code.contains(r#"c.transport.Call(ctx, "user-svc", "pong", csilReq)"#));
    go_test("services-client", &output, "package api\n");
    let server = generate("go", rules);
    let code = all_code(&server);
    assert!(code.contains("type UserSvc interface"));
    assert!(code.contains("type UserSvc2 interface"));
    go_test("services-server", &server, "package api\n");
}

/// The hoister names `Box.client` `BoxClient`, which is also the client struct of
/// service `BoxService`. The synthesized rule is renamed, not the client.
#[test]
fn hoisted_name_that_clashes_is_renamed() {
    let rules = vec![
        record(
            "Box",
            vec![entry(
                "client",
                CsilTypeExpression::Group(CsilGroupExpression {
                    entries: vec![entry("x", builtin("int"))],
                }),
            )],
        ),
        service(
            "BoxService",
            vec![("put", reference("Box"), reference("Box"))],
        ),
    ];
    let output = generate("go-client", rules);
    assert!(
        output
            .warnings
            .iter()
            .any(|w| w.message.contains("is generated as `BoxClientType`")),
        "{:?}",
        output.warnings
    );
    let code = all_code(&output);
    assert!(code.contains("type BoxClientType struct"));
    assert!(code.contains("type BoxClient struct"));
    go_test("hoisted-clash", &output, "package api\n");
}

fn holder_rules() -> Vec<CsilRule> {
    vec![
        rule("Counts", CsilRuleType::TypeDef(map_of("text", "int"))),
        rule("Labels", CsilRuleType::TypeDef(map_of("int", "text"))),
        record(
            "Holder",
            vec![
                entry("counts", reference("Counts")),
                entry("labels", reference("Labels")),
            ],
        ),
        record("Err", vec![entry("message", builtin("text"))]),
        service(
            "Lookup",
            vec![(
                "read-counts",
                reference("Holder"),
                CsilTypeExpression::Choice(vec![map_of("text", "int"), reference("Err")]),
            )],
        ),
    ]
}

/// A mixed op output is one named union with both arms, in the server interface and
/// in the client.
#[test]
fn mixed_op_output_is_a_named_union() {
    let server = generate("go", holder_rules());
    let code = all_code(&server);
    assert!(
        code.contains(
            "ReadCounts(ctx context.Context, req Holder) (LookupReadCountsOutput, error)"
        )
    );
    assert!(code.contains("type LookupReadCountsOutput interface{}"));
    assert!(code.contains("case map[string]int64:"));
    assert!(code.contains("case Err:"));
    go_test("op-union-server", &server, "package api\n");
    let client = generate("go-client", holder_rules());
    let code = all_code(&client);
    assert!(code.contains(
        "func (c *LookupClient) ReadCounts(ctx context.Context, req Holder) (LookupReadCountsOutput, error)"
    ));
    go_test("op-union-client", &client, "package api\n");
}

/// The encoder sorts dynamic map keys by their encoded bytes; the decoder refuses a
/// map out of that order and a repeated record key, and accepts record fields in any
/// order.
#[test]
fn dynamic_maps_are_canonical_and_duplicates_are_refused() {
    let output = generate("go", holder_rules());
    let test_go = r#"package api

import (
	"encoding/hex"
	"strings"
	"testing"
)

func TestCanonicalMaps(t *testing.T) {
	h := Holder{
		Counts: Counts{"aa": 2, "b": 1},
		Labels: Labels{-1: "minus", 10: "ten", 1: "one"},
	}
	for i := 0; i < 20; i++ {
		got, err := EncodeHolder(h)
		if err != nil {
			t.Fatal(err)
		}
		want := "a266636f756e7473a261620162616102666c6162656c73a301636f6e650a6374656e20656d696e7573"
		if hex.EncodeToString(got) != want {
			t.Fatalf("encoded %x", got)
		}
	}
	refuse := map[string]string{
		"a266636f756e7473a262616102616201666c6162656c73a0":   "not in canonical order",
		"a266636f756e7473a2616101616102666c6162656c73a0":     `duplicate key "a"`,
		"a366636f756e7473a066636f756e7473a0666c6162656c73a0": `duplicate key "counts"`,
	}
	for in, msg := range refuse {
		data, _ := hex.DecodeString(in)
		if _, err := DecodeHolder(data); err == nil || !strings.Contains(err.Error(), msg) {
			t.Fatalf("%s: got %v, want %q", in, err, msg)
		}
	}
	data, _ := hex.DecodeString("a2666c6162656c73a066636f756e7473a0")
	if _, err := DecodeHolder(data); err != nil {
		t.Fatal(err)
	}
}
"#;
    go_test("canonical-maps", &output, test_go);
}

/// gofmt keeps a one-line function body only while the line is short; a long enum
/// name must not produce one. Every one-line body that remains is under 100 columns.
#[test]
fn long_enum_names_stay_gofmt_stable() {
    let name = "ConsentRowPolicyForApplicationsThatRequestVeryLongNamesEverywhere";
    let arms = [
        "allow",
        "deny-and-remember-this-decision-for-future-requests",
    ]
    .iter()
    .enumerate()
    .map(|(i, label)| CsilTypeExpression::Constrained {
        base_type: Box::new(CsilTypeExpression::Literal(CsilLiteralValue::Text(
            label.to_string(),
        ))),
        constraints: vec![CsilControlOperator::EnumNumber(i as i64 + 1)],
    })
    .collect();
    let rules = vec![
        rule(
            name,
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(arms)),
        ),
        record("Row", vec![entry("policy", reference(name))]),
    ];
    let output = generate("go", rules);
    for file in &output.files {
        for line in file.content.lines() {
            let one_line_body = line.starts_with("func ") && line.ends_with('}');
            assert!(!one_line_body || line.len() <= 100, "{}: {line}", file.path);
        }
    }
    assert!(all_code(&output).contains(&format!("func (v {name}) ToNumber() int64 {{\n")));
    go_test("long-enum", &output, "package api\n");
}

/// A direction marker goes in a field comment, so every struct tag stays a list of
/// `key:"value"` pairs that `go vet` accepts.
#[test]
fn visibility_metadata_keeps_struct_tags_valid() {
    let mut sent = entry("token", builtin("text"));
    sent.metadata = vec![CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly)];
    let mut received = entry("created_at", builtin("text"));
    received.metadata = vec![CsilFieldMetadata::Visibility(
        CsilFieldVisibility::ReceiveOnly,
    )];
    let output = generate("go", vec![record("Account", vec![sent, received])]);
    let types = all_code(&output);
    assert!(!types.contains('#'), "{types}");
    assert!(types.contains("// send-only"));
    assert!(types.contains("// receive-only"));
    assert!(types.contains(r#"`json:"created_at" yaml:"created_at"`"#));
    go_test("visibility", &output, "package api\n");
}

/// CDDL builtins without a Go type of their own (`float16`, `float32`, `true`,
/// `false`, `undefined`) map to types the codec reads and writes, and a value
/// survives a round trip.
#[test]
fn cddl_only_builtins_round_trip() {
    let output = generate(
        "go",
        vec![record(
            "Builtins",
            vec![
                entry("half", builtin("float16")),
                entry("single", builtin("float32")),
                entry("yes", builtin("true")),
                entry("no", builtin("false")),
                entry("nothing", builtin("undefined")),
            ],
        )],
    );
    let test_go = r#"package api

import "testing"

func TestBuiltins(t *testing.T) {
	in := Builtins{Half: 1.5, Single: 2.25, Yes: true, No: false}
	data, err := EncodeBuiltins(in)
	if err != nil {
		t.Fatal(err)
	}
	out, err := DecodeBuiltins(data)
	if err != nil {
		t.Fatal(err)
	}
	if out.Half != 1.5 || out.Single != 2.25 || !out.Yes || out.No {
		t.Fatalf("round trip gave %+v", out)
	}
}
"#;
    go_test("cddl-builtins", &output, test_go);
}

/// A rule that aliases a record is a Go alias, so the op codec passes the value to
/// the record's encoder without a conversion.
#[test]
fn record_alias_boundary_builds_and_round_trips() {
    let rules = vec![
        record("User", vec![entry("name", builtin("text"))]),
        rule("GetUserResponse", CsilRuleType::TypeDef(reference("User"))),
        service(
            "UserService",
            vec![("get-user", builtin("text"), reference("GetUserResponse"))],
        ),
    ];
    let output = generate("go", rules);
    assert!(all_code(&output).contains("type GetUserResponse = User"));
    let test_go = r#"package api

import "testing"

func TestAlias(t *testing.T) {
	data, err := EncodeUserGetUserResponse(GetUserResponse{Name: "ada"})
	if err != nil {
		t.Fatal(err)
	}
	out, err := DecodeUserGetUserResponse(data)
	if err != nil || out.Name != "ada" {
		t.Fatalf("got %+v, %v", out, err)
	}
}
"#;
    go_test("record-alias", &output, test_go);
}

/// `Foo` and `FooService` are two identifiers, so no rename happens, but dropping
/// the `Service` suffix would give both the client `FooClient`. The second keeps it.
#[test]
fn service_suffix_stays_when_dropping_it_clashes() {
    let rules = vec![
        record("Ping", vec![entry("n", builtin("int"))]),
        service("Foo", vec![("ping", reference("Ping"), reference("Ping"))]),
        service(
            "FooService",
            vec![("pong", reference("Ping"), reference("Ping"))],
        ),
    ];
    let output = generate("go-client", rules);
    assert!(output.warnings.is_empty(), "{:?}", output.warnings);
    let code = all_code(&output);
    assert!(code.contains("type FooClient struct"));
    assert!(code.contains("type FooServiceClient struct"));
    go_test("service-suffix", &output, "package api\n");
}

/// A map key with a longer head than its value needs is refused, in a dynamic map
/// and in a record; the shortest form of the same map decodes.
#[test]
fn map_keys_must_use_the_shortest_head() {
    let output = generate("go", holder_rules());
    let test_go = r#"package api

import (
	"encoding/hex"
	"strings"
	"testing"
)

func TestShortestKeys(t *testing.T) {
	for _, in := range []string{
		"a266636f756e7473a1780161" + "01" + "666c6162656c73a0",
		"a266636f756e7473a0666c6162656c73a1" + "1801" + "636f6e65",
		"a2" + "7806" + "636f756e7473" + "a0666c6162656c73a0",
		"a266636f756e7473a0666c6162656c73a1" + "1a00000001" + "636f6e65",
	} {
		data, _ := hex.DecodeString(in)
		_, err := DecodeHolder(data)
		if err == nil || !strings.Contains(err.Error(), "map key is not in shortest form") {
			t.Fatalf("%s: got %v", in, err)
		}
	}
	data, _ := hex.DecodeString("a266636f756e7473a1616101666c6162656c73a1181863746f6e")
	if _, err := DecodeHolder(data); err != nil {
		t.Fatal(err)
	}
}
"#;
    go_test("shortest-keys", &output, test_go);
}

/// Text enum values that are not identifiers (a dot, a hyphen and a space, a leading
/// digit, a keyword) get legal constants, and each value round-trips as written.
#[test]
fn text_enum_values_get_legal_constants() {
    let values = ["calendars.read", "events.write", "a-b c", "2fa", "class"];
    let arms = values
        .iter()
        .map(|v| CsilTypeExpression::Literal(CsilLiteralValue::Text(v.to_string())))
        .collect();
    let rules = vec![
        rule(
            "Scope",
            CsilRuleType::TypeDef(CsilTypeExpression::Choice(arms)),
        ),
        record("Grant", vec![entry("scope", reference("Scope"))]),
    ];
    let output = generate("go", rules);
    let code = all_code(&output);
    for constant in [
        "ScopeCalendarsRead",
        "ScopeEventsWrite",
        "ScopeABC",
        "ScopeValue2fa",
        "ScopeClass",
    ] {
        assert!(
            code.contains(&format!("\t{constant} ")),
            "{constant}: {code}"
        );
    }
    let test_go = r#"package api

import (
	"bytes"
	"testing"
)

func TestScopes(t *testing.T) {
	for _, s := range []Scope{ScopeCalendarsRead, ScopeEventsWrite, ScopeABC, ScopeValue2fa, ScopeClass} {
		data, err := EncodeGrant(Grant{Scope: s})
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Contains(data, []byte(string(s))) {
			t.Fatalf("%q is not on the wire: %x", s, data)
		}
		out, err := DecodeGrant(data)
		if err != nil || out.Scope != s {
			t.Fatalf("%q: got %+v, %v", s, out, err)
		}
	}
	if string(ScopeCalendarsRead) != "calendars.read" || string(ScopeABC) != "a-b c" {
		t.Fatal("wire values changed")
	}
}
"#;
    go_test("dotted-enum", &output, test_go);
}
