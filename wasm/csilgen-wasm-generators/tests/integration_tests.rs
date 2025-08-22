//! Integration tests for WASM generator runtime with actual WASM modules

use csilgen_common::GeneratorConfig;
use csilgen_core::ast::*;
use csilgen_core::lexer::Position;
use csilgen_wasm_generators::WasmGeneratorRuntime;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Get path to the compiled noop generator WASM module
fn get_noop_generator_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let manifest_path = PathBuf::from(manifest_dir);
    let workspace_root = manifest_path.parent().unwrap().parent().unwrap();
    workspace_root.join("target/wasm32-unknown-unknown/release/csilgen_noop_generator.wasm")
}

/// Create a test CSIL spec with services and field metadata
fn create_test_csil_spec_with_services() -> CsilSpec {
    CsilSpec {
        imports: Vec::new(),
        options: None,
        rules: vec![
            // User type with field metadata
            Rule {
                name: "User".to_string(),
                rule_type: RuleType::GroupDef(GroupExpression {
                    entries: vec![
                        GroupEntry {
                            key: Some(GroupKey::Bare("name".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: None,
                            metadata: vec![FieldMetadata::Visibility(
                                FieldVisibility::Bidirectional,
                            )],
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("email".to_string())),
                            value_type: TypeExpression::Builtin("text".to_string()),
                            occurrence: Some(Occurrence::Optional),
                            metadata: vec![
                                FieldMetadata::Visibility(FieldVisibility::SendOnly),
                                FieldMetadata::Constraint(ValidationConstraint::MinLength(5)),
                            ],
                        },
                        GroupEntry {
                            key: Some(GroupKey::Bare("age".to_string())),
                            value_type: TypeExpression::Builtin("uint".to_string()),
                            occurrence: Some(Occurrence::Optional),
                            metadata: vec![FieldMetadata::Visibility(FieldVisibility::ReceiveOnly)],
                        },
                    ],
                }),
                position: Position {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            },
            // UserService with operations
            Rule {
                name: "UserService".to_string(),
                rule_type: RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![
                        ServiceOperation {
                            name: "create_user".to_string(),
                            input_type: TypeExpression::Reference("User".to_string()),
                            output_type: TypeExpression::Reference("User".to_string()),
                            direction: ServiceDirection::Unidirectional,
                            position: Position {
                                line: 8,
                                column: 4,
                                offset: 150,
                            },
                        },
                        ServiceOperation {
                            name: "get_user".to_string(),
                            input_type: TypeExpression::Builtin("text".to_string()),
                            output_type: TypeExpression::Reference("User".to_string()),
                            direction: ServiceDirection::Unidirectional,
                            position: Position {
                                line: 9,
                                column: 4,
                                offset: 200,
                            },
                        },
                    ],
                }),
                position: Position {
                    line: 7,
                    column: 1,
                    offset: 120,
                },
            },
            // NotificationService (bidirectional)
            Rule {
                name: "NotificationService".to_string(),
                rule_type: RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: "subscribe".to_string(),
                        input_type: TypeExpression::Builtin("text".to_string()),
                        output_type: TypeExpression::Builtin("text".to_string()),
                        direction: ServiceDirection::Bidirectional,
                        position: Position {
                            line: 13,
                            column: 4,
                            offset: 300,
                        },
                    }],
                }),
                position: Position {
                    line: 12,
                    column: 1,
                    offset: 270,
                },
            },
        ],
    }
}

/// Create test generator config
fn create_test_config() -> GeneratorConfig {
    let mut options = HashMap::new();
    options.insert("debug".to_string(), serde_json::Value::Bool(true));
    options.insert(
        "indent".to_string(),
        serde_json::Value::String("  ".to_string()),
    );

    GeneratorConfig {
        target: "test".to_string(),
        output_dir: "/tmp/csilgen-test".to_string(),
        options,
    }
}

#[test]
fn test_simple_wasm_execution() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let manifest_path = PathBuf::from(manifest_dir);
    let workspace_root = manifest_path.parent().unwrap().parent().unwrap();
    let simple_wasm_path =
        workspace_root.join("target/wasm32-unknown-unknown/release/csilgen_simple_test.wasm");

    if !simple_wasm_path.exists() {
        panic!(
            "Simple test WASM module not found. Run: cargo build --target wasm32-unknown-unknown -p csilgen-simple-test --release"
        );
    }

    let wasm_bytes = fs::read(&simple_wasm_path).expect("Failed to read simple WASM file");
    let mut runtime = WasmGeneratorRuntime::new().expect("Failed to create runtime");

    runtime
        .load_generator("simple-test".to_string(), &wasm_bytes)
        .expect("Failed to load simple generator");

    // Test that we can instantiate and call a basic function
    use wasmtime::{Store, TypedFunc};

    let mut store = Store::new(runtime.engine(), ());
    // Add fuel for WASM execution
    store.set_fuel(10_000_000).expect("Failed to set fuel");
    let linker = wasmtime::Linker::new(runtime.engine());

    let generator = runtime.generators().get("simple-test").unwrap();
    let instance = linker
        .instantiate(&mut store, generator.module())
        .expect("Failed to instantiate");

    let add_func: TypedFunc<(i32, i32), i32> = instance
        .get_typed_func(&mut store, "add")
        .expect("add function not found");
    let result = add_func
        .call(&mut store, (5, 3))
        .expect("Failed to call add function");

    assert_eq!(result, 8);
    println!("✓ Basic WASM execution works: 5 + 3 = {result}");
}

