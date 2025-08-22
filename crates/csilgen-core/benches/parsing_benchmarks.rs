//! Performance benchmarks for CSIL parsing and validation

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use csilgen_common::testing::TestFixtureLoader;
use csilgen_core::validate_spec;
use std::path::PathBuf;

/// Benchmark CSIL parsing performance
fn bench_parsing(c: &mut Criterion) {
    let fixtures_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    let loader = TestFixtureLoader::new(fixtures_path);

    // Load basic test cases for benchmarking
    let basic_cases = loader
        .load_test_cases_by_category("basic")
        .unwrap_or_else(|_| Vec::new());

    let mut group = c.benchmark_group("parsing");

    for test_case in basic_cases {
        if test_case.metadata.should_parse {
            group.bench_with_input(
                BenchmarkId::new("parse_csil", &test_case.name),
                &test_case.csil_content,
                |b, content| {
                    b.iter(|| {
                        // This will panic until parsing is implemented, but that's expected
                        // The benchmark infrastructure is in place for when parsing is ready
                        let _ = black_box(parse_csil_bench(black_box(content)));
                    })
                },
            );
        }
    }

    group.finish();
}

/// Benchmark CSIL validation performance
fn bench_validation(c: &mut Criterion) {
    let fixtures_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");

    let loader = TestFixtureLoader::new(fixtures_path);

    // Load basic test cases for benchmarking
    let basic_cases = loader
        .load_test_cases_by_category("basic")
        .unwrap_or_else(|_| Vec::new());

    let mut group = c.benchmark_group("validation");

    for test_case in basic_cases {
        if test_case.metadata.should_parse {
            // First parse the content (when parsing is implemented)
            match parse_csil_bench(&test_case.csil_content) {
                Ok(ast) => {
                    group.bench_with_input(
                        BenchmarkId::new("validate_csil", &test_case.name),
                        &ast,
                        |b, ast| {
                            b.iter(|| {
                                let _ = black_box(validate_spec_bench(black_box(ast)));
                            })
                        },
                    );
                }
                Err(_) => {
                    // Skip validation benchmarks for files that don't parse
                }
            }
        }
    }

    group.finish();
}

/// Benchmark large file parsing performance
fn bench_large_files(c: &mut Criterion) {
    // Create synthetic large CSIL content for performance testing
    let large_content = generate_large_csil_content(1000);
    let extra_large_content = generate_large_csil_content(10000);

    let mut group = c.benchmark_group("large_files");

    group.bench_function("parse_1k_rules", |b| {
        b.iter(|| {
            let _ = black_box(parse_csil_bench(black_box(&large_content)));
        })
    });

    group.bench_function("parse_10k_rules", |b| {
        b.iter(|| {
            let _ = black_box(parse_csil_bench(black_box(&extra_large_content)));
        })
    });

    group.finish();
}

/// Generate synthetic large CSIL content for benchmarking
fn generate_large_csil_content(num_rules: usize) -> String {
    let mut content = String::new();

    for i in 0..num_rules {
        content.push_str(&format!(
            "rule_{i} = {{\n  field_a: text,\n  field_b: int,\n  field_c: bool,\n}}\n\n"
        ));
    }

    content
}

// Benchmark wrapper functions
fn parse_csil_bench(content: &str) -> Result<csilgen_core::CsilSpec, anyhow::Error> {
    csilgen_core::parse_csil(content).map_err(|e| anyhow::anyhow!("Parse error: {}", e))
}

fn validate_spec_bench(ast: &csilgen_core::CsilSpec) -> Result<(), anyhow::Error> {
    validate_spec(ast).map_err(|e| anyhow::anyhow!("Validation error: {}", e))
}

criterion_group!(benches, bench_parsing, bench_validation, bench_large_files);
criterion_main!(benches);
