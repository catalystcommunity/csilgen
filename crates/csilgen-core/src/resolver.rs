//! Import resolution system for CSIL files

use crate::ast::*;
use crate::parser::parse_csil_file;
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Handles resolution of import statements in CSIL files
pub struct ImportResolver {
    /// Directories to search for imported files
    search_paths: Vec<PathBuf>,
    /// Cache of resolved specifications to avoid re-parsing
    resolved_cache: HashMap<PathBuf, CsilSpec>,
    /// Set of files currently being resolved (for circular dependency detection)
    resolving: HashSet<PathBuf>,
}

impl ImportResolver {
    /// Create a new import resolver with current directory as default search path
    pub fn new() -> Self {
        Self {
            search_paths: vec![PathBuf::from(".")],
            resolved_cache: HashMap::new(),
            resolving: HashSet::new(),
        }
    }

    /// Add a directory to the search paths for import resolution
    pub fn add_search_path(&mut self, path: PathBuf) {
        self.search_paths.push(path);
    }

    /// Resolve all imports in a specification recursively
    pub fn resolve_imports(&mut self, spec: &mut CsilSpec, base_path: &Path) -> Result<()> {
        let base_dir = base_path.parent().unwrap_or(Path::new("."));

        // Clone the imports to avoid borrowing conflicts
        let imports = spec.imports.clone();

        for import in &imports {
            match import {
                ImportStatement::Include { path, alias, .. } => {
                    self.resolve_include(spec, path, alias.as_deref(), base_dir)?;
                }
                ImportStatement::SelectiveImport { path, items, .. } => {
                    self.resolve_selective_import(spec, path, items, base_dir)?;
                }
            }
        }

        // Clear imports after resolution since they've been merged
        spec.imports.clear();
        Ok(())
    }

    /// Resolve an include statement (brings in all rules, optionally with namespace)
    fn resolve_include(
        &mut self,
        spec: &mut CsilSpec,
        path: &str,
        alias: Option<&str>,
        base_dir: &Path,
    ) -> Result<()> {
        let imported_spec = self.load_and_resolve_file(path, base_dir)?;

        // Merge rules with optional namespace prefix
        for mut rule in imported_spec.rules {
            if let Some(alias) = alias {
                rule.name = format!("{}.{}", alias, rule.name);
            }
            spec.rules.push(rule);
        }

        Ok(())
    }

    /// Resolve a selective import statement (brings in only specified rules)
    fn resolve_selective_import(
        &mut self,
        spec: &mut CsilSpec,
        path: &str,
        items: &[String],
        base_dir: &Path,
    ) -> Result<()> {
        let imported_spec = self.load_and_resolve_file(path, base_dir)?;

        // Only import specified items
        for item_name in items {
            if let Some(rule) = imported_spec.rules.iter().find(|r| &r.name == item_name) {
                spec.rules.push(rule.clone());
            } else {
                bail!("Item '{}' not found in '{}'", item_name, path);
            }
        }

        Ok(())
    }

    /// Load a file and resolve its imports recursively
    fn load_and_resolve_file(&mut self, path: &str, base_dir: &Path) -> Result<CsilSpec> {
        let resolved_path = self.resolve_file_path(path, base_dir)?;

        // Check for circular dependencies
        if self.resolving.contains(&resolved_path) {
            bail!("Circular dependency detected: {}", resolved_path.display());
        }

        // Check cache first
        if let Some(cached) = self.resolved_cache.get(&resolved_path) {
            return Ok(cached.clone());
        }

        // Mark as resolving
        self.resolving.insert(resolved_path.clone());

        // Load and resolve
        let mut spec = parse_csil_file(&resolved_path).with_context(|| {
            format!("Failed to parse imported file: {}", resolved_path.display())
        })?;

        self.resolve_imports(&mut spec, &resolved_path)?;

        // Cache and return
        self.resolving.remove(&resolved_path);
        self.resolved_cache.insert(resolved_path, spec.clone());

        Ok(spec)
    }

    /// Resolve a file path using search paths
    fn resolve_file_path(&self, path: &str, base_dir: &Path) -> Result<PathBuf> {
        let path_buf = PathBuf::from(path);

        // Try relative to base directory first
        let relative_path = base_dir.join(&path_buf);
        if relative_path.exists() {
            return Ok(relative_path.canonicalize()?);
        }

        // Try search paths
        for search_path in &self.search_paths {
            let candidate = search_path.join(&path_buf);
            if candidate.exists() {
                return Ok(candidate.canonicalize()?);
            }
        }

        bail!("Could not resolve import path: {}", path);
    }
}

impl Default for ImportResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_simple_include() {
        let temp_dir = TempDir::new().unwrap();