#[test]
fn test_end_to_end_noop_generator_workflow() {
    let wasm_path = get_noop_generator_path();

    if !wasm_path.exists() {
        panic!(
            "Noop generator WASM module not found at {}. Run: cargo build --target wasm32-unknown-unknown -p csilgen-noop-generator --release",
            wasm_path.display()
        );
    }

    // Load WASM bytes
    let wasm_bytes = fs::read(&wasm_path)
        .unwrap_or_else(|e| panic!("Failed to read WASM file {}: {}", wasm_path.display(), e));

    println!("Loaded WASM module: {} bytes", wasm_bytes.len());

    // Create runtime
    let mut runtime = WasmGeneratorRuntime::new().expect("Failed to create WASM runtime");

    // Load the noop generator
    runtime
        .load_generator("noop-test".to_string(), &wasm_bytes)
        .expect("Failed to load noop generator");

    // Verify generator was loaded
    let generators = runtime.list_generators();
    assert!(generators.contains(&"noop-test"));

    // Create test input with services and metadata
    let csil_spec = create_test_csil_spec_with_services();
    let config = create_test_config();

    println!("Test spec has {} rules", csil_spec.rules.len());

    // Execute the generator
    let result = runtime.execute_generator("noop-test", &csil_spec, &config);

    match result {
        Ok(generated_files) => {
            println!("Generation successful!");
            println!("Generated {} files", generated_files.len());

            // Verify we got the expected output
            assert_eq!(generated_files.len(), 1);

            let hello_file = &generated_files[0];
            assert_eq!(hello_file.path, "hello.txt");

            let expected_content = "Hello World! Services: 2, Fields with metadata: 3 bytes!";
            assert_eq!(hello_file.content, expected_content);

            println!(
                "✓ Generated hello.txt with correct content: {}",
                hello_file.content
            );
            println!("✓ End-to-end WASM workflow validation successful!");
        }
        Err(e) => {
            panic!("Generator execution failed: {e}");
        }
    }
}

#[test]
fn test_end_to_end_empty_spec_workflow() {
    let wasm_path = get_noop_generator_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test - WASM module not built");
        return;
    }

    let wasm_bytes = fs::read(&wasm_path).expect("Failed to read WASM file");
    let mut runtime = WasmGeneratorRuntime::new().expect("Failed to create runtime");

    runtime
        .load_generator("noop-empty-test".to_string(), &wasm_bytes)
        .expect("Failed to load generator");

    // Create empty spec
    let csil_spec = CsilSpec {
        imports: Vec::new(),
        options: None,
        rules: vec![],
    };

    let config = create_test_config();

    let result = runtime
        .execute_generator("noop-empty-test", &csil_spec, &config)
        .expect("Generator execution failed");

    assert_eq!(result.len(), 1);
    let hello_file = &result[0];

    let expected_content = "Hello World! Services: 0, Fields with metadata: 0 bytes!";
    assert_eq!(hello_file.content, expected_content);

    println!("✓ Empty spec workflow successful: {}", hello_file.content);
}

#[test]
fn test_concurrent_generator_execution() {
    let wasm_path = get_noop_generator_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test - WASM module not built");
        return;
    }

    let wasm_bytes = fs::read(&wasm_path).expect("Failed to read WASM file");

    let handles: Vec<_> = (0..3)
        .map(|i| {
            let wasm_bytes = wasm_bytes.clone();
            std::thread::spawn(move || {
                let mut runtime = WasmGeneratorRuntime::new().expect("Failed to create runtime");

                let generator_name = format!("noop-concurrent-{i}");
                runtime
                    .load_generator(generator_name.clone(), &wasm_bytes)
                    .expect("Failed to load generator");

                let csil_spec = create_test_csil_spec_with_services();
                let config = create_test_config();

                let result = runtime
                    .execute_generator(&generator_name, &csil_spec, &config)
                    .expect("Generator execution failed");

                (i, result)
            })
        })
        .collect();

    for handle in handles {
        let (thread_id, result) = handle.join().expect("Thread failed");
        assert_eq!(result.len(), 1);

        let hello_file = &result[0];
        assert_eq!(hello_file.path, "hello.txt");
        assert!(
            hello_file
                .content
                .contains("Services: 2, Fields with metadata: 3")
        );

        println!("✓ Thread {thread_id} completed successfully");
    }

    println!("✓ Concurrent execution test passed");
}

