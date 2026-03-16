//! Dependency graph analysis for CSIL files
//!
//! This module provides functionality to build dependency graphs from CSIL files,
//! identify entry points, and detect circular dependencies.

use crate::scanner::ImportScanner;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Represents a dependency graph of CSIL files
#[derive(Debug, Clone)]
pub struct FileDependencyGraph {
    /// Map from file path to its direct imports
    dependencies: HashMap<PathBuf, Vec<PathBuf>>,
    /// Map from file path to files that import it
    reverse_dependencies: HashMap<PathBuf, Vec<PathBuf>>,
    /// All files discovered in the dependency graph
    all_files: HashSet<PathBuf>,
}

impl FileDependencyGraph {
    /// Create a new empty dependency graph
    pub fn new() -> Self {
        Self {
            dependencies: HashMap::new(),
            reverse_dependencies: HashMap::new(),
            all_files: HashSet::new(),
        }
    }

    /// Build a dependency graph from all CSIL files in a directory
    pub fn build_from_directory(dir: &Path) -> Result<Self> {
        let csil_files = Self::discover_csil_files(dir)?;
        Self::build_from_files(&csil_files)
    }

    /// Build a dependency graph from a list of CSIL files
    pub fn build_from_files(files: &[PathBuf]) -> Result<Self> {
        let mut graph = Self::new();

        // Add all files to the graph
        for file in files {
            graph.all_files.insert(file.clone());
        }

        // Scan imports for each file and build dependency relationships
        for file in files {
            let imports = ImportScanner::scan_imports(file)
                .with_context(|| format!("Failed to scan imports in {}", file.display()))?;

            let base_dir = file.parent().unwrap_or(Path::new("."));
            let resolved_imports =
                ImportScanner::resolve_import_paths(&imports, file, &[base_dir.to_path_buf()])?;

            // Filter to only include imports that are in our file set
            let valid_imports: Vec<PathBuf> = resolved_imports
                .into_iter()
                .filter(|import_path| graph.all_files.contains(import_path))
                .collect();

            // Add dependencies
            graph
                .dependencies
                .insert(file.clone(), valid_imports.clone());

            // Add reverse dependencies
            for import_path in valid_imports {
                graph
                    .reverse_dependencies
                    .entry(import_path)
                    .or_default()
                    .push(file.clone());
            }
        }

        Ok(graph)
    }

    /// Find entry point files (files not imported by any other file in the graph)
    pub fn find_entry_points(&self) -> Vec<PathBuf> {
        self.all_files
            .iter()
            .filter(|file| !self.reverse_dependencies.contains_key(*file))
            .cloned()
            .collect()
    }

    /// Get all direct dependencies of a file
    pub fn get_dependencies(&self, file: &Path) -> Vec<PathBuf> {
        self.dependencies.get(file).cloned().unwrap_or_default()
    }

    /// Get all files that directly import the given file
    pub fn get_reverse_dependencies(&self, file: &Path) -> Vec<PathBuf> {
        self.reverse_dependencies
            .get(file)
            .cloned()
            .unwrap_or_default()
    }

    /// Check if there are circular dependencies and return the cycle if found
    pub fn has_circular_dependencies(&self) -> Option<Vec<PathBuf>> {
        for file in &self.all_files {
            if let Some(cycle) = self.detect_cycle_dfs(file) {
                return Some(cycle);
            }
        }
        None
    }

    /// Check if a file is a dependency (imported by at least one other file)
    pub fn is_dependency_file(&self, file: &Path) -> bool {
        self.reverse_dependencies.contains_key(file)
    }

    /// Get all files that are dependencies (imported by at least one other file)
    pub fn get_dependency_files(&self) -> Vec<PathBuf> {
        self.reverse_dependencies.keys().cloned().collect()
    }

    /// Get all files in the dependency graph
    pub fn get_all_files(&self) -> Vec<PathBuf> {
        self.all_files.iter().cloned().collect()
    }

    /// Detect cycles starting from a specific node using DFS
    fn detect_cycle_dfs(&self, start: &Path) -> Option<Vec<PathBuf>> {
        let mut visited = HashSet::new();
        let mut recursion_stack = HashSet::new();
        let mut path = Vec::new();

        self.dfs_visit(start, &mut visited, &mut recursion_stack, &mut path)
    }

