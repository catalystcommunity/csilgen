use std::process::Command;
use std::path::Path;
use std::collections::HashMap;

#[test]
fn test_cli_help_command() {
    let csilgen = get_csilgen_binary();
    
    // Test all help variations
    let help_commands = ["--help", "-h", "help"];
    
    for help_cmd in &help_commands {
        let help_output = run_csilgen_command(&[help_cmd]);
        
        assert!(help_output.status.success(),
            "Help command '{}' failed: {}", 
            help_cmd,
            String::from_utf8_lossy(&help_output.stderr));
        
        let help_text = String::from_utf8_lossy(&help_output.stdout);
        assert!(help_text.contains("csilgen") || help_text.contains("CSIL"),
            "Help output should contain program information");
    }
}

#[test]
fn test_cli_version_command() {
    let csilgen = get_csilgen_binary();
    
    let version_commands = ["--version", "-V"];
    
    for version_cmd in &version_commands {
        let version_output = run_csilgen_command(&[version_cmd]);
        
        assert!(version_output.status.success(),
            "Version command '{}' failed: {}", 
            version_cmd,
            String::from_utf8_lossy(&version_output.stderr));
        
        let version_text = String::from_utf8_lossy(&version_output.stdout);
        assert!(!version_text.is_empty(),
            "Version output should not be empty");
    }
}

#[test] 
fn test_cli_subcommand_help() {
    let csilgen = get_csilgen_binary();
    
    let subcommands = ["validate", "generate", "breaking", "format", "lint"];
    
    for subcmd in &subcommands {
        let help_output = run_csilgen_command(&[subcmd, "--help"]);
        
        assert!(help_output.status.success(),
            "Subcommand help '{}' failed: {}", 
            subcmd,
            String::from_utf8_lossy(&help_output.stderr));
        
        let help_text = String::from_utf8_lossy(&help_output.stdout);
        assert!(help_text.contains(subcmd),
            "Subcommand help should mention the command name: {}", subcmd);
    }
}

#[test]
fn test_cli_error_exit_codes() {
    let csilgen = get_csilgen_binary();
    
    // Test invalid command
    let invalid_output = run_csilgen_command(&["invalid-command"]);
    assert!(!invalid_output.status.success(),
        "Invalid command should return non-zero exit code");
    
    // Test missing required arguments
    let missing_args_output = run_csilgen_command(&["validate"]);
    assert!(!missing_args_output.status.success(),
        "Missing arguments should return non-zero exit code");
    
    // Test non-existent file
    let missing_file_output = run_csilgen_command(&[
        "validate", 
        "--input", "definitely-does-not-exist.csil"
    ]);
    assert!(!missing_file_output.status.success(),
        "Non-existent file should return non-zero exit code");
}

#[test]
fn test_cli_environment_variables() {
    let csilgen = get_csilgen_binary();
    
    let simple_csil = "tests/fixtures/basic/simple-types.csil";
    if !Path::new(simple_csil).exists() {
        return;
    }
    
    // Test CSIL_VERBOSE environment variable
    let verbose_output = Command::new(&csilgen)
        .args(&["validate", "--input", simple_csil])
        .env("CSIL_VERBOSE", "1")
        .output()
        .expect("Failed to run with CSIL_VERBOSE");
    
    // Should not crash with verbose mode
    assert!(
        verbose_output.status.success(),
        "Verbose mode should not crash: {}", 
        String::from_utf8_lossy(&verbose_output.stderr)
    );
    
    // Test other potential environment variables
    let env_vars = [
        ("CSIL_CONFIG", "test-config"),
        ("CSIL_LOG_LEVEL", "debug"),
        ("NO_COLOR", "1")
    ];
    
    for (env_var, value) in &env_vars {
        let env_output = Command::new(&csilgen)
            .args(&["--help"])
            .env(env_var, value)
            .output()
            .expect("Failed to run with environment variable");
        
        // Should not crash with environment variables set
        assert!(env_output.status.success(),
            "Should not crash with {}={}: {}", 
            env_var, value,
            String::from_utf8_lossy(&env_output.stderr));
    }
}

#[test]
fn test_cli_different_working_directories() {
    let csilgen = get_csilgen_binary();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp directory");
    
    // Copy a test file to temp directory
    let source_file = "tests/fixtures/basic/simple-types.csil";
    if !Path::new(source_file).exists() {
        return;
    }
    
    let temp_file = temp_dir.path().join("test.csil");
    std::fs::copy(source_file, &temp_file).expect("Failed to copy test file");
    
    // Run from different working directory
    let output = Command::new(&csilgen)
        .args(&["validate", "--input", temp_file.to_str().unwrap()])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to run from different directory");
    
    assert!(
        output.status.success(),
        "CLI should work from different working directories: {}", 
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cli_unicode_and_special_characters() {
    let csilgen = get_csilgen_binary();
    let temp_dir = tempfile::TempDir::new().expect("Failed to create temp directory");
    
    // Test with paths containing unicode characters
    let unicode_dir = temp_dir.path().join("测试-ディレクトリ");
    std::fs::create_dir_all(&unicode_dir).expect("Failed to create unicode directory");
    
    let unicode_file = unicode_dir.join("测试.csil");
    std::fs::write(&unicode_file, r#"
TestType = {
    name: text,
    value: int
}
"#).expect("Failed to write unicode test file");
    
    let validate_output = run_csilgen_command(&[
        "validate",
        "--input", unicode_file.to_str().unwrap()
    ]);
    
    // Should handle unicode paths gracefully
    assert!(
        validate_output.status.success(),
        "Should handle unicode paths: {}", 
        String::from_utf8_lossy(&validate_output.stderr)
    );
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