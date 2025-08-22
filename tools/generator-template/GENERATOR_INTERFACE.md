# CSIL Generator WASM Interface Specification

This document provides the complete technical specification for implementing CSIL generators as WASM modules.

## Interface Overview

CSIL generators are WebAssembly modules that implement a standardized interface for code generation. They receive parsed CSIL specifications (including services and field metadata) through a well-defined API and produce generated code files.

### Core Principles

1. **Sandboxed Execution**: WASM modules have no direct filesystem access for security
2. **Standardized Interface**: All generators implement the same four C-compatible functions
3. **JSON Communication**: All data exchange uses JSON serialization
4. **Memory Management**: Generators manage their own WASM linear memory
5. **Service-Aware**: Full support for CSIL service definitions and operations
6. **Metadata-Rich**: Complete access to field metadata for advanced code generation

## Required Exports

Every CSIL generator WASM module must export these four functions:

### 1. `get_metadata() -> *const u8`

Returns generator metadata as length-prefixed JSON.

**Return Value**: Pointer to memory containing:
- 4 bytes: JSON length (u32, little-endian)
- N bytes: JSON-serialized `GeneratorMetadata`

**Example Implementation**:
```rust
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "rust-generator".to_string(),
        version: "2.1.0".to_string(),
        description: "Rust code generator with service support".to_string(),
        target: "rust".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/rust-generator".to_string()),
    };
    
    serialize_and_return_ptr(&metadata)
}
```

### 2. `generate(input_ptr: *const u8, input_len: usize) -> *mut u8`

Main generation function that processes CSIL specifications and returns generated code.

**Parameters**:
- `input_ptr`: Pointer to JSON-serialized `WasmGeneratorInput`
- `input_len`: Length of input data in bytes

**Return Value**: Pointer to memory containing:
- 4 bytes: JSON length (u32, little-endian)  
- N bytes: JSON-serialized `WasmGeneratorOutput`
- Returns `null` on error

**Example Implementation**:
```rust
#[unsafe(no_mangle)]
pub extern "C" fn generate(
    input_ptr: *const u8,
    input_len: usize,
) -> *mut u8 {
    // Validate input parameters
    if input_ptr.is_null() || input_len == 0 {
        return create_error_result(error_codes::INVALID_INPUT);
    }
    
    if input_len > MAX_INPUT_SIZE {
        return create_error_result(error_codes::INVALID_INPUT);
    }
    
    // Read input data
    let input_slice = unsafe { 
        std::slice::from_raw_parts(input_ptr, input_len) 
    };
    
    let input_str = match std::str::from_utf8(input_slice) {
        Ok(s) => s,
        Err(_) => return create_error_result(error_codes::INVALID_INPUT),
    };
    
    // Deserialize input
    let input: WasmGeneratorInput = match serde_json::from_str(input_str) {
        Ok(input) => input,
        Err(_) => return create_error_result(error_codes::SERIALIZATION_ERROR),
    };
    
    // Process generation request
    let result = process_generation_request(input);
    
    match result {
        Ok(output) => serialize_and_return_ptr(&output),
        Err(_) => create_error_result(error_codes::GENERATION_ERROR),
    }
}
```

### 3. `allocate(size: usize) -> *mut u8`

Allocates memory in WASM linear memory for host to write input data.

**Parameters**:
- `size`: Number of bytes to allocate

**Return Value**: Pointer to allocated memory, or `null` on failure

**Example Implementation**:
```rust
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf); // Prevent deallocation
    ptr
}
```

### 4. `deallocate(ptr: *mut u8, size: usize)`

Deallocates memory previously allocated by `allocate()`.

**Parameters**:
- `ptr`: Pointer to memory to deallocate
- `size`: Size of memory block in bytes

**Example Implementation**:
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

## Data Structures

### Input: WasmGeneratorInput

The input to your generator contains:

```rust
pub struct WasmGeneratorInput {
    /// Parsed and validated CSIL specification
    pub csil_spec: CsilSpecSerialized,
    /// Configuration from CLI options
    pub config: GeneratorConfig,
    /// Your generator's metadata (for reference)
    pub generator_metadata: GeneratorMetadata,
}
```

### CSIL Specification: CsilSpecSerialized

The core CSIL specification data:

