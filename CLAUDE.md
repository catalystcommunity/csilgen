# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is `csilgen`, a library and CLI tool for implementing CBOR Service Interface Language (CSIL), an aspiring interface definition language that extends beyond what CDDL provides. The project uses a Rust workspace with multiple crates organized as a monorepo.

## Architecture

The architecture follows a plugin-based approach similar to protocgen for protobufs:

- **Core**: `csilgen-core` — CSIL parsing, validation, AST.
- **Common**: `csilgen-common` — shared types, errors, and the WASM boundary types (`CsilSpecSerialized`, `WasmGeneratorInput`, `WasmGeneratorOutput`).
- **CLI**: `csilgen-cli` — the `csilgen` command-line tool.
- **Runtime**: `csilgen-wasm-generators` — discovers and runs generator WASM modules.
- **Generators**: each language target is **exactly one `cdylib` crate under `wasm/`** named `csilgen-<target>-generator`, producing `csilgen_<target>_generator.wasm`. There is no parallel "library" copy of any generator — the wasm crate is the single source of truth.

### One pattern, no duplicates

Every generator lives in `wasm/` and follows the same shape: a `cdylib` exporting `get_metadata`, `allocate`, `deallocate`, and `generate`. **Do not create a `lib` twin in `crates/` or anywhere else** — that's how `csilgen-rust` and `csilgen-typescript` drifted in the past, with features authored in the lib copy ("…like one does") then mirrored to the wasm copy ("also apply to the wasm rust generator") and inevitably falling out of sync. There is one crate per generator. Edits land where the code runs.

`crates/csilgen-common` carries the classification/rewrite logic that is genuinely shared across every generator rather than per-language — the choice-arm classifier (`choice.rs`: literal-vs-general arm classification, enum-vs-union wire shape) and the inline-composite hoister (`hoist.rs`: synthesizing named rules for anonymous groups/choices). A generator must call into these, not re-derive its own copy of either — that reintroduces exactly the per-generator drift this section exists to prevent (see `choice.rs`'s module docs for a real historical bug: TypeScript and OCaml each grew their own, subtly wrong, literal-choice classifier before this module existed).

### Dynamic generator discovery

The CLI and runtime do not maintain any hardcoded list of generators. At startup the runtime scans, in priority order:

1. `target/wasm32-unknown-unknown/release` — local dev build output (only meaningful inside the csilgen workspace).
2. `./.generators` — a project-local pin/override.
3. `~/.csilgen/generators` — the user's installed baseline.

Discovery is **first-write-wins**: the first directory in this list that supplies a given generator id keeps it; later directories with the same id are ignored. That means a project can drop a specific build of a generator into `.generators/` to pin or replace whatever the user has installed system-wide, without touching `~/.csilgen/generators`. Files that don't match `csilgen_<target>_generator.wasm` are silently ignored everywhere.

`--target <name>` resolves by finding the discovered generator whose target equals `<name>` (or that `<name>` is a sub-target of, e.g. `typescript-client` → the `typescript` generator). A third-party generator self-registers simply by being named correctly and dropped in any search path — no CLI patch needed.

See `docs/generator-plugin-contract.md` for the normative, language-agnostic plugin interface (WASM ABI, JSON boundary shapes, conformance tiers) every generator implements.

### Other rules

- **No async.** Ever. Concurrency uses threads.
- **Sandboxed by construction.** WASM modules have no direct filesystem access.

## Best Practices
- We do not use async code. Ever. If we need concurrency, we do so with threads.
- We create tests for everything we add, and we make sure all tests for the entire repo pass _before_ we mark any task completed or consider our work on a task or feature to be completed
- We do not reach for unsafe code, and are careful to include any dependencies that are unsafe
- We only comment code with _why_ it does things the way that it does as opposed to other ways, and never _what_ the code is doing
- We use variables inside format strings per clippy's warning: "variables can be used directly in the `format!` string"

## Project Structure

```
csilgen/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── csilgen-core/            # CSIL parsing, validation, AST
│   ├── csilgen-cli/             # Command-line interface
│   └── csilgen-common/          # Shared types (incl. the WASM boundary types)
├── wasm/                        # All generators + the runtime live here
│   ├── csilgen-wasm-core/             # Core types/helpers compiled to wasm
│   ├── csilgen-wasm-generators/       # Discovery + execution runtime
│   ├── csilgen-noop-generator/        # No-op fixture
│   ├── csilgen-simple-test/           # Internal runtime test fixture (not a target)
│   ├── csilgen-rust-generator/        # --target rust (+ -typesonly/-client/-server)
│   ├── csilgen-go-generator/          # --target go (+ -typesonly/-client/-server)
│   ├── csilgen-typescript-generator/  # --target typescript (+ typescript-* sub-targets)
│   ├── csilgen-python-generator/      # --target python (+ -typesonly/-client/-server)
│   ├── csilgen-php-generator/         # --target php (+ -typesonly/-client/-server)
│   ├── csilgen-java-generator/        # --target java
│   ├── csilgen-csharp-generator/      # --target csharp (+ -client/-server)
│   ├── csilgen-c-generator/           # --target c
│   ├── csilgen-swift-generator/       # --target swift
│   ├── csilgen-kotlin-generator/      # --target kotlin
│   ├── csilgen-zig-generator/         # --target zig
│   ├── csilgen-ocaml-generator/       # --target ocaml
│   ├── csilgen-elixir-generator/      # --target elixir
│   ├── csilgen-ruby-generator/        # --target ruby
│   ├── csilgen-dart-generator/        # --target dart
│   ├── csilgen-json-generator/        # --target json
│   └── csilgen-openapi-generator/     # --target openapi
├── docs/csilgen-requests/        # Inbox for requests from consumer repos
├── examples/                     # Usage examples and demos
└── tools/xtask/                  # Development automation
```

Every entry under `wasm/csilgen-*-generator/` is a `cdylib`. No generator has a parallel lib crate.

## Development Commands

### Standard Rust Workspace Commands
- `cargo build --workspace` - Build all crates
- `cargo test --workspace` - Run tests for all crates
- `cargo clippy --workspace --all-targets -- -D warnings` - Lint all code
- `cargo fmt --all` - Format all code

### Using the xtask Automation Tool
- `cargo run -p xtask build` - Build all crates
- `cargo run -p xtask test` - Run all tests
- `cargo run -p xtask clippy` - Run clippy linting
- `cargo run -p xtask fmt` - Format code
- `cargo run -p xtask build-wasm` - Build WASM modules
- `cargo run -p xtask install-wasm` - Build and install WASM modules to ~/.csilgen/generators/

### CLI Usage
- `cargo run -p csilgen validate --input interface.csil` - Validate a CSIL file
- `cargo run -p csilgen generate --input interface.csil --target rust --output ./generated/` - Generate code
- `cargo run -p csilgen breaking --current A.csil --new B.csil` - Compare breaking changes betwen A and B, useful for change management
- `cargo run -p csilgen format path/to/dir/ --dry-run` - format a directory of files to the official style guide
- `cargo run -p csilgen lint path/to/dir/ --fix` - lint a directory of files to the official lint rules

### Installation
- `cargo install --path crates/csilgen-cli` - Install the CLI tool globally

## Implementation Status

This project has the core architecture implemented with some areas still under active development. See the README.md for current status of working vs. in-development features.

## License

The project uses the Apache License 2.0.
