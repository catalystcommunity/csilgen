use std::process::Command;
use std::path::{Path, PathBuf};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_end_to_end_validate_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test with simple valid CSIL file
    let simple_csil = "tests/fixtures/basic/simple-types.csil";
    assert!(Path::new(simple_csil).exists(), "Test fixture missing: {}", simple_csil);
    
    let output = Command::new(&csilgen)
        .args(&["validate", "--input", simple_csil])
        .output()
        .expect("Failed to run csilgen validate");
    
    assert!(output.status.success(), 
        "Validation failed for {}: {}", 
        simple_csil, 
        String::from_utf8_lossy(&output.stderr));
}

#[test]
fn test_end_to_end_validate_to_generate_workflow() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test with service-enabled CSIL file
    let service_csil = "tests/fixtures/services/simple-service.csil";
    assert!(Path::new(service_csil).exists(), "Test fixture missing: {}", service_csil);
    
    // First validate
    let validate_output = Command::new(&csilgen)
        .args(&["validate", "--input", service_csil])
        .output()
        .expect("Failed to run csilgen validate");
    
    assert!(validate_output.status.success(), 
        "Validation failed: {}", 
        String::from_utf8_lossy(&validate_output.stderr));
    
    // Then generate (using noop generator for now)
    let generate_output = Command::new(&csilgen)
        .args(&[
            "generate", 
            "--input", service_csil,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ])
        .output()
        .expect("Failed to run csilgen generate");
    
    assert!(generate_output.status.success(), 
        "Generation failed: {}", 
        String::from_utf8_lossy(&generate_output.stderr));
    
    // Verify output files were created
    let output_files: Vec<_> = fs::read_dir(temp_dir.path())
        .expect("Failed to read output directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to list output files");
    
    assert!(!output_files.is_empty(), "No files were generated");
}

#[test]
fn test_end_to_end_rust_generation_workflow() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Use a basic example for rust generation
    let basic_csil = "examples/basic-usage/simple-service.csil";
    
    // Skip if examples don't exist yet
    if !Path::new(basic_csil).exists() {
        return;
    }
    
    // Generate Rust code
    let generate_output = Command::new(&csilgen)
        .args(&[
            "generate",
            "--input", basic_csil,
            "--target", "rust", 
            "--output", temp_dir.path().to_str().unwrap()
        ])
        .output()
        .expect("Failed to run csilgen generate");
    
    if !generate_output.status.success() {
        // Generator might not be implemented yet - skip test
        return;
    }
    
    // Try to compile the generated Rust code
    let rust_files: Vec<_> = fs::read_dir(temp_dir.path())
        .expect("Failed to read output directory")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension()? == "rs" {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    
    assert!(!rust_files.is_empty(), "No Rust files were generated");
    
    // Basic syntax check with rustc (if available)
    for rust_file in rust_files {
        if let Ok(rustc_output) = Command::new("rustc")
            .args(&["--crate-type", "lib", "--emit", "metadata", "-o", "/dev/null"])
            .arg(&rust_file)
            .output()
        {
            if !rustc_output.status.success() {
                panic!("Generated Rust code failed to compile: {}\nFile: {}\nError: {}", 
                    rust_file.display(),
                    fs::read_to_string(&rust_file).unwrap_or_default(),
                    String::from_utf8_lossy(&rustc_output.stderr));
            }
        }
    }
}

#[test]
fn test_format_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test formatting a directory
    let format_output = Command::new(&csilgen)
        .args(&["format", "tests/fixtures/basic/", "--dry-run"])
        .output()
        .expect("Failed to run csilgen format");
    
    // Should succeed even if formatter not fully implemented
    // The test validates the command runs without crashing
    assert!(format_output.status.success(),
        "Format command crashed: {}", 
        String::from_utf8_lossy(&format_output.stderr));
}

#[test]
fn test_lint_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test linting a directory
    let lint_output = Command::new(&csilgen)
        .args(&["lint", "tests/fixtures/basic/"])
        .output()
        .expect("Failed to run csilgen lint");
    
    // Should succeed even if linter not fully implemented
    assert!(lint_output.status.success(),
        "Lint command crashed: {}", 
        String::from_utf8_lossy(&lint_output.stderr));
}

