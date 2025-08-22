//! Lightweight import scanning for CSIL files
//!
//! This module provides functionality to extract import statements from CSIL files
//! without performing full parsing, enabling fast dependency analysis.

use crate::lexer::Position;
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// Lightweight import scanner for CSIL files
pub struct ImportScanner;

/// Information about an import found in a CSIL file
#[derive(Debug, Clone, PartialEq)]
pub struct ImportInfo {
    pub path: String,
    pub import_type: ImportType,
    pub position: Position,
}

/// Type of import statement
#[derive(Debug, Clone, PartialEq)]
pub enum ImportType {
    /// Simple include: `include "file.csil"`
    Include { alias: Option<String> },
    /// Selective import: `from "file.csil" include Type1, Type2`
    SelectiveImport { items: Vec<String> },
}

impl ImportScanner {
    /// Extract import statements from a file without full parsing
    pub fn scan_imports(file_path: &Path) -> Result<Vec<ImportInfo>> {
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file {}", file_path.display()))?;

        Self::scan_imports_from_content(&content)
    }

    /// Extract import statements from file content
    pub fn scan_imports_from_content(content: &str) -> Result<Vec<ImportInfo>> {
        let mut imports = Vec::new();
        let lines: Vec<&str> = content.lines().collect();

        for (line_num, line) in lines.iter().enumerate() {
            let trimmed = line.trim();

            // Skip comments and empty lines
            if trimmed.is_empty() || trimmed.starts_with(';') {
                continue;
            }

            // Look for include statements
            if let Some(import_info) = Self::try_parse_include_line(trimmed, line_num + 1)? {
                imports.push(import_info);
            }
            // Look for from...include statements
            else if let Some(import_info) = Self::try_parse_from_line(trimmed, line_num + 1)? {
                imports.push(import_info);
            }
        }

        Ok(imports)
    }

    /// Resolve relative import paths to absolute paths
    pub fn resolve_import_paths(
        imports: &[ImportInfo],
        base_path: &Path,
        search_paths: &[PathBuf],
    ) -> Result<Vec<PathBuf>> {
        let mut resolved_paths = Vec::new();

        for import in imports {
            let resolved_path =
                Self::resolve_single_import_path(&import.path, base_path, search_paths)?;
            resolved_paths.push(resolved_path);
        }

        Ok(resolved_paths)
    }

    /// Try to parse an include statement from a line
    fn try_parse_include_line(line: &str, line_num: usize) -> Result<Option<ImportInfo>> {
        let trimmed = line.trim();

        if !trimmed.starts_with("include") {
            return Ok(None);
        }

        // Remove "include" keyword
        let rest = trimmed[7..].trim();

        // Extract the path string
        let (path, rest) = Self::extract_quoted_string(rest).ok_or_else(|| {
            anyhow::anyhow!("Expected quoted string after 'include' on line {line_num}")
        })?;

        // Check for optional "as alias"
        let alias = if rest.trim().starts_with("as") {
            let alias_part = rest.trim()[2..].trim();
            if alias_part.is_empty() {
                bail!("Expected alias identifier after 'as' on line {line_num}");
            }

            // Extract the first identifier
            let alias_name = alias_part.split_whitespace().next().unwrap_or("");
            if alias_name.is_empty() || !Self::is_valid_identifier(alias_name) {
                bail!("Invalid alias identifier '{alias_name}' on line {line_num}");
            }

            Some(alias_name.to_string())
        } else {
            None
        };

        Ok(Some(ImportInfo {
            path,
            import_type: ImportType::Include { alias },
            position: Position {
                line: line_num,
                column: 1,
                offset: 0,
            },
        }))
    }