        // Create base.csil
        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        include "types.csil"
        
        service TestService {
            test: Request -> Response
        }
        "#,
        )
        .unwrap();

        // Create types.csil
        fs::write(
            temp_dir.path().join("types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("base.csil"))
            .unwrap();

        // Should have 3 rules: Request, Response, TestService
        assert_eq!(spec.rules.len(), 3);
        assert!(spec.imports.is_empty()); // Imports should be cleared after resolution
    }

    #[test]
    fn test_selective_import() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        from "types.csil" include Request
        
        service TestService {
            test: Request -> { success: bool }
        }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        Internal = { secret: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("base.csil"))
            .unwrap();

        // Should have 2 rules: Request and TestService (not Response or Internal)
        assert_eq!(spec.rules.len(), 2);
        assert!(spec.rules.iter().any(|r| r.name == "Request"));
        assert!(!spec.rules.iter().any(|r| r.name == "Response"));
        assert!(!spec.rules.iter().any(|r| r.name == "Internal"));
    }

    #[test]
    fn test_namespace_alias() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        include "user/types.csil" as user
        
        service TestService {
            test: user.Request -> user.Response
        }
        "#,
        )
        .unwrap();

        fs::create_dir(temp_dir.path().join("user")).unwrap();
        fs::write(
            temp_dir.path().join("user/types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("base.csil"))
            .unwrap();

        // Should have namespaced names
        assert!(spec.rules.iter().any(|r| r.name == "user.Request"));
        assert!(spec.rules.iter().any(|r| r.name == "user.Response"));
    }

    #[test]
    fn test_circular_dependency_detection() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("a.csil"),
            r#"
        include "b.csil"
        TypeA = { field: int }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("b.csil"),
            r#"
        include "a.csil"
        TypeB = { field: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("a.csil")).unwrap();
        let result = resolver.resolve_imports(&mut spec, &temp_dir.path().join("a.csil"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Circular dependency")
        );
    }

    #[test]
    fn test_nested_imports() {
        let temp_dir = TempDir::new().unwrap();

        // Create main.csil -> common.csil -> base.csil
        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "common.csil"
        
        MainType = { data: CommonType }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("common.csil"),
            r#"
        include "base.csil"
        
        CommonType = { base: BaseType, extra: text }
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        BaseType = { id: int, name: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("main.csil"))
            .unwrap();

        // Should have all types from the chain
        assert_eq!(spec.rules.len(), 3);
        assert!(spec.rules.iter().any(|r| r.name == "BaseType"));
        assert!(spec.rules.iter().any(|r| r.name == "CommonType"));
        assert!(spec.rules.iter().any(|r| r.name == "MainType"));
    }

    #[test]
    fn test_missing_import_file() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        include "nonexistent.csil"
        MyType = { field: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        let result = resolver.resolve_imports(&mut spec, &temp_dir.path().join("base.csil"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Could not resolve import path")
        );
    }

    #[test]
    fn test_missing_selective_import_item() {
        let temp_dir = TempDir::new().unwrap();

        fs::write(
            temp_dir.path().join("base.csil"),
            r#"
        from "types.csil" include Request, NonExistent
        "#,
        )
        .unwrap();

        fs::write(
            temp_dir.path().join("types.csil"),
            r#"
        Request = { id: int }
        Response = { result: text }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        let mut spec = parse_csil_file(temp_dir.path().join("base.csil")).unwrap();
        let result = resolver.resolve_imports(&mut spec, &temp_dir.path().join("base.csil"));

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Item 'NonExistent' not found")
        );
    }

    #[test]
    fn test_search_paths() {
        let temp_dir = TempDir::new().unwrap();
        let lib_dir = temp_dir.path().join("lib");
        fs::create_dir(&lib_dir).unwrap();

        fs::write(
            temp_dir.path().join("main.csil"),
            r#"
        include "shared.csil"
        MainType = { field: text }
        "#,
        )
        .unwrap();

        fs::write(
            lib_dir.join("shared.csil"),
            r#"
        SharedType = { id: int }
        "#,
        )
        .unwrap();

        let mut resolver = ImportResolver::new();
        resolver.add_search_path(lib_dir);

        let mut spec = parse_csil_file(temp_dir.path().join("main.csil")).unwrap();
        resolver
            .resolve_imports(&mut spec, &temp_dir.path().join("main.csil"))
            .unwrap();

        // Should find shared.csil in the lib directory
        assert_eq!(spec.rules.len(), 2);
        assert!(spec.rules.iter().any(|r| r.name == "SharedType"));
        assert!(spec.rules.iter().any(|r| r.name == "MainType"));
    }
}