```rust
pub struct CsilSpecSerialized {
    /// All rules in the CSIL specification
    pub rules: Vec<CsilRule>,
    /// Original source content for error reporting
    pub source_content: Option<String>,
    /// Quick access: total number of services
    pub service_count: usize,
    /// Quick access: total fields with metadata
    pub fields_with_metadata_count: usize,
}
```

### CSIL Rules: CsilRule

Each rule in the specification:

```rust
pub struct CsilRule {
    /// Rule name/identifier
    pub name: String,
    /// Rule type and content
    pub rule_type: CsilRuleType,
    /// Source position for error reporting
    pub position: CsilPosition,
}

pub enum CsilRuleType {
    /// Type definition (MyType = text)
    TypeDef(CsilTypeExpression),
    /// Group definition (struct-like with metadata)
    GroupDef(CsilGroupExpression),
    /// Type choice (union/enum)
    TypeChoice(Vec<CsilTypeExpression>),
    /// Group choice
    GroupChoice(Vec<CsilGroupExpression>),
    /// Service definition with operations
    ServiceDef(CsilServiceDefinition),
}
```

### Service Definitions

Services are defined with operations:

```rust
pub struct CsilServiceDefinition {
    pub operations: Vec<CsilServiceOperation>,
}

pub struct CsilServiceOperation {
    pub name: String,
    pub input_type: CsilTypeExpression,
    pub output_type: CsilTypeExpression,
    pub direction: CsilServiceDirection,
    pub position: CsilPosition,
}

pub enum CsilServiceDirection {
    /// input -> output
    Unidirectional,
    /// input <-> output (bidirectional/streaming)  
    Bidirectional,
    /// input <- output (reverse flow)
    Reverse,
}
```

### Field Metadata

Groups can contain fields with rich metadata:

```rust
pub struct CsilGroupEntry {
    pub key: Option<CsilGroupKey>,
    pub value_type: CsilTypeExpression,
    pub occurrence: Option<CsilOccurrence>,
    /// Rich metadata annotations
    pub metadata: Vec<CsilFieldMetadata>,
}

pub enum CsilFieldMetadata {
    /// Field visibility (@send-only, @receive-only, @bidirectional)
    Visibility(CsilFieldVisibility),
    /// Field dependencies (@depends-on(field = value))
    DependsOn { field: String, value: Option<CsilLiteralValue> },
    /// Validation constraints (@min-length(5), @max-items(100))
    Constraint(CsilValidationConstraint),
    /// Documentation (@description("..."))
    Description(String),
    /// Custom generator hints (@rust(skip_serializing_if = "..."))
    Custom { name: String, parameters: Vec<CsilMetadataParameter> },
}

pub enum CsilFieldVisibility {
    /// Only included in outgoing messages
    SendOnly,
    /// Only included in incoming messages
    ReceiveOnly,
    /// Included in both directions (default)
    Bidirectional,
}
```

### Output: WasmGeneratorOutput

Your generator must return:

```rust
pub struct WasmGeneratorOutput {
    /// Generated files with paths and content
    pub files: Vec<GeneratedFile>,
    /// Warnings generated during processing
    pub warnings: Vec<GeneratorWarning>,
    /// Statistics about generation
    pub stats: GenerationStats,
}

pub struct GeneratedFile {
    /// Relative path for the file
    pub path: String,
    /// Generated content
    pub content: String,
}

pub struct GeneratorWarning {
    /// Warning severity
    pub level: WarningLevel,
    /// Human-readable message
    pub message: String,
    /// Optional location in CSIL source
    pub location: Option<SourceLocation>,
    /// Optional suggestion for fixing
    pub suggestion: Option<String>,
}

pub struct GenerationStats {
    /// Number of files generated
    pub files_generated: usize,
    /// Total size of generated content
    pub total_size_bytes: usize,
    /// Number of services processed
    pub services_count: usize,
    /// Number of fields with metadata processed
    pub fields_with_metadata_count: usize,
    /// Generation time in milliseconds
    pub generation_time_ms: u64,
    /// Peak memory usage in bytes
    pub peak_memory_bytes: Option<usize>,
}
```

## Memory Management Protocol

### Data Exchange Format

All data exchanged across the WASM boundary uses this format:

```
[4 bytes: length as u32 little-endian] [length bytes: JSON data]
```

