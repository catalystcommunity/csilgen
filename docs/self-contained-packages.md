# Self-contained publishable packages (`emit_packages`)

By default each generator emits only source files (`types.gen.*`, `codec.gen.*`,
`client.gen.*`, …). Set the `emit_packages` option to also emit the package
manifest(s) and any layout a language needs, so the **output directory is a valid,
publishable package** in that language's ecosystem — push it to a git repo and a
consumer can depend on it directly (or publish it to the registry).

## The option

`emit_packages` is a **per-language list**, so one `.csil` shared across many target
languages can select which ones get packaged. Each target emits its package files
only when its language token is in the list; otherwise its output is unchanged
(source only). Set it in the CSIL `options {}` block (entries are comma-separated):

```csil
options {
  emit_packages: ["go", "typescript", "rust"],
  go_module: "github.com/acme/corndogs-client",   // Go module path
  package_name: "corndogs_client",                // crate/gem/npm/pub/… name
  package_version: "0.1.0"
}
```

Language tokens: `go`, `rust`, `typescript`, `python`, `ruby`, `elixir`, `java`,
`kotlin`, `csharp`, `ocaml`, `swift`, `dart`. (C and Zig — no central registry — are
a separate follow-up.)

## Coordinates

| Option | Used for | Default |
| --- | --- | --- |
| `package_name` | crate / gem / npm / pub / opam / namespace name | derived from the service base, else `csilgen_client` |
| `package_version` | manifest version | `0.1.0` |
| `go_module` | Go module path (required for a real import path) | `package_name`, else `example.com/<name>` |
| `java_package`, `kotlin_package` | groupId / Gradle group | existing config |

## What each target emits (in package mode)

| Target | Manifest(s) | Layout | Verified by |
| --- | --- | --- | --- |
| go | `go.mod` (+ `README.md`) | flat root module | `go build` |
| rust | `Cargo.toml` (+ `src/lib.rs`) | sources under `src/` | `cargo build` (offline) |
| typescript | `package.json`, `tsconfig.json` | barrel `index.ts` | `tsc` |
| python | `pyproject.toml` | package dir + `__init__.py` | import + parse |
| ruby | `<name>.gemspec` | sources under `lib/` | `gem build` |
| elixir | `mix.exs` | sources under `lib/` | `mix compile` |
| java | `pom.xml` | `src/main/java/<pkg>/` | well-formed XML + `javac` |
| kotlin | `build.gradle.kts`, `settings.gradle.kts` | `src/main/kotlin/<pkg>/` | (no kotlinc here — unit-tested) |
| csharp | `<name>.csproj` | flat (SDK glob) | `dotnet build` |
| ocaml | `dune-project`, `<name>.opam`, `lib/dune` | sources under `lib/` | `dune build` |
| swift | `Package.swift` | `Sources/<Target>/` | (no swiftc here — unit-tested) |
| dart | `pubspec.yaml` | sources under `lib/` | `dart pub get` + `analyze` |

Every package is **dependency-free** (the generated codec owns the wire), so it
builds with no third-party deps beyond what the generated code already uses (e.g.
`chrono` only when a Rust spec uses `timestamp`). The default (no `emit_packages`)
output is byte-for-byte unchanged.
