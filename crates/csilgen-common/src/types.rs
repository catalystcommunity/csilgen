//! Common types used across csilgen

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for code generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorConfig {
    /// Target language or format
    pub target: String,
    /// Output directory
    pub output_dir: String,
    /// Generator-specific options
    pub options: HashMap<String, serde_json::Value>,
}

/// Result of code generation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedFile {
    /// Relative path for the generated file
    pub path: String,
    /// Generated content
    pub content: String,
}

/// Collection of generated files
pub type GeneratedFiles = Vec<GeneratedFile>;

/// WASM Generator Interface Definition
/// This defines the standardized interface that all WASM generators must implement
/// Metadata about a WASM generator
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeneratorMetadata {
    /// Human-readable generator name
    pub name: String,
    /// Semantic version of the generator
    pub version: String,
    /// Brief description of what the generator produces
    pub description: String,
    /// Target language or format
    pub target: String,
    /// List of capabilities this generator supports
    pub capabilities: Vec<GeneratorCapability>,
    /// Author information
    pub author: Option<String>,
    /// Homepage or repository URL
    pub homepage: Option<String>,
}

/// Capabilities that a generator can support
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GeneratorCapability {
    /// Supports basic CDDL types
    BasicTypes,
    /// Supports CDDL groups and choices
    ComplexStructures,
    /// Supports CSIL service definitions
    Services,
    /// Supports CSIL field metadata
    FieldMetadata,
    /// Supports field visibility controls
    FieldVisibility,
    /// Supports field dependency validation
    FieldDependencies,
    /// Supports validation constraints
    ValidationConstraints,
    /// Supports custom generator hints
    CustomHints,
    /// Supports streaming/large file outputs
    Streaming,
    /// Supports incremental generation
    Incremental,
}

/// Standardized input format for WASM generators
/// Contains everything a generator needs to produce code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmGeneratorInput {
    /// Parsed and validated CSIL specification with services and metadata
    pub csil_spec: CsilSpecSerialized,
    /// Generator configuration
    pub config: GeneratorConfig,
    /// Generator-specific metadata
    pub generator_metadata: GeneratorMetadata,
}

/// Serialized representation of CSIL specification for WASM boundary
/// This ensures consistent AST format across all generators
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilSpecSerialized {
    /// All rules in the CSIL specification
    pub rules: Vec<CsilRule>,
    /// Original source content for error reporting
    pub source_content: Option<String>,
    /// Total count of services for quick access
    pub service_count: usize,
    /// Total count of fields with metadata for quick access
    pub fields_with_metadata_count: usize,
}

/// Serialized CSIL rule with position information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilRule {
    /// Rule name/identifier
    pub name: String,
    /// Rule type and content
    pub rule_type: CsilRuleType,
    /// Source position
    pub position: CsilPosition,
}

/// Position in CSIL source file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilPosition {
    /// Line number (1-based)
    pub line: usize,
    /// Column number (1-based)
    pub column: usize,
    /// Byte offset from start of file
    pub offset: usize,
}

/// CSIL rule types for WASM boundary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CsilRuleType {
    /// Type definition rule
    TypeDef(CsilTypeExpression),
    /// Group definition rule
    GroupDef(CsilGroupExpression),
    /// Type choice rule
    TypeChoice(Vec<CsilTypeExpression>),
    /// Group choice rule  
    GroupChoice(Vec<CsilGroupExpression>),
    /// Service definition with operations
    ServiceDef(CsilServiceDefinition),
}

/// CSIL type expressions for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilTypeExpression {
    /// Built-in types (int, text, bool, etc.)
    Builtin(String),
    /// User-defined type reference
    Reference(String),
    /// Array type with occurrence
    Array {
        element_type: Box<CsilTypeExpression>,
        occurrence: Option<CsilOccurrence>,
    },
    /// Map type
    Map {
        key: Box<CsilTypeExpression>,
        value: Box<CsilTypeExpression>,
        occurrence: Option<CsilOccurrence>,
    },
    /// Group expression
    Group(CsilGroupExpression),
    /// Choice between types
    Choice(Vec<CsilTypeExpression>),
    /// Range expression
    Range {
        start: Option<i64>,
        end: Option<i64>,
        inclusive: bool,
    },
    /// Socket reference
    Socket(String),
    /// Plug reference
    Plug(String),
    /// Literal values
    Literal(CsilLiteralValue),
    /// Constrained type with constraints
    Constrained {
        base_type: Box<CsilTypeExpression>,
        constraints: Vec<CsilControlOperator>,
    },
}