### Memory Allocation Flow

1. **Host allocates input memory**: Calls `allocate(input_size)`
2. **Host writes input data**: Writes JSON to allocated memory  
3. **Host calls generate()**: Passes pointer and length
4. **Generator allocates output memory**: Uses internal `allocate()`
5. **Generator returns output pointer**: Points to length-prefixed JSON
6. **Host reads output data**: Reads length, then JSON data
7. **Host deallocates memories**: Calls `deallocate()` for both buffers

### Memory Safety

- Always check for null pointers before dereferencing
- Validate input lengths against `MAX_INPUT_SIZE` (64MB)
- Ensure output size doesn't exceed `MAX_OUTPUT_SIZE` (256MB)
- Use `std::mem::forget()` in `allocate()` to prevent premature deallocation
- Properly reconstruct `Vec` in `deallocate()` to free memory

## Error Codes

Standard error codes returned by WASM functions:

```rust
pub mod error_codes {
    pub const SUCCESS: i32 = 0;
    pub const INVALID_INPUT: i32 = 1;
    pub const SERIALIZATION_ERROR: i32 = 2;
    pub const GENERATION_ERROR: i32 = 3;
    pub const OUT_OF_MEMORY: i32 = 4;
    pub const UNSUPPORTED_FEATURE: i32 = 5;
}
```

## Generator Capabilities

Declare supported features to help users understand what your generator can handle:

```rust
pub enum GeneratorCapability {
    /// Basic CDDL types (int, text, bool, bytes, etc.)
    BasicTypes,
    /// CDDL groups, arrays, maps, choices
    ComplexStructures,
    /// CSIL service definitions
    Services,
    /// CSIL field metadata processing
    FieldMetadata,
    /// Field visibility controls
    FieldVisibility,
    /// Field dependency validation
    FieldDependencies,
    /// Validation constraints
    ValidationConstraints,
    /// Custom generator hints
    CustomHints,
    /// Large file/streaming output
    Streaming,
    /// Incremental generation
    Incremental,
}
```

## Processing Services

Services are first-class citizens in CSIL. Here's how to process them effectively:

### Iterating Through Services

```rust
for rule in &input.csil_spec.rules {
    if let CsilRuleType::ServiceDef(service) = &rule.rule_type {
        let service_name = &rule.name;
        println!("Processing service: {}", service_name);
        
        for operation in &service.operations {
            match operation.direction {
                CsilServiceDirection::Unidirectional => {
                    // Generate standard request-response method
                    generate_unidirectional_operation(service_name, operation);
                }
                CsilServiceDirection::Bidirectional => {
                    // Generate streaming/bidirectional method
                    generate_bidirectional_operation(service_name, operation);
                }
                CsilServiceDirection::Reverse => {
                    // Generate callback/event handler method
                    generate_reverse_operation(service_name, operation);
                }
            }
        }
    }
}
```

### Service Code Generation Patterns

Different service directions suggest different code generation patterns:

**Unidirectional (`input -> output`)**:
- HTTP REST endpoints
- RPC method calls
- Function signatures

**Bidirectional (`input <-> output`)**:
- WebSocket handlers
- Streaming APIs
- Interactive protocols

**Reverse (`input <- output`)**:
- Event handlers
- Callbacks
- Push notifications

## Processing Field Metadata

Field metadata enables sophisticated code generation:

### Visibility Metadata

```rust
for metadata in &field.metadata {
    if let CsilFieldMetadata::Visibility(visibility) = metadata {
        match visibility {
            CsilFieldVisibility::SendOnly => {
                // Exclude from response/incoming message types
                // Include in request/outgoing message types
                field_info.include_in_requests = true;
                field_info.include_in_responses = false;
            }
            CsilFieldVisibility::ReceiveOnly => {
                // Exclude from request/outgoing message types
                // Include in response/incoming message types
                field_info.include_in_requests = false;
                field_info.include_in_responses = true;
            }
            CsilFieldVisibility::Bidirectional => {
                // Include in all message types (default)
                field_info.include_in_requests = true;
                field_info.include_in_responses = true;
            }
        }
    }
}
```

### Validation Constraints

