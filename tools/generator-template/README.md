# CSIL Generator Development Template

This template walks through authoring a custom CSIL (CBOR Service Interface Language) generator as a WASM module. Your generator becomes `--target <yourname>` simply by *being named correctly* and dropped in a discovery directory — there is no CLI patch, no registration step, no manifest.

## Discovery & naming convention (the load-bearing rule)

The runtime discovers generators dynamically. **There is no hardcoded list of targets in the CLI.** It scans the following directories in priority order (first-write-wins):

1. `target/wasm32-unknown-unknown/release/` — local dev build (only meaningful inside the csilgen workspace).
2. `./.generators/` — project-local override of the user-installed baseline.
3. `~/.csilgen/generators/` — the user's installed baseline.

A file is registered as a generator iff its filename matches **`csilgen_<target>_generator.wasm`**. The `<target>` portion (the substring between `csilgen_` and `_generator.wasm`) is what users will pass to `--target`. Files that don't match the pattern are silently ignored. The `GeneratorMetadata.target` field returned by `get_metadata()` is **informational only** — the wire-level target comes from the filename.

That means: pick a target name (e.g. `mylang`), name your Cargo package `csilgen-mylang-generator`, and the build will produce `csilgen_mylang_generator.wasm`. Drop it in any discovery directory and `csilgen generate --target mylang …` resolves automatically.

## Quick Start

1. **Pick a target name and copy the template** with the matching crate name:
   ```bash
   cp -r tools/generator-template my-csilgen-mylang-generator
   cd my-csilgen-mylang-generator
   ```

2. **Customize `Cargo.toml`:** set `name = "csilgen-mylang-generator"`, `crate-type = ["cdylib"]` (already present), and add any deps you need. The `csilgen-common` dep gives you `WasmGeneratorInput`, `WasmGeneratorOutput`, and the `Csil*` AST types.

3. **Implement** in `src/lib.rs`. The four required exports (`get_metadata`, `allocate`, `deallocate`, `generate`) are scaffolded for you — fill in `process_generation` with your code emission. See "Generator Interface Overview" below.

4. **Build the WASM module:**
   ```bash
   ./build.sh
   ```
   produces `target/wasm32-unknown-unknown/release/csilgen_mylang_generator.wasm`.

5. **Install + use it:**
   ```bash
   # Per-user (visible to every csilgen project):
   mkdir -p ~/.csilgen/generators
   cp target/wasm32-unknown-unknown/release/csilgen_mylang_generator.wasm ~/.csilgen/generators/

   # Or per-project (overrides the per-user copy without touching the homedir):
   mkdir -p .generators
   cp target/wasm32-unknown-unknown/release/csilgen_mylang_generator.wasm .generators/

   csilgen generate --input api.csil --target mylang --output ./gen/
   ```

   No CLI patches, no `cargo install` of csilgen itself, no editing the runtime — the file's presence and name are the entire registration.

6. **Sub-targets** (optional): if you want `--target mylang-server`, `--target mylang-types`, etc. to all route to the same generator, just look at `input.config.target` in your `process_generation` and dispatch. The runtime resolves sub-targets via longest-prefix-match, so a target of `mylang-server` finds the `mylang` generator and passes the full `mylang-server` string in `config.target`.

## Generator Interface Overview

CSIL generators are WASM modules that implement a standardized interface for code generation. Your generator receives parsed CSIL specifications (including services and field metadata) and produces one or more output files.

### Core Interface Functions

Every CSIL generator must export these four functions:

#### 1. `get_metadata() -> *const u8`

Returns serialized JSON metadata about your generator:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "my-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "My custom CSIL generator".to_string(),
        target: "my-language".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
        ],
        author: Some("Your Name".to_string()),
        homepage: Some("https://github.com/you/my-generator".to_string()),
    };
    serialize_and_return_ptr(&metadata)
}
```

#### 2. `generate(input_ptr: *const u8, input_len: usize) -> *mut u8`

Main generation function that processes CSIL and returns generated files:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    // 1. Deserialize WasmGeneratorInput from input_ptr/input_len
    // 2. Process CSIL specification and configuration
    // 3. Generate code files
    // 4. Return serialized WasmGeneratorOutput
}
```

#### 3. `allocate(size: usize) -> *mut u8`

