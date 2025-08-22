//! Noop test generator for validating complete WASM workflow
//!
//! This generator serves as a validation tool for the WASM runtime infrastructure.
//! It receives CSIL AST with services and metadata, processes configuration,
//! and returns a simple "hello.txt" file with service and metadata counts.

use csilgen_common::{
    CsilRuleType, GeneratedFile, GenerationStats, GeneratorCapability, GeneratorMetadata,
    GeneratorWarning, WarningLevel, WasmGeneratorInput, WasmGeneratorOutput, wasm_interface::*,
};

// Simple no-op logging for WASM builds
macro_rules! console_log {
    ($($t:tt)*) => {
        // No-op - we can't easily do console logging in pure WASM without wasm-bindgen
    };
}

/// Generator metadata for the noop test generator
pub const NOOP_GENERATOR_METADATA: GeneratorMetadata = GeneratorMetadata {
    name: String::new(), // Will be set at runtime
    version: String::new(),
    description: String::new(),
    target: String::new(),
    capabilities: Vec::new(),
    author: None,
    homepage: None,
};

/// Get generator metadata (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn get_metadata() -> *const u8 {
    let metadata = GeneratorMetadata {
        name: "noop-test-generator".to_string(),
        version: "1.0.0".to_string(),
        description: "Noop test generator for validating WASM workflow".to_string(),
        target: "test".to_string(),
        capabilities: vec![
            GeneratorCapability::BasicTypes,
            GeneratorCapability::ComplexStructures,
            GeneratorCapability::Services,
            GeneratorCapability::FieldMetadata,
            GeneratorCapability::FieldVisibility,
            GeneratorCapability::ValidationConstraints,
        ],
        author: Some("CSIL Team".to_string()),
        homepage: Some("https://github.com/catalystcommunity/csilgen/noop-generator".to_string()),
    };

    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(_) => return std::ptr::null(),
    };

    let bytes = metadata_json.as_bytes();
    let ptr = allocate(bytes.len() + 4);
    if ptr.is_null() {
        return std::ptr::null();
    }

    unsafe {
        // Write length first (little-endian u32)
        let len = bytes.len() as u32;
        std::ptr::write(ptr as *mut u32, len);

        // Write the JSON data
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(4), bytes.len());
    }

    ptr
}

/// Memory allocation (WASM export)
#[unsafe(no_mangle)]
pub extern "C" fn allocate(size: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(size);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf); // Prevent deallocation
    ptr
}

/// Memory deallocation (WASM export)
#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn deallocate(ptr: *mut u8, size: usize) {
    if !ptr.is_null() && size > 0 {
        unsafe {
            let _ = Vec::from_raw_parts(ptr, 0, size);
        }
    }
}

/// Main generator function (WASM export)
/// Returns pointer to result data (length-prefixed JSON string)
#[unsafe(no_mangle)]
pub extern "C" fn generate(input_ptr: *const u8, input_len: usize) -> *mut u8 {
    console_log!("Noop generator: Starting generation process");

    let result = process_generation(input_ptr, input_len);

    match result {
        Ok(output) => {
            let output_json = match serde_json::to_string(&output) {
                Ok(json) => json,
                Err(_e) => {
                    console_log!("Noop generator: Failed to serialize output");
                    return std::ptr::null_mut();
                }
            };

            let bytes = output_json.as_bytes();
            let allocated_ptr = allocate(bytes.len() + 4);
            if allocated_ptr.is_null() {
                console_log!("Noop generator: Failed to allocate memory for output");
                return std::ptr::null_mut();
            }

            unsafe {
                // Write length first (little-endian u32)
                let len = bytes.len() as u32;
                std::ptr::write(allocated_ptr as *mut u32, len);

                // Write the JSON data
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), allocated_ptr.add(4), bytes.len());
            }

            console_log!("Noop generator: Successfully generated output");
            allocated_ptr
        }
        Err(_code) => {
            console_log!("Noop generator: Generation failed");
            std::ptr::null_mut()
        }
    }
}

