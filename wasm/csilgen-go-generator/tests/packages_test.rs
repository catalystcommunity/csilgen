//! Tests for the self-contained publishable-package mode (`emit_packages`): the
//! generator must emit a `go.mod` (and a 3-transport README) that turn the output
//! directory into a valid, `go build`-able Go module — but only when `emit_packages`
//! includes `"go"`. The build/round-trip tests prove the emitted module and the README
//! Quickstart sections actually compile and move bytes against the official
//! `transports/go` library.

use csilgen_common::*;
use csilgen_go_generator::generate_go_files;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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

fn op(
    name: &str,
    input: &str,
    output: &str,
    direction: CsilServiceDirection,
) -> CsilServiceOperation {
    CsilServiceOperation {
        name: name.to_string(),
        input_type: reference(input),
        output_type: reference(output),
        direction,
        position: pos(),
        doc_comments: Vec::new(),
        wire_id: None,
    }
}

/// The canonical verification spec for the 3-transport genquickstart: two records and a
/// service with both a `->` op (`ping`) and a record-typed `<->` op (`pulse`), so the
/// RPC, Events, and Datagrams sections all render against real ops.
fn echo_service_rules() -> Vec<CsilRule> {
    let service = CsilServiceDefinition {
        operations: vec![
            op("ping", "Ping", "Pong", CsilServiceDirection::Unidirectional),
            op("pulse", "Ping", "Pong", CsilServiceDirection::Bidirectional),
        ],
        wire_id: None,
    };
    vec![
        record_rule("Ping", vec![entry("msg", builtin("text"))]),
        record_rule("Pong", vec![entry("msg", builtin("text"))]),
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

// ---------------------------------------------------------------------------
// go.mod / package-mode gating (unchanged by the 3-transport README work)
// ---------------------------------------------------------------------------

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
        find(&files, "genquickstart.md").is_some(),
        "package README should accompany go.mod"
    );
}

#[test]
fn emit_readme_false_suppresses_only_readme_in_package_mode() {
    let default_files = generate_go_files(input(
        "go-client",
        opts(&[
            ("package_name", serde_json::json!("echoclient")),
            ("emit_packages", serde_json::json!(["go"])),
        ]),
    ))
    .unwrap();
    assert!(
        find(&default_files, "genquickstart.md").is_some(),
        "README must be emitted by default in package mode"
    );

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
        find(&opted_out, "genquickstart.md").is_none(),
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
        find(&files, "genquickstart.md").is_none(),
        "README must not be emitted without emit_packages"
    );
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
}

// ---------------------------------------------------------------------------
// 3-transport genquickstart: structure + per-section unit assertions
// ---------------------------------------------------------------------------

fn transports_readme(target: &str, extra: &[(&str, serde_json::Value)]) -> String {
    let mut pairs = vec![
        ("package_name", serde_json::json!("echoclient")),
        ("emit_packages", serde_json::json!(["go"])),
    ];
    pairs.extend_from_slice(extra);
    let files = generate_go_files(input(target, opts(&pairs))).unwrap();
    find(&files, "genquickstart.md")
        .expect("genquickstart.md emitted")
        .content
        .clone()
}

/// The slice of `md` from `heading` up to the next `## ` heading (or end).
fn section<'a>(md: &'a str, heading: &str) -> &'a str {
    let start = md.find(heading).expect("section heading present");
    let rest = &md[start..];
    match rest[heading.len()..].find("\n## ") {
        Some(off) => &rest[..heading.len() + off],
        None => rest,
    }
}

/// The `go` fenced block under the given `## ` heading (the section's first block).
fn section_go_block(md: &str, heading: &str) -> String {
    let sec = section(md, heading);
    let start = sec.find("```go\n").expect("section has a go block") + "```go\n".len();
    let rest = &sec[start..];
    let end = rest.find("\n```").expect("go block is closed");
    rest[..end].to_string()
}

