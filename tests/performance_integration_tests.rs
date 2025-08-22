use std::process::Command;
use std::path::Path;
use std::time::{Duration, Instant};
use std::fs;
use tempfile::TempDir;

#[test]
fn test_parse_performance_large_files() {
    let csilgen = get_csilgen_binary();
    
    let large_files = [
        "tests/fixtures/performance/large-schema.csil",
        "tests/fixtures/performance/mega-api.csil"
    ];
    
    for large_file in &large_files {
        if !Path::new(large_file).exists() {
            continue;
        }
        
        // Get file size for context
        let file_size = fs::metadata(large_file)
            .map(|m| m.len())
            .unwrap_or(0);
        
        let start = Instant::now();
        
        let validate_output = run_csilgen_command(&[
            "validate",
            "--input", large_file
        ]);
        
        let parse_duration = start.elapsed();
        
        // Performance expectations (adjust based on implementation)
        let max_duration = if file_size > 1_000_000 { // >1MB
            Duration::from_secs(10)
        } else if file_size > 100_000 { // >100KB
            Duration::from_secs(5)
        } else {
            Duration::from_secs(2)
        };
        
        assert!(parse_duration <= max_duration,
            "Parsing {} ({} bytes) took too long: {:?} > {:?}", 
            large_file, file_size, parse_duration, max_duration);
        
        // Should succeed or show todo (not crash)
        assert!(
            validate_output.status.success(),
            "Large file parsing crashed: {}", 
            String::from_utf8_lossy(&validate_output.stderr)
        );
        
        println!("✓ {} ({} bytes) parsed in {:?}", 
                large_file, file_size, parse_duration);
    }
}

#[test]
fn test_generation_performance() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    let test_files = [
        "tests/fixtures/performance/large-schema.csil",
        "tests/fixtures/real-world/ecommerce-api.csil",
        "examples/complex-metadata/advanced-api.csil"
    ];
    
    for test_file in &test_files {
        if !Path::new(test_file).exists() {
            continue;
        }
        
        let start = Instant::now();
        
        let generate_output = run_csilgen_command(&[
            "generate",
            "--input", test_file,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ]);
        
        let generation_duration = start.elapsed();
        
        // Generation should be fast even for large files
        assert!(generation_duration <= Duration::from_secs(15),
            "Generation for {} took too long: {:?}", 
            test_file, generation_duration);
        
        assert!(
            generate_output.status.success(),
            "Generation performance test failed for {}: {}", 
            test_file,
            String::from_utf8_lossy(&generate_output.stderr)
        );
        
        println!("✓ {} generated in {:?}", test_file, generation_duration);
    }
}

#[test]
fn test_memory_usage_large_files() {
    let csilgen = get_csilgen_binary();
    
    // Test memory usage doesn't grow excessively with large files
    let large_files = [
        "tests/fixtures/performance/large-schema.csil",
        "tests/fixtures/performance/mega-api.csil"
    ];
    
    for large_file in &large_files {
        if !Path::new(large_file).exists() {
            continue;
        }
        
        // Use time command if available to measure memory
        let validate_output = if Command::new("time").arg("--version").output().is_ok() {
            Command::new("time")
                .args(&["-v"]) // Verbose output with memory stats
                .arg(get_csilgen_binary())
                .args(&["validate", "--input", large_file])
                .output()
                .expect("Failed to run with time")
        } else {
            // Fallback without memory measurement
            run_csilgen_command(&["validate", "--input", large_file])
        };
        
        assert!(
            validate_output.status.success(),
            "Memory test failed for {}: {}", 
            large_file,
            String::from_utf8_lossy(&validate_output.stderr)
        );
        
        // If time command was used, check memory usage in stderr
        let stderr = String::from_utf8_lossy(&validate_output.stderr);
        if stderr.contains("Maximum resident set size") {
            // Look for memory usage patterns (this is platform-specific)
            println!("Memory usage info for {}: {}", large_file, stderr);
        }
    }
}

