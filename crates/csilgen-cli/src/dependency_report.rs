//! User-friendly dependency reporting for CSIL file analysis
//!
//! This module provides functions to report dependency analysis results to users
//! in a clear and informative way.

use csilgen_core::FileDependencyGraph;
use std::path::{Path, PathBuf};

/// Report the dependency analysis strategy to the user
pub fn report_dependency_strategy(graph: &FileDependencyGraph, entry_points: &[PathBuf]) {
    let dependency_files = graph.get_dependency_files();

    if !dependency_files.is_empty() {
        println!("📊 Dependency analysis completed:");
        println!("   Entry points: {} files", entry_points.len());
        println!("   Dependencies: {} files", dependency_files.len());
        println!("   Generating code from entry points only to avoid duplicates.\n");

        if std::env::var("CSIL_VERBOSE").is_ok() {
            print_detailed_analysis(graph, entry_points);
        }
    }
}

/// Print detailed dependency analysis information
pub fn print_detailed_analysis(graph: &FileDependencyGraph, entry_points: &[PathBuf]) {
    println!("Entry Points:");
    for entry in entry_points {
        println!("  📄 {}", format_file_name(entry));
        print_dependency_tree(graph, entry, 1);
    }

    let dependencies = graph.get_dependency_files();
    if !dependencies.is_empty() {
        println!("\nDependency Files:");
        for dep in dependencies {
            let importers = graph.get_reverse_dependencies(&dep);
            println!(
                "  📦 {} (imported by: {})",
                format_file_name(&dep),
                importers
                    .iter()
                    .map(|p| format_file_name(p))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    println!(); // Empty line for readability
}

/// Print a hierarchical dependency tree for a file
fn print_dependency_tree(graph: &FileDependencyGraph, file: &Path, depth: usize) {
    let deps = graph.get_dependencies(file);
    for (i, dep) in deps.iter().enumerate() {
        let is_last = i == deps.len() - 1;
        let prefix = if is_last { "└─" } else { "├─" };

        println!(
            "{}{}📦 {}",
            "  ".repeat(depth),
            prefix,
            format_file_name(dep)
        );

        // Recursively print dependencies, but avoid infinite loops
        if depth < 10 {
            // Reasonable recursion limit
            print_dependency_tree(graph, dep, depth + 1);
        }
    }
}

/// Format a file name for display (just the filename, not the full path)
fn format_file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}

/// Report circular dependency error with helpful information
pub fn report_circular_dependency_error(cycle: &[PathBuf]) -> String {
    let cycle_display: Vec<String> = cycle.iter().map(|p| format_file_name(p)).collect();

    format!(
        "Circular dependency detected: {}\n\
        \n\
        This creates an infinite loop during import resolution. Please restructure \n\
        your CSIL files to remove the circular reference. Consider:\n\
        \n\
        1. Moving shared types to a separate file\n\
        2. Consolidating related types into a single file\n\
        3. Using forward references instead of direct imports\n\
        \n\
        Cycle path: {}",
        cycle_display.join(" → "),
        cycle_display.join(" → ")
    )
}

/// Report when no entry points are found
pub fn report_no_entry_points_error(all_files: &[PathBuf]) -> String {
    let file_names: Vec<String> = all_files.iter().map(|p| format_file_name(p)).collect();

    format!(
        "No entry point files found. All files are imported by others: {}\n\
        \n\
        This suggests either:\n\
        1. A circular dependency exists among these files\n\
        2. You need to create a main entry point file that imports the others\n\
        \n\
        Consider creating a main.csil file that includes the files you want to generate code from.",
        file_names.join(", ")
    )
}

/// Format cycle paths for error messages
pub fn format_cycle_paths(cycle: &[PathBuf]) -> String {
    cycle
        .iter()
        .map(|p| format_file_name(p))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Report generation strategy summary
pub fn report_generation_summary(
    total_files: usize,
    entry_points: &[PathBuf],
    dependency_files: &[PathBuf],
) {
    if dependency_files.is_empty() {
        println!("🔄 Processing {total_files} independent CSIL files");
    } else {
        println!(
            "🔄 Processing {} entry points from {total_files} total files:",
            entry_points.len()
        );

        for entry in entry_points {
            println!("   📄 {}", format_file_name(entry));
        }

        if !dependency_files.is_empty() {
            println!(
                "   (Skipping {} dependency files to avoid duplicates)",
                dependency_files.len()
            );
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write test file");
        file_path
            .canonicalize()
            .expect("Failed to canonicalize path")
    }

    #[test]
    fn test_format_file_name() {
        let path = PathBuf::from("/some/long/path/to/file.csil");
        assert_eq!(format_file_name(&path), "file.csil");

        let path = PathBuf::from("simple.csil");
        assert_eq!(format_file_name(&path), "simple.csil");
    }

    #[test]
    fn test_format_cycle_paths() {
        let temp_dir = TempDir::new().unwrap();

        let a = create_test_file(temp_dir.path(), "a.csil", "");
        let b = create_test_file(temp_dir.path(), "b.csil", "");
        let c = create_test_file(temp_dir.path(), "c.csil", "");

        let cycle = vec![a, b, c];
        let formatted = format_cycle_paths(&cycle);

        assert_eq!(formatted, "a.csil → b.csil → c.csil");
    }

    #[test]
    fn test_report_circular_dependency_error() {
        let temp_dir = TempDir::new().unwrap();

        let a = create_test_file(temp_dir.path(), "a.csil", "");
        let b = create_test_file(temp_dir.path(), "b.csil", "");

        let cycle = vec![a, b];
        let error_msg = report_circular_dependency_error(&cycle);

        assert!(error_msg.contains("Circular dependency detected"));
        assert!(error_msg.contains("a.csil → b.csil"));
        assert!(error_msg.contains("Moving shared types"));
    }

    #[test]
    fn test_report_no_entry_points_error() {
        let temp_dir = TempDir::new().unwrap();

        let a = create_test_file(temp_dir.path(), "a.csil", "");
        let b = create_test_file(temp_dir.path(), "b.csil", "");

        let files = vec![a, b];
        let error_msg = report_no_entry_points_error(&files);

        assert!(error_msg.contains("No entry point files found"));
        assert!(error_msg.contains("a.csil, b.csil"));
        assert!(error_msg.contains("Consider creating a main.csil"));
    }

    #[test]
    fn test_dependency_strategy_output() {
        // This is more of a visual test to ensure the reporting functions work
        // We can't easily test stdout output, but we can ensure they don't panic
        let temp_dir = TempDir::new().unwrap();

        // Create files without imports to avoid path resolution issues in tests
        create_test_file(
            temp_dir.path(),
            "standalone1.csil",
            r#"User = { name: text }"#,
        );
        create_test_file(
            temp_dir.path(),
            "standalone2.csil",
            r#"Product = { title: text }"#,
        );

        let files = vec![
            temp_dir
                .path()
                .join("standalone1.csil")
                .canonicalize()
                .unwrap(),
            temp_dir
                .path()
                .join("standalone2.csil")
                .canonicalize()
                .unwrap(),
        ];

        let graph = csilgen_core::FileDependencyGraph::build_from_files(&files).unwrap();
        let entry_points = graph.find_entry_points();

        // These should not panic - with standalone files, both should be entry points
        report_dependency_strategy(&graph, &entry_points);
        report_generation_summary(files.len(), &entry_points, &graph.get_dependency_files());

        assert_eq!(entry_points.len(), 2); // Both files should be entry points
        assert_eq!(graph.get_dependency_files().len(), 0); // No dependencies
    }
}