    /// Try to parse a from...include statement from a line
    fn try_parse_from_line(line: &str, line_num: usize) -> Result<Option<ImportInfo>> {
        let trimmed = line.trim();

        if !trimmed.starts_with("from") {
            return Ok(None);
        }

        // Remove "from" keyword
        let rest = trimmed[4..].trim();

        // Extract the path string
        let (path, rest) = Self::extract_quoted_string(rest).ok_or_else(|| {
            anyhow::anyhow!("Expected quoted string after 'from' on line {line_num}")
        })?;

        // Look for "include" keyword
        let rest = rest.trim();
        if !rest.starts_with("include") {
            bail!("Expected 'include' keyword after file path on line {line_num}");
        }

        // Extract items list
        let items_part = rest[7..].trim();
        let items = Self::parse_import_items(items_part)
            .with_context(|| format!("Failed to parse import items on line {line_num}"))?;

        if items.is_empty() {
            bail!("Empty import list on line {line_num}");
        }

        Ok(Some(ImportInfo {
            path,
            import_type: ImportType::SelectiveImport { items },
            position: Position {
                line: line_num,
                column: 1,
                offset: 0,
            },
        }))
    }

    /// Extract a quoted string and return the string content and remaining text
    fn extract_quoted_string(text: &str) -> Option<(String, &str)> {
        let text = text.trim();

        if !text.starts_with('"') {
            return None;
        }

        // Find the closing quote
        let chars = text.chars().enumerate().skip(1);
        let mut escaped = false;

        for (i, ch) in chars {
            if escaped {
                escaped = false;
                continue;
            }

            match ch {
                '\\' => escaped = true,
                '"' => {
                    let string_content = text[1..i].to_string();
                    let remaining = &text[i + 1..];
                    return Some((string_content, remaining));
                }
                _ => {}
            }
        }

        None
    }

    /// Parse comma-separated import items
    fn parse_import_items(text: &str) -> Result<Vec<String>> {
        let mut items = Vec::new();

        for item in text.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }

            if !Self::is_valid_identifier(item) {
                bail!("Invalid import item identifier: '{item}'");
            }