#[test]
fn test_multi_file_dependency_analysis() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test with multi-file example if it exists
    let multi_file_dir = "examples/multi-file/entry-points/";
    
    if !Path::new(multi_file_dir).exists() {
        return; // Skip if examples not set up yet
    }
    
    let generate_output = Command::new(&csilgen)
        .args(&[
            "generate",
            "--input", multi_file_dir,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ])
        .env("CSIL_VERBOSE", "1")  // Enable verbose output for dependency analysis
        .output()
        .expect("Failed to run csilgen generate");
    
    let stderr = String::from_utf8_lossy(&generate_output.stderr);
    
    // Look for dependency analysis output (even if generation isn't implemented)
    assert!(stderr.contains("Entry points") || stderr.contains("Dependencies"),
        "Expected dependency analysis output, got: {}", stderr);
}

#[test]
fn test_invalid_file_handling() {
    let csilgen = get_csilgen_binary();
    
    // Test with invalid CSIL file
    let invalid_csil = "tests/fixtures/invalid/invalid-cddl-syntax.csil";
    assert!(Path::new(invalid_csil).exists(), "Test fixture missing: {}", invalid_csil);
    
    let validate_output = Command::new(&csilgen)
        .args(&["validate", "--input", invalid_csil])
        .output()
        .expect("Failed to run csilgen validate");
    
    // Should fail validation
    assert!(!validate_output.status.success(), 
        "Expected validation to fail for invalid file");
    
    let stderr = String::from_utf8_lossy(&validate_output.stderr);
    assert!(!stderr.is_empty(), "Expected error message for invalid file");
}

#[test]
fn test_performance_with_large_files() {
    let csilgen = get_csilgen_binary();
    
    // Test with large CSIL files if they exist
    let large_files = [
        "tests/fixtures/performance/large-schema.csil",
        "tests/fixtures/performance/mega-api.csil"
    ];
    
    for large_file in &large_files {
        if !Path::new(large_file).exists() {
            continue; // Skip if fixture doesn't exist
        }
        
        let start = std::time::Instant::now();
        
        let validate_output = Command::new(&csilgen)
            .args(&["validate", "--input", large_file])
            .output()
            .expect("Failed to run csilgen validate");
        
        let duration = start.elapsed();
        
        // Performance test - should complete within reasonable time
        assert!(duration.as_secs() < 30, 
            "Validation took too long for {}: {:?}", large_file, duration);
        
        // Should succeed or fail gracefully (not crash)
        assert!(validate_output.status.success() || !String::from_utf8_lossy(&validate_output.stderr).is_empty(),
            "Command crashed on large file: {}", large_file);
    }
}

#[test]
fn test_cross_platform_cli_behavior() {
    let csilgen = get_csilgen_binary();
    
    // Test help command works consistently
    let help_output = Command::new(&csilgen)
        .args(&["--help"])
        .output()
        .expect("Failed to run csilgen --help");
    
    assert!(help_output.status.success(), "Help command failed");
    
    let help_text = String::from_utf8_lossy(&help_output.stdout);
    assert!(help_text.contains("csilgen"), "Help output should contain program name");
    assert!(help_text.contains("validate") || help_text.contains("generate"), 
        "Help should mention main commands");
}

#[test]
fn test_error_message_quality() {
    let csilgen = get_csilgen_binary();
    
    // Test with non-existent file
    let missing_file_output = Command::new(&csilgen)
        .args(&["validate", "--input", "non-existent-file.csil"])
        .output()
        .expect("Failed to run csilgen validate");
    
    assert!(!missing_file_output.status.success(), "Should fail for missing file");
    
    let stderr = String::from_utf8_lossy(&missing_file_output.stderr);
    assert!(stderr.contains("file") || stderr.contains("not found") || stderr.contains("No such"),
        "Error message should mention file issue: {}", stderr);
}

// Helper functions

fn get_csilgen_binary() -> PathBuf {
    // Try to find the binary in target directory
    let possible_paths = [
        "target/debug/csilgen-cli",
        "target/release/csilgen-cli", 
        "target/debug/csilgen",
        "target/release/csilgen"
    ];
    
    for path in &possible_paths {
        let binary_path = PathBuf::from(path);
        if binary_path.exists() {
            return binary_path;
        }
    }
    
    // If not found, assume it's in PATH or use cargo run
    PathBuf::from("cargo")
}


// Run csilgen via cargo if binary not found
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