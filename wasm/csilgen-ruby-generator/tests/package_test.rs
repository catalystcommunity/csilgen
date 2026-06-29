//! Tests for the self-contained publishable RubyGem package mode (the `emit_packages`
//! option). One test asserts the gemspec is emitted iff `"ruby"` is requested; the
//! other proves the emitted directory is a buildable gem by running `gem build`.

mod common;

use common::*;
use csilgen_common::*;
use serde_json::json;
use std::collections::HashMap;
use std::process::Command;

fn package_config(target: &str, options: HashMap<String, serde_json::Value>) -> GeneratorConfig {
    GeneratorConfig {
        target: target.to_string(),
        output_dir: "/tmp/csilgen-ruby-pkg".to_string(),
        options,
    }
}

fn sample_spec() -> CsilSpecSerialized {
    spec(vec![
        group_rule(
            "user",
            vec![
                bare_entry("name", builtin("text")),
                bare_entry("id", builtin("int")),
            ],
        ),
        service_rule(
            "user_service",
            vec![op(
                "get-user",
                reference("user"),
                reference("user"),
                CsilServiceDirection::Unidirectional,
            )],
            None,
        ),
    ])
}

#[test]
fn gemspec_emitted_only_when_emit_packages_includes_ruby() {
    let s = sample_spec();

    // No option at all: the flat, non-package layout is unchanged.
    let plain =
        generate_ruby_code_from_serialized(&s, &package_config("ruby-client", HashMap::new()))
            .expect("generation succeeded");
    assert!(!plain.iter().any(|f| f.path.ends_with(".gemspec")));
    assert!(plain.iter().any(|f| f.path == "types.rb"));
    // The README rides with the gem packaging, never the flat layout.
    assert!(!plain.iter().any(|f| f.path == "genquickstart.md"));

    // emit_packages present but without "ruby": still unchanged.
    let mut other = HashMap::new();
    other.insert("emit_packages".to_string(), json!(["go", "python"]));
    let unaffected = generate_ruby_code_from_serialized(&s, &package_config("ruby-client", other))
        .expect("generation succeeded");
    assert!(!unaffected.iter().any(|f| f.path.ends_with(".gemspec")));
    assert!(unaffected.iter().any(|f| f.path == "types.rb"));

    // emit_packages includes "ruby": a gem layout with the configured name/version.
    let mut opts = HashMap::new();
    opts.insert("emit_packages".to_string(), json!(["ruby"]));
    opts.insert("package_name".to_string(), json!("acme_client"));
    opts.insert("package_version".to_string(), json!("1.2.3"));
    let pkg = generate_ruby_code_from_serialized(&s, &package_config("ruby-client", opts))
        .expect("generation succeeded");

    let gemspec = pkg
        .iter()
        .find(|f| f.path == "acme_client.gemspec")
        .expect("gemspec emitted");
    assert!(gemspec.content.contains("spec.name = \"acme_client\""));
    assert!(gemspec.content.contains("spec.version = \"1.2.3\""));
    assert!(gemspec.content.contains("spec.require_paths = [\"lib\"]"));

    // Generated sources relocate under lib/, and the entry point requires them.
    assert!(pkg.iter().any(|f| f.path == "lib/types.rb"));
    assert!(pkg.iter().any(|f| f.path == "lib/client.rb"));

    // Package mode also ships a root README with the copy-paste Quickstart carrier.
    let readme = pkg
        .iter()
        .find(|f| f.path == "genquickstart.md")
        .expect("genquickstart.md emitted in package mode");
    assert!(readme.content.contains("class HttpRpcTransport"));
    assert!(readme.content.contains("/csil/v1/rpc"));
    let entry = pkg
        .iter()
        .find(|f| f.path == "lib/acme_client.rb")
        .expect("entry point emitted");
    assert!(entry.content.contains("require_relative \"types\""));
    assert!(entry.content.contains("require_relative \"client\""));

    // The non-package layout must not leak into package mode (no bare root sources).
    assert!(!pkg.iter().any(|f| f.path == "types.rb"));
}

#[test]
fn defaults_apply_when_name_and_version_absent() {
    let s = sample_spec();
    let mut opts = HashMap::new();
    opts.insert("emit_packages".to_string(), json!(["ruby"]));
    // output_dir basename "csilgen-ruby-pkg" -> sanitized gem name.
    let pkg = generate_ruby_code_from_serialized(&s, &package_config("ruby-client", opts))
        .expect("generation succeeded");
    let gemspec = pkg
        .iter()
        .find(|f| f.path.ends_with(".gemspec"))
        .expect("gemspec emitted");
    assert_eq!(gemspec.path, "csilgen_ruby_pkg.gemspec");
    assert!(gemspec.content.contains("spec.name = \"csilgen_ruby_pkg\""));
    assert!(gemspec.content.contains("spec.version = \"0.1.0\""));
}

#[test]
fn ruby_client_package_builds_a_gem() {
    // ruby/gem may be absent in some CI images; skip rather than fail there.
    let gem_present = Command::new("gem")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !gem_present {
        eprintln!("skipping ruby_client_package_builds_a_gem: `gem` not available");
        return;
    }

    let s = sample_spec();
    let mut opts = HashMap::new();
    opts.insert("emit_packages".to_string(), json!(["ruby"]));
    opts.insert("package_name".to_string(), json!("csilgen_demo"));
    opts.insert("package_version".to_string(), json!("0.2.0"));
    let files = generate_ruby_code_from_serialized(&s, &package_config("ruby-client", opts))
        .expect("generation succeeded");

    let dir = std::env::temp_dir().join(format!("csilgen-ruby-pkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for f in &files {
        let path = dir.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap()).expect("create package subdir");
        std::fs::write(&path, &f.content).expect("write package file");
    }

    let build = Command::new("gem")
        .arg("build")
        .arg("csilgen_demo.gemspec")
        .current_dir(&dir)
        .output()
        .expect("run gem build");
    assert!(
        build.status.success(),
        "gem build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(
        dir.join("csilgen_demo-0.2.0.gem").exists(),
        "built gem artifact should exist"
    );

    // Optional load check: the entry point requires cleanly under the lib path.
    if let Ok(load) = Command::new("ruby")
        .arg("-Ilib")
        .arg("-e")
        .arg("require 'csilgen_demo'")
        .current_dir(&dir)
        .output()
    {
        assert!(
            load.status.success(),
            "ruby require failed:\nstderr: {}",
            String::from_utf8_lossy(&load.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
