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
