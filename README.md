# csilgen

> **⚠️ ALPHA SOFTWARE**  
> This project has core functionality implemented but is still evolving. The CSIL parser and basic code generation work, but the API may change before the first stable release. Suitable for experimentation and early adoption.

A library and CLI tool for implementing CBOR Service Interface Language (CSIL), an aspiring interface definition language that extends beyond what CDDL provides, with some reference generators.

The core is written in Rust, but calls WASM modules with the loaded CSIL datastructures. WASM modules will be given configuration options and return a set of filename/text-string combos that the core will then splat out into the target directory. Modules have no access to filesystem directly.

Flow is similar to protocgen for protobufs.

## Current Status

The core architecture is in place and the CLI is functional end-to-end: parsing, validation, formatting, linting, breaking-change detection, and code generation for Rust, Go, TypeScript, Python, JSON Schema, and OpenAPI all work today. See the [detailed Implementation Status](#implementation-status) below for what's polished vs. what's deferred to follow-ups (tracked in `docs/csilgen-requests/`).

## Current Usage

From a fresh clone, the minimal commands to generate code from CSIL files:

```bash
# Clone and build
git clone <repo-url>
cd csilgen
cargo build --workspace --release

# Build and install the WASM generators to ~/.csilgen/generators/
cargo run -p xtask install-wasm

# Generate code from CSIL files
cargo run -p csilgen -- generate --input your-file.csil --target rust --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target go --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target json --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target typescript --output ./generated/
# TypeScript also offers focused targets: typescript-typesonly, typescript-client, typescript-server
cargo run -p csilgen -- generate --input your-file.csil --target typescript-client --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target python --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target openapi --output ./generated/

# Or install and use the CLI directly
cargo install --path crates/csilgen-cli
csilgen generate --input your-file.csil --target rust --output ./generated/
```

See the examples directory for sample CSIL files to experiment with.

## CDDL Syntax Support

### ✅ Currently Supported CBOR Constraint Types
- `.size` - Size constraints (exact, range, min, max)
- `.regex` - Regular expression pattern matching  
- `.default` - Default value specification
- `.ge` - Greater than or equal comparison
- `.le` - Less than or equal comparison  
- `.gt` - Greater than comparison
- `.lt` - Less than comparison

### ❌ Planned CBOR Constraint Types (from RFC 8610)
- `.eq` - Equality comparison
- `.ne` - Not equal comparison  
- `.bits` - Bit control constraints
- `.and` - Type intersection
- `.within` - Subset constraints
- `.cbor` - CBOR validation
- `.cborseq` - CBOR sequence validation

### ❌ Additional Constraint Types (future extensions)
- `.json` - JSON text representation validation
- Base encoding operators (`.b64u`, `.b64c`, `.hex`, etc.)
- Text processing operators (`.printf`, `.join`, etc.)

### ✅ Core CDDL Syntax Support
- Basic types (`int`, `text`, `bool`, `bytes`, `float`, etc.)
- Arrays with occurrence indicators (`[* type]`, `[+ type]`, `[? type]`)  
- Maps with key-value pairs (`{ key => value }`)
- Groups and choices (`( a, b )`, `( a / b )`)
- Optional fields (`? fieldname: type`)
- Comments (`#` and `;;` syntax; `;;;` documentation comments attach to the following definition)
- Type definitions and references
- Range expressions (`0..100`)
- Literal values and enums

### ✅ CSIL Extensions (beyond CDDL)
- Service definitions with operations  
- Field metadata annotations
- Import/include statements
- File-level options
- Socket/plug type system
- Breaking change detection
- Multi-file dependency analysis

## Implementation Status

### ✅ Working today
- **Parser**: CDDL syntax plus the CSIL extensions (services, `/=`/`//=` choices, field metadata, `;;;` doc comments, options block, imports).
- **Validator**: Constraint checking, dependency analysis, breaking-change detection.
- **CLI tools**: `validate`, `generate`, `format`, `lint`, `breaking`.
- **Plugin runtime**: Dynamic generator discovery from `~/.csilgen/generators/`, `./.generators/`, and `target/wasm32-unknown-unknown/release/` — first-write-wins precedence. Generators are conforming `csilgen_<target>_generator.wasm` cdylibs; no CLI map to edit.
- **Generators**: Rust, Go, TypeScript (with the typescript-typesonly / typescript-client / typescript-server sub-targets), Python, JSON Schema, OpenAPI. Service directions `->`, `<->`, `<-` all emit consistently (handler + router + outbound encoders).
- **Testing**: 465+ tests across the workspace.

### 🔄 Deferred / partial
- **OpenAPI generator internals** still consume the core AST via a `Serialized → Core` shim in `wasm/csilgen-openapi-generator/src/wasm.rs`. Refactoring its body to operate on the serialized form directly (matching the other targets) is captured in `docs/csilgen-requests/openapi-generator-realignment.md`.
- **JSON Schema generator's bidirectional handling**: JSON Schema doesn't model service operations richly; current behavior is documented in `docs/csilgen-requests/json-generator-realignment.md`.
- **Additional CBOR constraint operators** (`.eq`, `.ne`, `.bits`, `.and`, `.within`, encoding/base operators) — see `csil-spec.md` for the supported subset.
- **Performance optimizations** for very large schemas.

### 📋 Future ideas
- IDE integrations / language server.
- Schema evolution & versioning tools beyond `breaking`.
- Live validation / hot-reload during development.
- Schema documentation generation as a first-class generator.

## Project Structure

This is a Rust workspace containing multiple crates:

- **csilgen-core**: CSIL parsing, validation, and AST functionality
- **csilgen-cli**: Command-line interface (`csilgen` binary)
- **csilgen-common**: Shared types incl. the WASM boundary types
- **Runtime (`wasm/`)**:
  - **csilgen-wasm-core**: Core types/helpers compiled to wasm
  - **csilgen-wasm-generators**: Discovery + execution runtime
- **Generators (`wasm/csilgen-<target>-generator/`)**: each is a single `cdylib` crate that produces `csilgen_<target>_generator.wasm`. Targets: `rust`, `go`, `typescript` (also `typescript-typesonly` / `typescript-client` / `typescript-server`), `json`, `python`, `openapi`, plus the `noop` test fixture. There is no parallel library copy — the wasm crate is the single source of truth for each generator.
- **Development tools**: `tools/xtask` build automation.

Generators are discovered dynamically, **first-write-wins**, scanning in this priority order: `target/wasm32-unknown-unknown/release` (local dev builds) → `./.generators` (project-local pin/override) → `~/.csilgen/generators/` (user-installed baseline). Files matching `csilgen_<target>_generator.wasm` are registered; everything else is ignored. A project can drop a wasm into `.generators/` to override the user's installed copy without touching the homedir. To ship a third-party generator, build a `cdylib` of the same shape and place it in any of those directories — `--target <yourname>` resolves automatically.

## Getting Started

```bash
# Build the entire workspace
cargo build --workspace

# Build WASM generators (using xtask)
cargo run -p xtask build-wasm

# Install WASM generators to ~/.csilgen/generators/ (for system-wide CLI usage)
cargo run -p xtask install-wasm

# Run tests
cargo test --workspace

# Install the CLI tool
cargo install --path crates/csilgen-cli

# Test the CLI
csilgen --help
csilgen validate --input examples/basic-usage/simple-service.csil
csilgen generate --input examples/basic-usage/simple-service.csil --target noop --output ./test-output/
```

### Full CLI Commands
```bash
csilgen validate --input interface.csil
csilgen generate --input interface.csil --target rust --output ./generated/
csilgen generate --input ./schemas/ --target rust --output ./generated/  # Multi-file with dependency analysis
csilgen breaking --current A.csil --new B.csil
csilgen format path/to/dir/ --dry-run
csilgen lint path/to/dir/ --fix
```

## Multi-File Projects and Dependency Analysis

When working with multiple CSIL files, csilgen automatically performs dependency analysis to avoid generating duplicate code:

### How It Works

1. **Single File**: Processes normally with import resolution
2. **Multiple Files**: 
   - Builds a dependency graph of all CSIL files
   - Identifies **entry points** (files not imported by others)
   - Generates code only from entry points
   - Dependencies are included automatically via import resolution

### Example Project Structure

```
schemas/
├── api.csil          # Entry point - defines services
├── admin.csil        # Entry point - defines admin services  
├── types/
│   ├── user.csil     # Dependency - imported by api.csil
│   ├── product.csil  # Dependency - imported by api.csil
│   └── common.csil   # Dependency - imported by user.csil, product.csil
└── standalone.csil   # Entry point - no imports/exports
```

**Command**: `csilgen generate --input ./schemas/ --target rust --output ./generated/`

**Result**: Generates code from 3 entry points (`api.csil`, `admin.csil`, `standalone.csil`) with all dependencies automatically included.

### Dependency Analysis Output

```bash
📊 Dependency analysis completed:
   Entry points: 3 files
   Dependencies: 3 files
   Generating code from entry points only to avoid duplicates.

🔄 Processing 3 entry points from 6 total files:
   📄 api.csil
   📄 admin.csil  
   📄 standalone.csil
   (Skipping 3 dependency files to avoid duplicates)
```

### Verbose Mode

Use `CSIL_VERBOSE=1` to see detailed dependency trees:

```bash
CSIL_VERBOSE=1 csilgen generate --input ./schemas/ --target rust --output ./generated/
```

Shows hierarchical dependency relationships:
```
Entry Points:
  📄 api.csil
  └─📦 types/user.csil
    └─📦 types/common.csil
  └─📦 types/product.csil
    └─📦 types/common.csil

Dependency Files:
  📦 types/user.csil (imported by: api.csil)
  📦 types/product.csil (imported by: api.csil)
  📦 types/common.csil (imported by: types/user.csil, types/product.csil)
```

### Error Detection

The system detects and reports circular dependencies:

```bash
Error: Circular dependency detected: a.csil → b.csil → a.csil

This creates an infinite loop during import resolution. Please restructure 
your CSIL files to remove the circular reference. Consider:

1. Moving shared types to a separate file
2. Consolidating related types into a single file
3. Using forward references instead of direct imports
```

### Best Practices for Multi-File Projects

1. **Organize by Purpose**:
   - **Entry Points**: Files that define services or main interfaces (e.g., `user-api.csil`, `admin-api.csil`)
   - **Dependencies**: Files with shared types and common definitions (e.g., `types/user.csil`, `common/errors.csil`)

2. **Clear File Naming**:
   - Use descriptive names that indicate the file's role
   - Consider prefixes like `api-`, `types-`, `shared-` for clarity
   - Group related files in subdirectories

3. **Dependency Flow**:
   - Structure imports to flow in one direction (avoid circular dependencies)
   - Place shared types in dedicated files at the bottom of the dependency tree
   - Keep service definitions at the top level as entry points

4. **Testing Your Structure**:
   ```bash
   # Verify dependency analysis matches expectations
   CSIL_VERBOSE=1 csilgen generate --input ./schemas/ --target noop --output /tmp/test
   
   # Should show clear entry points vs dependencies
   # Entry points should be service definitions
   # Dependencies should be shared types
   ```

5. **Migration from Single Files**:
   - Existing single-file workflows continue to work unchanged
   - Gradually extract shared types into separate files
   - Use dependency analysis to verify no duplicates are generated

See `examples/multi-file/` for concrete examples of these patterns.

## Custom Generator Development

csilgen supports custom code generators via WASM modules. Every generator — including the built-in ones — is a `cdylib` crate that follows exactly one naming convention; nothing in the CLI is hardcoded.

### How discovery works

At startup the runtime scans, **first-write-wins**, in priority order:

1. **`target/wasm32-unknown-unknown/release/`** — local dev build output (csilgen workspace only)
2. **`./.generators/`** — project-local override of the user-installed baseline
3. **`~/.csilgen/generators/`** — user-installed generators

Files that match **`csilgen_<target>_generator.wasm`** are registered as generators serving `--target <target>`. The target name is the substring between `csilgen_` and `_generator.wasm`. Anything that does not match the pattern is silently ignored, so a search directory can hold unrelated files without affecting discovery. A project pins or replaces a generator by dropping a wasm into `.generators/`; the homedir copy is shadowed without being touched. There is no map to edit — drop a conforming file in, run `--target <name>`, done.

### Authoring a generator

1. **Create the crate.** Place it under `wasm/` if upstreaming, or anywhere if external.

   ```bash
   cd wasm
   cargo new --lib csilgen-mylang-generator
   ```

2. **Configure as a cdylib in `Cargo.toml`:**

   ```toml
   [package]
   name = "csilgen-mylang-generator"
   edition = "2024"

   [lib]
   crate-type = ["cdylib"]

   [dependencies]
   csilgen-common = { path = "../../crates/csilgen-common" }
   serde = { version = "1", features = ["derive"] }
   serde_json = "1"
   ```

3. **Implement the four exports.** Use `csilgen-common`'s `WasmGeneratorInput` / `WasmGeneratorOutput` for I/O and `wasm_interface::*` for error codes / `MAX_INPUT_SIZE`. See any existing `wasm/csilgen-*-generator/` for the boilerplate.

4. **Wire into the workspace.** Add the crate path to `Cargo.toml` `members`, and the package name to `tools/xtask/src/main.rs` `build_wasm`'s `--package` list. Then `cargo run -p xtask install-wasm` builds and installs it.

5. **Use it.** `csilgen generate --input file.csil --target mylang --output ./out/`. No CLI changes required at any point.

### Rules

- **One crate per generator.** Do not create a parallel `lib` version under `crates/`; the `cdylib` is the single source of truth. The codebase has been bitten by this twice before.
- **Filename is the contract.** `csilgen_<target>_generator.wasm`. The CLI derives the target from it; if you rename, the target name changes.
- **No async, no filesystem access** — generators run in a sandbox and return everything via the WASM result.
- **Don't enumerate targets in the CLI.** If you find yourself patching a `match target` somewhere, stop — that's the anti-pattern the dynamic discovery exists to prevent.

### Existing generators

See any of `wasm/csilgen-{rust,go,typescript,json,python,openapi}-generator/` for working examples.