```rust
for metadata in &field.metadata {
    if let CsilFieldMetadata::Constraint(constraint) = metadata {
        match constraint {
            CsilValidationConstraint::MinLength(min) => {
                // Generate length validation
                validations.push(format!("if {}.len() < {} {{ return Err(...) }}", 
                    field_name, min));
            }
            CsilValidationConstraint::MaxLength(max) => {
                validations.push(format!("if {}.len() > {} {{ return Err(...) }}", 
                    field_name, max));
            }
            CsilValidationConstraint::MinItems(min) => {
                // For arrays/collections
            }
            CsilValidationConstraint::MaxItems(max) => {
                // For arrays/collections  
            }
            CsilValidationConstraint::Custom { name, value } => {
                // Handle custom validation rules
            }
        }
    }
}
```

### Field Dependencies

```rust
for metadata in &field.metadata {
    if let CsilFieldMetadata::DependsOn { field, value } = metadata {
        // Generate conditional validation
        if let Some(expected_value) = value {
            dependencies.push(format!(
                "if {} == {:?} && {}.is_none() {{ return Err(...) }}",
                field, expected_value, current_field_name
            ));
        } else {
            dependencies.push(format!(
                "if {}.is_some() && {}.is_none() {{ return Err(...) }}",
                field, current_field_name
            ));
        }
    }
}
```

### Custom Generator Hints

```rust
for metadata in &field.metadata {
    if let CsilFieldMetadata::Custom { name, parameters } = metadata {
        if name == "rust" {
            // Handle Rust-specific annotations
            for param in parameters {
                match param.name.as_deref() {
                    Some("skip_serializing_if") => {
                        attributes.push(format!("#[serde(skip_serializing_if = \"{}\")]", 
                            param.value));
                    }
                    Some("rename") => {
                        attributes.push(format!("#[serde(rename = \"{}\")]", 
                            param.value));
                    }
                    _ => {}
                }
            }
        }
    }
}
```

## Type Mapping

### CDDL Basic Types

```rust
fn map_builtin_type(name: &str) -> &'static str {
    match name {
        "int" => "i64",
        "uint" => "u64", 
        "nint" => "i64",  // negative int
        "float" => "f64",
        "float16" => "f32",
        "float32" => "f32",
        "float64" => "f64",
        "text" => "String",
        "tstr" => "String",  // text string
        "bytes" => "Vec<u8>",
        "bstr" => "Vec<u8>", // byte string
        "bool" => "bool",
        "nil" => "()",       // or Option<()>
        "null" => "()",
        "undefined" => "()", 
        "any" => "serde_json::Value", // or your any type
        _ => name, // Pass through for custom types
    }
}
```

### Complex Types

```rust
fn map_type_expression(expr: &CsilTypeExpression) -> String {
    match expr {
        CsilTypeExpression::Builtin(name) => {
            map_builtin_type(name).to_string()
        }
        CsilTypeExpression::Reference(name) => {
            // User-defined type reference
            name.clone()
        }
        CsilTypeExpression::Array { element_type, occurrence } => {
            let element = map_type_expression(element_type);
            match occurrence {
                Some(CsilOccurrence::Optional) => format!("Option<Vec<{}>>", element),
                Some(CsilOccurrence::ZeroOrMore) => format!("Vec<{}>", element),
                Some(CsilOccurrence::OneOrMore) => format!("Vec<{}>", element), // + constraint
                Some(CsilOccurrence::Exact(n)) => format!("[{}; {}]", element, n),
                Some(CsilOccurrence::Range { min, max }) => {
                    // Generate validation for range
                    format!("Vec<{}> /* {}..{} */", element, 
                        min.map_or("0".to_string(), |m| m.to_string()),
                        max.map_or("∞".to_string(), |m| m.to_string()))
                }
                None => format!("Vec<{}>", element),
            }
        }
        CsilTypeExpression::Map { key, value, occurrence } => {
            let key_type = map_type_expression(key);
            let value_type = map_type_expression(value);
            
            let base_type = format!("HashMap<{}, {}>", key_type, value_type);
            match occurrence {
                Some(CsilOccurrence::Optional) => format!("Option<{}>", base_type),
                _ => base_type,
            }
        }
        CsilTypeExpression::Group(group) => {
            // Inline group - generate anonymous struct
            format!("/* inline group with {} entries */", group.entries.len())
        }
        CsilTypeExpression::Choice(choices) => {
            // Generate enum/union
            format!("/* choice of {} types */", choices.len())
        }
        CsilTypeExpression::Range { start, end, inclusive } => {
            // Numeric range constraint
            let op = if *inclusive { "..=" } else { ".." };
            match (start, end) {
                (Some(s), Some(e)) => format!("RangeInclusive<i64> /* {}{}{}  */", s, op, e),
                (Some(s), None) => format!("RangeFrom<i64> /* {}{} */", s, op),
                (None, Some(e)) => format!("RangeTo<i64> /* {}{} */", op, e),
                (None, None) => "RangeFull /* .. */".to_string(),
            }
        }
        CsilTypeExpression::Literal(literal) => {
            // Literal value constraint
            format!("/* literal {:?} */", literal)
        }
        CsilTypeExpression::Socket(name) => {
            format!("${}", name) // Socket reference
        }
        CsilTypeExpression::Plug(name) => {
            format!("$${}", name) // Plug reference
        }
    }
}
```