/// CSIL control operators for constraints
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilControlOperator {
    /// Size constraint
    Size(CsilSizeConstraint),
    /// Regex constraint
    Regex(String),
    /// Default value constraint
    Default(CsilLiteralValue),
    /// Greater than or equal constraint
    GreaterEqual(CsilLiteralValue),
    /// Less than or equal constraint
    LessEqual(CsilLiteralValue),
    /// Greater than constraint
    GreaterThan(CsilLiteralValue),
    /// Less than constraint
    LessThan(CsilLiteralValue),
    /// Equal constraint
    Equal(CsilLiteralValue),
    /// Not equal constraint
    NotEqual(CsilLiteralValue),
    /// Bits constraint
    Bits(String),
    /// And constraint
    And(Box<CsilTypeExpression>),
    /// Within constraint
    Within(Box<CsilTypeExpression>),
    /// JSON encoding constraint
    Json,
    /// CBOR encoding constraint
    Cbor,
    /// CBOR sequence constraint
    Cborseq,
}

/// Size constraint specifications
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilSizeConstraint {
    Exact(u64),
    Range { min: u64, max: u64 },
    Min(u64),
    Max(u64),
}

/// CSIL literal values for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilLiteralValue {
    Integer(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Bool(bool),
    Null,
    Array(Vec<CsilLiteralValue>),
}

/// CSIL occurrence indicators for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilOccurrence {
    /// Optional (?)
    Optional,
    /// Zero or more (*)
    ZeroOrMore,
    /// One or more (+)
    OneOrMore,
    /// Exact count (5)
    Exact(u64),
    /// Range (1*5, *10, 1*)
    Range { min: Option<u64>, max: Option<u64> },
}

/// CSIL group expression for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsilGroupExpression {
    pub entries: Vec<CsilGroupEntry>,
}

/// CSIL group entry with field metadata for WASM boundary  
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsilGroupEntry {
    pub key: Option<CsilGroupKey>,
    pub value_type: CsilTypeExpression,
    pub occurrence: Option<CsilOccurrence>,
    /// Rich field metadata for code generation
    pub metadata: Vec<CsilFieldMetadata>,
}

/// CSIL group key types for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilGroupKey {
    /// Bare key (identifier)
    Bare(String),
    /// Type key (type:)
    Type(CsilTypeExpression),
    /// Literal key ("string": or 42:)
    Literal(CsilLiteralValue),
}

/// CSIL service definition for WASM boundary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilServiceDefinition {
    pub operations: Vec<CsilServiceOperation>,
}

/// CSIL service operation for WASM boundary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsilServiceOperation {
    pub name: String,
    pub input_type: CsilTypeExpression,
    pub output_type: CsilTypeExpression,
    pub direction: CsilServiceDirection,
    pub position: CsilPosition,
}

/// CSIL service direction for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilServiceDirection {
    /// Unidirectional operation (input -> output)
    Unidirectional,
    /// Bidirectional operation (input <-> output)
    Bidirectional,
    /// Reverse operation (input <- output)
    Reverse,
}

/// CSIL field metadata for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilFieldMetadata {
    /// Field visibility metadata
    Visibility(CsilFieldVisibility),
    /// Field dependency metadata
    DependsOn {
        field: String,
        value: Option<CsilLiteralValue>,
    },
    /// Validation constraint metadata
    Constraint(CsilValidationConstraint),
    /// Documentation metadata
    Description(String),
    /// Custom generator hints
    Custom {
        name: String,
        parameters: Vec<CsilMetadataParameter>,
    },
}

/// CSIL field visibility for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilFieldVisibility {
    /// Field is only included in outgoing requests/messages
    SendOnly,
    /// Field is only included in incoming responses/messages
    ReceiveOnly,
    /// Field is included in both directions (default)
    Bidirectional,
}

