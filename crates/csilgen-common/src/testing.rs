//! Testing utilities for csilgen

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Test case metadata for CSIL files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseMetadata {
    /// Human-readable description of what this test case covers
    pub description: String,
    /// Whether parsing should succeed (true) or fail (false)
    pub should_parse: bool,
    /// Expected validation errors (if any)
    pub expected_validation_errors: Vec<String>,
    /// Expected AST structure (for successful parses)
    pub expected_ast: Option<serde_json::Value>,
    /// Tags for categorizing tests
    pub tags: Vec<String>,
}

/// A complete test case including CSIL content and metadata
#[derive(Debug, Clone)]
pub struct TestCase {
    /// The name/identifier of this test case
    pub name: String,
    /// Path to the CSIL file
    pub csil_path: PathBuf,
    /// The CSIL file content
    pub csil_content: String,
    /// Test metadata
    pub metadata: TestCaseMetadata,
}

/// Test fixture loader for CSIL files
pub struct TestFixtureLoader {
    fixtures_dir: PathBuf,
}

impl TestFixtureLoader {
    /// Create a new test fixture loader
    pub fn new<P: AsRef<Path>>(fixtures_dir: P) -> Self {
        Self {
            fixtures_dir: fixtures_dir.as_ref().to_path_buf(),
        }
    }

    /// Load all test cases from the fixtures directory
    pub fn load_all_test_cases(&self) -> Result<Vec<TestCase>> {
        let mut test_cases = Vec::new();

        if !self.fixtures_dir.exists() {
            return Ok(test_cases);
        }

        self.load_test_cases_recursive(&self.fixtures_dir, &mut test_cases)?;
        Ok(test_cases)
    }

    /// Load test cases from a specific subdirectory
    pub fn load_test_cases_by_category(&self, category: &str) -> Result<Vec<TestCase>> {
        let category_dir = self.fixtures_dir.join(category);
        let mut test_cases = Vec::new();

        if category_dir.exists() {
            self.load_test_cases_recursive(&category_dir, &mut test_cases)?;
        }

        Ok(test_cases)
    }

    /// Load a specific test case by name
    pub fn load_test_case(&self, name: &str) -> Result<Option<TestCase>> {
        let csil_path = self.fixtures_dir.join(format!("{name}.csil"));
        let meta_path = self.fixtures_dir.join(format!("{name}.meta.json"));

        if !csil_path.exists() {
            return Ok(None);
        }

        let csil_content = fs::read_to_string(&csil_path)?;
        let metadata = if meta_path.exists() {
            let meta_content = fs::read_to_string(&meta_path)?;
            serde_json::from_str(&meta_content)?
        } else {
            TestCaseMetadata::default_for_valid_csil()
        };

        Ok(Some(TestCase {
            name: name.to_string(),
            csil_path,
            csil_content,
            metadata,
        }))
    }

    fn load_test_cases_recursive(&self, dir: &Path, test_cases: &mut Vec<TestCase>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                self.load_test_cases_recursive(&path, test_cases)?;
            } else if path.extension().is_some_and(|ext| ext == "csil")
                && let Some(test_case) = self.load_test_case_from_path(&path)?
            {
                test_cases.push(test_case);
            }
        }
        Ok(())
    }

    fn load_test_case_from_path(&self, csil_path: &Path) -> Result<Option<TestCase>> {
        let name = csil_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow::anyhow!("Invalid file name: {:?}", csil_path))?;

        let meta_path = csil_path.with_extension("meta.json");
        let csil_content = fs::read_to_string(csil_path)?;

        let metadata = if meta_path.exists() {
            let meta_content = fs::read_to_string(&meta_path)?;
            serde_json::from_str(&meta_content)?
        } else {
            TestCaseMetadata::default_for_valid_csil()
        };

        Ok(Some(TestCase {
            name: name.to_string(),
            csil_path: csil_path.to_path_buf(),
            csil_content,
            metadata,
        }))
    }
}

impl TestCaseMetadata {
    /// Create default metadata for a valid CSIL file that should parse and validate
    pub fn default_for_valid_csil() -> Self {
        Self {
            description: "Valid CSIL that should parse and validate successfully".to_string(),
            should_parse: true,
            expected_validation_errors: Vec::new(),
            expected_ast: None,
            tags: vec!["valid".to_string()],
        }
    }