Allocates WASM memory for the host to write input data:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}
```

#### 4. `deallocate(ptr: *mut u8, size: usize)`

Deallocates previously allocated memory:

```rust
#[unsafe(no_mangle)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}
```

### Input Data Structure

Your generator receives a `WasmGeneratorInput` containing:

```rust
pub struct WasmGeneratorInput {
    /// Parsed CSIL specification with services and metadata
    pub csil_spec: CsilSpecSerialized,
    /// Generator configuration from CLI
    pub config: GeneratorConfig,
    /// Your generator's metadata
    pub generator_metadata: GeneratorMetadata,
}
```

#### CSIL Specification Structure

The `CsilSpecSerialized` contains all parsed CSIL rules:

```rust
pub struct CsilSpecSerialized {
    /// All rules (types, groups, services) in the specification
    pub rules: Vec<CsilRule>,
    /// Original source content for error reporting
    pub source_content: Option<String>,
    /// Quick access: total number of services
    pub service_count: usize,
    /// Quick access: total fields with metadata annotations
    pub fields_with_metadata_count: usize,
}
```

Each rule can be:
- **TypeDef**: Simple type definitions (`MyType = text`)
- **GroupDef**: Struct-like definitions with field metadata
- **ServiceDef**: Service definitions with operations
- **TypeChoice**: Union/enum-like type choices
- **GroupChoice**: Group-based choices

### Processing Services

Services are first-class citizens in CSIL. The agreed cross-generator emission model for each direction:

| Direction | Server side | Client side |
|---|---|---|
| `->` Unidirectional | Handler returning `Output` | Method calling the transport (request/response) |
| `<->` Bidirectional | Inbound handler for `Input` + outbound encoder for `Output` | Inbound handler for `Output` + outbound encoder for `Input` |
| `<-` Reverse | Outbound encoder for `Output` only (server-pushed) | Inbound handler for `Output` only |

Bidirectional and reverse ops are **not** streams in the gRPC sense — generators emit only typed handler interfaces, a router that decodes inbound bytes by wire method name, and outbound encoders that return `(method, bytes)`. The implementer wires those to their connection (WebSocket / TCP / whatever); the generator never owns the wire. See `csil-spec.md` "Operation Directions" for the full contract.

```rust
for rule in &input.csil_spec.rules {
    if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
        for operation in &service.operations {
            match operation.direction {
                CsilServiceDirection::Unidirectional => {
                    // Emit your language's request/response method:
                    // input_type -> output_type
                }
                CsilServiceDirection::Bidirectional => {
                    // Emit on the server side: an inbound handler for
                    // input_type (fire-and-forget) + an outbound encoder
                    // for output_type. On the client side: an inbound
                    // handler for output_type + an outbound encoder for
                    // input_type. Plus a router function that decodes one
                    // frame and dispatches by wire-method name.
                }
                CsilServiceDirection::Reverse => {
                    // Server pushes output_type to the client. Emit a
                    // server-side outbound encoder for output_type and a
                    // client-side inbound handler for output_type. No
                    // server inbound; no client outbound.
                }
            }
        }
    }
}
```

### Processing Field Metadata

Field metadata provides rich annotations for code generation:

```rust
for rule in &input.csil_spec.rules {
    if let CsilRuleType::GroupDef(group) = &rule.rule_type {
        for entry in &group.entries {
            let field_name = match &entry.key {
                Some(CsilGroupKey::Bare(name)) => name,
                _ => continue,
            };
            
            for metadata in &entry.metadata {
                match metadata {
                    CsilFieldMetadata::Visibility(vis) => {
                        match vis {
                            CsilFieldVisibility::SendOnly => {
                                // Only include in requests/outgoing messages
                            }
                            CsilFieldVisibility::ReceiveOnly => {
                                // Only include in responses/incoming messages
                            }
                            CsilFieldVisibility::Bidirectional => {
                                // Include in both directions (default)
                            }
                        }
                    }
                    CsilFieldMetadata::Constraint(constraint) => {
                        match constraint {
                            CsilValidationConstraint::MinLength(min) => {
                                // Generate validation for minimum length
                            }
                            CsilValidationConstraint::MaxLength(max) => {
                                // Generate validation for maximum length
                            }
                            _ => {}
                        }
                    }
                    CsilFieldMetadata::Description(desc) => {
                        // Generate documentation comments
                    }
                    CsilFieldMetadata::DependsOn { field, value } => {
                        // Generate conditional validation
                    }
                    CsilFieldMetadata::Custom { name, parameters } => {
                        // Handle custom annotations for your target language
                    }
                }
            }
        }
    }
}
```

### Output Data Structure

Your generator must return a `WasmGeneratorOutput`:

```rust
pub struct WasmGeneratorOutput {
    /// Generated files with paths and content
    pub files: Vec<GeneratedFile>,
    /// Any warnings generated during processing
    pub warnings: Vec<GeneratorWarning>,
    /// Statistics about the generation process
    pub stats: GenerationStats,
}
```

Example output:

```rust
let output = WasmGeneratorOutput {
    files: vec![
        GeneratedFile {
            path: "models.rs".to_string(),
            content: "// Generated Rust structs\n...".to_string(),
        },
        GeneratedFile {
            path: "services.rs".to_string(),
            content: "// Generated service traits\n...".to_string(),
        },
    ],
    warnings: vec![
        GeneratorWarning {
            level: WarningLevel::Warning,
            message: "Field 'password' has no visibility annotation".to_string(),
            location: Some(SourceLocation {
                line: 5,
                column: 4,
                offset: 89,
                context: Some("User.password".to_string()),
            }),
            suggestion: Some("Add @send-only annotation for security".to_string()),
        },
    ],
    stats: GenerationStats {
        files_generated: 2,
        total_size_bytes: 4096,
        services_count: 1,
        fields_with_metadata_count: 5,
        generation_time_ms: 45,
        peak_memory_bytes: Some(2048),
    },
};
```

## Generator Capabilities

Declare what features your generator supports:

- **BasicTypes**: CDDL basic types (int, text, bool, etc.)
- **ComplexStructures**: Groups, arrays, maps, choices
- **Services**: CSIL service definitions and operations
- **FieldMetadata**: Processing field annotations
- **FieldVisibility**: Handling @send-only/@receive-only
- **FieldDependencies**: Conditional field validation
- **ValidationConstraints**: Min/max length, items, etc.
- **CustomHints**: Target-language-specific annotations
- **Streaming**: Large file output support
- **Incremental**: Incremental generation support

## Type Mapping Examples

### Basic Types
```rust
match type_expr {
    CsilTypeExpression::Builtin(name) => {
        match name.as_str() {
            "int" => "i64",      // or your language's integer type
            "uint" => "u64",
            "text" => "String",   // or your language's string type
            "bytes" => "Vec<u8>", // or your language's byte array
            "bool" => "bool",
            "float" => "f64",
            _ => name, // Pass through unknown types
        }
    }
    CsilTypeExpression::Reference(name) => {
        // User-defined type reference
        format!("/* reference to {} */", name)
    }
    _ => "/* complex type */".to_string(),
}
```

### Arrays and Maps
```rust
match type_expr {
    CsilTypeExpression::Array { element_type, occurrence } => {
        let element = map_type(element_type);
        match occurrence {
            Some(CsilOccurrence::Optional) => format!("Option<Vec<{}>>", element),
            Some(CsilOccurrence::ZeroOrMore) => format!("Vec<{}>", element),
            _ => format!("Vec<{}>", element),
        }
    }
    CsilTypeExpression::Map { key, value, .. } => {
        let key_type = map_type(key);
        let value_type = map_type(value);
        format!("HashMap<{}, {}>", key_type, value_type)
    }
    _ => {}
}
```

## Configuration Options

Your generator can accept custom configuration, but **only from the CSIL
source file's `options { … }` block** — the CLI has no `--option` flag, so
there is no way to pass options on the command line itself:

```csil
options {
  use_tabs: true,
  max_line_length: 120,
  generate_docs: true,
}
```

```bash
csilgen generate --input api.csil --target my-generator --output ./gen/
```

These flow through to your generator as `WasmGeneratorInput.config.options:
HashMap<String, serde_json::Value>`. Process them in your generator:

```rust
fn process_config(options: &HashMap<String, serde_json::Value>) -> MyConfig {
    let mut config = MyConfig::default();
    
    if let Some(serde_json::Value::Bool(use_tabs)) = options.get("use_tabs") {
        config.use_tabs = *use_tabs;
    }
    
    if let Some(serde_json::Value::Number(max_len)) = options.get("max_line_length") {
        config.max_line_length = max_len.as_u64().unwrap_or(80) as usize;
    }
    
    config
}
```

## Error Handling and Warnings

Generate helpful warnings for users:

```rust
// Check for potential issues and guide users
let mut warnings = Vec::new();