## Configuration Processing

Your generator can accept custom options via the CLI:

```bash
csilgen generate --input api.csil --target my-generator \
  --option indent_size=4 \
  --option use_tabs=false \
  --option generate_docs=true \
  --option custom_header="// Custom header comment"
```

Process these options:

```rust
#[derive(Debug)]
struct MyGeneratorConfig {
    indent_size: usize,
    use_tabs: bool,
    generate_docs: bool,
    custom_header: Option<String>,
}

impl Default for MyGeneratorConfig {
    fn default() -> Self {
        Self {
            indent_size: 2,
            use_tabs: false,
            generate_docs: true,
            custom_header: None,
        }
    }
}

fn process_config(options: &HashMap<String, serde_json::Value>) -> MyGeneratorConfig {
    let mut config = MyGeneratorConfig::default();
    
    if let Some(serde_json::Value::Number(size)) = options.get("indent_size") {
        if let Some(size) = size.as_u64() {
            config.indent_size = size as usize;
        }
    }
    
    if let Some(serde_json::Value::Bool(use_tabs)) = options.get("use_tabs") {
        config.use_tabs = *use_tabs;
    }
    
    if let Some(serde_json::Value::Bool(gen_docs)) = options.get("generate_docs") {
        config.generate_docs = *gen_docs;
    }
    
    if let Some(serde_json::Value::String(header)) = options.get("custom_header") {
        config.custom_header = Some(header.clone());
    }
    
    config
}
```

## Warning Generation

Generate helpful warnings to guide users:

```rust
fn generate_warnings(input: &WasmGeneratorInput) -> Vec<GeneratorWarning> {
    let mut warnings = Vec::new();
    
    // Check for empty specification
    if input.csil_spec.service_count == 0 {
        warnings.push(GeneratorWarning {
            level: WarningLevel::Info,
            message: "No services found - generated code will only include data types".to_string(),
            location: None,
            suggestion: Some("Add service definitions for complete API generation".to_string()),
        });
    }
    
    // Check for security issues
    for rule in &input.csil_spec.rules {
        if let CsilRuleType::GroupDef(group) = &rule.rule_type {
            for entry in &group.entries {
                if let Some(CsilGroupKey::Bare(field_name)) = &entry.key {
                    // Check for potentially sensitive fields without proper visibility
                    if is_sensitive_field(field_name) {
                        let has_visibility = entry.metadata.iter().any(|m| {
                            matches!(m, CsilFieldMetadata::Visibility(_))
                        });
                        
                        if !has_visibility {
                            warnings.push(GeneratorWarning {
                                level: WarningLevel::Warning,
                                message: format!("Potentially sensitive field '{}' has no visibility annotation", field_name),
                                location: Some(SourceLocation {
                                    line: rule.position.line,
                                    column: rule.position.column,
                                    offset: rule.position.offset,
                                    context: Some(format!("{}.{}", rule.name, field_name)),
                                }),
                                suggestion: Some("Consider adding @send-only, @receive-only, or @bidirectional".to_string()),
                            });
                        }
                    }
                }
            }
        }
    }
    
    warnings
}

fn is_sensitive_field(name: &str) -> bool {
    let sensitive_patterns = [
        "password", "secret", "token", "key", "auth", "credential",
        "private", "confidential", "sensitive", "ssn", "social"
    ];
    
    let lower_name = name.to_lowercase();
    sensitive_patterns.iter().any(|pattern| lower_name.contains(pattern))
}
```