/// CSIL validation constraint for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CsilValidationConstraint {
    /// Minimum length for strings/arrays
    MinLength(u64),
    /// Maximum length for strings/arrays
    MaxLength(u64),
    /// Minimum number of items for arrays/maps
    MinItems(u64),
    /// Maximum number of items for arrays/maps
    MaxItems(u64),
    /// Minimum value for numeric types
    MinValue(CsilLiteralValue),
    /// Maximum value for numeric types
    MaxValue(CsilLiteralValue),
    /// Custom validation constraint
    Custom {
        name: String,
        value: CsilLiteralValue,
    },
}

/// CSIL metadata parameter for WASM boundary
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CsilMetadataParameter {
    pub name: Option<String>,
    pub value: CsilLiteralValue,
}

/// Standardized output format from WASM generators
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WasmGeneratorOutput {
    /// Generated files with paths and content
    pub files: GeneratedFiles,
    /// Any warnings generated during processing
    pub warnings: Vec<GeneratorWarning>,
    /// Generation statistics and metadata
    pub stats: GenerationStats,
}

/// Warning message from generator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorWarning {
    /// Warning severity level
    pub level: WarningLevel,
    /// Human-readable warning message
    pub message: String,
    /// Optional location context in the CSIL spec
    pub location: Option<SourceLocation>,
    /// Optional suggestion for resolving the warning
    pub suggestion: Option<String>,
}

/// Warning severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WarningLevel {
    /// Informational message
    Info,
    /// Warning that should be addressed but doesn't prevent generation
    Warning,
    /// Deprecated feature usage
    Deprecated,
}

/// Location in source CSIL specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Line number (1-based)
    pub line: usize,
    /// Column number (1-based)  
    pub column: usize,
    /// Byte offset from start of file
    pub offset: usize,
    /// Optional rule or element name for context
    pub context: Option<String>,
}

/// Statistics about the generation process
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GenerationStats {
    /// Number of files generated
    pub files_generated: usize,
    /// Total size of generated content in bytes
    pub total_size_bytes: usize,
    /// Number of services processed
    pub services_count: usize,
    /// Number of types with field metadata
    pub fields_with_metadata_count: usize,
    /// Generation duration in milliseconds
    pub generation_time_ms: u64,
    /// Memory usage peak in bytes
    pub peak_memory_bytes: Option<usize>,
}

/// Structured error format for WASM generator failures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmGeneratorError {
    /// Error code for programmatic handling
    pub code: String,
    /// Human-readable error message
    pub message: String,
    /// Location in CSIL spec where error occurred
    pub location: Option<SourceLocation>,
    /// Additional context or debugging information
    pub context: Option<HashMap<String, serde_json::Value>>,
    /// Nested or related errors
    pub related_errors: Vec<WasmGeneratorError>,
}

/// WASM Memory Management Interface
/// Defines the memory allocation/deallocation functions that WASM modules should export
///
/// Standard WASM function signatures that generators must implement:
///
/// ```no_run
/// // Core generator function - takes serialized input, returns serialized output
/// #[unsafe(no_mangle)]
/// pub extern "C" fn generate(
///     input_ptr: *const u8,
///     input_len: usize,
///     output_ptr: *mut *mut u8,
///     output_len: *mut usize
/// ) -> i32 {
///     // Implementation would go here
///     0 // Return success code
/// }
///
/// // Memory management functions
/// #[unsafe(no_mangle)]
/// pub extern "C" fn allocate(size: usize) -> *mut u8 {
///     // Implementation would go here
///     std::ptr::null_mut()
/// }
///
/// #[unsafe(no_mangle)]
/// pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
///     // Implementation would go here
/// }
///
/// // Metadata function
/// #[unsafe(no_mangle)]
/// pub extern "C" fn get_metadata() -> *const u8 {
///     // Returns serialized GeneratorMetadata
///     std::ptr::null()
/// }
/// ```
/// Constants for WASM interface
pub mod wasm_interface {
    /// Standard function name for the main generator entry point
    pub const GENERATE_FUNCTION: &str = "generate";

    /// Standard function name for memory allocation
    pub const ALLOCATE_FUNCTION: &str = "allocate";

    /// Standard function name for memory deallocation  
    pub const DEALLOCATE_FUNCTION: &str = "deallocate";

    /// Standard function name for getting generator metadata
    pub const GET_METADATA_FUNCTION: &str = "get_metadata";

