use std::process::Command;
use std::path::Path;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_format_dry_run_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test formatting basic fixtures
    let format_output = run_csilgen_command(&[
        "format", 
        "tests/fixtures/basic/",
        "--dry-run"
    ]);
    
    assert!(
        format_output.status.success(),
        "Format dry-run failed: {}", 
        String::from_utf8_lossy(&format_output.stderr)
    );
    
    // Dry run should not modify any files
    let output_text = String::from_utf8_lossy(&format_output.stdout);
    if output_text.contains("formatted") {
        // If implemented, should show what would be formatted without changing files
        assert!(!output_text.contains("wrote") && !output_text.contains("modified"),
            "Dry run should not modify files");
    }
}

#[test]
fn test_format_in_place_workflow() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let csilgen = get_csilgen_binary();
    
    // Copy a test file to temp directory
    let source_file = "tests/fixtures/basic/simple-types.csil";
    if !Path::new(source_file).exists() {
        return;
    }
    
    let temp_file = temp_dir.path().join("test.csil");
    fs::copy(source_file, &temp_file).expect("Failed to copy test file");
    
    let original_content = fs::read_to_string(&temp_file).expect("Failed to read original file");
    
    // Format the file in place
    let format_output = run_csilgen_command(&[
        "format",
        temp_file.to_str().unwrap()
    ]);
    
    // Should succeed or show todo error
    assert!(
        format_output.status.success(),
        "Format in-place failed: {}", 
        String::from_utf8_lossy(&format_output.stderr)
    );
    
    // File should still exist and be valid
    assert!(temp_file.exists(), "Formatted file should still exist");
    
    let new_content = fs::read_to_string(&temp_file).expect("Failed to read formatted file");
    assert!(!new_content.is_empty(), "Formatted file should not be empty");
    
    // If formatter is implemented, content might change; if not, should be same
    if format_output.status.success() {
        // File should still be parseable after formatting
        let validate_output = run_csilgen_command(&[
            "validate",
            "--input", temp_file.to_str().unwrap()
        ]);
        
        assert!(
            validate_output.status.success(),
            "Formatted file should still be valid: {}", 
            String::from_utf8_lossy(&validate_output.stderr)
        );
    }
}

#[test]
fn test_format_directory_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test formatting entire directories
    let directories = [
        "tests/fixtures/basic/",
        "tests/fixtures/services/",
        "examples/basic-usage/"
    ];
    
    for dir in &directories {
        if !Path::new(dir).exists() {
            continue;
        }
        
        let format_output = run_csilgen_command(&[
            "format",
            dir,
            "--dry-run"
        ]);
        
        assert!(
            format_output.status.success(),
            "Format directory {} failed: {}", 
            dir,
            String::from_utf8_lossy(&format_output.stderr)
        );
    }
}

#[test]
fn test_lint_basic_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test linting basic fixtures
    let lint_output = run_csilgen_command(&[
        "lint",
        "tests/fixtures/basic/"
    ]);
    
    assert!(
        lint_output.status.success(),
        "Lint basic workflow failed: {}", 
        String::from_utf8_lossy(&lint_output.stderr)
    );
}

#[test]
fn test_lint_with_fix_workflow() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let csilgen = get_csilgen_binary();
    
    // Copy a test file to temp directory
    let source_file = "tests/fixtures/basic/simple-types.csil";
    if !Path::new(source_file).exists() {
        return;
    }
    
    let temp_file = temp_dir.path().join("test.csil");
    fs::copy(source_file, &temp_file).expect("Failed to copy test file");
    
    // Lint with auto-fix
    let lint_output = run_csilgen_command(&[
        "lint",
        temp_file.to_str().unwrap(),
        "--fix"
    ]);
    
    assert!(
        lint_output.status.success(),
        "Lint with fix failed: {}", 
        String::from_utf8_lossy(&lint_output.stderr)
    );
    
    // File should still exist and be valid after auto-fix
    assert!(temp_file.exists(), "File should still exist after lint --fix");
    
    if lint_output.status.success() {
        let validate_output = run_csilgen_command(&[
            "validate",
            "--input", temp_file.to_str().unwrap()
        ]);
        
        assert!(
            validate_output.status.success(),
            "File should still be valid after lint --fix: {}", 
            String::from_utf8_lossy(&validate_output.stderr)
        );
    }
}

#[test]
fn test_lint_directory_workflow() {
    let csilgen = get_csilgen_binary();
    
    // Test linting various directories
    let directories = [
        "tests/fixtures/basic/",
        "tests/fixtures/services/",
        "tests/fixtures/metadata/",
        "examples/basic-usage/"
    ];
    
    for dir in &directories {
        if !Path::new(dir).exists() {
            continue;
        }
        
        let lint_output = run_csilgen_command(&[
            "lint",
            dir
        ]);
        
        assert!(
            lint_output.status.success(),
            "Lint directory {} failed: {}", 
            dir,
            String::from_utf8_lossy(&lint_output.stderr)
        );
    }
}

#[test]
fn test_format_lint_integration() {
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    let csilgen = get_csilgen_binary();
    
    // Copy a test file
    let source_file = "tests/fixtures/services/simple-service.csil";
    if !Path::new(source_file).exists() {
        return;
    }
    
    let temp_file = temp_dir.path().join("test.csil");
    fs::copy(source_file, &temp_file).expect("Failed to copy test file");
    
    // Format first
    let format_output = run_csilgen_command(&[
        "format",
        temp_file.to_str().unwrap()
    ]);
    
    if !format_output.status.success() {
        panic!("Format failed: {}", String::from_utf8_lossy(&format_output.stderr));
    }
    
    // Then lint
    let lint_output = run_csilgen_command(&[
        "lint", 
        temp_file.to_str().unwrap()
    ]);
    
    assert!(
        lint_output.status.success(),
        "Lint after format failed: {}", 
        String::from_utf8_lossy(&lint_output.stderr)
    );
    
    // File should still be valid
    let validate_output = run_csilgen_command(&[
        "validate",
        "--input", temp_file.to_str().unwrap()
    ]);
    
    assert!(
        validate_output.status.success(),
        "File invalid after format+lint: {}", 
        String::from_utf8_lossy(&validate_output.stderr)
    );
}

#[test]
fn test_format_lint_on_invalid_files() {
    let csilgen = get_csilgen_binary();
    
    // Test how format/lint handle invalid files
    let invalid_files = [
        "tests/fixtures/invalid/invalid-cddl-syntax.csil",
        "tests/fixtures/invalid/invalid-csil-services.csil",
        "tests/fixtures/invalid/invalid-metadata.csil"
    ];
    
    for invalid_file in &invalid_files {
        if !Path::new(invalid_file).exists() {
            continue;
        }
        
        // Format should handle invalid files gracefully
        let format_output = run_csilgen_command(&[
            "format",
            invalid_file,
            "--dry-run"
        ]);
        
        // Should either refuse to format (exit with error) or show todo
        assert!(
            !format_output.status.success(),
            "Format should refuse invalid file: {}", invalid_file
        );
        
        // Lint should also handle invalid files gracefully  
        let lint_output = run_csilgen_command(&[
            "lint",
            invalid_file
        ]);
        
        assert!(
            !lint_output.status.success(),
            "Lint should refuse invalid file: {}", invalid_file
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