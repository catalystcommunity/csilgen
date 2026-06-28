//! Tests for the self-contained publishable-package mode (`emit_packages`): the
//! generator must emit a `go.mod` (and README) that turn the output directory into
//! a valid, `go build`-able Go module — but only when `emit_packages` includes
//! `"go"`. The build test proves the emitted module actually compiles standalone.

use csilgen_common::*;
use csilgen_go_generator::generate_go_files;
use std::collections::HashMap;

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

fn record_rule(name: &str, entries: Vec<CsilGroupEntry>) -> CsilRule {
    CsilRule {
        name: name.to_string(),
        rule_type: CsilRuleType::GroupDef(CsilGroupExpression { entries }),
        position: pos(),
        doc_comments: Vec::new(),
    }
}

/// A small but representative spec: two records and a unidirectional service, which
/// drives the type, codec, and client surfaces — enough for a meaningful `go build`.
fn echo_service_rules() -> Vec<CsilRule> {
    let service = CsilServiceDefinition {
        operations: vec![CsilServiceOperation {
            name: "ping".to_string(),
            input_type: reference("PingRequest"),
            output_type: reference("PingResponse"),
            direction: CsilServiceDirection::Unidirectional,
            position: pos(),
            doc_comments: Vec::new(),
            wire_id: None,
        }],
        wire_id: None,
    };
    vec![
        record_rule("PingRequest", vec![entry("msg", builtin("text"))]),
        record_rule("PingResponse", vec![entry("msg", builtin("text"))]),
        CsilRule {
            name: "EchoService".to_string(),
            rule_type: CsilRuleType::ServiceDef(service),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ]
}

fn opts(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn input(target: &str, options: HashMap<String, serde_json::Value>) -> WasmGeneratorInput {
    WasmGeneratorInput {
        csil_spec: CsilSpecSerialized {
            rules: echo_service_rules(),
            source_content: None,
            service_count: 1,
            fields_with_metadata_count: 0,
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
    }
}

fn find<'a>(files: &'a [GeneratedFile], path: &str) -> Option<&'a GeneratedFile> {
    files.iter().find(|f| f.path == path)
}

#[test]
fn go_mod_emitted_when_emit_packages_includes_go() {
    let options = opts(&[
        ("package_name", serde_json::json!("echoclient")),
        ("emit_packages", serde_json::json!(["go", "typescript"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();

    let go_mod = find(&files, "go.mod").expect("go.mod must be emitted when emit_packages has go");
    assert_eq!(
        go_mod.content.lines().next(),
        Some("module echoclient"),
        "module line should name the resolved module path: {}",
        go_mod.content
    );
    assert!(
        go_mod.content.contains("\ngo 1.21\n"),
        "go.mod must pin a go directive: {}",
        go_mod.content
    );
    assert!(
        find(&files, "README.md").is_some(),
        "package README should accompany go.mod"
    );
}

#[test]
fn emit_readme_false_suppresses_only_readme_in_package_mode() {
    // Default (no emit_readme): the README accompanies the go.mod.
    let default_files = generate_go_files(input(
        "go-client",
        opts(&[
            ("package_name", serde_json::json!("echoclient")),
            ("emit_packages", serde_json::json!(["go"])),
        ]),
    ))
    .unwrap();
    assert!(
        find(&default_files, "README.md").is_some(),
        "README must be emitted by default in package mode"
    );

    // Explicit opt-out: README gone, but go.mod and the source remain.
    let opted_out = generate_go_files(input(
        "go-client",
        opts(&[
            ("package_name", serde_json::json!("echoclient")),
            ("emit_packages", serde_json::json!(["go"])),
            ("emit_readme", serde_json::json!(false)),
        ]),
    ))
    .unwrap();
    assert!(
        find(&opted_out, "README.md").is_none(),
        "emit_readme: false must suppress the README"
    );
    assert!(
        find(&opted_out, "go.mod").is_some(),
        "emit_readme: false must leave go.mod untouched"
    );
    assert!(
        find(&opted_out, "client.gen.go").is_some(),
        "emit_readme: false must leave the source untouched"
    );
}

#[test]
fn go_mod_absent_when_emit_packages_missing() {
    let options = opts(&[("package_name", serde_json::json!("echoclient"))]);
    let files = generate_go_files(input("go-client", options)).unwrap();

    assert!(
        find(&files, "go.mod").is_none(),
        "go.mod must not be emitted without emit_packages"
    );
    assert!(
        find(&files, "README.md").is_none(),
        "README must not be emitted without emit_packages"
    );
    // The source itself is still produced.
    assert!(
        find(&files, "client.gen.go").is_some(),
        "source output should be unchanged when packaging is off"
    );
}

#[test]
fn go_mod_absent_when_emit_packages_excludes_go() {
    let options = opts(&[
        ("package_name", serde_json::json!("echoclient")),
        ("emit_packages", serde_json::json!(["typescript", "python"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();

    assert!(
        find(&files, "go.mod").is_none(),
        "go.mod must not be emitted when emit_packages lacks go"
    );
}

#[test]
fn emit_packages_parsed_defensively_when_not_an_array() {
    // A non-array value (here a bare string) must not crash and must not trigger
    // packaging — it is simply not an `emit_packages` array containing "go".
    let options = opts(&[
        ("package_name", serde_json::json!("echoclient")),
        ("emit_packages", serde_json::json!("go")),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();

    assert!(
        find(&files, "go.mod").is_none(),
        "a non-array emit_packages must be ignored"
    );
}

#[test]
fn go_module_option_overrides_module_path() {
    let options = opts(&[
        ("go_module", serde_json::json!("github.com/acme/echo")),
        ("package_name", serde_json::json!("echoclient")),
        ("emit_packages", serde_json::json!(["go"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();
    let go_mod = find(&files, "go.mod").unwrap();
    assert_eq!(
        go_mod.content.lines().next(),
        Some("module github.com/acme/echo")
    );
}

#[test]
fn module_path_defaults_to_example_domain_when_unset() {
    // Neither go_module nor package_name set: derive from the first service base
    // (`EchoService` -> `echo`) under the example.com domain.
    let options = opts(&[("emit_packages", serde_json::json!(["go"]))]);
    let files = generate_go_files(input("go-client", options)).unwrap();
    let go_mod = find(&files, "go.mod").unwrap();
    assert_eq!(
        go_mod.content.lines().next(),
        Some("module example.com/echo"),
        "unset coordinates should derive from the service base: {}",
        go_mod.content
    );
}

#[test]
fn path_style_package_name_splits_module_path_from_package_clause() {
    // The natural input for a `require`+`replace` consumer is a real module path.
    // It must land verbatim in go.mod's module line, while every `.go` file's
    // `package` clause uses the bare, sanitized last segment (a path there would not
    // compile).
    let options = opts(&[
        (
            "package_name",
            serde_json::json!("github.com/CatalystCommunity/corndogs/gen/corndogsapi"),
        ),
        ("emit_packages", serde_json::json!(["go"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();

    let go_mod = find(&files, "go.mod").unwrap();
    assert_eq!(
        go_mod.content.lines().next(),
        Some("module github.com/CatalystCommunity/corndogs/gen/corndogsapi"),
        "module line must keep the full path: {}",
        go_mod.content
    );

    let client = find(&files, "client.gen.go").expect("client source should be emitted");
    assert!(
        client.content.contains("package corndogsapi\n"),
        "package clause must be the sanitized last segment, not the path: {}",
        client.content
    );
    assert!(
        client.content.contains("// Package corndogsapi "),
        "package doc comment must use the bare identifier, not the path: {}",
        client.content
    );
}

#[test]
fn readme_has_quickstart_carrier_and_example() {
    let options = opts(&[
        (
            "package_name",
            serde_json::json!("github.com/CatalystCommunity/corndogs/gen/corndogsapi"),
        ),
        ("emit_packages", serde_json::json!(["go"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();
    let readme = &find(&files, "README.md")
        .expect("README should be emitted")
        .content;

    // The carrier: a type implementing the generated Transport seam (matching Call
    // signature), the CSIL-RPC POST path, the tag-24 payload, and the
    // status/ServiceError handling the envelope spec requires.
    assert!(
        readme.contains("type CsilRpcTransport struct"),
        "README must define the carrier type: {readme}"
    );
    assert!(
        readme.contains("func (t *CsilRpcTransport) Call(ctx context.Context, service, op string, req []byte) ([]byte, error)"),
        "carrier must implement the generated Transport seam: {readme}"
    );
    assert!(
        readme.contains("/csil/v1/rpc"),
        "carrier must POST to the CSIL-RPC path: {readme}"
    );
    assert!(
        readme.contains("csilTag24 = 24"),
        "carrier must wrap the payload in CBOR tag 24: {readme}"
    );
    assert!(
        readme.contains("transport status") && readme.contains("ServiceError"),
        "carrier must handle the status and ServiceError arms: {readme}"
    );
    // Path 2 (hand-rolled, dep-free) must be stated, and no third-party CBOR lib used.
    assert!(
        readme.contains("hand-roll") && readme.contains("standard library"),
        "README must state the dep-free hand-rolled path: {readme}"
    );
    assert!(
        !readme.contains("fxamacker"),
        "carrier must not pull a third-party CBOR dependency: {readme}"
    );
    // The example call: construct the client over the carrier and call the first op
    // with a generated sample literal.
    assert!(
        readme.contains("api.NewEchoClient(&CsilRpcTransport{BaseURL:"),
        "README must construct the typed client over the carrier: {readme}"
    );
    assert!(
        readme.contains("client.Ping(context.Background(), api.PingRequest{Msg: \"example\"})"),
        "README must show the example call with a sample literal: {readme}"
    );
}

/// Extract the `package main` Go program from the README's fenced code blocks.
fn extract_go_main(readme: &str) -> String {
    let mut blocks = readme.split("```go\n");
    blocks.next(); // text before the first go fence
    for seg in blocks {
        if let Some(end) = seg.find("\n```") {
            let code = &seg[..end + 1];
            if code.contains("package main") {
                return code.to_string();
            }
        }
    }
    panic!("README has no `go` package-main block:\n{readme}");
}

/// Compile the README's Quickstart carrier against the generated package and run a
/// hermetic round-trip through it with NO cross-process socket: the carrier's HTTP
/// layer is replaced by a custom `http.RoundTripper` that echoes the tag-24 inner
/// payload, so a typed `Ping` call must round-trip its field. This proves the README
/// carrier is valid against the real generated codec + client AND moves bytes
/// correctly. Skips cleanly when `go` is absent.
#[test]
fn readme_quickstart_compiles_and_round_trips() {
    if std::process::Command::new("go")
        .arg("version")
        .output()
        .is_err()
    {
        eprintln!("skipping readme_quickstart_compiles_and_round_trips: no go on PATH");
        return;
    }

    let module = "github.com/CatalystCommunity/corndogs/gen/corndogsapi";
    let options = opts(&[
        ("package_name", serde_json::json!(module)),
        ("emit_packages", serde_json::json!(["go"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();
    let readme = &find(&files, "README.md").expect("README emitted").content;
    let main_go = extract_go_main(readme);

    let dir = std::env::temp_dir().join(format!("csilgen-go-readme-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, &file.content).unwrap();
    }

    // The README program goes in a subpackage that imports the generated root package,
    // exactly as a consumer would. A sibling test file reuses the carrier's own
    // hand-rolled CBOR helpers (same `package main`) to echo the request envelope back
    // through a stub RoundTripper — no socket, so the sandbox cannot kill it.
    let cmd_dir = dir.join("cmd/quickstart");
    std::fs::create_dir_all(&cmd_dir).unwrap();
    std::fs::write(cmd_dir.join("main.go"), &main_go).unwrap();
    std::fs::write(cmd_dir.join("roundtrip_test.go"), roundtrip_test_go(module)).unwrap();

    let gocache = dir.join(".gocache");
    let out = std::process::Command::new("go")
        .arg("test")
        .arg("./...")
        .current_dir(&dir)
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "-mod=mod")
        .env("GOPROXY", "off")
        .env("GO111MODULE", "on")
        .env("GOCACHE", &gocache)
        .output()
        .expect("failed to spawn go test");

    let ok = out.status.success();
    if ok {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        ok,
        "go test (README compile + round-trip) failed in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The hermetic round-trip test dropped beside the README's `main.go`. It is in the
/// same `package main`, so it reuses the carrier's hand-rolled CBOR helpers
/// (`cborRead`, `cborHead`, `cborText`, `cborBytes`, `cborTagged`, `csilTag24`) to
/// echo the tag-24 request payload back as a `status:0` response — no real socket.
fn roundtrip_test_go(module: &str) -> String {
    format!(
        r#"package main

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"testing"

	api "{module}"
)

// echoRoundTripper decodes the CsilRpcRequest, pulls the tag-24 inner payload, and
// returns it verbatim inside a status:0 CsilRpcResponse — an in-process echo with no
// socket (the sandbox kills cross-process loopback sockets).
type echoRoundTripper struct{{}}

func (echoRoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {{
	reqBody, err := io.ReadAll(req.Body)
	if err != nil {{
		return nil, err
	}}
	pos := 0
	v, err := cborRead(reqBody, &pos)
	if err != nil {{
		return nil, err
	}}
	m, _ := v.(map[string]interface{{}})
	pl, _ := m["payload"].(cborTagged)
	inner, _ := pl.inner.([]byte)

	var resp []byte
	resp = append(resp, cborHead(5, 3)...)
	resp = append(resp, cborText("v")...)
	resp = append(resp, cborHead(0, 1)...)
	resp = append(resp, cborText("status")...)
	resp = append(resp, cborHead(0, 0)...)
	resp = append(resp, cborText("payload")...)
	resp = append(resp, cborHead(6, csilTag24)...)
	resp = append(resp, cborBytes(inner)...)
	return &http.Response{{
		StatusCode: 200,
		Body:       io.NopCloser(bytes.NewReader(resp)),
		Header:     make(http.Header),
	}}, nil
}}

func TestQuickstartRoundTrip(t *testing.T) {{
	carrier := &CsilRpcTransport{{
		BaseURL: "http://echo.invalid",
		HTTP:    &http.Client{{Transport: echoRoundTripper{{}}}},
	}}
	client := api.NewEchoClient(carrier)
	resp, err := client.Ping(context.Background(), api.PingRequest{{Msg: "hello"}})
	if err != nil {{
		t.Fatalf("Ping: %v", err)
	}}
	if resp.Msg != "hello" {{
		t.Fatalf("round-trip mismatch: got %q want %q", resp.Msg, "hello")
	}}
}}
"#
    )
}

/// Generate a `go-client` package into a temp dir and run `go build ./...` there to
/// prove the emitted output is a genuinely valid, buildable, dependency-free module.
/// Hermetic: no toolchain download, no module proxy, isolated build cache. Skips
/// cleanly when `go` is not installed so the suite stays portable.
#[test]
fn generated_module_go_builds() {
    if std::process::Command::new("go")
        .arg("version")
        .output()
        .is_err()
    {
        eprintln!("skipping generated_module_go_builds: no go on PATH");
        return;
    }

    // A path-style package_name is the dogfooding case: a real module path a
    // consumer can require+replace. The emitted go.mod must carry the full path while
    // the package clauses use the derived bare identifier, and the whole thing must
    // still compile.
    let options = opts(&[
        (
            "package_name",
            serde_json::json!("github.com/CatalystCommunity/corndogs/gen/corndogsapi"),
        ),
        ("emit_packages", serde_json::json!(["go"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();
    assert!(
        find(&files, "go.mod").is_some(),
        "build fixture must include a go.mod"
    );

    let dir = std::env::temp_dir().join(format!("csilgen-go-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, &file.content).unwrap();
    }

    let gocache = dir.join(".gocache");
    let out = std::process::Command::new("go")
        .arg("build")
        .arg("./...")
        .current_dir(&dir)
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "-mod=mod")
        .env("GOPROXY", "off")
        .env("GO111MODULE", "on")
        .env("GOCACHE", &gocache)
        .output()
        .expect("failed to spawn go build");

    let ok = out.status.success();
    if ok {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        ok,
        "go build failed in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
