use std::process::Command;
use std::path::Path;
use std::env;

#[test]
fn test_cross_platform_path_handling() {
    let csilgen = get_csilgen_binary();
    
    // Test with different path separators and formats
    let test_file = "tests/fixtures/basic/simple-types.csil";
    if !Path::new(test_file).exists() {
        return;
    }
    
    // Test absolute path
    let absolute_path = std::fs::canonicalize(test_file)
        .expect("Failed to get absolute path");
    
    let abs_output = run_csilgen_command(&[
        "validate",
        "--input", absolute_path.to_str().unwrap()
    ]);
    
    assert!(
        abs_output.status.success(),
        "Absolute path handling failed: {}", 
        String::from_utf8_lossy(&abs_output.stderr)
    );
    
    // Test relative path
    let rel_output = run_csilgen_command(&[
        "validate", 
        "--input", test_file
    ]);
    
    assert!(
        rel_output.status.success(),
        "Relative path handling failed: {}", 
        String::from_utf8_lossy(&rel_output.stderr)
    );
}

#[test]
fn test_cross_platform_line_endings() {
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp directory");
    let csilgen = get_csilgen_binary();
    
    // Create test files with different line endings
    let unix_file = temp_dir.path().join("unix.csil");
    let windows_file = temp_dir.path().join("windows.csil");
    
    let test_content = "SimpleType = {\n    field: text,\n    value: int\n}";
    let windows_content = test_content.replace('\n', "\r\n");
    
    std::fs::write(&unix_file, test_content).expect("Failed to write unix file");
    std::fs::write(&windows_file, windows_content).expect("Failed to write windows file");
    
    // Both should parse successfully
    for (label, file) in [("Unix", &unix_file), ("Windows", &windows_file)] {
        let validate_output = run_csilgen_command(&[
            "validate",
            "--input", file.to_str().unwrap()
        ]);
        
        assert!(
            validate_output.status.success(),
            "{} line endings failed: {}", 
            label,
            String::from_utf8_lossy(&validate_output.stderr)
        );
    }
}

#[test]
fn test_cross_platform_file_permissions() {
    let csilgen = get_csilgen_binary();
    
    // Test handling of read-only files
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp directory");
    let readonly_file = temp_dir.path().join("readonly.csil");
    
    std::fs::write(&readonly_file, "ReadOnlyType = { value: text }").expect("Failed to write readonly file");
    
    // Make file read-only (Unix/Linux only)
    if cfg!(unix) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&readonly_file).expect("Failed to get metadata").permissions();
        perms.set_mode(0o444); // Read-only
        std::fs::set_permissions(&readonly_file, perms).expect("Failed to set permissions");
    }
    
    // Should be able to validate read-only files
    let validate_output = run_csilgen_command(&[
        "validate",
        "--input", readonly_file.to_str().unwrap()
    ]);
    
    assert!(
        validate_output.status.success(),
        "Read-only file validation failed: {}", 
        String::from_utf8_lossy(&validate_output.stderr)
    );
    
    // Format should handle read-only files gracefully (should not try to modify)
    let format_output = run_csilgen_command(&[
        "format",
        readonly_file.to_str().unwrap(),
        "--dry-run"
    ]);
    
    // Dry run should work even on read-only files
    assert!(
        format_output.status.success(),
        "Format dry-run on read-only file failed: {}", 
        String::from_utf8_lossy(&format_output.stderr)
    );
}

#[test]
fn test_cross_platform_environment_detection() {
    let csilgen = get_csilgen_binary();
    
    // Test that CLI behaves appropriately on different platforms
    let help_output = run_csilgen_command(&["--help"]);
    
    assert!(help_output.status.success(),
        "Help should work on all platforms: {}", 
        String::from_utf8_lossy(&help_output.stderr));
    
    let help_text = String::from_utf8_lossy(&help_output.stdout);
    
    // Check for platform-appropriate behavior hints in help text
    if cfg!(windows) {
        // Windows-specific checks if needed
        assert!(!help_text.is_empty(), "Help output should not be empty on Windows");
    } else if cfg!(unix) {
        // Unix-specific checks if needed  
        assert!(!help_text.is_empty(), "Help output should not be empty on Unix");
    }
}

#[test]
fn test_cross_platform_temp_directory_usage() {
    let csilgen = get_csilgen_binary();
    
    // Test generation to different temp directory locations
    let test_file = "tests/fixtures/basic/simple-types.csil";
    if !Path::new(test_file).exists() {
        return;
    }
    
    // Use platform-appropriate temp directory
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp directory");
    
    let generate_output = run_csilgen_command(&[
        "generate",
        "--input", test_file,
        "--target", "noop", 
        "--output", temp_dir.path().to_str().unwrap()
    ]);
    
    assert!(
        generate_output.status.success(),
        "Cross-platform temp directory usage failed: {}", 
        String::from_utf8_lossy(&generate_output.stderr)
    );
    
    // Verify temp directory handling
    assert!(temp_dir.path().exists(), "Temp directory should exist");
}

#[test]
fn test_cross_platform_executable_detection() {
    // Test that the test framework correctly finds the csilgen binary on different platforms
    let binary = get_csilgen_binary();
    
    if binary.file_name().unwrap() == "cargo" {
        // Using cargo run - verify cargo is available
        let cargo_version = Command::new("cargo")
            .args(&["--version"])
            .output()
            .expect("Cargo should be available for cargo run");
        
        assert!(cargo_version.status.success(), "Cargo should be functional");
    } else {
        // Using direct binary - verify it exists and is executable
        assert!(binary.exists(), "csilgen binary should exist: {}", binary.display());
        
        let version_output = Command::new(&binary)
            .args(&["--version"])
            .output()
            .expect("Binary should be executable");
        
        assert!(version_output.status.success(), 
            "Binary should respond to --version: {}", binary.display());
    }
}

// Helper functions

fn get_csilgen_binary() -> std::path::PathBuf {
    // Check for platform-specific binary extensions
    let binary_name = if cfg!(windows) { "csilgen-cli.exe" } else { "csilgen-cli" };
    let alt_binary_name = if cfg!(windows) { "csilgen.exe" } else { "csilgen" };
    
    let possible_paths = [
        format!("target/debug/{}", binary_name),
        format!("target/release/{}", binary_name),
        format!("target/debug/{}", alt_binary_name),
        format!("target/release/{}", alt_binary_name),
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
    run_csilgen_command_with_binary(&get_csilgen_binary(), args)
}

fn run_csilgen_command_with_binary(binary: &std::path::Path, args: &[&str]) -> std::process::Output {
    if binary.file_name().unwrap() == "cargo" {
        let mut cargo_args = vec!["run", "-p", "csilgen-cli", "--"];
        cargo_args.extend_from_slice(args);
        
        Command::new("cargo")
            .args(&cargo_args)
            .output()
            .expect("Failed to run csilgen via cargo")
    } else {
        Command::new(binary)
            .args(args)
            .output()
            .expect("Failed to run csilgen binary")
    }
}