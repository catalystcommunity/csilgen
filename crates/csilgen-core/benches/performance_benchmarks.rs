use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use csilgen_common::GeneratorConfig;
use csilgen_core::parser::{parse_csil, parse_csil_file_streaming};
use csilgen_core::validator::{validate_spec, validate_spec_optimized};
use std::path::Path;
use std::time::Duration;

fn benchmark_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("parsing");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    // Small file benchmark
    if Path::new("tests/fixtures/basic/simple-types.csil").exists() {
        let small_content =
            std::fs::read_to_string("tests/fixtures/basic/simple-types.csil").unwrap();
        group.bench_with_input(
            BenchmarkId::new("small_file", "simple-types"),
            &small_content,
            |b, content| b.iter(|| parse_csil(black_box(content))),
        );
    }

    // Medium file benchmark
    if Path::new("tests/fixtures/services/complex-service.csil").exists() {
        let medium_content =
            std::fs::read_to_string("tests/fixtures/services/complex-service.csil").unwrap();
        group.bench_with_input(
            BenchmarkId::new("medium_file", "complex-service"),
            &medium_content,
            |b, content| b.iter(|| parse_csil(black_box(content))),
        );
    }

    // Large file benchmark
    if Path::new("tests/fixtures/performance/large-schema.csil").exists() {
        let large_content =
            std::fs::read_to_string("tests/fixtures/performance/large-schema.csil").unwrap();
        group.bench_with_input(
            BenchmarkId::new("large_file", "large-schema"),
            &large_content,
            |b, content| b.iter(|| parse_csil(black_box(content))),
        );

        // Compare streaming vs regular parsing for large files
        group.bench_function("large_file_streaming", |b| {
            b.iter(|| {
                parse_csil_file_streaming(black_box("tests/fixtures/performance/large-schema.csil"))
            })
        });
    }

    group.finish();
}

fn benchmark_validation(c: &mut Criterion) {
    let mut group = c.benchmark_group("validation");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    // Create test specs of different sizes
    let small_spec = create_test_spec(10); // 10 rules
    let medium_spec = create_test_spec(100); // 100 rules
    let large_spec = create_test_spec(1500); // 1500 rules (triggers parallel validation)

    group.bench_function("small_spec_standard", |b| {
        b.iter(|| validate_spec(black_box(&small_spec)))
    });

    group.bench_function("small_spec_optimized", |b| {
        b.iter(|| validate_spec_optimized(black_box(&small_spec)))
    });

    group.bench_function("medium_spec_standard", |b| {
        b.iter(|| validate_spec(black_box(&medium_spec)))
    });

    group.bench_function("medium_spec_optimized", |b| {
        b.iter(|| validate_spec_optimized(black_box(&medium_spec)))
    });

    group.bench_function("large_spec_standard", |b| {
        b.iter(|| validate_spec(black_box(&large_spec)))
    });

    group.bench_function("large_spec_optimized", |b| {
        b.iter(|| validate_spec_optimized(black_box(&large_spec)))
    });

    group.finish();
}

fn benchmark_wasm_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("wasm_execution");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));

    // Test WASM generator performance with different input sizes
    let small_spec = create_test_spec(5);
    let medium_spec = create_test_spec(50);
    let large_spec = create_test_spec(500);

    let mut runtime = csilgen_wasm_generators::WasmGeneratorRuntime::new().unwrap();
    runtime.discover_generators().ok();

    let test_config = create_test_generator_config();

    group.bench_function("noop_generator_small", |b| {
        b.iter(|| {
            runtime.execute_generator(
                black_box("noop"),
                black_box(&small_spec),
                black_box(&test_config),
            )
        })
    });

    group.bench_function("noop_generator_medium", |b| {
        b.iter(|| {
            runtime.execute_generator(
                black_box("noop"),
                black_box(&medium_spec),
                black_box(&test_config),
            )
        })
    });

    group.bench_function("noop_generator_large", |b| {
        b.iter(|| {
            runtime.execute_generator(
                black_box("noop"),
                black_box(&large_spec),
                black_box(&test_config),
            )
        })
    });

    group.finish();
}