    /// Standard memory export name
    pub const MEMORY_EXPORT: &str = "memory";

    /// Error codes returned by WASM functions
    pub mod error_codes {
        pub const SUCCESS: i32 = 0;
        pub const INVALID_INPUT: i32 = 1;
        pub const SERIALIZATION_ERROR: i32 = 2;
        pub const GENERATION_ERROR: i32 = 3;
        pub const OUT_OF_MEMORY: i32 = 4;
        pub const UNSUPPORTED_FEATURE: i32 = 5;
    }

    /// Maximum input size in bytes (64MB)
    pub const MAX_INPUT_SIZE: usize = 64 * 1024 * 1024;

    /// Maximum output size in bytes (256MB)
    pub const MAX_OUTPUT_SIZE: usize = 256 * 1024 * 1024;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generator_metadata_serialization() {
        let metadata = GeneratorMetadata {
            name: "test-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test WASM generator".to_string(),
            target: "rust".to_string(),
            capabilities: vec![
                GeneratorCapability::BasicTypes,
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: Some("Test Author".to_string()),
            homepage: Some("https://example.com".to_string()),
        };

        let json = serde_json::to_string(&metadata).expect("Serialization failed");
        let deserialized: GeneratorMetadata =
            serde_json::from_str(&json).expect("Deserialization failed");

        assert_eq!(metadata, deserialized);
        assert_eq!(metadata.name, "test-generator");
        assert_eq!(metadata.capabilities.len(), 3);
        assert!(
            metadata
                .capabilities
                .contains(&GeneratorCapability::Services)
        );
    }

    #[test]
    fn test_generator_capabilities() {
        let capabilities = vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::FieldDependencies,
            GeneratorCapability::ValidationConstraints,
            GeneratorCapability::CustomHints,
            GeneratorCapability::Streaming,
            GeneratorCapability::Incremental,
        ];

        for capability in &capabilities {
            let json = serde_json::to_string(capability).expect("Serialization failed");
            let deserialized: GeneratorCapability =
                serde_json::from_str(&json).expect("Deserialization failed");
            assert_eq!(*capability, deserialized);
        }
    }