for rule in &input.csil_spec.rules {
    if let CsilRuleType::GroupDef(group) = &rule.rule_type {
        for entry in &group.entries {
            if entry.metadata.is_empty() {
                warnings.push(GeneratorWarning {
                    level: WarningLevel::Info,
                    message: "Field has no metadata annotations".to_string(),
                    location: Some(SourceLocation {
                        line: rule.position.line,
                        column: rule.position.column,
                        offset: rule.position.offset,
                        context: Some(format!("{}.{:?}", rule.name, entry.key)),
                    }),
                    suggestion: Some("Consider adding @send-only, @receive-only, or validation constraints".to_string()),
                });
            }
        }
    }
}
```

## Memory Management

Output (from your `generate`/`get_metadata`) is length-prefixed:

```
[4 bytes: length (u32 LE)] [length bytes: JSON data]
```

Input is not — the host writes the raw `WasmGeneratorInput` JSON straight into
your module's memory and passes the length as a separate argument, without
ever calling your `allocate`; you only need `allocate` to produce the *output*
buffer, and the host never calls your `deallocate` at all (each generation
runs in its own disposable `wasmtime::Store`). See
[`GENERATOR_INTERFACE.md`](GENERATOR_INTERFACE.md#memory-allocation-flow) for
the full call-by-call breakdown and
[`docs/generator-plugin-contract.md`](../../docs/generator-plugin-contract.md)
§2 for the normative version.

## Testing Your Generator

The template ships with unit tests in `src/lib.rs`. Once you've written a CSIL fixture and a host-side decoder for `WasmGeneratorOutput`, your in-crate `#[cfg(test)] mod tests` can exercise `process_generation` directly without going through the wasm runtime.

