# CSIL Generator Development Template

This template provides everything you need to create custom CSIL (CBOR Service Interface Language) code generators as WASM modules. Your generator will integrate seamlessly with the `csilgen` CLI tool and can process CSIL specifications with services, field metadata, and all CDDL features.

## Quick Start

1. **Clone this template:**
   ```bash
   cp -r tools/generator-template my-custom-generator
   cd my-custom-generator
   ```

2. **Customize the generator:**
   - Edit `Cargo.toml` with your generator details
   - Update the metadata in `src/lib.rs`
   - Implement your code generation logic

3. **Build the WASM module:**
   ```bash
   ./build.sh
   ```

4. **Test your generator:**
   ```bash
   csilgen generate --input example.csil --target my-custom-generator --output ./generated/
   ```

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

Services are first-class citizens in CSIL. Here's how to process them:

```rust
for rule in &input.csil_spec.rules {
    match &rule.rule_type {
        CsilRuleType::ServiceDef(service) => {
            println!("Service: {}", rule.name);
            
            for operation in &service.operations {
                println!("  Operation: {} ({:?} -> {:?})",
                    operation.name,
                    operation.input_type,
                    operation.output_type);
                    
                match operation.direction {
                    CsilServiceDirection::Unidirectional => {
                        // Generate request-response method
                    }
                    CsilServiceDirection::Bidirectional => {
                        // Generate bidirectional/streaming method
                    }
                    CsilServiceDirection::Reverse => {
                        // Generate reverse-flow method
                    }
                }
            }
        }
        _ => {}
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

Your generator can accept custom configuration through the CLI:

```bash
csilgen generate --input api.csil --target my-generator --output ./gen/ \
  --option use_tabs=true \
  --option max_line_length=120 \
  --option generate_docs=true
```

Process these in your generator:

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

The WASM interface requires careful memory management:

- **allocate()**: Create memory for host to write input
- **deallocate()**: Clean up allocated memory
- **Length-prefixed data**: All data is prefixed with 4-byte length (little-endian)
- **JSON serialization**: All data is exchanged as JSON strings

Example memory layout:
```
[4 bytes: length (u32 LE)] [length bytes: JSON data]
```

## Testing Your Generator

The template includes comprehensive tests:

```bash
# Run unit tests
cargo test

# Build and test WASM module
./build.sh
csilgen generate --input test.csil --target my-generator --output ./test-output/
```

Create test CSIL files with services and metadata:

```csil
; Basic types with metadata
User = {
  name: text @bidirectional @description("User's display name"),
  email: text ? @send-only @min-length(5),
  id: uint @receive-only
}

; Service definition
service UserAPI {
  create-user: User -> User
  get-user: uint -> User
  update-user: User <-> User  ; bidirectional
}
```

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

1. **Test thoroughly** with various CSIL specifications
2. **Document** your generator's capabilities and options
3. **Package** the WASM file with installation instructions
4. **Consider** contributing to the official generator registry

Your WASM module can be distributed as a single file and used by anyone with the csilgen CLI tool.

## Need Help?

- Check the template source code for complete examples
- Look at existing generators in the `wasm/` directory
- Review the CSIL specification documentation
- Ask questions in the csilgen community

Happy code generating! 🚀