#[test]
fn genquickstart_has_all_three_sections_by_default() {
    let readme = transports_readme("go-client", &[]);
    for heading in [
        "## CSIL-RPC (HTTP)",
        "## CSIL-Events (TLS)",
        "## CSIL-Datagrams (UDP)",
    ] {
        assert!(
            readme.contains(heading),
            "default genquickstart must contain {heading}:\n{readme}"
        );
    }
    // The Install section pulls in the transport library alongside the package.
    assert!(readme.contains("go get github.com/catalystcommunity/csilgen/transports/go"));
}

#[test]
fn genquickstart_transports_subset_emits_only_listed_sections() {
    let readme = transports_readme(
        "go-client",
        &[("genquickstart_transports", serde_json::json!(["rpc"]))],
    );
    assert!(readme.contains("## CSIL-RPC (HTTP)"));
    assert!(
        !readme.contains("## CSIL-Events (TLS)"),
        "events section must be suppressed:\n{readme}"
    );
    assert!(
        !readme.contains("## CSIL-Datagrams (UDP)"),
        "datagrams section must be suppressed:\n{readme}"
    );
}

#[test]
fn genquickstart_transports_unknown_or_empty_falls_back_to_all() {
    for opt in [serde_json::json!([]), serde_json::json!(["bogus"])] {
        let readme = transports_readme("go-client", &[("genquickstart_transports", opt.clone())]);
        assert!(
            readme.contains("## CSIL-RPC (HTTP)")
                && readme.contains("## CSIL-Events (TLS)")
                && readme.contains("## CSIL-Datagrams (UDP)"),
            "{opt} must fall back to all three sections:\n{readme}"
        );
    }

    let readme = transports_readme(
        "go-client",
        &[(
            "genquickstart_transports",
            serde_json::json!(["datagrams", "bogus"]),
        )],
    );
    assert!(readme.contains("## CSIL-Datagrams (UDP)"));
    assert!(!readme.contains("## CSIL-RPC (HTTP)"));
    assert!(!readme.contains("## CSIL-Events (TLS)"));
}

#[test]
fn each_section_names_its_library_imports_and_seam() {
    let readme = transports_readme("go-client", &[]);
    let rpc = section(&readme, "## CSIL-RPC (HTTP)");
    let events = section(&readme, "## CSIL-Events (TLS)");
    let datagrams = section(&readme, "## CSIL-Datagrams (UDP)");

    // RPC: the library envelope types + the canonical HTTP mount, no hand-rolled CBOR.
    assert!(rpc.contains("transport \"github.com/catalystcommunity/csilgen/transports/go\""));
    assert!(rpc.contains("transport.NewRpcRequest(service, op, req).Encode()"));
    assert!(rpc.contains("transport.DecodeRpcResponse(body)"));
    assert!(rpc.contains("/csil/v1/rpc"));
    assert!(rpc.contains("resp.AsTransportError()"));
    assert!(rpc.contains("*resp.Variant == \"ServiceError\""));
    assert!(rpc.contains("api.NewEchoClient(&HTTPRpcCarrier{BaseURL:"));
    assert!(rpc.contains("client.Ping(context.Background(), api.Ping{Msg: \"example\"})"));
    assert!(
        !readme.contains("hand-roll"),
        "the lib-based carrier must not hand-roll CBOR:\n{readme}"
    );

    // Events: the lib's handshake/framing/heartbeat surface + the generated channel
    // router. Inbound (the router decodes the op input, Ping) rides a codec adapter
    // over the per-type codec; outbound (the op success output, Pong) rides the
    // generated encoder; dispatch goes through Route<Service>Channel — not codec-direct.
    assert!(events.contains("transport.NewStreamCarrier(conn)"));
    assert!(events.contains("transport.Hello{"));
    assert!(events.contains("$hello"));
    assert!(events.contains("transport.DecodeHelloAck(ackFrame)"));
    assert!(events.contains("transport.PingName"));
    assert!(events.contains("transport.PongName"));
    assert!(
        events.contains("api.EncodeEchoServicePulse(codec, api.Pong{Msg: \"example\"})"),
        "outbound must ride the generated encoder:\n{events}"
    );
    assert!(
        events.contains("api.RouteEchoServiceChannel(handlers, ctx, codec, *ev.Event, ev.Payload)"),
        "inbound dispatch must go through the generated channel router:\n{events}"
    );
    assert!(
        events.contains("api.EchoService"),
        "the handler must implement the generated service interface:\n{events}"
    );
    assert!(
        events.contains("api.DecodePing(data)") && events.contains("api.EncodePong(v)"),
        "the codec adapter must bridge the per-type codec:\n{events}"
    );
    assert!(
        !events.contains("api.DecodePong(ev.Payload)"),
        "the Events section must not decode payloads directly anymore:\n{events}"
    );

    // Datagrams: the lib's Datagram + carrier seam, and the no-sync-response warning.
    assert!(datagrams.contains("transport.NewUDPDatagramCarrier(conn)"));
    assert!(datagrams.contains("transport.NewDatagram(opOrd, 0, api.EncodePing(req)).Encode()"));
    assert!(datagrams.contains("transport.DecodeDatagram(inbound)"));
    assert!(datagrams.contains("api.DecodePong(dg.Payload)"));
    assert!(datagrams.contains("NO synchronous response"));
}