    /// DFS visit for cycle detection
    fn dfs_visit(
        &self,
        node: &Path,
        visited: &mut HashSet<PathBuf>,
        recursion_stack: &mut HashSet<PathBuf>,
        path: &mut Vec<PathBuf>,
    ) -> Option<Vec<PathBuf>> {
        let node_buf = node.to_path_buf();

        if recursion_stack.contains(&node_buf) {
            // Found a cycle - return the cycle path
            let cycle_start = path.iter().position(|p| p == &node_buf)?;
            let mut cycle = path[cycle_start..].to_vec();
            cycle.push(node_buf);
            return Some(cycle);
        }

        if visited.contains(&node_buf) {
            return None;
        }

        visited.insert(node_buf.clone());
        recursion_stack.insert(node_buf.clone());
        path.push(node_buf.clone());

        // Visit all dependencies
        if let Some(deps) = self.dependencies.get(&node_buf) {
            for dep in deps {
                if let Some(cycle) = self.dfs_visit(dep, visited, recursion_stack, path) {
                    return Some(cycle);
                }
            }
        }

        recursion_stack.remove(&node_buf);
        path.pop();
        None
    }

    /// Recursively discover all CSIL files in a directory
    fn discover_csil_files(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut csil_files = Vec::new();

        fn visit_dir(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
            if dir.is_dir() {
                let entries = fs::read_dir(dir)
                    .with_context(|| format!("Error reading directory {}", dir.display()))?;

                for entry in entries {
                    let entry = entry.with_context(|| "Error reading directory entry")?;
                    let path = entry.path();

                    if path.is_dir() {
                        visit_dir(&path, files)?;
                    } else if path.is_file()
                        && let Some(extension) = path.extension()
                        && extension == "csil"
                    {
                        files.push(path.canonicalize().with_context(|| {
                            format!("Failed to canonicalize path {}", path.display())
                        })?);
                    }
                }
            }
            Ok(())
        }

        visit_dir(dir, &mut csil_files)?;
        csil_files.sort();
        Ok(csil_files)
    }
}