    /// Create metadata for an invalid CSIL file that should fail parsing
    pub fn for_parse_error(description: String) -> Self {
        Self {
            description,
            should_parse: false,
            expected_validation_errors: Vec::new(),
            expected_ast: None,
            tags: vec!["invalid".to_string(), "parse-error".to_string()],
        }
    }

    /// Create metadata for a CSIL file that parses but has validation errors
    pub fn for_validation_error(description: String, errors: Vec<String>) -> Self {
        Self {
            description,
            should_parse: true,
            expected_validation_errors: errors,
            expected_ast: None,
            tags: vec!["invalid".to_string(), "validation-error".to_string()],
        }
    }
}

/// Utilities for creating temporary test fixtures
pub struct TempTestFixture {
    pub temp_dir: tempfile::TempDir,
}

impl TempTestFixture {
    /// Create a new temporary test fixture directory
    pub fn new() -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        Ok(Self { temp_dir })
    }

    /// Create a CSIL file in the temp directory
    pub fn create_csil_file(&self, name: &str, content: &str) -> Result<PathBuf> {
        let path = self.temp_dir.path().join(format!("{name}.csil"));
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Create a test metadata file
    pub fn create_metadata_file(&self, name: &str, metadata: &TestCaseMetadata) -> Result<PathBuf> {
        let path = self.temp_dir.path().join(format!("{name}.meta.json"));
        let content = serde_json::to_string_pretty(metadata)?;
        fs::write(&path, content)?;
        Ok(path)
    }

    /// Get the path to the temp directory
    pub fn path(&self) -> &Path {
        self.temp_dir.path()
    }
}

/// AST Testing Utilities Module
///
/// Provides builder patterns, comparison utilities, and validation helpers
/// for working with CSIL AST structures in tests.
pub mod ast {

    // Forward declare AST types from csilgen-core - these will be available when this module is used
    // We use a generic approach here so this module can work with different AST versions

    /// Builder for creating test CsilSpec structures
    pub struct CsilSpecBuilder {
        rules: Vec<TestRule>,
    }

    /// Builder for creating test Rule structures  
    pub struct RuleBuilder {
        name: String,
        rule_type: Option<TestRuleType>,
    }

    /// Builder for creating test TypeExpression structures
    pub struct TypeExpressionBuilder {
        expr_type: Option<TestTypeExpression>,
    }

    /// Builder for creating test GroupExpression structures
    pub struct GroupExpressionBuilder {
        entries: Vec<TestGroupEntry>,
    }

    /// Builder for creating test GroupEntry structures
    pub struct GroupEntryBuilder {
        key: Option<String>,
        value_type: Option<TestTypeExpression>,
        optional: bool,
    }

    // Test-friendly representations of AST nodes
    #[derive(Debug, Clone, PartialEq)]
    pub struct TestRule {
        pub name: String,
        pub rule_type: TestRuleType,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum TestRuleType {
        TypeDef(TestTypeExpression),
        GroupDef(TestGroupExpression),
    }

