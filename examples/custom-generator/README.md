# Custom Generator Development Example

This example demonstrates how to create a custom WASM generator for CSIL. This generator creates Go code from CSIL specifications, showing how to handle services, field metadata, and code generation patterns.

## What This Generator Does

- **Input**: CSIL specification with services and metadata
- **Output**: Go code with structs, service interfaces, and HTTP clients
- **Features**: 
  - Respects field visibility metadata (`@send-only`, `@receive-only`, `@admin-only`)
  - Generates service interfaces and client implementations
  - Handles bidirectional operations for real-time services
  - Creates appropriate Go naming conventions

## Files

- **`Cargo.toml`**: WASM crate configuration with required dependencies
- **`src/lib.rs`**: The generator implementation with WASM exports
- **`example-input.csil`**: Sample CSIL file to test the generator
- **`build.sh`**: Script to build the WASM module

## Building the Generator

```bash
cd examples/custom-generator

# Install wasm-pack if not already installed
cargo install wasm-pack

# Build the WASM module
./build.sh

# The built module will be in pkg/custom_csil_generator.wasm
```

## Testing the Generator

```bash
# Test with the CLI (once csilgen supports custom generators)
csilgen generate \
    --input example-input.csil \
    --target ./pkg/custom_csil_generator.wasm \
    --output ./generated/

# Check the generated Go code
ls generated/
cat generated/types.go
cat generated/userapi_service.go
cat generated/userapi_client.go
```

## Generator Interface

Every CSIL generator must implement this WASM interface:

```rust
#[wasm_bindgen]
pub fn generate(csil_spec_json: &str, config_json: &str) -> String {
    // Parse the CSIL specification
    // Apply the configuration
    // Generate code files
    // Return JSON array of GeneratedFile objects
}
```

### Input Format

- **`csil_spec_json`**: Serialized `CsilSpec` containing parsed AST with services and metadata
- **`config_json`**: Generator-specific configuration options

### Output Format

Returns JSON array of `GeneratedFile` objects:
```json
[
  {
    "filename": "types.go",
    "content": "package api\n\n// Generated types..."
  },
  {
    "filename": "service.go", 
    "content": "package api\n\n// Generated service..."
  }
]
```

## Key Implementation Patterns

### 1. Handling Field Metadata
```rust
fn create_json_tag(field: &Field) -> String {
    let mut tag = field.name.clone();
    
    if field.metadata.send_only {
        tag.push_str(",send_only");
    }
    
    if field.metadata.receive_only {
        tag.push_str(",receive_only");
    }
    
    tag
}
```

### 2. Service Code Generation
```rust
fn generate_go_service(service: &ServiceDefinition, config: &GeneratorConfig) -> String {
    // Generate interface definition
    // Handle bidirectional operations differently
    // Include metadata-driven documentation
}
```

### 3. Language-Specific Naming
```rust
fn to_go_field_name(name: &str) -> String {
    // Convert snake_case to PascalCase for Go
}
```

## Development Tips

1. **Start Simple**: Begin with basic type generation, add services later
2. **Test Incrementally**: Use small CSIL files to test each feature
3. **Handle Metadata**: CSIL's power comes from rich metadata - use it!
4. **Follow Conventions**: Generate idiomatic code for your target language
5. **Error Handling**: Provide clear error messages for invalid inputs

## Integration with csilgen

Once built, custom generators can be used with:

```bash
# Register the generator (future feature)
csilgen generator install ./pkg/custom_csil_generator.wasm --name go

# Use the custom generator
csilgen generate --input api.csil --target go --output ./generated/
```

This example serves as a template for creating generators for any target language or framework.