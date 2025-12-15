# csilgen

> **⚠️ ALPHA SOFTWARE**  
> This project has core functionality implemented but is still evolving. The CSIL parser and basic code generation work, but the API may change before the first stable release. Suitable for experimentation and early adoption.

A library and CLI tool for implementing CBOR Service Interface Language (CSIL), an aspiring interface definition language that extends beyond what CDDL provides, with some reference generators.

The core is written in Rust, but calls WASM modules with the loaded CSIL datastructures. WASM modules will be given configuration options and return a set of filename/text-string combos that the core will then splat out into the target directory. Modules have no access to filesystem directly.

Flow is similar to protocgen for protobufs.

## Current Status

This is a newly created project with the core architecture in place but key components still under development:

### ✅ Working
- CLI interface and command structure
- Project structure and build system
- WASM runtime for generators
- WASM build automation
- Core data types and error handling
- Comprehensive test suite
- CSIL parser (full CDDL + CSIL syntax including services and metadata)
- Basic code generation through WASM modules

### ✅ Complete Implementation
- CSIL parser and lexer
- CSIL validator with comprehensive error checking
- Full code generation for JSON Schema, Rust, Go, Python, TypeScript, and OpenAPI
- CSIL formatter for canonical code style
- WASM plugin system for extensible generators
- Breaking change detection and dependency analysis
- Comprehensive testing and benchmarking infrastructure

The CLI is fully functional with complete parsing, validation, formatting, and code generation capabilities.

## Current Usage

From a fresh clone, the minimal commands to generate code from CSIL files:

```bash
# Clone and build
git clone <repo-url>
cd csilgen
cargo build --workspace --release

# Build core WASM generators (required for generation)
cargo build --target wasm32-unknown-unknown --release -p csilgen-noop-generator -p csilgen-json-generator -p csilgen-rust-generator -p csilgen-typescript-generator -p csilgen-python -p csilgen-openapi -p csilgen-go

# Generate code from CSIL files
cargo run -p csilgen -- generate --input your-file.csil --target rust --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target go --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target json --output ./generated/
cargo run -p csilgen -- generate --input your-file.csil --target typescript --output ./generated/
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
- Comments (`#` and `;;` syntax)
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

### ✅ Fully Implemented
- **Parser**: Complete CDDL parsing with CSIL extensions
- **Validator**: Comprehensive validation with constraint checking
- **Generators**: JSON Schema, Rust, Go, Python, TypeScript, OpenAPI
- **CLI Tools**: validate, generate, format, lint, breaking change detection
- **WASM System**: Plugin architecture for extensible generators
- **Testing**: 430+ tests across all components

### 🔄 Under Development  
- Additional CBOR constraint types (`.eq`, `.ne`, `.bits`, etc.)
- Advanced field metadata syntax (`@description`, `@bidirectional`, etc.)
- Enhanced error messages for unsupported syntax
- Performance optimizations for large schemas

### 📋 Planned Features
- IDE integrations and language server  
- Schema evolution and versioning tools
- Additional target language generators
- Live validation and hot-reload development mode
- Schema documentation generation

## Project Structure

This is a Rust workspace containing multiple crates:

- **csilgen-core**: Core CSIL parsing, validation, and AST functionality
- **csilgen-cli**: Command-line interface (`csilgen` binary)
- **csilgen-common**: Shared utilities, types, and error handling
- **Core Generators**:
  - **csilgen-json**: JSON Schema generator
  - **csilgen-rust**: Rust code generator
  - **csilgen-go**: Go code generator
  - **csilgen-python**: Python code generator
  - **csilgen-typescript**: TypeScript generator
  - **csilgen-openapi**: OpenAPI specification generator
- **WASM Modules**:
  - **csilgen-wasm-core**: Core functionality as WASM
  - **csilgen-wasm-generators**: WASM runtime for plugin generators
- **Development Tools**:
  - **xtask**: Build automation and development tasks

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

csilgen supports custom code generators via WASM modules. This allows you to create generators for any target language or use case.

### Generator Discovery

The CLI discovers generators in two ways:

1. **Built-in Generators**: Packaged with the CLI installation
2. **User Generators**: Located in `~/.csilgen/generators/`

Generator filenames must use the format: `csilgen-<target>.wasm` where `<target>` is the name you'll use with `--target <target>`.

**Example**: `csilgen-go.wasm` is used with `--target go`

### Creating a Custom Generator

1. **Create a new Rust crate** in `crates/core_generators/`:
   ```bash
   cd crates/core_generators
   cargo new --lib csilgen-go
   ```

2. **Configure for WASM** in `Cargo.toml`:
   ```toml
   [lib]
   crate-type = ["cdylib"]

   [dependencies]
   csilgen-common = { path = "../../csilgen-common" }
   # Add other dependencies as needed
   ```

3. **Implement the generator** in `src/lib.rs`:
   ```rust
   use csilgen_common::*;

   #[no_mangle]
   pub extern "C" fn generate(/* ... */) -> String {
       // Your generation logic here
       // Return JSON with filename/content pairs
   }
   ```

### Building and Installing

1. **Build the WASM module**:
   ```bash
   cargo build --target wasm32-unknown-unknown --release -p csilgen-go
   ```

   Output location: `target/wasm32-unknown-unknown/release/csilgen_go.wasm`

2. **Install for CLI use**:
   ```bash
   # Create user generators directory if it doesn't exist
   mkdir -p ~/.csilgen/generators/

   # Copy with correct naming (hyphen not underscore!)
   cp target/wasm32-unknown-unknown/release/csilgen_go.wasm ~/.csilgen/generators/csilgen-go.wasm
   ```

3. **Use your generator**:
   ```bash
   csilgen generate --input your-file.csil --target go --output ./generated/
   ```

### Important Notes

- **Naming Convention**: Build outputs use underscores (`csilgen_go.wasm`), but runtime discovery expects hyphens (`csilgen-go.wasm`)
- **WASM Limitations**: Generators cannot print to stdout/stderr - all output must be returned via the function result
- **Testing**: Generators run in a sandbox with no filesystem access for security
- **Development**: After making changes, rebuild and recopy the WASM file to `~/.csilgen/generators/`

### Example: Go Generator

See `crates/core_generators/csilgen-go/` for a complete example of a custom generator that produces Go structs from CSIL definitions.