## Testing Your Generator

Create comprehensive tests for your WASM generator:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_basic_type_mapping() {
        assert_eq!(map_builtin_type("text"), "String");
        assert_eq!(map_builtin_type("int"), "i64");
        assert_eq!(map_builtin_type("bool"), "bool");
    }
    
    #[test]
    fn test_service_processing() {
        let input = create_test_input_with_service();
        let result = process_generation_request(input);
        
        assert!(result.is_ok());
        let output = result.unwrap();
        
        // Should generate service code
        assert!(output.files.iter().any(|f| f.path.contains("service")));
        
        // Should report service count correctly
        assert_eq!(output.stats.services_count, 1);
    }
    
    #[test]
    fn test_metadata_processing() {
        let input = create_test_input_with_metadata();
        let result = process_generation_request(input);
        
        assert!(result.is_ok());
        let output = result.unwrap();
        
        // Should handle field metadata
        assert_eq!(output.stats.fields_with_metadata_count, 2);
        
        // Should generate appropriate warnings
        assert!(output.warnings.iter().any(|w| 
            w.message.contains("visibility")));
    }
    
    #[test]
    fn test_wasm_interface_compliance() {
        // Test the actual WASM interface functions
        let test_input = create_test_input();
        let input_json = serde_json::to_string(&test_input).unwrap();
        let input_bytes = input_json.as_bytes();
        
        // Test generate function
        let result_ptr = generate(input_bytes.as_ptr(), input_bytes.len());
        assert!(!result_ptr.is_null());
        
        // Read result
        let result_len = unsafe { std::ptr::read(result_ptr as *const u32) };
        assert!(result_len > 0);
        
        let result_data = unsafe {
            std::slice::from_raw_parts(result_ptr.add(4), result_len as usize)
        };
        
        let result_str = std::str::from_utf8(result_data).unwrap();
        let output: WasmGeneratorOutput = serde_json::from_str(result_str).unwrap();
        
        assert!(!output.files.is_empty());
        
        // Clean up
        deallocate(result_ptr, result_len as usize + 4);
    }
    
    #[test]
    fn test_metadata_export() {
        let metadata_ptr = get_metadata();
        assert!(!metadata_ptr.is_null());
        
        let metadata_len = unsafe { std::ptr::read(metadata_ptr as *const u32) };
        assert!(metadata_len > 0);
        
        let metadata_data = unsafe {
            std::slice::from_raw_parts(metadata_ptr.add(4), metadata_len as usize)
        };
        
        let metadata_str = std::str::from_utf8(metadata_data).unwrap();
        let metadata: GeneratorMetadata = serde_json::from_str(metadata_str).unwrap();
        
        assert!(!metadata.name.is_empty());
        assert!(!metadata.target.is_empty());
        assert!(!metadata.capabilities.is_empty());
    }
    
    fn create_test_input() -> WasmGeneratorInput {
        // Helper function to create test input
        // ... implementation details
    }
}
```

## Performance Considerations

### Memory Usage

- Monitor peak memory usage and report in `GenerationStats`
- Use streaming approaches for large outputs when possible
- Consider generator capability flags for memory-intensive features

### Generation Speed

- Cache expensive operations when processing multiple similar structures
- Use efficient string building (e.g., `String::with_capacity()`)
- Consider parallelization for independent file generation

### WASM Module Size

- Use release builds with optimization
- Consider `wasm-opt` for further size reduction
- Minimize dependencies to reduce module size

## Security Considerations

### Input Validation

- Always validate input sizes against limits
- Check for null pointers before dereferencing
- Validate UTF-8 encoding of input strings
- Sanitize user-provided field names and values

### Output Validation

- Prevent code injection through generated content
- Validate file paths to prevent directory traversal
- Sanitize generated identifiers for target language safety

### Resource Limits

- Respect memory limits (`MAX_INPUT_SIZE`, `MAX_OUTPUT_SIZE`)
- Implement timeouts for long-running operations
- Monitor and report resource usage

This interface specification enables powerful, secure, and standardized CSIL code generation while maintaining full compatibility with the CSIL ecosystem.