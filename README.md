# csilgen

> **⚠️ ALPHA SOFTWARE**  
> This project has core functionality implemented but is still evolving. The CSIL parser and basic code generation work, but the API may change before the first stable release. Suitable for experimentation and early adoption.

A library and CLI tool for implementing CBOR Service Interface Language (CSIL), an aspiring interface definition language that extends beyond what CDDL provides, with some reference generators.

The core is written in Rust, but calls WASM modules with the loaded CSIL datastructures. WASM modules will be given configuration options and return a set of filename/text-string combos that the core will then splat out into the target directory. Modules have no access to filesystem directly.

Flow is similar to protocgen for protobufs.

## Current Status

The core architecture is in place and the CLI is functional end-to-end: parsing, validation, formatting, linting, breaking-change detection, and code generation all work today for 18 targets — `c`, `csharp`, `dart`, `elixir`, `go`, `java`, `json`, `kotlin`, `noop`, `ocaml`, `openapi`, `php`, `python`, `ruby`, `rust`, `swift`, `typescript`, `zig` (run `csilgen generate --target <bogus>` for this list straight from the CLI). See the [detailed Implementation Status](#implementation-status) below for what's polished vs. what's still on the roadmap.

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
# rust, go, python, typescript, and php also offer focused -typesonly / -client / -server sub-targets
cargo run -p csilgen -- generate --input your-file.csil --target typescript-client --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target python --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target openapi --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target php --output ./generated/
# java, csharp, c, swift, kotlin, zig, ocaml, elixir, ruby, and dart are also
# available targets — see the full list in "Current Status" above
cargo run -p csilgen -- generate --input your-file.csil --target java --output ./generated/

# Or install and use the CLI directly
cargo install --path crates/csilgen-cli
csilgen generate --input your-file.csil --target rust --output ./generated/
```

See the examples directory for sample CSIL files to experiment with.

### Use a released CLI

Download the CLI archive for your operating system and architecture from the
GitHub Release. Extract the archive. Put `csilgen` or `csilgen.exe` in a
directory on your `PATH`.

The CLI archives do not contain generators. Download the generator archive
from the same GitHub Release. Extract the WASM files to one of these
directories:

- `~/.csilgen/generators/` for a user installation
- `./.generators/` for a project installation

For example, the archive contains `csilgen_rust_generator.wasm`.

## Continuous Integration

This repository uses reactorcide for continuous integration and releases. Each
job starts runnerlib. Remote jobs load the Python plugin from the trusted
`main` CI checkout. The plugin runs the job in the runnerlib `POST_SOURCE_PREP`
phase. Pull request source code cannot replace the trusted plugin.

The pull request workflow runs these jobs in parallel after the commit check:

- Rust workspace checks and tests
- WASM generator builds
- Transport library tests
- Cross-language interoperability tests
- Release archive builds

You can run each job on your computer. See
[`.reactorcide/README.md`](.reactorcide/README.md) for the commands and the
required tools.

Releases use Conventional Commits. All files in the repository use one version
and one `csilgen/vX.Y.Z` tag. A release contains one CLI archive for each
supported operating system and architecture. It also contains one archive with
all production generator WASM modules and one source archive for each
transport. The specifications and tests do not have separate release targets.

## CDDL Syntax Support

### ✅ Supported CBOR Constraint Types

All of the RFC 8610 control operators below are parsed, validated, and carried
through every generator. Comparison/size/regex/default constraints are emitted as
runtime checks (code generators) and schema keywords (JSON Schema / OpenAPI);
the encoding/structural operators are represented as encoding hints (e.g.
`contentMediaType`, `allOf`, vendor extensions, doc comments) rather than runtime
checks.

- `.size` - Size constraints (exact, range, min, max)
- `.regex` - Regular expression pattern matching
- `.default` - Default value specification
- `.ge` / `.le` / `.gt` / `.lt` - Ordered comparisons
- `.eq` / `.ne` - Equality / inequality comparisons
- `.bits` - Bit control constraints
- `.and` - Type intersection
- `.within` - Subset constraints
- `.json` - JSON text representation
- `.cbor` / `.cborseq` - CBOR / CBOR-sequence encoding

CSIL also accepts an `@`-annotation constraint system (`@min-length`,
`@max-length`, `@min-items`, `@max-items`, `@min-value`, `@max-value`) that
composes with the control operators above; both are honored by every generator.

### ❌ Not yet supported (future extensions)
- Base encoding operators (`.b64u`, `.b64c`, `.hex`, etc.)
- Text processing operators (`.printf`, `.join`, etc.)

### ✅ Core CDDL Syntax Support
- Basic types (`int`, `text`, `bool`, `bytes`, `float`, etc.)
- Tagged core types: `timestamp` (CBOR tag 0, RFC3339, always UTC) and `decimal`
  (CBOR tag 4, exact). `decimal` maps to a generated, dependency-free
  `CsilDecimal` by default, or to the language's decimal library via
  `options { decimal_mapping: "library" }`. See `docs/cbor-wire-contract.md` and
  `examples/tagged-types/`.
- Arrays with occurrence indicators (`[* type]`, `[+ type]`, `[? type]`, bounded `[*10 type]`, `[2*5 type]`)
- Fixed-shape arrays: heterogeneous tuples `[text, int, bool]` and keyed arrays `[tag: text, value: any]`
- Maps with key-value pairs (`{ key => value }`), including nested maps inside records
- Groups (`( name: text, age: int )`), type choice `/`, and group choice `//`
- Optional fields (`? fieldname: type`)
- Comments — CDDL `;` line comments (a leading `;;` is just a `;` comment)
- Type definitions and references
- Range expressions — inclusive `1..100`, open-ended `1..` / `..100`, exclusive `1...100`, and float ranges (lowered to `.ge`/`.le`/`.lt` constraints)
- Hex byte-string literals (`h'89504E47'`)
- Literal values and enums

### ✅ CSIL Extensions (beyond CDDL)
- `;;;` documentation comments (attach to the following definition); the linter warns on them as a non-CDDL extension
- Service definitions with operations, including push-only ops with no input payload (`op: <- Event`)
- Field metadata annotations, including boolean `@depends-on` conditions (`@depends-on(a = "x" & b != "y" | c > 5)`)
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
- **Generators**: Rust, Go, TypeScript, Python, PHP (each with `-typesonly` / `-client` / `-server` sub-targets, e.g. `rust-client`, `go-typesonly`, `php-client`), Java, C#, C, Swift, Kotlin, Zig, OCaml, Elixir, Ruby, Dart, plus JSON Schema and OpenAPI — 18 targets total (`noop` included, a test fixture). Service directions `->`, `<->`, `<-` all emit consistently (handler + router + outbound encoders).
- **Constraints & tagged types**: the full RFC 8610 control-operator set and the `@`-annotation constraints are honored across every generator; the `timestamp` (tag 0) and `decimal` (tag 4) core types emit per the CBOR wire contract.
- **Testing**: 1,481 tests across the workspace.

### 🔄 Deferred / partial
- **Base-encoding and text-processing operators** (`.b64u`, `.hex`, `.printf`, `.join`, …) — not yet parsed; everything else in RFC 8610's control-operator set is supported.
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
- **Generators (`wasm/csilgen-<target>-generator/`)**: each is a single `cdylib` crate that produces `csilgen_<target>_generator.wasm`. Targets: `rust`, `go`, `typescript`, `python`, `php` (each also accepting `-typesonly` / `-client` / `-server` sub-targets, e.g. `php-client`), `java`, `csharp`, `c`, `swift`, `kotlin`, `zig`, `ocaml`, `elixir`, `ruby`, `dart`, `json`, `openapi`, plus the `noop` test fixture. There is no parallel library copy — the wasm crate is the single source of truth for each generator.
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

The `validate` and `breaking` commands accept file paths. The `format` command
processes a directory tree. The `lint` command processes CSIL files directly
in one directory.

Generators can create a `genquickstart.md` file with transport examples. By
default, this file contains the CSIL-RPC, CSIL-Events, and CSIL-Datagrams
examples. Use one or more of these options to select examples:

```bash
--readme-csil-rpc
--readme-csil-events
--readme-csil-datagrams
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

csilgen supports custom code generators via WASM modules. Every generator — including the built-in ones — is a `cdylib` crate that follows exactly one naming convention; nothing in the CLI is hardcoded. See [`docs/generator-plugin-contract.md`](docs/generator-plugin-contract.md) for the normative plugin interface (WASM ABI, JSON boundary shapes, discovery, and conformance tiers).

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

See any of `wasm/csilgen-*-generator/` for working examples — every real target (`rust`, `go`, `typescript`, `python`, `php`, `java`, `csharp`, `c`, `swift`, `kotlin`, `zig`, `ocaml`, `elixir`, `ruby`, `dart`, `json`, `openapi`) follows the same shape.