/// Process the generation request
fn process_generation(input_ptr: *const u8, input_len: usize) -> Result<WasmGeneratorOutput, i32> {
    console_log!("Noop generator: Processing input ({} bytes)", input_len);

    if input_ptr.is_null() || input_len == 0 {
        return Err(error_codes::INVALID_INPUT);
    }

    if input_len > MAX_INPUT_SIZE {
        console_log!(
            "Noop generator: Input size {} exceeds maximum {}",
            input_len,
            MAX_INPUT_SIZE
        );
        return Err(error_codes::INVALID_INPUT);
    }

    let input_slice = unsafe { std::slice::from_raw_parts(input_ptr, input_len) };
    let input_str = match std::str::from_utf8(input_slice) {
        Ok(s) => s,
        Err(_e) => {
            console_log!("Noop generator: Invalid UTF-8 input");
            return Err(error_codes::INVALID_INPUT);
        }
    };

    let input: WasmGeneratorInput = match serde_json::from_str(input_str) {
        Ok(input) => input,
        Err(_e) => {
            console_log!("Noop generator: Failed to deserialize input");
            return Err(error_codes::SERIALIZATION_ERROR);
        }
    };

    console_log!("Noop generator: Successfully parsed input");
    console_log!(
        "Noop generator: Target: {}, Output dir: {}",
        input.config.target,
        input.config.output_dir
    );

    // Count services and fields with metadata
    let service_count = input.csil_spec.service_count;
    let fields_with_metadata_count = input.csil_spec.fields_with_metadata_count;

    console_log!(
        "Noop generator: Found {} services, {} fields with metadata",
        service_count,
        fields_with_metadata_count
    );

    // Verify we can actually access the services and metadata from the AST
    let mut _actual_service_count = 0;
    let mut _actual_fields_with_metadata_count = 0;

    for rule in &input.csil_spec.rules {
        match &rule.rule_type {
            CsilRuleType::ServiceDef(_service) => {
                _actual_service_count += 1;
                console_log!(
                    "Noop generator: Found service '{}' with {} operations",
                    rule.name,
                    _service.operations.len()
                );
            }
            CsilRuleType::GroupDef(group) => {
                for entry in &group.entries {
                    if !entry.metadata.is_empty() {
                        _actual_fields_with_metadata_count += 1;
                        console_log!(
                            "Noop generator: Found field with {} metadata items",
                            entry.metadata.len()
                        );
                    }
                }
            }
            _ => {}
        }
    }

    console_log!(
        "Noop generator: Actual counts - services: {}, fields with metadata: {}",
        actual_service_count,
        actual_fields_with_metadata_count
    );

    // Create the hello.txt content with counts
    let hello_content = format!(
        "Hello World! Services: {service_count}, Fields with metadata: {fields_with_metadata_count} bytes!"
    );

    let generated_file = GeneratedFile {
        path: "hello.txt".to_string(),
        content: hello_content.clone(),
    };

    // Generate statistics
    let stats = GenerationStats {
        files_generated: 1,
        total_size_bytes: hello_content.len(),
        services_count: service_count,
        fields_with_metadata_count,
        generation_time_ms: 10,        // Minimal time for noop generator
        peak_memory_bytes: Some(1024), // Small memory footprint
    };

    // Create some test warnings to validate the warning system
    let mut warnings = Vec::new();

    if service_count == 0 {
        warnings.push(GeneratorWarning {
            level: WarningLevel::Info,
            message: "No services found in CSIL specification".to_string(),
            location: None,
            suggestion: Some(
                "Consider adding service definitions for better API generation".to_string(),
            ),
        });
    }

    if fields_with_metadata_count == 0 {
        warnings.push(GeneratorWarning {
            level: WarningLevel::Warning,
            message: "No fields with metadata found".to_string(),
            location: None,
            suggestion: Some(
                "Add @send-only, @receive-only, or other metadata to fields".to_string(),
            ),
        });
    }

    let output = WasmGeneratorOutput {
        files: vec![generated_file],
        warnings,
        stats,
    };

    console_log!(
        "Noop generator: Generated output with {} files and {} warnings",
        output.files.len(),
        output.warnings.len()
    );

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::*;
    use std::collections::HashMap;

    fn create_test_input_with_services_and_metadata() -> WasmGeneratorInput {
        let metadata = GeneratorMetadata {
            name: "test-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Test generator".to_string(),
            target: "test".to_string(),
            capabilities: vec![
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: None,
            homepage: None,
        };

        let config = GeneratorConfig {
            target: "test".to_string(),
            output_dir: "/tmp/test".to_string(),
            options: HashMap::new(),
        };

        let spec = CsilSpecSerialized {
            rules: vec![
                CsilRule {
                    name: "User".to_string(),
                    rule_type: CsilRuleType::GroupDef(CsilGroupExpression {
                        entries: vec![
                            CsilGroupEntry {
                                key: Some(CsilGroupKey::Bare("name".to_string())),
                                value_type: CsilTypeExpression::Builtin("text".to_string()),
                                occurrence: None,
                                metadata: vec![CsilFieldMetadata::Visibility(
                                    CsilFieldVisibility::Bidirectional,
                                )],
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
            source_content: Some("Test CSIL with services and metadata".to_string()),
            service_count: 1,
            fields_with_metadata_count: 2,
        };

        WasmGeneratorInput {
            csil_spec: spec,
            config,
            generator_metadata: metadata,
        }
    }

    #[test]
    fn test_process_generation_with_services_and_metadata() {
        let input = create_test_input_with_services_and_metadata();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);
        assert_eq!(output.files[0].path, "hello.txt");

        let content = &output.files[0].content;
        assert!(content.contains("Services: 1"));
        assert!(content.contains("Fields with metadata: 2 bytes!"));

        assert_eq!(output.stats.services_count, 1);
        assert_eq!(output.stats.fields_with_metadata_count, 2);
        assert_eq!(output.stats.files_generated, 1);
    }

    #[test]
    fn test_process_generation_empty_spec() {
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "test".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "test".to_string(),
                target: "test".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        };

        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output.files.len(), 1);

        let content = &output.files[0].content;
        assert!(content.contains("Services: 0"));
        assert!(content.contains("Fields with metadata: 0 bytes!"));

        // Should have warnings about empty spec
        assert!(!output.warnings.is_empty());
        assert!(
            output
                .warnings
                .iter()
                .any(|w| w.message.contains("No services found"))
        );
    }

    #[test]
    fn test_process_generation_invalid_input() {
        let result = process_generation(std::ptr::null(), 0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::INVALID_INPUT);
    }

    #[test]
    fn test_process_generation_invalid_json() {
        let invalid_json = b"not valid json";
        let result = process_generation(invalid_json.as_ptr(), invalid_json.len());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::SERIALIZATION_ERROR);
    }

    #[test]
    fn test_process_generation_too_large() {
        let large_size = MAX_INPUT_SIZE + 1;
        let dummy_data = [0u8; 100]; // Small actual data
        let result = process_generation(dummy_data.as_ptr(), large_size);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), error_codes::INVALID_INPUT);
    }

    #[test]
    fn test_memory_allocation_deallocation() {
        let size = 1024;
        let ptr = allocate(size);
        assert!(!ptr.is_null());

        deallocate(ptr, size);
        // Test passes if no crash occurs
    }

    #[test]
    fn test_metadata_serialization() {
        let metadata = GeneratorMetadata {
            name: "noop-test-generator".to_string(),
            version: "1.0.0".to_string(),
            description: "Noop test generator for validating WASM workflow".to_string(),
            target: "test".to_string(),
            capabilities: vec![
                GeneratorCapability::BasicTypes,
                GeneratorCapability::Services,
                GeneratorCapability::FieldMetadata,
            ],
            author: Some("CSIL Team".to_string()),
            homepage: Some(
                "https://github.com/catalystcommunity/csilgen/noop-generator".to_string(),
            ),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("noop-test-generator"));
        assert!(json.contains("Services"));
        assert!(json.contains("FieldMetadata"));
    }

    #[test]
    fn test_output_format_compliance() {
        let input = create_test_input_with_services_and_metadata();
        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();

        // Verify output structure compliance
        assert!(!output.files.is_empty());
        assert!(output.stats.files_generated > 0);
        assert!(output.stats.total_size_bytes > 0);
        assert_eq!(output.stats.services_count, input.csil_spec.service_count);
        assert_eq!(
            output.stats.fields_with_metadata_count,
            input.csil_spec.fields_with_metadata_count
        );

        // Verify file structure
        let file = &output.files[0];
        assert_eq!(file.path, "hello.txt");
        assert!(!file.content.is_empty());

        // Verify stats are reasonable
        assert!(output.stats.generation_time_ms < 1000); // Should be very fast
        assert!(output.stats.peak_memory_bytes.is_some());
    }

    #[test]
    fn test_warning_generation() {
        let input = WasmGeneratorInput {
            csil_spec: CsilSpecSerialized {
                rules: vec![],
                source_content: None,
                service_count: 0,
                fields_with_metadata_count: 0,
            },
            config: GeneratorConfig {
                target: "test".to_string(),
                output_dir: "/tmp".to_string(),
                options: HashMap::new(),
            },
            generator_metadata: GeneratorMetadata {
                name: "test".to_string(),
                version: "1.0.0".to_string(),
                description: "test".to_string(),
                target: "test".to_string(),
                capabilities: vec![],
                author: None,
                homepage: None,
            },
        };

        let input_json = serde_json::to_string(&input).unwrap();
        let input_bytes = input_json.as_bytes();

        let result = process_generation(input_bytes.as_ptr(), input_bytes.len());
        assert!(result.is_ok());

        let output = result.unwrap();

        // Should generate warnings for empty spec
        assert!(output.warnings.len() >= 2);

        let service_warning = output
            .warnings
            .iter()
            .find(|w| w.message.contains("No services found"));
        assert!(service_warning.is_some());
        assert_eq!(service_warning.unwrap().level, WarningLevel::Info);

        let metadata_warning = output
            .warnings
            .iter()
            .find(|w| w.message.contains("No fields with metadata"));
        assert!(metadata_warning.is_some());
        assert_eq!(metadata_warning.unwrap().level, WarningLevel::Warning);
    }
}
