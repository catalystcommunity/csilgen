//! Integration tests for csilgen-core

use anyhow::Result;
use csilgen_common::testing::{TestCase, TestFixtureLoader};
use csilgen_core::{CsilSpec, validate_spec};
use std::path::PathBuf;

/// Integration test harness for end-to-end CSIL workflows
pub struct IntegrationTestHarness {
    fixture_loader: TestFixtureLoader,
}

impl Default for IntegrationTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl IntegrationTestHarness {
    /// Create a new integration test harness
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

    /// Run all integration tests
    pub fn run_all_tests(&self) -> Result<TestResults> {
        let test_cases = self.fixture_loader.load_all_test_cases()?;
        let mut results = TestResults::new();

        for test_case in test_cases {
            let result = self.run_single_test(&test_case);
            results.add_result(test_case.name.clone(), result);
        }

        Ok(results)
    }

    /// Run tests for a specific category
    pub fn run_category_tests(&self, category: &str) -> Result<TestResults> {
        let test_cases = self.fixture_loader.load_test_cases_by_category(category)?;
        let mut results = TestResults::new();

        for test_case in test_cases {
            let result = self.run_single_test(&test_case);
            results.add_result(test_case.name.clone(), result);
        }

        Ok(results)
    }

    /// Run a single test case
    fn run_single_test(&self, test_case: &TestCase) -> TestResult {
        let parse_result = parse_csil(&test_case.csil_content);

        match (parse_result, test_case.metadata.should_parse) {
            (Ok(ast), true) => {
                // Parse succeeded as expected, now validate
                let validation_result = validate_spec(&ast);
                self.check_validation_result(validation_result, &test_case.metadata)
            }
            (Ok(_), false) => TestResult::Failure {
                reason: "Expected parse failure but parsing succeeded".to_string(),
            },
            (Err(parse_error), true) => TestResult::Failure {
                reason: format!("Expected parse success but got error: {parse_error}"),
            },
            (Err(_), false) => TestResult::Success {
                message: "Parse failed as expected".to_string(),
            },
        }
    }

    fn check_validation_result(
        &self,
        validation_result: Result<()>,
        metadata: &csilgen_common::testing::TestCaseMetadata,
    ) -> TestResult {
        match (
            validation_result,
            metadata.expected_validation_errors.is_empty(),
        ) {
            (Ok(()), true) => TestResult::Success {
                message: "Parse and validation succeeded as expected".to_string(),
            },
            (Ok(()), false) => TestResult::Failure {
                reason: "Expected validation errors but validation succeeded".to_string(),
            },
            (Err(validation_error), true) => TestResult::Failure {
                reason: format!("Unexpected validation error: {validation_error}"),
            },
            (Err(_), false) => TestResult::Success {
                message: "Validation failed as expected".to_string(),
            },
        }
    }
}

/// Result of running a single test
#[derive(Debug, Clone)]
pub enum TestResult {
    Success { message: String },
    Failure { reason: String },
    Skipped { reason: String },
}

/// Collection of test results
#[derive(Debug)]
pub struct TestResults {
    results: Vec<(String, TestResult)>,
}

impl TestResults {
    fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    fn add_result(&mut self, test_name: String, result: TestResult) {
        self.results.push((test_name, result));
    }

    /// Get the total number of tests
    pub fn total_count(&self) -> usize {
        self.results.len()
    }

    /// Get the number of successful tests
    pub fn success_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, result)| matches!(result, TestResult::Success { .. }))
            .count()
    }

    /// Get the number of failed tests
    pub fn failure_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, result)| matches!(result, TestResult::Failure { .. }))
            .count()
    }

    /// Get the number of skipped tests
    pub fn skipped_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, result)| matches!(result, TestResult::Skipped { .. }))
            .count()
    }

    /// Check if all tests passed
    pub fn all_passed(&self) -> bool {
        self.failure_count() == 0
    }

    /// Get details of failed tests
    pub fn failed_tests(&self) -> Vec<&(String, TestResult)> {
        self.results
            .iter()
            .filter(|(_, result)| matches!(result, TestResult::Failure { .. }))
            .collect()
    }
}

// Re-use the parse function from the core crate
fn parse_csil(content: &str) -> Result<CsilSpec> {
    csilgen_core::parse_csil(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use csilgen_common::testing::{TempTestFixture, TestCaseMetadata};

    #[test]
    fn test_integration_harness_creation() {
        let harness = IntegrationTestHarness::new();
        // Just verify we can create the harness without panic
        let test_cases = harness.fixture_loader.load_all_test_cases().unwrap();
        // We should have loaded the test fixtures we created
        assert!(
            !test_cases.is_empty(),
            "Expected to load test fixtures, got {} cases",
            test_cases.len()
        );
    }

    #[test]
    fn test_results_counting() {
        let mut results = TestResults::new();

        results.add_result(
            "test1".to_string(),
            TestResult::Success {
                message: "OK".to_string(),
            },
        );
        results.add_result(
            "test2".to_string(),
            TestResult::Failure {
                reason: "Error".to_string(),
            },
        );
        results.add_result(
            "test3".to_string(),
            TestResult::Skipped {
                reason: "Skip".to_string(),
            },
        );

        assert_eq!(results.total_count(), 3);
        assert_eq!(results.success_count(), 1);
        assert_eq!(results.failure_count(), 1);
        assert_eq!(results.skipped_count(), 1);
        assert!(!results.all_passed());
    }

    #[test]
    fn test_with_temp_fixtures() {
        let temp_fixture = TempTestFixture::new().unwrap();

        // Create a simple valid CSIL file
        let csil_content = "name = text";
        let metadata = TestCaseMetadata::default_for_valid_csil();

        temp_fixture
            .create_csil_file("simple", csil_content)
            .unwrap();
        temp_fixture
            .create_metadata_file("simple", &metadata)
            .unwrap();

        let loader = TestFixtureLoader::new(temp_fixture.path());
        let test_cases = loader.load_all_test_cases().unwrap();

        assert_eq!(test_cases.len(), 1);
        assert_eq!(test_cases[0].name, "simple");
        assert!(test_cases[0].metadata.should_parse);
    }
}