fn benchmark_wasm_compilation_caching(c: &mut Criterion) {
    let mut group = c.benchmark_group("wasm_caching");
    group.sample_size(20);

    let mut runtime = csilgen_wasm_generators::WasmGeneratorRuntime::new().unwrap();
    runtime.discover_generators().ok();

    // Benchmark cache hit vs miss performance
    group.bench_function("cache_miss_compilation", |b| {
        b.iter(|| {
            // Clear cache to force recompilation
            runtime.cleanup_cache();
            runtime.precompile_generator(black_box("noop"))
        })
    });

    group.bench_function("cache_hit_lookup", |b| {
        // Precompile to ensure cache hit
        runtime.precompile_generator("noop").ok();

        b.iter(|| runtime.precompile_generator(black_box("noop")))
    });

    group.finish();
}

fn benchmark_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_usage");
    group.sample_size(5);
    group.measurement_time(Duration::from_secs(30));

    // Test memory usage with increasingly large specs
    let spec_sizes = [100, 500, 1000, 2000];

    for size in spec_sizes {
        let spec = create_test_spec(size);

        group.bench_with_input(
            BenchmarkId::new("parse_and_validate", size),
            &spec,
            |b, _| {
                b.iter(|| {
                    // Simulate the full pipeline
                    let spec_json = serde_json::to_string(&spec).unwrap();
                    let parsed = parse_csil(black_box(&spec_json));
                    if let Ok(parsed_spec) = parsed {
                        validate_spec_optimized(black_box(&parsed_spec)).ok();
                    }
                })
            },
        );
    }

    group.finish();
}

fn benchmark_parallel_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_processing");
    group.sample_size(5);
    group.measurement_time(Duration::from_secs(30));

    // Test parallel vs sequential validation performance
    let large_spec = create_test_spec(2000); // Large enough to trigger parallel processing

    group.bench_function("sequential_validation", |b| {
        b.iter(|| validate_spec(black_box(&large_spec)))
    });

    group.bench_function("parallel_validation", |b| {
        b.iter(|| validate_spec_optimized(black_box(&large_spec)))
    });

    group.finish();
}

// Helper function to create test generator config
fn create_test_generator_config() -> GeneratorConfig {
    GeneratorConfig {
        target: "test".to_string(),
        output_dir: "/tmp".to_string(),
        options: std::collections::HashMap::new(),
    }
}

// Helper function to create test specs of various sizes
fn create_test_spec(num_rules: usize) -> csilgen_core::ast::CsilSpec {
    use csilgen_core::ast::*;
    use csilgen_core::lexer::Position;

    let mut rules = Vec::new();

    for i in 0..num_rules {
        let rule = Rule {
            name: format!("Type{i}"),
            rule_type: RuleType::TypeDef(TypeExpression::Group(GroupExpression {
                entries: vec![
                    GroupEntry {
                        key: Some(GroupKey::Bare(format!("field{i}"))),
                        value_type: TypeExpression::Builtin("text".to_string()),
                        occurrence: None,
                        metadata: vec![
                            FieldMetadata::Visibility(FieldVisibility::Bidirectional),
                            FieldMetadata::Description(format!("Field {i} description")),
                        ],
                    },
                    GroupEntry {
                        key: Some(GroupKey::Bare("value".to_string())),
                        value_type: TypeExpression::Builtin("int".to_string()),
                        occurrence: None,
                        metadata: vec![],
                    },
                ],
            })),
            position: Position {
                line: i + 1,
                column: 1,
                offset: i * 50,
            },
        };
        rules.push(rule);
    }

    // Add some service rules for complexity
    if num_rules > 50 {
        for i in 0..(num_rules / 10).max(1) {
            let service_rule = Rule {
                name: format!("Service{i}"),
                rule_type: RuleType::ServiceDef(ServiceDefinition {
                    operations: vec![ServiceOperation {
                        name: format!("operation{i}"),
                        input_type: TypeExpression::Reference(format!("Type{i}")),
                        output_type: TypeExpression::Reference(format!(
                            "Type{}",
                            (i + 1) % num_rules
                        )),
                        direction: ServiceDirection::Unidirectional,
                        position: Position {
                            line: num_rules + i + 1,
                            column: 5,
                            offset: (num_rules + i) * 50 + 20,
                        },
                    }],
                }),
                position: Position {
                    line: num_rules + i + 1,
                    column: 1,
                    offset: (num_rules + i) * 50,
                },
            };
            rules.push(service_rule);
        }
    }

    CsilSpec {
        imports: Vec::new(),
        options: None,
        rules,
    }
}

criterion_group!(
    benches,
    benchmark_parsing,
    benchmark_validation,
    benchmark_wasm_execution,
    benchmark_wasm_compilation_caching,
    benchmark_memory_usage,
    benchmark_parallel_processing
);
criterion_main!(benches);
