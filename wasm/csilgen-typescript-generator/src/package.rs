//! Self-contained, publishable npm-package emission.
//!
//! When `emit_packages` includes `"typescript"`, the generator turns the output
//! directory into a buildable npm package: a barrel `index.ts`, a `package.json`,
//! and a `tsconfig.json`. The package is CommonJS because the generated `.gen.ts`
//! modules import each other with extension-less relative specifiers
//! (`./types.gen`), which only resolve under `node`-style module resolution; an
//! ESM (`nodenext`) package would require `.js` suffixes the emitters do not
//! write.

use crate::common;
use csilgen_common::{GeneratedFile, WasmGeneratorInput};
use std::collections::BTreeSet;

/// Whether the consumer asked for a TypeScript package. The option is a JSON
/// array of target ids; a lone string is accepted too so a hand-written config
/// (`"emit_packages": "typescript"`) still works. Anything else means "no".
pub fn wants_typescript_package(input: &WasmGeneratorInput) -> bool {
    match input.config.options.get("emit_packages") {
        Some(serde_json::Value::Array(items)) => {
            items.iter().any(|v| v.as_str() == Some("typescript"))
        }
        Some(serde_json::Value::String(s)) => s == "typescript",
        _ => false,
    }
}

/// Append the package scaffolding (barrel + `package.json` + `tsconfig.json`) to
/// the already-generated module files. A no-op unless `emit_packages` opts in.
pub fn augment(input: &WasmGeneratorInput, files: &mut Vec<GeneratedFile>) {
    if !wants_typescript_package(input) {
        return;
    }

    let barrel = barrel(input, files);
    files.push(GeneratedFile {
        path: "index.ts".to_string(),
        content: barrel,
    });
    files.push(GeneratedFile {
        path: "package.json".to_string(),
        content: package_json(input),
    });
    files.push(GeneratedFile {
        path: "tsconfig.json".to_string(),
        content: TSCONFIG.to_string(),
    });
}

/// The npm package name. An explicit `package_name` wins; otherwise it is derived
/// from the first service's base name (`AuthService` -> `auth-client`), falling
/// back to `csilgen-client` for a service-less spec.
fn package_name(input: &WasmGeneratorInput) -> String {
    if let Some(name) = option_string(input, "package_name") {
        return name;
    }
    let services = common::sorted_services(&input.csil_spec);
    if let Some((name, _)) = services.first() {
        let base = common::service_base(name).to_lowercase();
        if !base.is_empty() {
            return format!("{base}-client");
        }
    }
    "csilgen-client".to_string()
}

fn package_version(input: &WasmGeneratorInput) -> String {
    option_string(input, "package_version").unwrap_or_else(|| "0.1.0".to_string())
}

/// A non-empty string option, trimmed. Returns `None` for a missing, blank, or
/// non-string value so callers fall back to their derived default.
fn option_string(input: &WasmGeneratorInput, key: &str) -> Option<String> {
    input
        .config
        .options
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn package_json(input: &WasmGeneratorInput) -> String {
    let name = package_name(input);
    let version = package_version(input);
    let value = serde_json::json!({
        "name": name,
        "version": version,
        // CommonJS: the emitted modules use extension-less relative imports, which
        // require `node` module resolution rather than ESM.
        "type": "commonjs",
        "main": "dist/index.js",
        "types": "dist/index.d.ts",
        "exports": {
            ".": {
                "types": "./dist/index.d.ts",
                "require": "./dist/index.js",
                "default": "./dist/index.js"
            }
        },
        "files": ["dist"],
        "scripts": {
            "build": "tsc"
        },
        "devDependencies": {
            "typescript": "^5"
        }
    });
    // Pretty-print with a trailing newline so the file reads like a hand-authored one.
    format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
}

/// Strict, declaration-emitting config matching the CommonJS modules the
/// generator writes. `*.ts` picks up the barrel plus every `*.gen.ts`.
const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "es2020",
    "module": "commonjs",
    "moduleResolution": "node",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "declaration": true,
    "outDir": "dist",
    "lib": ["es2020", "dom"]
  },
  "include": ["*.ts"]
}
"#;

/// Re-export every generated module from a single entry point. Modules are
/// re-exported via `export *` where their names are unique; a module that would
/// reintroduce an already-exported name (client and server both export `Codec`)
/// falls back to an explicit named re-export of only its fresh names, since TS
/// rejects ambiguous star re-exports (TS2308).
fn barrel(input: &WasmGeneratorInput, files: &[GeneratedFile]) -> String {
    let mut out = common::header(input, "index");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // Fixed order keeps the barrel deterministic and lets `types`/`codec` claim
    // the shared names before the client/server surfaces do.
    for path in [
        "types.gen.ts",
        "codec.gen.ts",
        "client.gen.ts",
        "server.gen.ts",
    ] {
        let Some(file) = files.iter().find(|f| f.path == path) else {
            continue;
        };
        let stem = path.strip_suffix(".ts").unwrap_or(path);
        let names = exported_names(&file.content);
        if names.iter().any(|n| seen.contains(n)) {
            let mut fresh: Vec<String> = Vec::new();
            for n in &names {
                if !seen.contains(n) && !fresh.contains(n) {
                    fresh.push(n.clone());
                }
            }
            if !fresh.is_empty() {
                out.push_str(&format!(
                    "export {{ {} }} from \"./{stem}\";\n",
                    fresh.join(", ")
                ));
            }
        } else {
            out.push_str(&format!("export * from \"./{stem}\";\n"));
        }
        for n in names {
            seen.insert(n);
        }
    }
    out
}

/// Top-level exported binding names in a generated module. A line-anchored scan
/// (only `export <kw> <name>` at the start of a line) avoids false positives,
/// which matter because a name listed in an explicit re-export must really exist
/// or TS errors; a missed name merely goes un-re-exported.
fn exported_names(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.trim_start().strip_prefix("export ") else {
            continue;
        };
        let rest = rest.strip_prefix("async ").unwrap_or(rest);
        let after = [
            "interface ",
            "type ",
            "class ",
            "function ",
            "const ",
            "enum ",
        ]
        .iter()
        .find_map(|kw| rest.strip_prefix(kw));
        let Some(after) = after else {
            continue;
        };
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
    }
    out
}