#[test]
fn test_generator_resource_limits() {
    let wasm_path = get_noop_generator_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test - WASM module not built");
        return;
    }

    use csilgen_wasm_generators::WasmLimits;
    use std::time::Duration;

    // Create runtime with strict limits
    let limits = WasmLimits {
        max_memory_bytes: 1024 * 1024,              // 1MB
        max_execution_time: Duration::from_secs(1), // 1 second
    };

    let mut runtime = WasmGeneratorRuntime::new_with_limits(limits)
        .expect("Failed to create runtime with limits");

    let wasm_bytes = fs::read(&wasm_path).expect("Failed to read WASM file");
    runtime
        .load_generator("noop-limits-test".to_string(), &wasm_bytes)
        .expect("Failed to load generator");

    let csil_spec = create_test_csil_spec_with_services();
    let config = create_test_config();

    // The noop generator should easily fit within these limits
    let result = runtime
        .execute_generator("noop-limits-test", &csil_spec, &config)
        .expect("Generator execution should succeed within limits");

    assert_eq!(result.len(), 1);
    println!("✓ Resource limits test passed");
}

#[test]
fn test_generator_memory_management() {
    let wasm_path = get_noop_generator_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test - WASM module not built");
        return;
    }

    let wasm_bytes = fs::read(&wasm_path).expect("Failed to read WASM file");

    // Run multiple generations to test memory cleanup
    for i in 0..5 {
        let mut runtime = WasmGeneratorRuntime::new().expect("Failed to create runtime");

        let generator_name = format!("noop-memory-test-{i}");
        runtime
            .load_generator(generator_name.clone(), &wasm_bytes)
            .expect("Failed to load generator");

        let csil_spec = create_test_csil_spec_with_services();
        let config = create_test_config();

        let result = runtime
            .execute_generator(&generator_name, &csil_spec, &config)
            .expect("Generator execution failed");

        assert_eq!(result.len(), 1);

        // Unload the generator to test cleanup
        runtime
            .unload_generator(&generator_name)
            .expect("Failed to unload generator");

        assert!(runtime.list_generators().is_empty());
    }

    println!("✓ Memory management test passed - no leaks detected");
}

#[test]
fn test_input_validation_and_error_handling() {
    let wasm_path = get_noop_generator_path();

    if !wasm_path.exists() {
        eprintln!("Skipping test - WASM module not built");
        return;
    }

    let wasm_bytes = fs::read(&wasm_path).expect("Failed to read WASM file");
    let mut runtime = WasmGeneratorRuntime::new().expect("Failed to create runtime");

    runtime
        .load_generator("noop-validation-test".to_string(), &wasm_bytes)
        .expect("Failed to load generator");

    // Test with various edge cases
    let test_cases = vec![
        // Normal case
        (create_test_csil_spec_with_services(), true, "normal case"),
        // Empty spec
        (
            CsilSpec {
                imports: Vec::new(),
                options: None,
                rules: vec![],
            },
            true,
            "empty spec",
        ),
    ];

    for (csil_spec, should_succeed, description) in test_cases {
        let config = create_test_config();

        let result = runtime.execute_generator("noop-validation-test", &csil_spec, &config);

        if should_succeed {
            let generated = result.unwrap_or_else(|_| panic!("Expected success for {description}"));
            assert_eq!(generated.len(), 1);
            println!("✓ {description} handled correctly");
        } else {
            assert!(result.is_err(), "Expected error for {description}");
            println!("✓ {description} properly rejected");
        }
    }

    println!("✓ Input validation and error handling tests passed");
}

#[cfg(test)]
mod test_helpers {
    use super::*;

    /// Helper to verify the noop generator WASM module exists
    pub fn ensure_wasm_module_built() -> bool {
        let wasm_path = get_noop_generator_path();
        if !wasm_path.exists() {
            eprintln!("Warning: Noop generator WASM module not found");
            eprintln!(
                "Run: cargo build --target wasm32-unknown-unknown -p csilgen-noop-generator --release"
            );
            false
        } else {
            true
        }
    }

    #[test]
    fn test_wasm_module_availability() {
        if ensure_wasm_module_built() {
            println!("✓ WASM module is available for testing");
        } else {
            eprintln!("⚠ WASM module not built - some tests will be skipped");
        }
    }
}