    #[test]
    fn test_csil_spec_serialized_with_services() {
        let spec = CsilSpecSerialized {
            rules: vec![
                CsilRule {
                    name: "User".to_string(),
                    rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![CsilGroupEntry {
                            key: Some(CsilGroupKey::Bare("name".to_string())),
                            value_type: CsilTypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![CsilFieldMetadata::Visibility(
                                CsilFieldVisibility::Bidirectional,
                            )],
                        }],
                    }),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                },
                CsilRule {
                    name: "UserService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![CsilServiceOperation {
                            name: "create_user".to_string(),
                            input_type: CsilTypeExpression::Reference("User".to_string()),
                            output_type: CsilTypeExpression::Reference("User".to_string()),
                            direction: CsilServiceDirection::Unidirectional,
                            position: CsilPosition {
                                line: 5,
                                column: 4,
                                offset: 100,
                            },
                        }],
                    }),
                    position: CsilPosition {
                        line: 4,
                        column: 1,
                        offset: 80,
                    },
                },
            ],
            source_content: Some("User = { name: text @bidirectional }".to_string()),
            service_count: 1,
            fields_with_metadata_count: 1,
        };

        let json = serde_json::to_string(&spec).expect("Serialization failed");
        let deserialized: CsilSpecSerialized =
            serde_json::from_str(&json).expect("Deserialization failed");

        assert_eq!(spec.service_count, 1);
        assert_eq!(spec.fields_with_metadata_count, 1);
        assert_eq!(deserialized.rules.len(), 2);

        // Verify service rule exists
        let service_rule = deserialized
            .rules
            .iter()
            .find(|r| r.name == "UserService")
            .expect("Service rule not found");

        match &service_rule.rule_type {
            CsilRuleType::ServiceDef(service) => {
                assert_eq!(service.operations.len(), 1);
                assert_eq!(service.operations[0].name, "create_user");
                assert_eq!(
                    service.operations[0].direction,
                    CsilServiceDirection::Unidirectional
                );
            }
            _ => panic!("Expected service definition"),
        }
    }

    #[test]
    fn test_csil_field_metadata_serialization() {
        let metadata_types = vec![
            CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly),
            CsilFieldMetadata::Visibility(CsilFieldVisibility::ReceiveOnly),
            CsilFieldMetadata::Visibility(CsilFieldVisibility::Bidirectional),
            CsilFieldMetadata::DependsOn {
                field: "status".to_string(),
                value: Some(CsilLiteralValue::Text("active".to_string())),
            },
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MinLength(5)),
            CsilFieldMetadata::Constraint(CsilValidationConstraint::MaxItems(100)),
            CsilFieldMetadata::Description("User's email address".to_string()),
            CsilFieldMetadata::Custom {
                name: "rust".to_string(),
                parameters: vec![CsilMetadataParameter {
                    name: Some("skip_serializing_if".to_string()),
                    value: CsilLiteralValue::Text("Option::is_none".to_string()),
                }],
            },
        ];

        for metadata in &metadata_types {
            let json = serde_json::to_string(metadata).expect("Serialization failed");
            let deserialized: CsilFieldMetadata =
                serde_json::from_str(&json).expect("Deserialization failed");

            // Match on the variants to verify proper serialization
            match (metadata, &deserialized) {
                (CsilFieldMetadata::Visibility(v1), CsilFieldMetadata::Visibility(v2)) => {
                    assert_eq!(v1, v2)
                }
                (
                    CsilFieldMetadata::DependsOn {
                        field: f1,
                        value: v1,
                    },
                    CsilFieldMetadata::DependsOn {
                        field: f2,
                        value: v2,
                    },
                ) => {
                    assert_eq!(f1, f2);
                    assert_eq!(v1, v2);
                }
                (CsilFieldMetadata::Constraint(c1), CsilFieldMetadata::Constraint(c2)) => {
                    assert_eq!(c1, c2)
                }
                (CsilFieldMetadata::Description(d1), CsilFieldMetadata::Description(d2)) => {
                    assert_eq!(d1, d2)
                }
                (
                    CsilFieldMetadata::Custom {
                        name: n1,
                        parameters: p1,
                    },
                    CsilFieldMetadata::Custom {
                        name: n2,
                        parameters: p2,
                    },
                ) => {
                    assert_eq!(n1, n2);
                    assert_eq!(p1.len(), p2.len());
                }
                _ => panic!("Metadata variant mismatch"),
            }
        }
    }

    #[test]
    fn test_wasm_generator_input_serialization() {
        let metadata = GeneratorMetadata {
            name: "test-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "rust".to_string(),
            capabilities: vec![
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: None,
            homepage: None,
        };

        let config = GeneratorConfig {
            target: "rust".to_string(),
            output_dir: "/tmp/output".to_string(),
            options: std::collections::HashMap::new(),
        };

        let spec = CsilSpecSerialized {
            rules: vec![],
            source_content: None,
            service_count: 2,
            fields_with_metadata_count: 5,
        };

        let input = WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        };

        let json = serde_json::to_string(&input).expect("Serialization failed");
        let deserialized: WasmGeneratorInput =
            serde_json::from_str(&json).expect("Deserialization failed");

        assert_eq!(
            input.csil_spec.service_count,
            deserialized.csil_spec.service_count
        );
        assert_eq!(
            input.csil_spec.fields_with_metadata_count,
            deserialized.csil_spec.fields_with_metadata_count
        );
        assert_eq!(input.config.target, deserialized.config.target);
        assert_eq!(
            input.generator_metadata.name,
            deserialized.generator_metadata.name
        );
    }

    #[test]
    fn test_wasm_generator_output_with_warnings() {
        let output = WasmGeneratorOutput {
            files: vec![
                GeneratedFile {
                    path: "user.rs".to_string(),
                    content: "struct User { name: String }".to_string(),
                },
                GeneratedFile {
                    path: "service.rs".to_string(),
                    content: "trait UserService {}".to_string(),
                },
            ],
            warnings: vec![
                GeneratorWarning {
                    level: WarningLevel::Warning,
                    message: "Field 'email' has no visibility metadata".to_string(),
                    location: Some(SourceLocation {
                        line: 3,
                        column: 5,
                        offset: 45,
                        context: Some("User.email".to_string()),
                    }),
                    suggestion: Some("Add @send-only or @receive-only annotation".to_string()),
                },
                GeneratorWarning {
                    level: WarningLevel::Deprecated,
                    message: "Using deprecated service syntax".to_string(),
                    location: None,
                    suggestion: None,
                },
            ],
            stats: GenerationStats {
                files_generated: 2,
                total_size_bytes: 1024,
                services_count: 1,
                fields_with_metadata_count: 3,
                generation_time_ms: 150,
                peak_memory_bytes: Some(2048),
            },
        };

        let json = serde_json::to_string(&output).expect("Serialization failed");
        let deserialized: WasmGeneratorOutput =
            serde_json::from_str(&json).expect("Deserialization failed");

        assert_eq!(output.files.len(), deserialized.files.len());
        assert_eq!(output.warnings.len(), deserialized.warnings.len());
        assert_eq!(
            output.stats.services_count,
            deserialized.stats.services_count
        );

        // Verify warning details
        assert_eq!(deserialized.warnings[0].level, WarningLevel::Warning);
        assert!(deserialized.warnings[0].location.is_some());
        assert_eq!(deserialized.warnings[1].level, WarningLevel::Deprecated);
        assert!(deserialized.warnings[1].location.is_none());
    }

    #[test]
    fn test_wasm_generator_error_structure() {
        let error = WasmGeneratorError {
            code: "INVALID_SERVICE_DEFINITION".to_string(),
            message: "Service operation 'create_user' has invalid input type".to_string(),
            location: Some(SourceLocation {
                line: 10,
                column: 15,
                offset: 200,
                context: Some("UserService.create_user".to_string()),
            }),
            context: Some({
                let mut context = std::collections::HashMap::new();
                context.insert(
                    "operation".to_string(),
                    serde_json::Value::String("create_user".to_string()),
                );
                context.insert(
                    "expected_type".to_string(),
                    serde_json::Value::String("User".to_string()),
                );
                context
            }),
            related_errors: vec![WasmGeneratorError {
                code: "UNDEFINED_TYPE".to_string(),
                message: "Type 'UserInput' is not defined".to_string(),
                location: None,
                context: None,
                related_errors: vec![],
            }],
        };

        let json = serde_json::to_string(&error).expect("Serialization failed");
        let deserialized: WasmGeneratorError =
            serde_json::from_str(&json).expect("Deserialization failed");

        assert_eq!(error.code, deserialized.code);
        assert_eq!(error.message, deserialized.message);
        assert!(deserialized.location.is_some());
        assert!(deserialized.context.is_some());
        assert_eq!(
            error.related_errors.len(),
            deserialized.related_errors.len()
        );
        assert_eq!(deserialized.related_errors[0].code, "UNDEFINED_TYPE");
    }

    #[test]
    fn test_generation_stats_default() {
        let stats = GenerationStats::default();

        assert_eq!(stats.files_generated, 0);
        assert_eq!(stats.total_size_bytes, 0);
        assert_eq!(stats.services_count, 0);
        assert_eq!(stats.fields_with_metadata_count, 0);
        assert_eq!(stats.generation_time_ms, 0);
        assert!(stats.peak_memory_bytes.is_none());
    }

    #[test]
    fn test_wasm_interface_constants() {
        use wasm_interface::*;

        assert_eq!(GENERATE_FUNCTION, "generate");
        assert_eq!(ALLOCATE_FUNCTION, "allocate");
        assert_eq!(DEALLOCATE_FUNCTION, "deallocate");
        assert_eq!(GET_METADATA_FUNCTION, "get_metadata");
        assert_eq!(MEMORY_EXPORT, "memory");

        assert_eq!(error_codes::SUCCESS, 0);
        assert_eq!(error_codes::INVALID_INPUT, 1);
        assert_eq!(error_codes::SERIALIZATION_ERROR, 2);
        assert_eq!(error_codes::GENERATION_ERROR, 3);
        assert_eq!(error_codes::OUT_OF_MEMORY, 4);
        assert_eq!(error_codes::UNSUPPORTED_FEATURE, 5);

        assert_eq!(MAX_INPUT_SIZE, 64 * 1024 * 1024);
        assert_eq!(MAX_OUTPUT_SIZE, 256 * 1024 * 1024);
    }

    #[test]
    fn test_service_directions() {
        let directions = vec![
            CsilServiceDirection::Unidirectional,
            CsilServiceDirection::Bidirectional,
            CsilServiceDirection::Reverse,
        ];

        for direction in &directions {
            let json = serde_json::to_string(direction).expect("Serialization failed");
            let deserialized: CsilServiceDirection =
                serde_json::from_str(&json).expect("Deserialization failed");
            assert_eq!(*direction, deserialized);
        }
    }

    #[test]
    fn test_warning_levels() {
        let levels = vec![
            WarningLevel::Info,
            WarningLevel::Warning,
            WarningLevel::Deprecated,
        ];

        for level in &levels {
            let json = serde_json::to_string(level).expect("Serialization failed");
            let deserialized: WarningLevel =
                serde_json::from_str(&json).expect("Deserialization failed");
            assert_eq!(*level, deserialized);
        }
    }

    #[test]
    fn test_complex_csil_type_expressions() {
        let complex_type = CsilTypeExpression::Array {
            element_type: Box::new(CsilTypeExpression::Map {
                key: Box::new(CsilTypeExpression::Builtin("text".to_string())),
                value: Box::new(CsilTypeExpression::Choice(vec![
                    CsilTypeExpression::Reference("User".to_string()),
                    CsilTypeExpression::Literal(CsilLiteralValue::Null),
                ])),
                occurrence: Some(CsilOccurrence::Range {
                    min: Some(1),
                    max: Some(10),
                }),
            }),
            occurrence: Some(CsilOccurrence::OneOrMore),
        };

        let json = serde_json::to_string(&complex_type).expect("Serialization failed");
        let deserialized: CsilTypeExpression =
            serde_json::from_str(&json).expect("Deserialization failed");

        match &deserialized {
            CsilTypeExpression::Array {
                element_type,
                occurrence,
            } => {
                assert_eq!(*occurrence, Some(CsilOccurrence::OneOrMore));
                match element_type.as_ref() {
                    CsilTypeExpression::Map {
                        key,
                        value,
                        occurrence,
                    } => {
                        assert_eq!(**key, CsilTypeExpression::Builtin("text".to_string()));
                        assert_eq!(
                            *occurrence,
                            Some(CsilOccurrence::Range {
                                min: Some(1),
                                max: Some(10)
                            })
                        );
                        match value.as_ref() {
                            CsilTypeExpression::Choice(choices) => {
                                assert_eq!(choices.len(), 2);
                            }
                            _ => panic!("Expected choice type"),
                        }
                    }
                    _ => panic!("Expected map type"),
                }
            }
            _ => panic!("Expected array type"),
        }
    }

    #[test]
    fn test_interface_compliance_scenario() {
        // This test simulates the full interface compliance scenario from the requirements

        // Create a realistic CSIL spec with services and metadata
        let spec = CsilSpecSerialized {
            rules: vec![
                // User type with rich metadata
                CsilRule {
                    name: "User".to_string(),
                    rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![
                            CsilGroupEntry {
                                key: Some(CsilGroupKey::Bare("name".to_string())),
                                value_type: CsilTypeExpression::Builtin("text".to_string()),
                                occurrence: None,
                                metadata: vec![
                                    CsilFieldMetadata::Visibility(
                                        CsilFieldVisibility::Bidirectional,
                                    ),
                                    CsilFieldMetadata::Description(
                                        "User's display name".to_string(),
                                    ),
                                ],
                            },
                            CsilGroupEntry {
                                key: Some(CsilGroupKey::Bare("email".to_string())),
                                value_type: CsilTypeExpression::Builtin("text".to_string()),
                                occurrence: Some(CsilOccurrence::Optional),
                                metadata: vec![
                                    CsilFieldMetadata::Visibility(CsilFieldVisibility::SendOnly),
                                    CsilFieldMetadata::Constraint(
                                        CsilValidationConstraint::MinLength(5),
                                    ),
                                ],
                            },
                        ],
                    }),
                    position: CsilPosition {
                        line: 1,
                        column: 1,
                        offset: 0,
                    },
                },
                // Service with multiple operations
                CsilRule {
                    name: "UserService".to_string(),
                    rule_type: CsilRuleType::ServiceDef(CsilServiceDefinition {
                        operations: vec![
                            CsilServiceOperation {
                                name: "create_user".to_string(),
                                input_type: CsilTypeExpression::Reference("User".to_string()),
                                output_type: CsilTypeExpression::Reference("User".to_string()),
                                direction: CsilServiceDirection::Unidirectional,
                                position: CsilPosition {
                                    line: 5,
                                    column: 4,
                                    offset: 100,
                                },
                            },
                            CsilServiceOperation {
                                name: "get_user".to_string(),
                                input_type: CsilTypeExpression::Builtin("text".to_string()),
                                output_type: CsilTypeExpression::Reference("User".to_string()),
                                direction: CsilServiceDirection::Unidirectional,
                                position: CsilPosition {
                                    line: 6,
                                    column: 4,
                                    offset: 150,
                                },
                            },
                        ],
                    }),
                    position: CsilPosition {
                        line: 4,
                        column: 1,
                        offset: 80,
                    },
                },
            ],
            source_content: Some("Complete CSIL spec with services and metadata".to_string()),
            service_count: 1,
            fields_with_metadata_count: 2,
        };

        let config = GeneratorConfig {
            target: "rust".to_string(),
            output_dir: "/tmp/generated".to_string(),
            options: {
                let mut opts = std::collections::HashMap::new();
                opts.insert("derive_debug".to_string(), serde_json::Value::Bool(true));
                opts.insert("serde_support".to_string(), serde_json::Value::Bool(true));
                opts
            },
        };

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
            homepage: Some(
                "https://github.com/catalystcommunity/csilgen/rust-generator".to_string(),
            ),
        };

        let input = WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        };

        // Test full serialization round-trip
        let input_json = serde_json::to_string(&input).expect("Input serialization failed");
        assert!(input_json.len() > 100); // Sanity check - should be substantial JSON

        let deserialized_input: WasmGeneratorInput =
            serde_json::from_str(&input_json).expect("Input deserialization failed");

        // Verify key aspects are preserved
        assert_eq!(deserialized_input.csil_spec.service_count, 1);
        assert_eq!(deserialized_input.csil_spec.fields_with_metadata_count, 2);
        assert_eq!(deserialized_input.generator_metadata.capabilities.len(), 6);

        // Verify service operations are intact
        let service_rule = deserialized_input
            .csil_spec
            .rules
            .iter()
            .find(|r| r.name == "UserService")
            .expect("Service rule should exist");

        match &service_rule.rule_type {
            CsilRuleType::ServiceDef(service) => {
                assert_eq!(service.operations.len(), 2);
                assert_eq!(service.operations[0].name, "create_user");
                assert_eq!(service.operations[1].name, "get_user");
            }
            _ => panic!("Should be a service definition"),
        }

        // Create expected output
        let output = WasmGeneratorOutput {
            files: vec![
                GeneratedFile {
                    path: "user.rs".to_string(),
                    content: "#[derive(Debug, Serialize, Deserialize)]\npub struct User {\n    pub name: String,\n    pub email: Option<String>,\n}".to_string(),
                },
                GeneratedFile {
                    path: "service.rs".to_string(),
                    content: "pub trait UserService {\n    fn create_user(&self, input: User) -> User;\n    fn get_user(&self, input: String) -> User;\n}".to_string(),
                },
            ],
            warnings: vec![],
            stats: GenerationStats {
                files_generated: 2,
                total_size_bytes: 200,
                services_count: 1,
                fields_with_metadata_count: 2,
                generation_time_ms: 85,
                peak_memory_bytes: Some(1024),
            },
        };

        let output_json = serde_json::to_string(&output).expect("Output serialization failed");
        let deserialized_output: WasmGeneratorOutput =
            serde_json::from_str(&output_json).expect("Output deserialization failed");

        assert_eq!(deserialized_output.files.len(), 2);
        assert_eq!(deserialized_output.stats.services_count, 1);
        assert_eq!(deserialized_output.stats.fields_with_metadata_count, 2);

        // This test demonstrates that the complete interface can handle:
        // 1. Complex CSIL specifications with services and metadata
        // 2. Full serialization/deserialization round trips
        // 3. Rich generator metadata and capabilities
        // 4. Structured output with statistics and warnings
        // 5. Memory management constraints and error reporting

        println!(
            "Interface compliance test passed - all components serialize/deserialize correctly"
        );
    }
}
