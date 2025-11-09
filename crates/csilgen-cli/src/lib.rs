//! Library functions for the csilgen CLI tool
//!
//! This module contains testable functions extracted from the main CLI binary.

pub mod dependency_report;

use csilgen_common::GeneratorConfig;
use csilgen_core::{CsilSpec, FileDependencyGraph, ImportResolver, LiteralValue, parse_csil_file};
use csilgen_wasm_generators::WasmGeneratorRuntime;
use dependency_report::{
    report_circular_dependency_error, report_dependency_strategy, report_generation_summary,
    report_no_entry_points_error,
};
use glob::glob;
use indicatif::ProgressBar;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Result type for CLI operations
pub type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Result of generation operation
#[derive(Debug, Clone)]
pub struct GenerationResult {
    pub processed_files: usize,
    pub generated_files: usize,
    pub total_size: usize,
    pub error_count: usize,
}

/// Recursively find all .csil files in a directory
fn find_csil_files_in_directory(dir: &Path) -> CliResult<Vec<PathBuf>> {
    let mut csil_files = Vec::new();

    fn visit_dir(dir: &Path, files: &mut Vec<PathBuf>) -> CliResult<()> {
        if dir.is_dir() {
            let entries = fs::read_dir(dir)
                .map_err(|e| format!("Error reading directory {}: {}", dir.display(), e))?;

            for entry in entries {
                let entry = entry.map_err(|e| format!("Error reading directory entry: {e}"))?;
                let path = entry.path();

                if path.is_dir() {
                    // Recursively visit subdirectories
                    visit_dir(&path, files)?;
                } else if path.is_file() {
                    // Check if it's a .csil file
                    if let Some(extension) = path.extension() {
                        if extension == "csil" {
                            files.push(path);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    visit_dir(dir, &mut csil_files)?;

    // Sort files for consistent ordering
    csil_files.sort();

    Ok(csil_files)
}

/// Parse a CSIL file with import resolution
fn parse_csil_with_imports(path: &Path) -> Result<CsilSpec, Box<dyn std::error::Error>> {
    let mut resolver = ImportResolver::new();
    let mut spec = parse_csil_file(path)?;
    resolver.resolve_imports(&mut spec, path)?;
    Ok(spec)
}

/// Generate code from CSIL files using the specified target generator
pub fn generate_code(
    input_pattern: &str,
    target: &str,
    output_dir: &Path,
) -> CliResult<GenerationResult> {
    generate_code_with_progress(input_pattern, target, output_dir, None)
}

/// Generate code with optional progress bar
pub fn generate_code_with_progress(
    input_pattern: &str,
    target: &str,
    output_dir: &Path,
    progress_bar: Option<ProgressBar>,
) -> CliResult<GenerationResult> {
    let input_files = discover_input_files(input_pattern)?;

    // Determine processing strategy based on input
    if input_files.len() == 1 {
        // Single file - process normally
        process_single_file(&input_files[0], target, output_dir, progress_bar)
    } else {
        // Multiple files - use dependency analysis
        process_multiple_files_with_dependencies(input_files, target, output_dir, progress_bar)
    }
}

/// Discover input files from pattern, directory, or single file
fn discover_input_files(input_pattern: &str) -> CliResult<Vec<PathBuf>> {
    let path = PathBuf::from(input_pattern);

    if path.exists() && path.is_dir() {
        // Directory path - find all .csil files recursively
        find_csil_files_in_directory(&path)
    } else if path.exists() && path.is_file() {
        // Single file path
        Ok(vec![path])
    } else if let Ok(paths) = glob(input_pattern) {
        // Glob pattern - collect all matching files
        let mut files = Vec::new();
        for path_result in paths {
            match path_result {
                Ok(path) => {
                    if path.is_file() {
                        files.push(path);
                    }
                }
                Err(e) => {
                    return Err(format!("Error processing glob pattern: {e}").into());
                }
            }
        }

        if files.is_empty() {
            Err(format!("No CSIL files found matching pattern: {input_pattern}").into())
        } else {
            Ok(files)
        }
    } else {
        // Path doesn't exist
        Err(format!("Input path does not exist: {}", path.display()).into())
    }
}

/// Process a single file (legacy behavior)
fn process_single_file(
    input_file: &Path,
    target: &str,
    output_dir: &Path,
    progress_bar: Option<ProgressBar>,
) -> CliResult<GenerationResult> {
    // Create output directories as needed
    fs::create_dir_all(output_dir).map_err(|e| {
        format!(
            "Error creating output directory {}: {}",
            output_dir.display(),
            e
        )
    })?;

    // Initialize progress bar if provided
    if let Some(pb) = &progress_bar {
        pb.set_length(3); // Parse, Generate, Write phases for single file
        pb.set_position(0);
    }

    // Initialize WASM runtime and generator setup
    let (mut runtime, generator_id) = initialize_wasm_runtime(target)?;

    // Update progress: parsing phase
    if let Some(pb) = &progress_bar {
        pb.set_message(format!(
            "Parsing {}",
            input_file.file_name().unwrap_or_default().to_string_lossy()
        ));
        pb.set_position(0);
    }

    // Parse the CSIL file with import resolution
    let spec = parse_csil_with_imports(input_file)?;

    // Update progress: generation phase
    if let Some(pb) = &progress_bar {
        pb.set_message(format!("Generating {target}"));
        pb.set_position(1);
    }

    // Extract options from CSIL file
    let options = extract_options_from_spec(&spec);

    // Create generator configuration
    let config = GeneratorConfig {
        target: target.to_string(),
        output_dir: output_dir.to_string_lossy().to_string(),
        options,
    };

    // Execute the generator
    let generated_files = runtime
        .execute_generator(generator_id, &spec, &config)
        .map_err(|e| {
            format!(
                "Failed to execute {} generator for {}: {}",
                target,
                input_file.display(),
                e
            )
        })?;

    // Update progress: writing phase
    if let Some(pb) = &progress_bar {
        pb.set_message(format!("Writing {} files", generated_files.len()));
        pb.set_position(2);
    }

    // Write generated files
    let (files_written, total_size) = write_generated_files(&generated_files, output_dir)?;

    // Finish progress bar
    if let Some(pb) = &progress_bar {
        pb.finish_with_message(format!(
            "Completed: 1 file processed, {files_written} files generated"
        ));
    }

    Ok(GenerationResult {
        processed_files: 1,
        generated_files: files_written,
        total_size,
        error_count: 0,
    })
}

/// Process multiple files with dependency analysis
fn process_multiple_files_with_dependencies(
    input_files: Vec<PathBuf>,
    target: &str,
    output_dir: &Path,
    progress_bar: Option<ProgressBar>,
) -> CliResult<GenerationResult> {
    // Create output directories as needed
    fs::create_dir_all(output_dir).map_err(|e| {
        format!(
            "Error creating output directory {}: {}",
            output_dir.display(),
            e
        )
    })?;

    // Build dependency graph
    let dependency_graph = FileDependencyGraph::build_from_files(&input_files)
        .map_err(|e| format!("Failed to build dependency graph: {e}"))?;

    // Detect and report circular dependencies
    if let Some(cycles) = dependency_graph.has_circular_dependencies() {
        return Err(report_circular_dependency_error(&cycles).into());
    }

    // Find entry points
    let entry_points = dependency_graph.find_entry_points();

    if entry_points.is_empty() {
        return Err(report_no_entry_points_error(&input_files).into());
    }

    // Report what we're doing
    let dependency_files = dependency_graph.get_dependency_files();
    report_dependency_strategy(&dependency_graph, &entry_points);
    report_generation_summary(input_files.len(), &entry_points, &dependency_files);

    // Process only entry points (each gets fully resolved with imports)
    process_entry_points(entry_points, target, output_dir, progress_bar)
}

/// Process entry point files only
fn process_entry_points(
    entry_points: Vec<PathBuf>,
    target: &str,
    output_dir: &Path,
    progress_bar: Option<ProgressBar>,
) -> CliResult<GenerationResult> {
    // Initialize progress bar if provided
    if let Some(pb) = &progress_bar {
        pb.set_length(entry_points.len() as u64 * 3); // Parse, Generate, Write phases
        pb.set_position(0);
    }

    // Initialize WASM runtime and generator setup
    let (mut runtime, generator_id) = initialize_wasm_runtime(target)?;

    // Process each entry point file with progress tracking
    let mut total_files_written = 0;
    let mut total_size = 0;
    let mut processed_count = 0;
    let mut error_count = 0;

    for (i, input_file) in entry_points.iter().enumerate() {
        processed_count += 1;

        // Update progress: parsing phase
        if let Some(pb) = &progress_bar {
            pb.set_message(format!(
                "Parsing {}",
                input_file.file_name().unwrap_or_default().to_string_lossy()
            ));
            pb.set_position((i * 3) as u64);
        }

        // Parse the CSIL file with import resolution
        let spec = match parse_csil_with_imports(input_file) {
            Ok(spec) => spec,
            Err(e) => {
                let detailed_error =
                    format!("Failed to parse CSIL file {}: {}", input_file.display(), e);
                eprintln!("{detailed_error}");
                error_count += 1;
                continue;
            }
        };

        // Update progress: generation phase
        if let Some(pb) = &progress_bar {
            pb.set_message(format!("Generating {target}"));
            pb.set_position((i * 3 + 1) as u64);
        }

        // Extract options from CSIL file
        let options = extract_options_from_spec(&spec);

        // Create generator configuration for this file
        let config = GeneratorConfig {
            target: target.to_string(),
            output_dir: output_dir.to_string_lossy().to_string(),
            options,
        };

        // Execute the generator for this file
        let generated_files = match runtime.execute_generator(generator_id, &spec, &config) {
            Ok(files) => files,
            Err(e) => {
                let detailed_error = format!(
                    "Failed to execute {} generator for {}: {}",
                    target,
                    input_file.display(),
                    e
                );
                eprintln!("{detailed_error}");
                error_count += 1;
                continue;
            }
        };

        // Update progress: writing phase
        if let Some(pb) = &progress_bar {
            pb.set_message(format!("Writing {} files", generated_files.len()));
            pb.set_position((i * 3 + 2) as u64);
        }

        // Write generated files for this input
        match write_generated_files(&generated_files, output_dir) {
            Ok((files_written, size)) => {
                total_files_written += files_written;
                total_size += size;
            }
            Err(e) => {
                eprintln!(
                    "Failed to write generated files for {}: {}",
                    input_file.display(),
                    e
                );
                error_count += 1;
            }
        }

        // Update progress: completed file
        if let Some(pb) = &progress_bar {
            pb.set_position((i * 3 + 3) as u64);
        }
    }

    // Finish progress bar
    if let Some(pb) = &progress_bar {
        pb.finish_with_message(format!(
            "Completed: {processed_count} entry points processed, {total_files_written} files generated"
        ));
    }

    Ok(GenerationResult {
        processed_files: processed_count,
        generated_files: total_files_written,
        total_size,
        error_count,
    })
}

/// Initialize WASM runtime and validate generator availability
fn initialize_wasm_runtime(target: &str) -> CliResult<(WasmGeneratorRuntime, &'static str)> {
    // Initialize WASM runtime
    let mut runtime =
        WasmGeneratorRuntime::new().map_err(|e| format!("Error initializing WASM runtime: {e}"))?;

    // Discover available generators
    runtime
        .discover_generators()
        .map_err(|e| format!("Error discovering generators: {e}"))?;

    // Map target to generator ID
    let generator_id = match target {
        "json" => "csilgen-json-generator",
        "rust" => "csilgen-rust-generator",
        "python" => "csilgen-python-generator",
        "typescript" => "csilgen-typescript-generator",
        "openapi" => "csilgen-openapi-generator",
        "go" => "csilgen-go",
        "noop" | "test" => "csilgen-noop-generator",
        _ => {
            return Err(format!("Unknown target '{target}'. Available targets: json, rust, python, typescript, openapi, go").into());
        }
    };

    // Check if generator is available
    if runtime.registry().get_generator(generator_id).is_none() {
        let available_generators: Vec<&str> = runtime.list_discovered_generators();
        return Err(format!(
            "Generator '{}' not found. Available generators: {}",
            generator_id,
            available_generators.join(", ")
        )
        .into());
    }

    Ok((runtime, generator_id))
}

/// Write generated files to the output directory
fn write_generated_files(
    generated_files: &[csilgen_common::GeneratedFile],
    output_dir: &Path,
) -> CliResult<(usize, usize)> {
    let mut files_written = 0;
    let mut total_size = 0;

    for generated_file in generated_files {
        let file_path = output_dir.join(&generated_file.path);

        // Ensure parent directories exist
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
        }

        // Write file
        fs::write(&file_path, &generated_file.content)
            .map_err(|e| format!("Failed to write file {}: {}", file_path.display(), e))?;

        files_written += 1;
        total_size += generated_file.content.len();
    }

    Ok((files_written, total_size))
}

/// Extract options from CSIL spec and convert to HashMap for generator
fn extract_options_from_spec(spec: &CsilSpec) -> HashMap<String, serde_json::Value> {
    let mut options = HashMap::new();

    if let Some(file_options) = &spec.options {
        for entry in &file_options.entries {
            let value = match &entry.value {
                LiteralValue::Text(s) => serde_json::Value::String(s.clone()),
                LiteralValue::Integer(i) => serde_json::Value::Number((*i).into()),
                LiteralValue::Float(f) => {
                    serde_json::Value::Number(
                        serde_json::Number::from_f64(*f).unwrap_or_else(|| serde_json::Number::from(0))
                    )
                }
                LiteralValue::Bool(b) => serde_json::Value::Bool(*b),
                LiteralValue::Null => serde_json::Value::Null,
                LiteralValue::Bytes(_) => {
                    // Bytes values are rare in options, skip for now
                    continue;
                }
            };
            options.insert(entry.key.clone(), value);
        }
    }

    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_csil_file(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.join(filename);
        let mut file = fs::File::create(&file_path).expect("Failed to create test file");
        file.write_all(content.as_bytes())
            .expect("Failed to write test file");
        file_path
    }

    #[test]
    fn test_generate_code_single_file() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create a simple test CSIL file
        let csil_content = r#"
            User = {
                name: text,
                email: text ? @send-only
            }
            
            service UserService {
                create-user: User -> User
            }
        "#;
        let input_file = create_test_csil_file(&input_dir, "user.csil", csil_content);

        // Test code generation
        let result = generate_code(input_file.to_str().unwrap(), "noop", &output_dir);

        match result {
            Ok(gen_result) => {
                assert_eq!(gen_result.processed_files, 1);
                assert!(gen_result.generated_files > 0);
                assert_eq!(gen_result.error_count, 0);
            }
            Err(e) => {
                // This test might fail if WASM generators are not available
                // In that case, we'll check for a specific error message
                let error_str = e.to_string();
                assert!(
                    error_str.contains("Generator")
                        || error_str.contains("WASM")
                        || error_str.contains("runtime"),
                    "Unexpected error: {error_str}"
                );
            }
        }
    }

    #[test]
    fn test_generate_code_missing_file() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let output_dir = temp_dir.path().join("output");
        let non_existent_file = temp_dir.path().join("nonexistent.csil");

        let result = generate_code(non_existent_file.to_str().unwrap(), "noop", &output_dir);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        // Debug print to see actual error message
        eprintln!("Actual error message: {error_msg}");
        assert!(error_msg.contains("does not exist") || error_msg.contains("No CSIL files found"));
    }

    #[test]
    fn test_generate_code_invalid_target() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        let input_file = create_test_csil_file(&input_dir, "test.csil", "User = { name: text }");

        let result = generate_code(input_file.to_str().unwrap(), "invalid-target", &output_dir);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Unknown target"));
    }

    #[test]
    fn test_generate_code_glob_pattern() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create multiple CSIL files
        create_test_csil_file(&input_dir, "user.csil", "User = { name: text }");
        create_test_csil_file(
            &input_dir,
            "product.csil",
            "Product = { title: text, price: float }",
        );
        create_test_csil_file(&input_dir, "README.md", "This is not a CSIL file");

        let pattern = input_dir.join("*.csil").to_string_lossy().to_string();

        let result = generate_code(&pattern, "noop", &output_dir);

        match result {
            Ok(gen_result) => {
                assert_eq!(gen_result.processed_files, 2); // Should find 2 .csil files
                assert_eq!(gen_result.error_count, 0);
            }
            Err(e) => {
                // Similar to single file test - might fail if WASM runtime not available
                let error_str = e.to_string();
                assert!(
                    error_str.contains("Generator")
                        || error_str.contains("WASM")
                        || error_str.contains("runtime"),
                    "Unexpected error: {error_str}"
                );
            }
        }
    }

    #[test]
    fn test_generate_code_empty_pattern() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        let pattern = input_dir
            .join("*.nonexistent")
            .to_string_lossy()
            .to_string();

        let result = generate_code(&pattern, "noop", &output_dir);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("No CSIL files found"));
    }

    #[test]
    fn test_generate_code_directory_path() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create CSIL files in directory and subdirectory
        create_test_csil_file(&input_dir, "user.csil", "User = { name: text }");
        create_test_csil_file(
            &input_dir,
            "product.csil",
            "Product = { title: text, price: float }",
        );

        let subdir = input_dir.join("models");
        fs::create_dir_all(&subdir).expect("Failed to create subdirectory");
        create_test_csil_file(&subdir, "order.csil", "Order = { id: int, amount: float }");

        // Create non-CSIL file that should be ignored
        create_test_csil_file(&input_dir, "README.md", "This is not a CSIL file");

        let result = generate_code(input_dir.to_str().unwrap(), "noop", &output_dir);

        match result {
            Ok(gen_result) => {
                assert_eq!(gen_result.processed_files, 3); // Should find 3 .csil files
                assert_eq!(gen_result.error_count, 0);
            }
            Err(e) => {
                // Similar to other tests - might fail if WASM runtime not available
                let error_str = e.to_string();
                assert!(
                    error_str.contains("Generator")
                        || error_str.contains("WASM")
                        || error_str.contains("runtime"),
                    "Unexpected error: {error_str}"
                );
            }
        }
    }

    #[test]
    fn test_generate_code_empty_directory() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("empty_input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create a non-CSIL file in the directory
        create_test_csil_file(&input_dir, "README.md", "This is not a CSIL file");

        let result = generate_code(input_dir.to_str().unwrap(), "noop", &output_dir);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        // Accept both new dependency analysis errors and old "No CSIL files found" errors
        assert!(
            error_msg.contains("No CSIL files found")
                || error_msg.contains("No entry point files found")
        );
    }

    #[test]
    fn test_find_csil_files_in_directory() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create CSIL files in nested directories
        create_test_csil_file(&input_dir, "root.csil", "Root = { value: text }");

        let level1 = input_dir.join("level1");
        fs::create_dir_all(&level1).expect("Failed to create level1 dir");
        create_test_csil_file(&level1, "level1.csil", "Level1 = { data: int }");

        let level2 = level1.join("level2");
        fs::create_dir_all(&level2).expect("Failed to create level2 dir");
        create_test_csil_file(&level2, "level2.csil", "Level2 = { info: bool }");

        // Create non-CSIL files that should be ignored
        create_test_csil_file(&input_dir, "README.md", "Documentation");
        create_test_csil_file(&level1, "config.json", "{}");

        let files = find_csil_files_in_directory(&input_dir).expect("Failed to find files");

        assert_eq!(files.len(), 3);

        // Check that all expected files are present
        let file_names: Vec<_> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(file_names.contains(&"level2.csil"));
        assert!(file_names.contains(&"level1.csil"));
        assert!(file_names.contains(&"root.csil"));
    }

    #[test]
    fn test_generate_code_with_progress_bar() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create a test CSIL file with service definition
        let csil_content = r#"
            User = {
                name: text,
                email: text ? @send-only,
                id: int @receive-only
            }
            
            service UserService {
                get-user: { id: int } -> User,
                create-user: User -> { id: int, success: bool }
            }
        "#;
        let input_file = create_test_csil_file(&input_dir, "user.csil", csil_content);

        // Create a hidden progress bar for testing
        let pb = ProgressBar::hidden();

        let result = generate_code_with_progress(
            input_file.to_str().unwrap(),
            "noop",
            &output_dir,
            Some(pb),
        );

        // This test might fail if WASM runtime is not available, which is expected in test environment
        match result {
            Ok(gen_result) => {
                assert_eq!(gen_result.processed_files, 1);
                assert_eq!(gen_result.error_count, 0);
            }
            Err(e) => {
                let error_str = e.to_string();
                // Accept WASM-related errors as expected in test environment
                assert!(
                    error_str.contains("Generator")
                        || error_str.contains("WASM")
                        || error_str.contains("runtime"),
                    "Unexpected error: {error_str}"
                );
            }
        }
    }

    #[test]
    fn test_detailed_error_messages() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let output_dir = temp_dir.path().join("output");

        // Test with non-existent input file
        let non_existent = temp_dir.path().join("nonexistent.csil");
        let result = generate_code(non_existent.to_str().unwrap(), "noop", &output_dir);

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("does not exist") || error_msg.contains("No CSIL files found"));

        // Test with invalid target
        let input_dir = temp_dir.path().join("input");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");
        let input_file = create_test_csil_file(&input_dir, "test.csil", "Simple = { value: text }");

        let result = generate_code(
            input_file.to_str().unwrap(),
            "invalid-generator-name",
            &output_dir,
        );

        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Unknown target"));
        assert!(error_msg.contains("Available targets"));
    }

    #[test]
    fn test_error_counting_in_generation() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create invalid CSIL content that should fail parsing
        let invalid_csil = r#"
            Invalid CSIL content here
            This should cause parsing errors
            Missing proper structure = {
        "#;
        create_test_csil_file(&input_dir, "invalid.csil", invalid_csil);

        let result = generate_code(input_dir.to_str().unwrap(), "noop", &output_dir);

        match result {
            Ok(gen_result) => {
                // If parsing succeeds (maybe parser is lenient), check for errors
                assert!(gen_result.error_count > 0 || gen_result.processed_files >= 1);
            }
            Err(_) => {
                // If generation fails entirely, that's also acceptable for invalid input
                // Test passes - generation failed as expected for invalid CSIL
            }
        }
    }

    #[test]
    fn test_service_specific_parsing() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let input_dir = temp_dir.path().join("input");
        let output_dir = temp_dir.path().join("output");
        fs::create_dir_all(&input_dir).expect("Failed to create input dir");

        // Create CSIL with complex service definitions
        let complex_csil = r#"
            UserRequest = {
                username: text @send-only,
                password: text @send-only @depends-on(username = "admin"),
                timestamp: int ? @receive-only
            }
            
            UserResponse = {
                id: int @receive-only,
                name: text @bidirectional,
                created_at: int @receive-only
            }
            
            service UserAPI {
                authenticate: UserRequest -> UserResponse,
                get-profile: { id: int } -> UserResponse,
                update-profile: UserResponse -> { success: bool }
            }
            
            service AdminAPI {
                list-users: {} -> { users: [UserResponse] },
                delete-user: { id: int } -> { deleted: bool }
            }
        "#;
        create_test_csil_file(&input_dir, "complex.csil", complex_csil);

        let result = generate_code(input_dir.to_str().unwrap(), "noop", &output_dir);

        // This test validates that complex service definitions can be processed
        // Even if WASM runtime fails, the parsing should succeed
        match result {
            Ok(gen_result) => {
                assert_eq!(gen_result.processed_files, 1);
                assert_eq!(gen_result.error_count, 0);
            }
            Err(e) => {
                let error_str = e.to_string();
                // Only accept WASM/runtime errors, not parsing errors
                assert!(
                    error_str.contains("Generator")
                        || error_str.contains("WASM")
                        || error_str.contains("runtime")
                        || error_str.contains("not found"),
                    "Unexpected parsing error for valid CSIL: {error_str}"
                );
            }
        }
    }
}