impl Default for FileDependencyGraph {
    fn default() -> Self {
        Self::new()
    }
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
    fn test_simple_chain_dependency() {
        let temp_dir = TempDir::new().unwrap();

        // A -> B -> C
        create_test_file(
            temp_dir.path(),
            "a.csil",
            r#"include "b.csil"
TypeA = { field: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "b.csil",
            r#"include "c.csil"
TypeB = { field: text }"#,
        );
        create_test_file(temp_dir.path(), "c.csil", r#"TypeC = { field: bool }"#);

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        // A should be the only entry point
        let entry_points = graph.find_entry_points();
        assert_eq!(entry_points.len(), 1);
        assert!(entry_points[0].file_name().unwrap() == "a.csil");

        // B and C should be dependency files
        let deps = graph.get_dependency_files();
        assert_eq!(deps.len(), 2);

        // No circular dependencies
        assert!(graph.has_circular_dependencies().is_none());
    }

    #[test]
    fn test_diamond_dependency() {
        let temp_dir = TempDir::new().unwrap();

        // A -> {B, C}, B -> D, C -> D
        create_test_file(
            temp_dir.path(),
            "a.csil",
            r#"include "b.csil"
include "c.csil"
TypeA = { field: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "b.csil",
            r#"include "d.csil"
TypeB = { field: text }"#,
        );
        create_test_file(
            temp_dir.path(),
            "c.csil",
            r#"include "d.csil"
TypeC = { field: bool }"#,
        );
        create_test_file(temp_dir.path(), "d.csil", r#"TypeD = { field: float }"#);

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        // A should be the only entry point
        let entry_points = graph.find_entry_points();
        assert_eq!(entry_points.len(), 1);
        assert!(entry_points[0].file_name().unwrap() == "a.csil");

        // B, C, and D should be dependency files
        let deps = graph.get_dependency_files();
        assert_eq!(deps.len(), 3);

        // No circular dependencies
        assert!(graph.has_circular_dependencies().is_none());
    }

    #[test]
    fn test_multiple_entry_points() {
        let temp_dir = TempDir::new().unwrap();

        // main.csil -> {types.csil, errors.csil}, standalone.csil (no imports)
        create_test_file(
            temp_dir.path(),
            "main.csil",
            r#"include "types.csil"
include "errors.csil"
MainType = { field: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "types.csil",
            r#"UserType = { name: text }"#,
        );
        create_test_file(
            temp_dir.path(),
            "errors.csil",
            r#"include "common.csil"
ErrorType = { code: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "common.csil",
            r#"CommonType = { timestamp: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "standalone.csil",
            r#"StandaloneType = { value: text }"#,
        );

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        // main.csil and standalone.csil should be entry points
        let entry_points = graph.find_entry_points();
        assert_eq!(entry_points.len(), 2);
        let entry_names: Vec<_> = entry_points
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(entry_names.contains(&"main.csil"));
        assert!(entry_names.contains(&"standalone.csil"));

        // types.csil, errors.csil, common.csil should be dependencies
        let deps = graph.get_dependency_files();
        assert_eq!(deps.len(), 3);

        // No circular dependencies
        assert!(graph.has_circular_dependencies().is_none());
    }

    #[test]
    fn test_circular_dependency_detection() {
        let temp_dir = TempDir::new().unwrap();

        // A -> B -> C -> A (circular)
        create_test_file(
            temp_dir.path(),
            "a.csil",
            r#"include "b.csil"
TypeA = { field: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "b.csil",
            r#"include "c.csil"
TypeB = { field: text }"#,
        );
        create_test_file(
            temp_dir.path(),
            "c.csil",
            r#"include "a.csil"
TypeC = { field: bool }"#,
        );

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        // Should detect circular dependency
        let cycle = graph.has_circular_dependencies();
        assert!(cycle.is_some());

        let cycle_paths = cycle.unwrap();
        assert!(cycle_paths.len() >= 3); // Should include at least A, B, C
    }

    #[test]
    fn test_orphaned_files_no_dependencies() {
        let temp_dir = TempDir::new().unwrap();

        // Three standalone files with no imports
        create_test_file(temp_dir.path(), "file1.csil", r#"Type1 = { field: int }"#);
        create_test_file(temp_dir.path(), "file2.csil", r#"Type2 = { field: text }"#);
        create_test_file(temp_dir.path(), "file3.csil", r#"Type3 = { field: bool }"#);

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        // All files should be entry points (no imports)
        let entry_points = graph.find_entry_points();
        assert_eq!(entry_points.len(), 3);

        // No dependency files
        let deps = graph.get_dependency_files();
        assert_eq!(deps.len(), 0);

        // No circular dependencies
        assert!(graph.has_circular_dependencies().is_none());
    }

    #[test]
    fn test_complex_real_world_scenario() {
        let temp_dir = TempDir::new().unwrap();

        // Complex scenario with nested directories and multiple entry points
        let api_dir = temp_dir.path().join("api");
        fs::create_dir_all(&api_dir).unwrap();
        let shared_dir = temp_dir.path().join("shared");
        fs::create_dir_all(&shared_dir).unwrap();

        // API entry point
        create_test_file(
            &api_dir,
            "main.csil",
            r#"include "../shared/types.csil"
include "../shared/errors.csil"
service UserAPI {
    create-user: UserRequest -> UserResponse
}"#,
        );

        // Shared types with their own dependencies
        create_test_file(
            &shared_dir,
            "types.csil",
            r#"include "common.csil"
UserRequest = { name: text }
UserResponse = { id: int }"#,
        );

        create_test_file(
            &shared_dir,
            "errors.csil",
            r#"include "common.csil"
ErrorResponse = { code: int, message: text }"#,
        );

        create_test_file(
            &shared_dir,
            "common.csil",
            r#"BaseType = { timestamp: int }"#,
        );

        // Second entry point
        create_test_file(
            temp_dir.path(),
            "admin.csil",
            r#"include "shared/types.csil"
service AdminAPI {
    list-users: {} -> { users: [UserResponse] }
}"#,
        );

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        // Should have 2 entry points: api/main.csil and admin.csil
        let entry_points = graph.find_entry_points();
        assert_eq!(entry_points.len(), 2);

        // Should have 3 dependency files: types.csil, errors.csil, common.csil
        let deps = graph.get_dependency_files();
        assert_eq!(deps.len(), 3);

        // No circular dependencies
        assert!(graph.has_circular_dependencies().is_none());
    }

    #[test]
    fn test_empty_directory() {
        let temp_dir = TempDir::new().unwrap();

        let graph = FileDependencyGraph::build_from_directory(temp_dir.path()).unwrap();

        assert_eq!(graph.find_entry_points().len(), 0);
        assert_eq!(graph.get_dependency_files().len(), 0);
        assert_eq!(graph.get_all_files().len(), 0);
        assert!(graph.has_circular_dependencies().is_none());
    }

    #[test]
    fn test_build_from_files_subset() {
        let temp_dir = TempDir::new().unwrap();

        // Create files with dependencies
        create_test_file(
            temp_dir.path(),
            "a.csil",
            r#"include "b.csil"
TypeA = { field: int }"#,
        );
        create_test_file(
            temp_dir.path(),
            "b.csil",
            r#"include "c.csil"
TypeB = { field: text }"#,
        );
        create_test_file(temp_dir.path(), "c.csil", r#"TypeC = { field: bool }"#);
        create_test_file(temp_dir.path(), "d.csil", r#"TypeD = { field: float }"#); // Not in subset

        // Build graph from only a subset of files
        let subset_files = vec![
            temp_dir.path().join("a.csil").canonicalize().unwrap(),
            temp_dir.path().join("b.csil").canonicalize().unwrap(),
            temp_dir.path().join("c.csil").canonicalize().unwrap(),
        ];

        let graph = FileDependencyGraph::build_from_files(&subset_files).unwrap();

        // Should only include files in the subset
        assert_eq!(graph.get_all_files().len(), 3);

        // A should be the entry point
        let entry_points = graph.find_entry_points();
        assert_eq!(entry_points.len(), 1);
        assert!(entry_points[0].file_name().unwrap() == "a.csil");
    }
}
