# CSIL Testing Infrastructure

This document describes the comprehensive testing framework implemented for the csilgen project as part of Phase 1.1 of the implementation plan.

## Overview

The testing infrastructure provides a robust foundation for testing CSIL (CBOR Service Interface Language) parsing, validation, and code generation workflows. It includes fixtures, test utilities, integration tests, snapshot testing, and performance benchmarks.

## Structure

```
tests/
├── fixtures/                    # Test case files and metadata
│   ├── basic/                  # Basic CDDL functionality tests
│   │   ├── simple-types.csil   # Basic types (text, int, bool, bytes)
│   │   ├── group-definition.csil  # Group with optional fields
│   │   └── arrays-and-maps.csil   # Arrays and maps with cardinality
│   ├── invalid/                # Invalid CSIL that should fail parsing
│   │   ├── missing-assignment.csil
│   │   └── unclosed-group.csil
│   ├── services/               # CSIL service definitions (future)
│   │   └── simple-service.csil
│   ├── metadata/               # Field metadata tests (future)
│   │   └── field-visibility.csil
│   ├── breaking-changes/       # Breaking change detection tests
│   │   ├── v1-user.csil       # Baseline version
│   │   ├── v2-user-breaking.csil   # Version with breaking changes
│   │   └── v2-user-compatible.csil # Version with compatible changes
│   └── performance/            # Large files for performance testing
│       └── large-schema.csil
└── README.md                   # This file
```

## Test Case Metadata

Each `.csil` test file can have an optional `.meta.json` file that describes the expected behavior:

```json
{
  "description": "Human-readable description",
  "should_parse": true,
  "expected_validation_errors": [],
  "expected_ast": null,
  "tags": ["valid", "basic-types", "cddl"]
}
```

## Testing Utilities (csilgen-common)

### TestFixtureLoader
- Loads test cases from the fixtures directory
- Supports loading by category or all test cases
- Automatically pairs `.csil` files with `.meta.json` metadata

### TempTestFixture
- Creates temporary test fixtures for unit tests
- Manages temporary directories and cleanup
- Useful for testing the testing infrastructure itself

### TestCaseMetadata
- Structured metadata for test expectations
- Helper methods for common test scenarios
- Support for different test types (valid, parse errors, validation errors)

## Integration Testing (csilgen-core)

### IntegrationTestHarness
- End-to-end testing of CSIL workflows
- Loads fixtures and runs parse/validate cycles
- Collects and reports test results
- Support for category-specific test runs

### TestResults
- Aggregates test results across multiple test cases
- Provides counts for success/failure/skipped tests
- Identifies failed tests for detailed reporting

## Snapshot Testing

### SnapshotTestHarness
- Uses `insta` crate for snapshot testing
- Tests AST generation consistency
- Tests formatted output consistency
- Detects regressions in output format

## Performance Benchmarking

### Parsing Benchmarks
- Uses `criterion` crate for statistical benchmarking
- Measures parsing performance across different file sizes
- Measures validation performance
- Generates HTML reports for performance analysis

### Benchmark Categories
- **Small files**: Basic test fixtures from fixtures/basic/
- **Large files**: Synthetic files with 1K and 10K rules
- **Real-world files**: Performance test fixtures

## Usage

### Running All Tests
```bash
cargo test --workspace
```

### Running Integration Tests
```bash
cargo test --package csilgen-core --test integration_tests
```

### Running Snapshot Tests
```bash
cargo test --package csilgen-core --test snapshot_tests
```

### Running Benchmarks
```bash
cargo bench --package csilgen-core
```

### Loading Test Fixtures
```rust
use csilgen_common::testing::TestFixtureLoader;

let loader = TestFixtureLoader::new("tests/fixtures");
let basic_tests = loader.load_test_cases_by_category("basic")?;
let all_tests = loader.load_all_test_cases()?;
```

## Future Extensions

This testing infrastructure is designed to support the full CSIL implementation plan:

1. **Phase 2**: CDDL and CSIL parsing tests will use the fixture loader
2. **Phase 3**: Formatter and linter tests will use snapshot testing
3. **Phase 4**: WASM generator tests will extend the integration harness
4. **Phase 5**: Code generation tests will use snapshot testing for output validation

## Test Case Guidelines

When adding new test cases:

1. **Naming**: Use descriptive names that indicate what is being tested
2. **Metadata**: Always include `.meta.json` files for complex test cases
3. **Categories**: Organize tests into appropriate subdirectories
4. **Documentation**: Include comments in CSIL files explaining the test purpose
5. **Coverage**: Include both positive and negative test cases

## Dependencies

The testing infrastructure uses these key dependencies:

- `tempfile`: Temporary file and directory management
- `assert_fs`: Filesystem assertions
- `predicates`: Assertion predicates
- `pretty_assertions`: Better assertion output
- `insta`: Snapshot testing
- `criterion`: Performance benchmarking

All testing dependencies are feature-gated and optional to avoid bloating production builds.