    #[derive(Debug, Clone, PartialEq)]
    pub enum TestTypeExpression {
        Builtin(String),
        Reference(String),
        Array(Box<TestTypeExpression>),
        Map {
            key: Box<TestTypeExpression>,
            value: Box<TestTypeExpression>,
        },
        Group(TestGroupExpression),
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct TestGroupExpression {
        pub entries: Vec<TestGroupEntry>,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct TestGroupEntry {
        pub key: Option<String>,
        pub value_type: TestTypeExpression,
        pub optional: bool,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct TestCsilSpec {
        pub rules: Vec<TestRule>,
    }

    // Builder implementations
    impl Default for CsilSpecBuilder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CsilSpecBuilder {
        pub fn new() -> Self {
            Self { rules: Vec::new() }
        }

        pub fn rule(mut self, rule: TestRule) -> Self {
            self.rules.push(rule);
            self
        }

        pub fn with_rule<F>(mut self, f: F) -> Self
        where
            F: FnOnce(RuleBuilder) -> TestRule,
        {
            let rule = f(RuleBuilder::new());
            self.rules.push(rule);
            self
        }

        pub fn build(self) -> TestCsilSpec {
            TestCsilSpec { rules: self.rules }
        }
    }

    impl Default for RuleBuilder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl RuleBuilder {
        pub fn new() -> Self {
            Self {
                name: String::new(),
                rule_type: None,
            }
        }

        pub fn name(mut self, name: impl Into<String>) -> Self {
            self.name = name.into();
            self
        }

        pub fn type_def(mut self, type_expr: TestTypeExpression) -> Self {
            self.rule_type = Some(TestRuleType::TypeDef(type_expr));
            self
        }

        pub fn group_def(mut self, group_expr: TestGroupExpression) -> Self {
            self.rule_type = Some(TestRuleType::GroupDef(group_expr));
            self
        }

        pub fn with_type_def<F>(mut self, f: F) -> Self
        where
            F: FnOnce(TypeExpressionBuilder) -> TestTypeExpression,
        {
            let type_expr = f(TypeExpressionBuilder::new());
            self.rule_type = Some(TestRuleType::TypeDef(type_expr));
            self
        }

        pub fn with_group_def<F>(mut self, f: F) -> Self
        where
            F: FnOnce(GroupExpressionBuilder) -> TestGroupExpression,
        {
            let group_expr = f(GroupExpressionBuilder::new());
            self.rule_type = Some(TestRuleType::GroupDef(group_expr));
            self
        }

        pub fn build(self) -> TestRule {
            TestRule {
                name: self.name,
                rule_type: self.rule_type.expect("Rule type must be set"),
            }
        }
    }

    impl Default for TypeExpressionBuilder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl TypeExpressionBuilder {
        pub fn new() -> Self {
            Self { expr_type: None }
        }

        pub fn builtin(mut self, name: impl Into<String>) -> Self {
            self.expr_type = Some(TestTypeExpression::Builtin(name.into()));
            self
        }

        pub fn reference(mut self, name: impl Into<String>) -> Self {
            self.expr_type = Some(TestTypeExpression::Reference(name.into()));
            self
        }

        pub fn array(mut self, element_type: TestTypeExpression) -> Self {
            self.expr_type = Some(TestTypeExpression::Array(Box::new(element_type)));
            self
        }

        pub fn with_array<F>(mut self, f: F) -> Self
        where
            F: FnOnce(TypeExpressionBuilder) -> TestTypeExpression,
        {
            let element_type = f(TypeExpressionBuilder::new());
            self.expr_type = Some(TestTypeExpression::Array(Box::new(element_type)));
            self
        }

        pub fn map(mut self, key: TestTypeExpression, value: TestTypeExpression) -> Self {
            self.expr_type = Some(TestTypeExpression::Map {
                key: Box::new(key),
                value: Box::new(value),
            });
            self
        }

        pub fn group(mut self, group: TestGroupExpression) -> Self {
            self.expr_type = Some(TestTypeExpression::Group(group));
            self
        }

        pub fn with_group<F>(mut self, f: F) -> Self
        where
            F: FnOnce(GroupExpressionBuilder) -> TestGroupExpression,
        {
            let group = f(GroupExpressionBuilder::new());
            self.expr_type = Some(TestTypeExpression::Group(group));
            self
        }

        pub fn build(self) -> TestTypeExpression {
            self.expr_type.expect("Type expression must be set")
        }
    }

    impl Default for GroupExpressionBuilder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl GroupExpressionBuilder {
        pub fn new() -> Self {
            Self {
                entries: Vec::new(),
            }
        }

        pub fn entry(mut self, entry: TestGroupEntry) -> Self {
            self.entries.push(entry);
            self
        }

        pub fn with_entry<F>(mut self, f: F) -> Self
        where
            F: FnOnce(GroupEntryBuilder) -> TestGroupEntry,
        {
            let entry = f(GroupEntryBuilder::new());
            self.entries.push(entry);
            self
        }

        pub fn field(mut self, key: impl Into<String>, value_type: TestTypeExpression) -> Self {
            self.entries.push(TestGroupEntry {
                key: Some(key.into()),
                value_type,
                optional: false,
            });
            self
        }

        pub fn optional_field(
            mut self,
            key: impl Into<String>,
            value_type: TestTypeExpression,
        ) -> Self {
            self.entries.push(TestGroupEntry {
                key: Some(key.into()),
                value_type,
                optional: true,
            });
            self
        }

        pub fn build(self) -> TestGroupExpression {
            TestGroupExpression {
                entries: self.entries,
            }
        }
    }

    impl Default for GroupEntryBuilder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl GroupEntryBuilder {
        pub fn new() -> Self {
            Self {
                key: None,
                value_type: None,
                optional: false,
            }
        }

        pub fn key(mut self, key: impl Into<String>) -> Self {
            self.key = Some(key.into());
            self
        }

        pub fn value_type(mut self, value_type: TestTypeExpression) -> Self {
            self.value_type = Some(value_type);
            self
        }

        pub fn with_value_type<F>(mut self, f: F) -> Self
        where
            F: FnOnce(TypeExpressionBuilder) -> TestTypeExpression,
        {
            let value_type = f(TypeExpressionBuilder::new());
            self.value_type = Some(value_type);
            self
        }

        pub fn optional(mut self) -> Self {
            self.optional = true;
            self
        }

        pub fn required(mut self) -> Self {
            self.optional = false;
            self
        }

        pub fn build(self) -> TestGroupEntry {
            TestGroupEntry {
                key: self.key,
                value_type: self.value_type.expect("Value type must be set"),
                optional: self.optional,
            }
        }
    }

    // Convenience functions for common patterns
    pub fn builtin_type(name: impl Into<String>) -> TestTypeExpression {
        TestTypeExpression::Builtin(name.into())
    }

    pub fn reference_type(name: impl Into<String>) -> TestTypeExpression {
        TestTypeExpression::Reference(name.into())
    }

    pub fn array_of(element_type: TestTypeExpression) -> TestTypeExpression {
        TestTypeExpression::Array(Box::new(element_type))
    }

    pub fn map_of(key: TestTypeExpression, value: TestTypeExpression) -> TestTypeExpression {
        TestTypeExpression::Map {
            key: Box::new(key),
            value: Box::new(value),
        }
    }

    /// Create a simple group with key-value pairs
    pub fn simple_group(fields: Vec<(&str, TestTypeExpression)>) -> TestGroupExpression {
        let entries = fields
            .into_iter()
            .map(|(key, value_type)| TestGroupEntry {
                key: Some(key.to_string()),
                value_type,
                optional: false,
            })
            .collect();
        TestGroupExpression { entries }
    }

    /// AST Comparison and Assertion Utilities
    pub mod assertion {
        use super::*;

        /// Compare two AST nodes with detailed error reporting
        pub fn assert_ast_eq<T: std::fmt::Debug + PartialEq>(
            expected: &T,
            actual: &T,
            context: &str,
        ) {
            if expected != actual {
                panic!(
                    "AST assertion failed in {context}:\n  Expected: {expected:#?}\n  Actual: {actual:#?}"
                );
            }
        }

        /// Deep comparison with path tracking for better error messages
        pub fn deep_compare_specs(
            expected: &TestCsilSpec,
            actual: &TestCsilSpec,
        ) -> Result<(), String> {
            if expected.rules.len() != actual.rules.len() {
                return Err(format!(
                    "Rule count mismatch: expected {}, got {}",
                    expected.rules.len(),
                    actual.rules.len()
                ));
            }

            for (i, (expected_rule, actual_rule)) in
                expected.rules.iter().zip(&actual.rules).enumerate()
            {
                if let Err(err) = deep_compare_rules(expected_rule, actual_rule) {
                    return Err(format!("Rule {i} ('{}'): {err}", expected_rule.name));
                }
            }

            Ok(())
        }

        fn deep_compare_rules(expected: &TestRule, actual: &TestRule) -> Result<(), String> {
            if expected.name != actual.name {
                return Err(format!(
                    "Name mismatch: expected '{}', got '{}'",
                    expected.name, actual.name
                ));
            }

            deep_compare_rule_types(&expected.rule_type, &actual.rule_type)
        }

        fn deep_compare_rule_types(
            expected: &TestRuleType,
            actual: &TestRuleType,
        ) -> Result<(), String> {
            match (expected, actual) {
                (TestRuleType::TypeDef(expected_expr), TestRuleType::TypeDef(actual_expr)) => {
                    deep_compare_type_expressions(expected_expr, actual_expr)
                }
                (TestRuleType::GroupDef(expected_group), TestRuleType::GroupDef(actual_group)) => {
                    deep_compare_group_expressions(expected_group, actual_group)
                }
                _ => Err(format!(
                    "Rule type mismatch: expected {expected:?}, got {actual:?}"
                )),
            }
        }

        fn deep_compare_type_expressions(
            expected: &TestTypeExpression,
            actual: &TestTypeExpression,
        ) -> Result<(), String> {
            match (expected, actual) {
                (
                    TestTypeExpression::Builtin(expected_name),
                    TestTypeExpression::Builtin(actual_name),
                ) => {
                    if expected_name != actual_name {
                        Err(format!(
                            "Builtin type mismatch: expected '{expected_name}', got '{actual_name}'"
                        ))
                    } else {
                        Ok(())
                    }
                }
                (
                    TestTypeExpression::Reference(expected_name),
                    TestTypeExpression::Reference(actual_name),
                ) => {
                    if expected_name != actual_name {
                        Err(format!(
                            "Reference mismatch: expected '{expected_name}', got '{actual_name}'"
                        ))
                    } else {
                        Ok(())
                    }
                }
                (
                    TestTypeExpression::Array(expected_elem),
                    TestTypeExpression::Array(actual_elem),
                ) => deep_compare_type_expressions(expected_elem, actual_elem)
                    .map_err(|err| format!("Array element: {err}")),
                (
                    TestTypeExpression::Map {
                        key: expected_key,
                        value: expected_value,
                    },
                    TestTypeExpression::Map {
                        key: actual_key,
                        value: actual_value,
                    },
                ) => {
                    deep_compare_type_expressions(expected_key, actual_key)
                        .map_err(|err| format!("Map key: {err}"))?;
                    deep_compare_type_expressions(expected_value, actual_value)
                        .map_err(|err| format!("Map value: {err}"))
                }
                (
                    TestTypeExpression::Group(expected_group),
                    TestTypeExpression::Group(actual_group),
                ) => deep_compare_group_expressions(expected_group, actual_group),
                _ => Err(format!(
                    "Type expression mismatch: expected {expected:?}, got {actual:?}"
                )),
            }
        }

        fn deep_compare_group_expressions(
            expected: &TestGroupExpression,
            actual: &TestGroupExpression,
        ) -> Result<(), String> {
            if expected.entries.len() != actual.entries.len() {
                return Err(format!(
                    "Group entry count mismatch: expected {}, got {}",
                    expected.entries.len(),
                    actual.entries.len()
                ));
            }

            for (i, (expected_entry, actual_entry)) in
                expected.entries.iter().zip(&actual.entries).enumerate()
            {
                if let Err(err) = deep_compare_group_entries(expected_entry, actual_entry) {
                    return Err(format!("Entry {i}: {err}"));
                }
            }

            Ok(())
        }

        fn deep_compare_group_entries(
            expected: &TestGroupEntry,
            actual: &TestGroupEntry,
        ) -> Result<(), String> {
            if expected.key != actual.key {
                return Err(format!(
                    "Key mismatch: expected {:?}, got {:?}",
                    expected.key, actual.key
                ));
            }

            if expected.optional != actual.optional {
                return Err(format!(
                    "Optional flag mismatch: expected {}, got {}",
                    expected.optional, actual.optional
                ));
            }

            deep_compare_type_expressions(&expected.value_type, &actual.value_type)
                .map_err(|err| format!("Value type: {err}"))
        }

        /// Macro for asserting AST equality with better error messages
        #[macro_export]
        macro_rules! assert_ast_deep_eq {
            ($expected:expr, $actual:expr) => {
                if let Err(err) =
                    $crate::testing::ast::assertion::deep_compare_specs(&$expected, &$actual)
                {
                    panic!("AST deep comparison failed: {}", err);
                }
            };
            ($expected:expr, $actual:expr, $msg:literal) => {
                if let Err(err) =
                    $crate::testing::ast::assertion::deep_compare_specs(&$expected, &$actual)
                {
                    panic!("AST deep comparison failed ({}): {}", $msg, err);
                }
            };
        }
    }

    /// AST Visitor Pattern for Test Traversal
    pub mod visitor {
        use super::*;

        pub trait AstVisitor {
            fn visit_spec(&mut self, spec: &TestCsilSpec) {
                for rule in &spec.rules {
                    self.visit_rule(rule);
                }
            }

            fn visit_rule(&mut self, rule: &TestRule) {
                match &rule.rule_type {
                    TestRuleType::TypeDef(expr) => self.visit_type_expression(expr),
                    TestRuleType::GroupDef(group) => self.visit_group_expression(group),
                }
            }

            fn visit_type_expression(&mut self, expr: &TestTypeExpression) {
                match expr {
                    TestTypeExpression::Builtin(_) => {}
                    TestTypeExpression::Reference(_) => {}
                    TestTypeExpression::Array(elem) => self.visit_type_expression(elem),
                    TestTypeExpression::Map { key, value } => {
                        self.visit_type_expression(key);
                        self.visit_type_expression(value);
                    }
                    TestTypeExpression::Group(group) => self.visit_group_expression(group),
                }
            }

            fn visit_group_expression(&mut self, group: &TestGroupExpression) {
                for entry in &group.entries {
                    self.visit_group_entry(entry);
                }
            }

            fn visit_group_entry(&mut self, entry: &TestGroupEntry) {
                self.visit_type_expression(&entry.value_type);
            }
        }

        /// Visitor that collects all type references
        pub struct TypeReferenceCollector {
            pub references: Vec<String>,
        }

        impl Default for TypeReferenceCollector {
            fn default() -> Self {
                Self::new()
            }
        }

        impl TypeReferenceCollector {
            pub fn new() -> Self {
                Self {
                    references: Vec::new(),
                }
            }

            pub fn collect_from_spec(spec: &TestCsilSpec) -> Vec<String> {
                let mut collector = Self::new();
                collector.visit_spec(spec);
                collector.references
            }
        }

        impl AstVisitor for TypeReferenceCollector {
            fn visit_type_expression(&mut self, expr: &TestTypeExpression) {
                if let TestTypeExpression::Reference(name) = expr {
                    self.references.push(name.clone());
                }
                // Continue traversal
                match expr {
                    TestTypeExpression::Array(elem) => self.visit_type_expression(elem),
                    TestTypeExpression::Map { key, value } => {
                        self.visit_type_expression(key);
                        self.visit_type_expression(value);
                    }
                    TestTypeExpression::Group(group) => self.visit_group_expression(group),
                    _ => {}
                }
            }
        }

        /// Visitor that validates AST invariants
        pub struct InvariantValidator {
            pub errors: Vec<String>,
        }

        impl Default for InvariantValidator {
            fn default() -> Self {
                Self::new()
            }
        }

        impl InvariantValidator {
            pub fn new() -> Self {
                Self { errors: Vec::new() }
            }

            pub fn validate_spec(spec: &TestCsilSpec) -> Vec<String> {
                let mut validator = Self::new();
                validator.visit_spec(spec);
                validator.errors
            }
        }

        impl AstVisitor for InvariantValidator {
            fn visit_rule(&mut self, rule: &TestRule) {
                if rule.name.is_empty() {
                    self.errors.push("Rule name cannot be empty".to_string());
                }

                // Continue with normal traversal
                match &rule.rule_type {
                    TestRuleType::TypeDef(expr) => self.visit_type_expression(expr),
                    TestRuleType::GroupDef(group) => self.visit_group_expression(group),
                }
            }

            fn visit_group_entry(&mut self, entry: &TestGroupEntry) {
                if let Some(key) = &entry.key
                    && key.is_empty()
                {
                    self.errors
                        .push("Group entry key cannot be empty".to_string());
                }

                self.visit_type_expression(&entry.value_type);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_fixture_creation() {
        let fixture = TempTestFixture::new().unwrap();

        let csil_content = r#"
person = {
    name: text,
    age: int,
}
"#;

        let metadata = TestCaseMetadata::default_for_valid_csil();

        fixture.create_csil_file("person", csil_content).unwrap();
        fixture.create_metadata_file("person", &metadata).unwrap();

        let loader = TestFixtureLoader::new(fixture.path());
        let test_case = loader.load_test_case("person").unwrap().unwrap();

        assert_eq!(test_case.name, "person");
        assert_eq!(test_case.csil_content.trim(), csil_content.trim());
        assert!(test_case.metadata.should_parse);
    }

    #[test]
    fn test_metadata_creation() {
        let valid_meta = TestCaseMetadata::default_for_valid_csil();
        assert!(valid_meta.should_parse);
        assert!(valid_meta.expected_validation_errors.is_empty());

        let parse_error_meta = TestCaseMetadata::for_parse_error("Test parse error".to_string());
        assert!(!parse_error_meta.should_parse);
        assert!(parse_error_meta.tags.contains(&"parse-error".to_string()));

        let validation_error_meta = TestCaseMetadata::for_validation_error(
            "Test validation error".to_string(),
            vec!["Missing field".to_string()],
        );
        assert!(validation_error_meta.should_parse);
        assert_eq!(validation_error_meta.expected_validation_errors.len(), 1);
    }

    mod ast_tests {
        use super::ast::*;

        #[test]
        fn test_csil_spec_builder() {
            let spec = CsilSpecBuilder::new()
                .with_rule(|rule| {
                    rule.name("person")
                        .with_group_def(|group| {
                            group
                                .field("name", builtin_type("text"))
                                .field("age", builtin_type("int"))
                                .build()
                        })
                        .build()
                })
                .build();

            assert_eq!(spec.rules.len(), 1);
            assert_eq!(spec.rules[0].name, "person");
        }

        #[test]
        fn test_type_expression_builder() {
            let array_type = TypeExpressionBuilder::new()
                .with_array(|elem| elem.builtin("text").build())
                .build();

            if let TestTypeExpression::Array(elem) = array_type {
                assert_eq!(*elem, TestTypeExpression::Builtin("text".to_string()));
            } else {
                panic!("Expected array type");
            }
        }

        #[test]
        fn test_deep_comparison() {
            let spec1 = CsilSpecBuilder::new()
                .with_rule(|rule| rule.name("test").type_def(builtin_type("int")).build())
                .build();

            let spec2 = CsilSpecBuilder::new()
                .with_rule(|rule| rule.name("test").type_def(builtin_type("int")).build())
                .build();

            assert!(assertion::deep_compare_specs(&spec1, &spec2).is_ok());

            let spec3 = CsilSpecBuilder::new()
                .with_rule(|rule| rule.name("test").type_def(builtin_type("text")).build())
                .build();

            assert!(assertion::deep_compare_specs(&spec1, &spec3).is_err());
        }

        #[test]
        fn test_visitor_pattern() {
            let spec = CsilSpecBuilder::new()
                .with_rule(|rule| rule.name("person").type_def(reference_type("User")).build())
                .with_rule(|rule| {
                    rule.name("users")
                        .type_def(array_of(reference_type("User")))
                        .build()
                })
                .build();

            let references = visitor::TypeReferenceCollector::collect_from_spec(&spec);
            assert_eq!(references.len(), 2);
            assert!(references.contains(&"User".to_string()));
        }

        #[test]
        fn test_invariant_validation() {
            let valid_spec = CsilSpecBuilder::new()
                .with_rule(|rule| rule.name("test").type_def(builtin_type("int")).build())
                .build();

            let errors = visitor::InvariantValidator::validate_spec(&valid_spec);
            assert!(errors.is_empty());

            // Test with invalid spec (empty rule name)
            let invalid_spec = TestCsilSpec {
                rules: vec![TestRule {
                    name: String::new(),
                    rule_type: TestRuleType::TypeDef(builtin_type("int")),
                }],
            };

            let errors = visitor::InvariantValidator::validate_spec(&invalid_spec);
            assert!(!errors.is_empty());
            assert!(errors[0].contains("Rule name cannot be empty"));
        }
    }
}