For end-to-end testing against the real CLI:

```bash
# Build the WASM module (assuming your package is csilgen-mylang-generator)
./build.sh

# Drop it into the project-local override slot — first-write-wins, so this
# beats any homedir copy without you having to touch ~/.csilgen/generators/
mkdir -p .generators
cp target/wasm32-unknown-unknown/release/csilgen_mylang_generator.wasm .generators/

csilgen generate --input test.csil --target mylang --output ./test-output/
```

Create test CSIL files exercising services, metadata, and `;;;` doc comments:

```csil
;;; A logged-in user.
User = {
  ;;; Display name shown in the UI.
  name: text @bidirectional @description("User's display name"),
  ? email: text @send-only,
  id: uint @receive-only
}

;;; Top-level user API.
service UserAPI {
  create-user: User -> User,            ;; unidirectional
  get-user: uint -> User,                ;; unidirectional
  subscribe: uint <-> User,              ;; bidirectional connection
  notify-deleted: uint <- Acknowledgment ;; reverse (server-pushed)
}
```

(Note: the `@bidirectional` in `name`'s field metadata is the *field visibility* annotation — the field flows in both directions in send/receive payloads. It's unrelated to the `<->` *operation direction* on `subscribe`. They share the word but the AST keeps them in separate enums: `CsilFieldVisibility::Bidirectional` vs `CsilServiceDirection::Bidirectional`.)

## Common Patterns

### Iterating Through All Rules
```rust
for rule in &input.csil_spec.rules {
    println!("Processing rule: {}", rule.name);
    
    match &rule.rule_type {
        CsilRuleType::TypeDef(type_expr) => {
            // Handle type definitions
        }
        CsilRuleType::GroupDef(group) => {
            // Handle group definitions (structs/classes)
        }
        CsilRuleType::ServiceDef(service) => {
            // Handle service definitions
        }
        CsilRuleType::TypeChoice(choices) => {
            // Handle type unions/enums
        }
        CsilRuleType::GroupChoice(choices) => {
            // Handle group unions
        }
    }
}
```

### Generating Multiple Files
```rust
let mut files = Vec::new();

// Generate types file
files.push(GeneratedFile {
    path: "types.go".to_string(),
    content: generate_types(&input.csil_spec)?,
});

// Generate services file  
if input.csil_spec.service_count > 0 {
    files.push(GeneratedFile {
        path: "services.go".to_string(),
        content: generate_services(&input.csil_spec)?,
    });
}

// Generate utilities
files.push(GeneratedFile {
    path: "utils.go".to_string(),
    content: generate_utilities(&config)?,
});
```

### Handling Complex Metadata
```rust
fn process_field_metadata(metadata: &[CsilFieldMetadata]) -> FieldInfo {
    let mut info = FieldInfo::default();
    
    for meta in metadata {
        match meta {
            CsilFieldMetadata::Visibility(vis) => {
                info.visibility = *vis;
            }
            CsilFieldMetadata::Constraint(constraint) => {
                info.constraints.push(constraint.clone());
            }
            CsilFieldMetadata::Description(desc) => {
                info.description = Some(desc.clone());
            }
            CsilFieldMetadata::Custom { name, parameters } => {
                if name == "my-language" {
                    // Handle language-specific annotations
                    info.custom_annotations.push((name.clone(), parameters.clone()));
                }
            }
            _ => {}
        }
    }
    
    info
}
```

## Publishing Your Generator

1. **Test thoroughly** with various CSIL specifications.
2. **Document** what target name your generator serves, the capabilities it advertises, and any options it reads from `WasmGeneratorInput.config.options`.
3. **Distribute the wasm file directly.** Because discovery is filename-based, "installing" your generator is literally copying `csilgen_<target>_generator.wasm` into `~/.csilgen/generators/` (or `./.generators/` for a project pin). No registry, no Cargo dep, no patch to csilgen.

## Need Help?

- Check the template source code for complete examples
- Look at existing generators in the `wasm/` directory
- Review the CSIL specification documentation
- Ask questions in the csilgen community

Happy code generating! 🚀