#[test]
fn rpc_section_renders_for_server_target_in_package_mode() {
    // Package mode emits every surface (client + server) regardless of the requested
    // target — mirroring OCaml — so even the `go` server target's genquickstart carries
    // a working typed RPC client example rather than a pointer to `go-client`.
    let readme = transports_readme("go", &[]);
    let rpc = section(&readme, "## CSIL-RPC (HTTP)");
    assert!(
        rpc.contains("api.NewEchoClient(") && !rpc.contains("no typed RPC client"),
        "server-target RPC section must render the typed client in package mode:\n{rpc}"
    );
    assert!(readme.contains("## CSIL-Events (TLS)"));
    assert!(readme.contains("## CSIL-Datagrams (UDP)"));
}

#[test]
fn package_mode_emits_both_client_and_server_surfaces() {
    // The genquickstart's RPC/Datagrams sections ride the client surface and its Events
    // section rides the server-side channel router, so a self-contained package must
    // carry both — for either requested target.
    for target in ["go-client", "go"] {
        let files = generate_go_files(input(
            target,
            opts(&[
                ("package_name", serde_json::json!("echoclient")),
                ("emit_packages", serde_json::json!(["go"])),
            ]),
        ))
        .unwrap();
        assert!(
            find(&files, "client.gen.go").is_some(),
            "{target}: package mode must emit the client surface"
        );
        assert!(
            find(&files, "services.gen.go").is_some(),
            "{target}: package mode must emit the server/router surface"
        );
    }
}

#[test]
fn flat_mode_emits_only_requested_surface() {
    // Without emit_packages the output stays byte-identical: the requested target's
    // single surface only — `go-client` emits the client, `go` emits the services.
    let client = generate_go_files(input(
        "go-client",
        opts(&[("package_name", serde_json::json!("echoclient"))]),
    ))
    .unwrap();
    assert!(find(&client, "client.gen.go").is_some());
    assert!(
        find(&client, "services.gen.go").is_none(),
        "flat go-client must not emit the server surface"
    );

    let server = generate_go_files(input(
        "go",
        opts(&[("package_name", serde_json::json!("echoclient"))]),
    ))
    .unwrap();
    assert!(find(&server, "services.gen.go").is_some());
    assert!(
        find(&server, "client.gen.go").is_none(),
        "flat go server must not emit the client surface"
    );
}

#[test]
fn events_section_without_channel_ops_emits_a_note() {
    // A spec with only a `->` op keeps the handshake but replaces typed dispatch with a
    // note (no generated-codec decode of a channel event).
    let rules = vec![
        record_rule("Ping", vec![entry("msg", builtin("text"))]),
        record_rule("Pong", vec![entry("msg", builtin("text"))]),
        CsilRule {
            name: "EchoService".to_string(),
            rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                operations: vec![op(
                    "ping",
                    "Ping",
                    "Pong",
                    CsilServiceDirection::Unidirectional,
                )],
                wire_id: None,
            }),
            position: pos(),
            doc_comments: Vec::new(),
        },
    ];
    let mut inp = input(
        "go-client",
        opts(&[
            ("package_name", serde_json::json!("echoclient")),
            ("emit_packages", serde_json::json!(["go"])),
        ]),
    );
    inp.csil_spec.rules = rules;
    let files = generate_go_files(inp).unwrap();
    let readme = &find(&files, "genquickstart.md").unwrap().content;
    let events = section(readme, "## CSIL-Events (TLS)");
    assert!(events.contains("$hello"));
    assert!(
        events.contains("no <->/<- operations"),
        "must note the absence of channel ops:\n{events}"
    );
    assert!(
        !events.contains("api.Decode"),
        "no typed channel decode when there are no channel ops:\n{events}"
    );
}

