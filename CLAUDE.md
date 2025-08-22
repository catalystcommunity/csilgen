# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is `csilgen`, a library and CLI tool for implementing CBOR Service Interface Language (CSIL), an aspiring interface definition language that extends beyond what CDDL provides. The project uses a Rust workspace with multiple crates organized as a monorepo.

## Architecture

The architecture follows a plugin-based approach similar to protocgen for protobufs:

- **Core**: Written in Rust (`csilgen-core`), handles CSIL parsing, validation, and AST management
- **CLI**: Separate crate (`csilgen-cli`) that provides the `csilgen` command-line tool
- **Generators**: Multiple generator crates for different target languages (JSON, Rust, Python, TypeScript, OpenAPI)
- **WASM Modules**: Sandboxed generators that receive loaded CSIL data structures and configuration, then return filename/content pairs
- **Common**: Shared utilities, types, and error handling (`csilgen-common`)
- **Security**: WASM modules have no direct filesystem access for security isolation

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
│   ├── csilgen-core/            # Core CSIL parsing, validation, AST
│   ├── csilgen-cli/             # Command-line interface
│   ├── csilgen-common/          # Shared utilities, types, errors
│   └── core_generators/         # Core generator modules
│       ├── csilgen-json/        # JSON Schema generator
│       ├── csilgen-rust/        # Rust code generator
│       ├── csilgen-python/      # Python code generator
│       ├── csilgen-typescript/  # TypeScript generator
│       └── csilgen-openapi/     # OpenAPI spec generator
├── wasm/                        # WASM modules for CLI plugin system
│   ├── csilgen-wasm-core/       # Core functionality as WASM
│   └── csilgen-wasm-generators/ # Generator runtime/loader
├── examples/                    # Usage examples and demos
└── tools/xtask/                 # Development automation
```

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
- `cargo run -p xtask build-wasm` - Build WASM modules (when implemented)

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
