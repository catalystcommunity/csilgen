//! Snapshot tests for generated output consistency

use anyhow::Result;
use csilgen_common::testing::{TestCase, TestFixtureLoader};
use csilgen_core::CsilSpec;
use insta::assert_snapshot;
use std::path::PathBuf;

/// Snapshot test harness for validating generated output consistency
pub struct SnapshotTestHarness {
    fixture_loader: TestFixtureLoader,
}

impl Default for SnapshotTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotTestHarness {
    /// Create a new snapshot test harness
    pub fn new() -> Self {
        let fixtures_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("tests")
            .join("fixtures");

        Self {
            fixture_loader: TestFixtureLoader::new(fixtures_path),
        }
    }

    /// Run snapshot tests for AST generation
    pub fn run_ast_snapshot_tests(&self) -> Result<()> {
        let test_cases = self.fixture_loader.load_test_cases_by_category("basic")?;

        for test_case in test_cases {
            if test_case.metadata.should_parse {
                self.run_ast_snapshot_test(&test_case)?;
            }
        }

        Ok(())
    }

    /// Run a single AST snapshot test
    fn run_ast_snapshot_test(&self, test_case: &TestCase) -> Result<()> {
        match parse_csil(&test_case.csil_content) {
            Ok(ast) => {
                let serialized_ast = serde_json::to_string_pretty(&ast)?;
                assert_snapshot!(format!("ast_{}", test_case.name), serialized_ast);
            }
            Err(e) => {
                // For tests that should parse but don't, we still want to snapshot the error
                // for regression testing
                assert_snapshot!(
                    format!("ast_error_{}", test_case.name),
                    format!("Parse error: {}", e)
                );
            }
        }
        Ok(())
    }

    /// Run snapshot tests for formatted output
    pub fn run_format_snapshot_tests(&self) -> Result<()> {
        let test_cases = self.fixture_loader.load_test_cases_by_category("basic")?;

        for test_case in test_cases {
            if test_case.metadata.should_parse {
                self.run_format_snapshot_test(&test_case)?;
            }
        }

        Ok(())
    }

    /// Run a single format snapshot test
    fn run_format_snapshot_test(&self, test_case: &TestCase) -> Result<()> {
        match parse_csil(&test_case.csil_content) {
            Ok(ast) => {
                let formatted_output = format_csil(&ast)?;
                assert_snapshot!(format!("formatted_{}", test_case.name), formatted_output);
            }
            Err(_) => {
                // Skip formatting tests for files that don't parse
            }
        }
        Ok(())
    }
}

fn format_csil(ast: &CsilSpec) -> Result<String> {
    csilgen_core::formatter::format_spec(ast, &csilgen_core::formatter::FormatConfig::default())
}

// Re-use the parse function from integration tests
fn parse_csil(content: &str) -> Result<CsilSpec> {
    csilgen_core::parse_csil(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::testing::{TempTestFixture, TestCaseMetadata};

    #[test]
    fn test_snapshot_harness_creation() {
        let _harness = SnapshotTestHarness::new();
        // Just verify we can create the harness without panic
        // The actual snapshot tests will run once parsing is implemented
    }

    #[test]
    fn test_snapshot_with_temp_fixture() {
        let temp_fixture = TempTestFixture::new().unwrap();

        // Create test fixtures
        let csil_content = r#"
person = {
  name: text,
  age: int,
}
"#;
        let metadata = TestCaseMetadata::default_for_valid_csil();

        temp_fixture
            .create_csil_file("person", csil_content)
            .unwrap();
        temp_fixture
            .create_metadata_file("person", &metadata)
            .unwrap();

        let loader = TestFixtureLoader::new(temp_fixture.path());
        let test_cases = loader.load_all_test_cases().unwrap();

        assert_eq!(test_cases.len(), 1);
        // Once parsing is implemented, we can add snapshot assertions here
    }
}