// ---------------------------------------------------------------------------
// Hermetic execution of the genquickstart examples (go, in-process loopback)
// ---------------------------------------------------------------------------

fn have_go() -> bool {
    std::process::Command::new("go")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The absolute path to the in-repo `transports/go` library, for the staged module's
/// `replace` directive.
fn transport_lib_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../transports/go")
        .canonicalize()
        .expect("transports/go must exist")
}

/// Stage the generated go-client package plus the three README programs into a fresh
/// temp module, repointing the `csilgen-transport` import at the local library via a
/// `replace` directive. Returns the module dir.
fn stage_transports_module(module: &str) -> PathBuf {
    let options = opts(&[
        ("package_name", serde_json::json!(module)),
        ("emit_packages", serde_json::json!(["go"])),
    ]);
    let files = generate_go_files(input("go-client", options)).unwrap();
    let readme = find(&files, "genquickstart.md").unwrap().content.clone();

    let dir = std::env::temp_dir().join(format!("csilgen-go-3t-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for file in &files {
        if file.path == "go.mod" {
            // Rewrite the staged go.mod to require the (unpublished) transport library
            // via a local `replace`, and bump the go directive to satisfy the library's
            // own. The generated go.mod (tested elsewhere) is untouched.
            let lib = transport_lib_dir();
            let content = format!(
                "module {module}\n\ngo 1.26.3\n\n\
                 require github.com/catalystcommunity/csilgen/transports/go v0.0.0\n\n\
                 replace github.com/catalystcommunity/csilgen/transports/go => {}\n",
                lib.display()
            );
            std::fs::write(dir.join("go.mod"), content).unwrap();
            continue;
        }
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, &file.content).unwrap();
    }

    // Each README section is dropped as its own command package, with a sibling
    // in-process test for the runnable ones (RPC + Datagrams).
    let rpc = section_go_block(&readme, "## CSIL-RPC (HTTP)");
    let events = section_go_block(&readme, "## CSIL-Events (TLS)");
    let datagrams = section_go_block(&readme, "## CSIL-Datagrams (UDP)");

    write_cmd(&dir, "rpc", &rpc, Some(&rpc_roundtrip_test_go(module)));
    write_cmd(&dir, "events", &events, None);
    write_cmd(
        &dir,
        "datagrams",
        &datagrams,
        Some(&datagrams_loopback_test_go(module)),
    );
    dir
}

fn write_cmd(dir: &Path, name: &str, main_go: &str, test_go: Option<&str>) {
    let cmd = dir.join("cmd").join(name);
    std::fs::create_dir_all(&cmd).unwrap();
    std::fs::write(cmd.join("main.go"), main_go).unwrap();
    if let Some(test) = test_go {
        std::fs::write(cmd.join(format!("{name}_test.go")), test).unwrap();
    }
}

fn run_go_test(dir: &Path) -> std::process::Output {
    let gocache = dir.join(".gocache");
    std::process::Command::new("go")
        .arg("test")
        .arg("./...")
        .current_dir(dir)
        .env("GOTOOLCHAIN", "local")
        .env("GOFLAGS", "-mod=mod")
        .env("GOPROXY", "off")
        .env("GO111MODULE", "on")
        .env("GOCACHE", &gocache)
        .output()
        .expect("failed to spawn go test")
}

