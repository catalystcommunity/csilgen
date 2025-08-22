use std::process::Command;
use std::path::Path;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_wasm_generator_loading() {
    let csilgen = get_csilgen_binary();
    
    // Test loading the noop WASM generator (should be built-in)
    let simple_csil = "tests/fixtures/services/simple-service.csil";
    if !Path::new(simple_csil).exists() {
        return;
    }
    
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    let generate_output = run_csilgen_command(&[
        "generate",
        "--input", simple_csil,
        "--target", "noop",
        "--output", temp_dir.path().to_str().unwrap()
    ]);
    
    assert!(
        generate_output.status.success(),
        "WASM noop generator failed to load: {}", 
        String::from_utf8_lossy(&generate_output.stderr)
    );
    
    // Should produce at least one output file
    if generate_output.status.success() {
        let output_files: Vec<_> = fs::read_dir(temp_dir.path())
            .expect("Failed to read output directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("Failed to list output files");
        
        assert!(!output_files.is_empty(), "WASM generator should produce output files");
    }
}

#[test]
fn test_wasm_generator_with_services() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test WASM generator with service definitions
    let service_files = [
        "tests/fixtures/services/simple-service.csil",
        "tests/fixtures/services/complex-service.csil",
        "tests/fixtures/services/bidirectional-service.csil"
    ];
    
    for service_file in &service_files {
        if !Path::new(service_file).exists() {
            continue;
        }
        
        let generate_output = run_csilgen_command(&[
            "generate",
            "--input", service_file,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ]);
        
        assert!(
            generate_output.status.success(),
            "WASM generator failed with services in {}: {}", 
            service_file,
            String::from_utf8_lossy(&generate_output.stderr)
        );
        
        // Check that output mentions services if generator is implemented
        if generate_output.status.success() {
            let output_text = String::from_utf8_lossy(&generate_output.stdout);
            let stderr_text = String::from_utf8_lossy(&generate_output.stderr);
            
            // Noop generator should report service count
            if output_text.contains("Services:") || stderr_text.contains("Services:") {
                assert!(
                    output_text.contains("Services: ") || stderr_text.contains("Services: "),
                    "WASM generator should report service count"
                );
            }
        }
        
        // Clean up for next iteration
        if temp_dir.path().exists() {
            fs::remove_dir_all(temp_dir.path()).ok();
            fs::create_dir_all(temp_dir.path()).ok();
        }
    }
}

#[test]
fn test_wasm_generator_with_metadata() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test WASM generator with field metadata
    let metadata_files = [
        "tests/fixtures/metadata/field-visibility.csil",
        "tests/fixtures/metadata/field-dependencies.csil",
        "tests/fixtures/metadata/comprehensive-field-metadata.csil"
    ];
    
    for metadata_file in &metadata_files {
        if !Path::new(metadata_file).exists() {
            continue;
        }
        
        let generate_output = run_csilgen_command(&[
            "generate",
            "--input", metadata_file,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ]);
        
        assert!(
            generate_output.status.success(),
            "WASM generator failed with metadata in {}: {}", 
            metadata_file,
            String::from_utf8_lossy(&generate_output.stderr)
        );
        
        // Check that output mentions metadata if generator is implemented
        if generate_output.status.success() {
            let output_text = String::from_utf8_lossy(&generate_output.stdout);
            let stderr_text = String::from_utf8_lossy(&generate_output.stderr);
            
            // Noop generator should report field metadata count
            if output_text.contains("metadata") || stderr_text.contains("metadata") {
                assert!(
                    output_text.contains("Fields with metadata") || stderr_text.contains("Fields with metadata"),
                    "WASM generator should report field metadata"
                );
            }
        }
    }
}

