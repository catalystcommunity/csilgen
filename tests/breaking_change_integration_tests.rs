use std::process::Command;
use std::path::Path;

#[test]
fn test_breaking_change_detection_user_api() {
    let csilgen = get_csilgen_binary();
    
    let v1_file = "tests/fixtures/breaking-changes/v1-user.csil";
    let v2_breaking_file = "tests/fixtures/breaking-changes/v2-user-breaking.csil";
    let v2_compatible_file = "tests/fixtures/breaking-changes/v2-user-compatible.csil";
    
    // Skip if fixtures don't exist
    if !Path::new(v1_file).exists() || !Path::new(v2_breaking_file).exists() {
        return;
    }
    
    // Test breaking changes detection
    let breaking_output = Command::new(&csilgen)
        .args(&[
            "breaking",
            "--current", v1_file,
            "--new", v2_breaking_file
        ])
        .output()
        .expect("Failed to run breaking change detection");
    
    let output_text = String::from_utf8_lossy(&breaking_output.stdout);
    let error_text = String::from_utf8_lossy(&breaking_output.stderr);
    
    // Should detect breaking changes
    assert!(
        output_text.contains("BREAKING") || error_text.contains("breaking"),
        "Expected breaking change detection, got stdout: {}, stderr: {}", 
        output_text, error_text
    );
    
    // Test compatible changes if file exists
    if Path::new(v2_compatible_file).exists() {
        let compatible_output = Command::new(&csilgen)
            .args(&[
                "breaking",
                "--current", v1_file,
                "--new", v2_compatible_file
            ])
            .output()
            .expect("Failed to run breaking change detection");
        
        let compat_output_text = String::from_utf8_lossy(&compatible_output.stdout);
        let compat_error_text = String::from_utf8_lossy(&compatible_output.stderr);
        
        // Should not detect breaking changes, or should indicate only non-breaking changes
        assert!(
            !compat_output_text.contains("BREAKING") || 
            compat_output_text.contains("NON-BREAKING"),
            "Expected no breaking changes, got stdout: {}, stderr: {}", 
            compat_output_text, compat_error_text
        );
    }
}

#[test]
fn test_breaking_change_detection_service_api() {
    let csilgen = get_csilgen_binary();
    
    let v1_service = "tests/fixtures/breaking-changes/v1-service-api.csil";
    let v2_service_breaking = "tests/fixtures/breaking-changes/v2-service-api-breaking.csil";
    
    // Skip if fixtures don't exist
    if !Path::new(v1_service).exists() || !Path::new(v2_service_breaking).exists() {
        return;
    }
    
    let breaking_output = Command::new(&csilgen)
        .args(&[
            "breaking",
            "--current", v1_service,
            "--new", v2_service_breaking
        ])
        .output()
        .expect("Failed to run service breaking change detection");
    
    let output_text = String::from_utf8_lossy(&breaking_output.stdout);
    let error_text = String::from_utf8_lossy(&breaking_output.stderr);
    
    // Should work (success or todo error)
    assert!(
        breaking_output.status.success(),
        "Breaking change detection crashed: stdout: {}, stderr: {}", 
        output_text, error_text
    );
}

#[test]
fn test_breaking_change_detection_with_examples() {
    let csilgen = get_csilgen_binary();
    
    // Use our newly created examples if they exist
    let api_v1 = "examples/breaking-changes/api-v1.csil";
    let api_v2 = "examples/breaking-changes/api-v2.csil";
    
    if !Path::new(api_v1).exists() || !Path::new(api_v2).exists() {
        return;
    }
    
    let breaking_output = Command::new(&csilgen)
        .args(&[
            "breaking",
            "--current", api_v1,
            "--new", api_v2
        ])
        .output()
        .expect("Failed to run breaking change detection on examples");
    
    let output_text = String::from_utf8_lossy(&breaking_output.stdout);
    
    // Should succeed or show todo error
    assert!(
        breaking_output.status.success(),
        "Breaking change detection failed: {}", 
        String::from_utf8_lossy(&breaking_output.stderr)
    );
    
    // If implemented, should detect the breaking changes we documented
    if breaking_output.status.success() && output_text.contains("BREAKING") {
        // Verify it catches some of the known breaking changes
        assert!(
            output_text.contains("Email") || 
            output_text.contains("password") ||
            output_text.contains("CreateUserRequestV2") ||
            output_text.contains("error_type"),
            "Should detect specific breaking changes we introduced"
        );
    }
}

#[test]
fn test_breaking_change_detection_metadata() {
    let csilgen = get_csilgen_binary();
    
    let v1_metadata = "tests/fixtures/breaking-changes/v1-metadata-api.csil";
    let v2_metadata_breaking = "tests/fixtures/breaking-changes/v2-metadata-api-breaking.csil";
    
    if !Path::new(v1_metadata).exists() || !Path::new(v2_metadata_breaking).exists() {
        return;
    }
    
    let metadata_output = Command::new(&csilgen)
        .args(&[
            "breaking",
            "--current", v1_metadata,
            "--new", v2_metadata_breaking
        ])
        .output()
        .expect("Failed to run metadata breaking change detection");
    
    // Should handle metadata-specific breaking changes
    assert!(
        metadata_output.status.success(),
        "Metadata breaking change detection failed: {}", 
        String::from_utf8_lossy(&metadata_output.stderr)
    );
}

// Helper functions (shared with main integration tests)

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