/// `go test ./...` over the staged module: it compiles all three README programs
/// (CSIL-Events is interactive/socket-driven, so this is its compile-check) and *runs*
/// the in-process RPC and Datagrams round-trips. A green run proves every section is
/// valid Go against the real generated package + the `transports/go` library, and that
/// the runnable carriers move bytes correctly. Hermetic (no sockets). Skips without go.
#[test]
fn genquickstart_sections_compile_and_round_trip() {
    if !have_go() {
        eprintln!("skipping genquickstart_sections_compile_and_round_trip: no go on PATH");
        return;
    }
    let module = "github.com/CatalystCommunity/corndogs/gen/corndogsapi";
    let dir = stage_transports_module(module);
    let out = run_go_test(&dir);
    let ok = out.status.success();
    if ok {
        let _ = std::fs::remove_dir_all(&dir);
    }
    assert!(
        ok,
        "go test (3-transport README compile + round-trip) failed in {}:\n{}{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The hermetic CSIL-RPC round-trip dropped beside the README's RPC `main.go`. It drives
/// the emitted `HTTPRpcCarrier` through an in-process `http.RoundTripper` echo built on
/// the library's `RpcRequest`/`RpcResponse` — no real socket — so a typed `Ping` call
/// must round-trip its field through the generated codec + the carrier.
fn rpc_roundtrip_test_go(module: &str) -> String {
    format!(
        r#"package main

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"testing"

	transport "github.com/catalystcommunity/csilgen/transports/go"
	api "{module}"
)

type echoRoundTripper struct{{}}

func (echoRoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {{
	body, err := io.ReadAll(req.Body)
	if err != nil {{
		return nil, err
	}}
	rpcReq, err := transport.DecodeRpcRequest(body)
	if err != nil {{
		return nil, err
	}}
	respBytes, err := transport.NewRpcResponseOk("Pong", rpcReq.Payload).Encode()
	if err != nil {{
		return nil, err
	}}
	return &http.Response{{
		StatusCode: 200,
		Body:       io.NopCloser(bytes.NewReader(respBytes)),
		Header:     make(http.Header),
	}}, nil
}}

func TestRPCRoundTrip(t *testing.T) {{
	carrier := &HTTPRpcCarrier{{
		BaseURL: "http://echo.invalid",
		HTTP:    &http.Client{{Transport: echoRoundTripper{{}}}},
	}}
	client := api.NewEchoClient(carrier)
	resp, err := client.Ping(context.Background(), api.Ping{{Msg: "hello"}})
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

/// The hermetic CSIL-Datagrams round-trip dropped beside the README's datagrams
/// `main.go`. It seeds the library's in-process `LoopbackDatagramCarrier` with a response
/// datagram, drives the emitted `runDatagrams` over it (no socket), then decodes the
/// datagram it sent to prove the generated codec + the lib `Datagram` envelope round-trip.
fn datagrams_loopback_test_go(module: &str) -> String {
    format!(
        r#"package main

import (
	"testing"

	transport "github.com/catalystcommunity/csilgen/transports/go"
	api "{module}"
)

func TestDatagramsRoundTrip(t *testing.T) {{
	lb := transport.NewLoopbackDatagramCarrier()
	seed, err := transport.NewDatagram(opOrd, 0, api.EncodePong(api.Pong{{Msg: "late"}})).Encode()
	if err != nil {{
		t.Fatalf("seed encode: %v", err)
	}}
	lb.PushInbound(seed)

	if err := runDatagrams(lb); err != nil {{
		t.Fatalf("runDatagrams: %v", err)
	}}

	out := lb.TakeOutbound()
	if out == nil {{
		t.Fatal("no datagram was sent")
	}}
	dg, err := transport.DecodeDatagram(out)
	if err != nil {{
		t.Fatalf("decode sent: %v", err)
	}}
	req, err := api.DecodePing(dg.Payload)
	if err != nil {{
		t.Fatalf("decode payload: %v", err)
	}}
	if req.Msg != "example" {{
		t.Fatalf("sent payload mismatch: got %q want %q", req.Msg, "example")
	}}
}}
"#
    )
}

/// Generate a `go-client` package and run `go build ./...` to prove the emitted output
/// is a genuinely valid, buildable, dependency-free module. Hermetic. Skips without go.
#[test]
fn generated_module_go_builds() {
    if !have_go() {
        eprintln!("skipping generated_module_go_builds: no go on PATH");
        return;
    }

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