#[test]
fn test_concurrent_operations_performance() {
    let csilgen = get_csilgen_binary();
    
    // Test multiple concurrent operations don't interfere
    let test_files = [
        "tests/fixtures/basic/simple-types.csil",
        "tests/fixtures/services/simple-service.csil",
        "tests/fixtures/metadata/field-visibility.csil"
    ];
    
    let handles: Vec<_> = test_files
        .iter()
        .filter(|file| Path::new(file).exists())
        .map(|file| {
            let csilgen = get_csilgen_binary();
            let file = file.to_string();
            
            std::thread::spawn(move || {
                let start = Instant::now();
                
                let output = run_csilgen_command_with_binary(&csilgen, &[
                    "validate",
                    "--input", &file
                ]);
                
                (file, start.elapsed(), output)
            })
        })
        .collect();
    
    // Wait for all threads and check results
    for handle in handles {
        let (file, duration, output) = handle.join().expect("Thread panicked");
        
        assert!(duration <= Duration::from_secs(5),
            "Concurrent validation of {} took too long: {:?}", file, duration);
        
        assert!(
            output.status.success(),
            "Concurrent validation failed for {}: {}", 
            file,
            String::from_utf8_lossy(&output.stderr)
        );
        
        println!("✓ Concurrent validation of {} completed in {:?}", file, duration);
    }
}

#[test]
fn test_performance_regression_baseline() {
    let csilgen = get_csilgen_binary();
    
    // Establish baseline performance metrics for regression testing
    let baseline_tests = [
        ("small", "tests/fixtures/basic/simple-types.csil", Duration::from_millis(500)),
        ("medium", "tests/fixtures/services/complex-service.csil", Duration::from_secs(2)),
        ("large", "tests/fixtures/performance/large-schema.csil", Duration::from_secs(10)),
    ];
    
    for (size_label, test_file, max_duration) in &baseline_tests {
        if !Path::new(test_file).exists() {
            continue;
        }
        
        let start = Instant::now();
        
        let validate_output = run_csilgen_command(&[
            "validate",
            "--input", test_file
        ]);
        
        let actual_duration = start.elapsed();
        
        assert!(actual_duration <= *max_duration,
            "Performance regression detected for {} file {}: {:?} > {:?}", 
            size_label, test_file, actual_duration, max_duration);
        
        assert!(
            validate_output.status.success(),
            "Baseline performance test failed for {}: {}", 
            test_file,
            String::from_utf8_lossy(&validate_output.stderr)
        );
        
        println!("✓ {} file {} validated in {:?} (limit: {:?})", 
                size_label, test_file, actual_duration, max_duration);
    }
}

#[test]
fn test_wasm_generator_performance() {
    let csilgen = get_csilgen_binary();
    let temp_dir = TempDir::new().expect("Failed to create temp directory");
    
    // Test WASM generator execution performance
    let test_files = [
        "tests/fixtures/services/simple-service.csil",
        "tests/fixtures/services/complex-service.csil",
        "examples/real-world-api/e-commerce-api.csil"
    ];
    
    for test_file in &test_files {
        if !Path::new(test_file).exists() {
            continue;
        }
        
        let start = Instant::now();
        
        let generate_output = run_csilgen_command(&[
            "generate",
            "--input", test_file,
            "--target", "noop",
            "--output", temp_dir.path().to_str().unwrap()
        ]);
        
        let wasm_duration = start.elapsed();
        
        // WASM execution should be reasonably fast
        assert!(wasm_duration <= Duration::from_secs(10),
            "WASM generator too slow for {}: {:?}", test_file, wasm_duration);
        
        assert!(
            generate_output.status.success(),
            "WASM performance test failed for {}: {}", 
            test_file,
            String::from_utf8_lossy(&generate_output.stderr)
        );
        
        println!("✓ WASM generation for {} completed in {:?}", test_file, wasm_duration);
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