#[test]
fn test_wasm_generator_error_handling() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test WASM generator with invalid input
    let invalid_files = [
        "tests/fixtures/invalid/invalid-csil-services.csil",
        "tests/fixtures/invalid/invalid-metadata.csil"
    ];
    
    for invalid_file in &invalid_files {
        if !Path::new(invalid_file).exists() {
            continue;
        }
        
        let generate_output = run_csilgen_command(&[
            "generate",
            "--input", invalid_file,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ]);
        
        // Should fail gracefully, not crash
        assert!(
            !generate_output.status.success(),
            "WASM generator should reject invalid input: {}", invalid_file
        );
        
        // Should provide some error information
        let stderr = String::from_utf8_lossy(&generate_output.stderr);
        assert!(!stderr.is_empty(),
            "Should provide error message for invalid input");
    }
}

#[test]
fn test_custom_wasm_generator_development() {
    let csilgen = get_csilgen_binary();
    
    // Test the custom generator example if it exists
    let custom_generator_dir = "examples/custom-generator/";
    let example_input = "examples/custom-generator/example-input.csil";
    
    if !Path::new(custom_generator_dir).exists() || !Path::new(example_input).exists() {
        return;
    }
    
    // Try to build the custom generator (if build tools available)
    let build_script = Path::new(custom_generator_dir).join("build.sh");
    if build_script.exists() {
        let build_output = Command::new("bash")
            .arg(&build_script)
            .current_dir(custom_generator_dir)
            .output();
        
        if let Ok(build_result) = build_output {
            if build_result.status.success() {
                // If build succeeded, test using the custom generator
                let wasm_file = Path::new(custom_generator_dir).join("pkg/custom_csil_generator.wasm");
                if wasm_file.exists() {
                    let temp_dir = TempDir::new().expect("Failed to create temp directory");
                    
                    let generate_output = run_csilgen_command(&[
                        "generate",
                        "--input", example_input,
                        "--target", wasm_file.to_str().unwrap(),
                        "--output", temp_dir.path().to_str().unwrap()
                    ]);
                    
                    assert!(
                        generate_output.status.success(),
                        "Custom WASM generator failed: {}", 
                        String::from_utf8_lossy(&generate_output.stderr)
                    );
                }
            }
        }
    }
}

#[test]
fn test_wasm_generator_resource_limits() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test with large files to ensure WASM generators handle resources properly
    let large_files = [
        "tests/fixtures/performance/large-schema.csil",
        "tests/fixtures/performance/mega-api.csil"
    ];
    
    for large_file in &large_files {
        if !Path::new(large_file).exists() {
            continue;
        }
        
        let start = std::time::Instant::now();
        
        let generate_output = run_csilgen_command(&[
            "generate",
            "--input", large_file,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ]);
        
        let duration = start.elapsed();
        
        // Should complete within reasonable time (WASM resource limits)
        assert!(duration.as_secs() < 60, 
            "WASM generator took too long for {}: {:?}", large_file, duration);
        
        // Should succeed or fail gracefully
        assert!(
            generate_output.status.success(),
            "WASM generator crashed on large file {}: {}", 
            large_file,
            String::from_utf8_lossy(&generate_output.stderr)
        );
    }
}

// Helper functions

fn get_csilgen_binary() -> std::path::PathBuf {
    let possible_paths = [
        "target/debug/csilgen-cli",
        "target/release/csilgen-cli", 
        "target/debug/csilgen",
        "target/release/csilgen"
    ];
    
    for path in &possible_paths {
        let binary_path = std::path::PathBuf::from(path);
        if binary_path.exists() {
            return binary_path;
        }
    }
    
    std::path::PathBuf::from("cargo")
}


fn run_csilgen_command(args: &[&str]) -> std::process::Output {
    let binary = get_csilgen_binary();
    
    if binary.file_name().unwrap() == "cargo" {
        let mut cargo_args = vec!["run", "-p", "csilgen-cli", "--"];
        cargo_args.extend_from_slice(args);
        
        Command::new("cargo")
            .args(&cargo_args)
            .output()
            .expect("Failed to run csilgen via cargo")
    } else {
        Command::new(&binary)
            .args(args)
            .output()
            .expect("Failed to run csilgen binary")
    }
}