            items.push(item.to_string());
        }

        Ok(items)
    }

    /// Check if a string is a valid CSIL identifier
    fn is_valid_identifier(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }

        let mut chars = name.chars();

        // First character must be letter or underscore
        if let Some(first) = chars.next() {
            if !first.is_ascii_alphabetic() && first != '_' {
                return false;
            }
        } else {
            return false;
        }

        // Remaining characters must be alphanumeric, underscore, or hyphen
        for ch in chars {
            if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' {
                return false;
            }
        }

        true
    }

    /// Resolve a single import path to an absolute path
    fn resolve_single_import_path(
        import_path: &str,
        base_path: &Path,
        search_paths: &[PathBuf],
    ) -> Result<PathBuf> {
        let path_buf = PathBuf::from(import_path);

        // Try relative to base directory first
        if let Some(base_dir) = base_path.parent() {
            let relative_path = base_dir.join(&path_buf);
            if relative_path.exists() {
                return relative_path.canonicalize().with_context(|| {
                    format!("Failed to canonicalize path {}", relative_path.display())
                });
            }
        }

        // Try search paths
        for search_path in search_paths {
            let candidate = search_path.join(&path_buf);
            if candidate.exists() {
                return candidate.canonicalize().with_context(|| {
                    format!("Failed to canonicalize path {}", candidate.display())
                });
            }
        }

        bail!("Could not resolve import path: {import_path}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_file(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let file_path = dir.join(filename);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).expect("Failed to create parent directory");
        }
        fs::write(&file_path, content).expect("Failed to write test file");
        file_path
    }

    #[test]
    fn test_scan_simple_include() {
        let content = r#"
            include "types.csil"
            include "errors.csil" as errors
            
            User = { name: text }
        "#;

        let imports = ImportScanner::scan_imports_from_content(content).unwrap();

        assert_eq!(imports.len(), 2);

        assert_eq!(imports[0].path, "types.csil");
        assert!(matches!(
            imports[0].import_type,
            ImportType::Include { alias: None }
        ));

        assert_eq!(imports[1].path, "errors.csil");
        assert!(
            matches!(imports[1].import_type, ImportType::Include { alias: Some(ref alias) } if alias == "errors")
        );
    }

    #[test]
    fn test_scan_selective_import() {
        let content = r#"
            from "shared/types.csil" include UserType, ProductType
            from "common.csil" include BaseType
            
            MyType = { field: UserType }
        "#;

        let imports = ImportScanner::scan_imports_from_content(content).unwrap();

        assert_eq!(imports.len(), 2);

        assert_eq!(imports[0].path, "shared/types.csil");
        if let ImportType::SelectiveImport { ref items } = imports[0].import_type {
            assert_eq!(items.len(), 2);
            assert!(items.contains(&"UserType".to_string()));
            assert!(items.contains(&"ProductType".to_string()));
        } else {
            panic!("Expected SelectiveImport");
        }

        assert_eq!(imports[1].path, "common.csil");
        if let ImportType::SelectiveImport { ref items } = imports[1].import_type {
            assert_eq!(items.len(), 1);
            assert!(items.contains(&"BaseType".to_string()));
        } else {
            panic!("Expected SelectiveImport");
        }
    }

    #[test]
    fn test_scan_with_comments() {
        let content = r#"
            ; This is a comment
            include "types.csil"  ; End of line comment
            ; Another comment
            from "errors.csil" include ErrorType
        "#;

        let imports = ImportScanner::scan_imports_from_content(content).unwrap();

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "types.csil");
        assert_eq!(imports[1].path, "errors.csil");
    }

    #[test]
    fn test_scan_no_imports() {
        let content = r#"
            User = { name: text, age: int }
            
            service UserService {
                get-user: { id: int } -> User
            }
        "#;

        let imports = ImportScanner::scan_imports_from_content(content).unwrap();
        assert_eq!(imports.len(), 0);
    }

    #[test]
    fn test_scan_malformed_include() {
        let content = r#"include missing-quotes.csil"#;

        let result = ImportScanner::scan_imports_from_content(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_scan_malformed_from_include() {
        let content = r#"from "file.csil" missing_include_keyword"#;

        let result = ImportScanner::scan_imports_from_content(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_import_paths() {
        let temp_dir = TempDir::new().unwrap();

        // Create test files
        create_test_file(temp_dir.path(), "main.csil", "Main = { field: int }");
        create_test_file(temp_dir.path(), "types.csil", "Types = { field: text }");

        let subdir = temp_dir.path().join("shared");
        fs::create_dir_all(&subdir).unwrap();
        create_test_file(&subdir, "common.csil", "Common = { field: bool }");

        let imports = vec![
            ImportInfo {
                path: "types.csil".to_string(),
                import_type: ImportType::Include { alias: None },
                position: Position {
                    line: 1,
                    column: 1,
                    offset: 0,
                },
            },
            ImportInfo {
                path: "shared/common.csil".to_string(),
                import_type: ImportType::Include { alias: None },
                position: Position {
                    line: 2,
                    column: 1,
                    offset: 0,
                },
            },
        ];

        let main_file = temp_dir.path().join("main.csil");
        let resolved = ImportScanner::resolve_import_paths(
            &imports,
            &main_file,
            &[temp_dir.path().to_path_buf()],
        )
        .unwrap();

        assert_eq!(resolved.len(), 2);
        assert!(resolved[0].file_name().unwrap() == "types.csil");
        assert!(resolved[1].file_name().unwrap() == "common.csil");
    }

    #[test]
    fn test_resolve_nonexistent_import() {
        let temp_dir = TempDir::new().unwrap();

        let imports = vec![ImportInfo {
            path: "nonexistent.csil".to_string(),
            import_type: ImportType::Include { alias: None },
            position: Position {
                line: 1,
                column: 1,
                offset: 0,
            },
        }];

        let main_file = temp_dir.path().join("main.csil");
        let result = ImportScanner::resolve_import_paths(
            &imports,
            &main_file,
            &[temp_dir.path().to_path_buf()],
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Could not resolve import path")
        );
    }

    #[test]
    fn test_extract_quoted_string() {
        assert_eq!(
            ImportScanner::extract_quoted_string(r#""hello.csil" rest"#),
            Some(("hello.csil".to_string(), " rest"))
        );

        assert_eq!(
            ImportScanner::extract_quoted_string(r#""path/to/file.csil""#),
            Some(("path/to/file.csil".to_string(), ""))
        );

        assert_eq!(ImportScanner::extract_quoted_string("no-quotes"), None);

        assert_eq!(
            ImportScanner::extract_quoted_string(r#""unclosed-quote"#),
            None
        );

        // Test escaped quotes
        assert_eq!(
            ImportScanner::extract_quoted_string(r#""escaped\"quote.csil" rest"#),
            Some(("escaped\\\"quote.csil".to_string(), " rest"))
        );
    }

    #[test]
    fn test_parse_import_items() {
        assert_eq!(
            ImportScanner::parse_import_items("Type1, Type2, Type3").unwrap(),
            vec!["Type1", "Type2", "Type3"]
        );

        assert_eq!(
            ImportScanner::parse_import_items("SingleType").unwrap(),
            vec!["SingleType"]
        );

        assert_eq!(
            ImportScanner::parse_import_items("Type1,Type2,Type3").unwrap(),
            vec!["Type1", "Type2", "Type3"]
        );

        // Test with trailing comma
        assert_eq!(
            ImportScanner::parse_import_items("Type1, Type2,").unwrap(),
            vec!["Type1", "Type2"]
        );

        // Test invalid identifier
        let result = ImportScanner::parse_import_items("Invalid-Name-123!, ValidName");
        assert!(result.is_err());
    }

    #[test]
    fn test_is_valid_identifier() {
        assert!(ImportScanner::is_valid_identifier("ValidName"));
        assert!(ImportScanner::is_valid_identifier("valid_name"));
        assert!(ImportScanner::is_valid_identifier("valid-name"));
        assert!(ImportScanner::is_valid_identifier("_underscore"));
        assert!(ImportScanner::is_valid_identifier("Name123"));

        assert!(!ImportScanner::is_valid_identifier("123Invalid"));
        assert!(!ImportScanner::is_valid_identifier("invalid!"));
        assert!(!ImportScanner::is_valid_identifier("invalid.name"));
        assert!(!ImportScanner::is_valid_identifier(""));
        assert!(!ImportScanner::is_valid_identifier("invalid space"));
    }

    #[test]
    fn test_scan_imports_from_file() {
        let temp_dir = TempDir::new().unwrap();

        let content = r#"
            include "types.csil"
            from "errors.csil" include ErrorType, ValidationError
            
            User = { name: text }
        "#;

        let test_file = create_test_file(temp_dir.path(), "test.csil", content);

        let imports = ImportScanner::scan_imports(&test_file).unwrap();

        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].path, "types.csil");
        assert_eq!(imports[1].path, "errors.csil");
    }

    #[test]
    fn test_scan_complex_imports() {
        let content = r#"
            ; Main imports
            include "core/types.csil" as core
            include "shared/common.csil"
            from "utils/helpers.csil" include Helper1, Helper2, Helper3
            from "validation/rules.csil" include ValidationRule
            
            ; Some type definitions
            MyType = { 
                field1: core.BaseType,
                field2: Helper1
            }
        "#;

        let imports = ImportScanner::scan_imports_from_content(content).unwrap();

        assert_eq!(imports.len(), 4);

        // Check first include with alias
        assert_eq!(imports[0].path, "core/types.csil");
        if let ImportType::Include { ref alias } = imports[0].import_type {
            assert_eq!(alias.as_ref().unwrap(), "core");
        } else {
            panic!("Expected Include with alias");
        }

        // Check second include without alias
        assert_eq!(imports[1].path, "shared/common.csil");
        if let ImportType::Include { ref alias } = imports[1].import_type {
            assert!(alias.is_none());
        } else {
            panic!("Expected Include without alias");
        }

        // Check selective imports
        assert_eq!(imports[2].path, "utils/helpers.csil");
        if let ImportType::SelectiveImport { ref items } = imports[2].import_type {
            assert_eq!(items.len(), 3);
            assert!(items.contains(&"Helper1".to_string()));
            assert!(items.contains(&"Helper2".to_string()));
            assert!(items.contains(&"Helper3".to_string()));
        } else {
            panic!("Expected SelectiveImport");
        }
    }
}
