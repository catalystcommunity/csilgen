use criterion::{Criterion, black_box, criterion_group, criterion_main};
use csilgen_core::FileDependencyGraph;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_large_dependency_graph(
    num_files: usize,
    deps_per_file: usize,
) -> (TempDir, Vec<PathBuf>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let mut files = Vec::new();

    // Create dependency files first
    for i in 0..num_files - 1 {
        let content = format!("Type{i} = {{ field: int }}");
        let file_path = temp_dir.path().join(format!("dep_{i}.csil"));
        fs::write(&file_path, content).expect("Failed to write file");
        files.push(file_path);
    }

    // Create entry point files that import multiple dependencies
    for i in 0..std::cmp::min(deps_per_file, num_files - 1) {
        let mut imports = String::new();
        for j in 0..std::cmp::min(deps_per_file, num_files - 1) {
            imports.push_str(&format!("include \"dep_{j}.csil\"\n"));
        }
        imports.push_str(&format!("EntryType{i} = {{ field: int }}"));

        let file_path = temp_dir.path().join(format!("entry_{i}.csil"));
        fs::write(&file_path, imports).expect("Failed to write file");
        files.push(file_path);
    }

    (temp_dir, files)
}

fn bench_dependency_graph_build_small(c: &mut Criterion) {
    c.bench_function("dependency_graph_build_10_files", |b| {
        b.iter(|| {
            let (_temp_dir, files) = create_large_dependency_graph(10, 3);
            let graph = FileDependencyGraph::build_from_files(black_box(&files)).unwrap();
            black_box(graph)
        });
    });
}

fn bench_dependency_graph_build_medium(c: &mut Criterion) {
    c.bench_function("dependency_graph_build_100_files", |b| {
        b.iter(|| {
            let (_temp_dir, files) = create_large_dependency_graph(100, 10);
            let graph = FileDependencyGraph::build_from_files(black_box(&files)).unwrap();
            black_box(graph)
        });
    });
}

fn bench_dependency_graph_build_large(c: &mut Criterion) {
    c.bench_function("dependency_graph_build_1000_files", |b| {
        b.iter(|| {
            let (_temp_dir, files) = create_large_dependency_graph(1000, 50);
            let graph = FileDependencyGraph::build_from_files(black_box(&files)).unwrap();
            black_box(graph)
        });
    });
}

fn bench_entry_point_detection(c: &mut Criterion) {
    let (_temp_dir, files) = create_large_dependency_graph(500, 25);
    let graph = FileDependencyGraph::build_from_files(&files).unwrap();

    c.bench_function("entry_point_detection_500_files", |b| {
        b.iter(|| {
            let entry_points = graph.find_entry_points();
            black_box(entry_points)
        });
    });
}

fn bench_circular_dependency_detection(c: &mut Criterion) {
    let (_temp_dir, files) = create_large_dependency_graph(200, 15);
    let graph = FileDependencyGraph::build_from_files(&files).unwrap();

    c.bench_function("circular_dependency_detection_200_files", |b| {
        b.iter(|| {
            let cycles = graph.has_circular_dependencies();
            black_box(cycles)
        });
    });
}

fn bench_import_scanning(c: &mut Criterion) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Create a file with many imports
    let mut content = String::new();
    for i in 0..100 {
        content.push_str(&format!("include \"file_{i}.csil\"\n"));
    }
    content.push_str("MainType = { field: int }");

    let file_path = temp_dir.path().join("large_imports.csil");
    fs::write(&file_path, content).expect("Failed to write file");

    c.bench_function("import_scanning_100_imports", |b| {
        b.iter(|| {
            let imports = csilgen_core::ImportScanner::scan_imports(black_box(&file_path)).unwrap();
            black_box(imports)
        });
    });
}

criterion_group!(
    benches,
    bench_dependency_graph_build_small,
    bench_dependency_graph_build_medium,
    bench_dependency_graph_build_large,
    bench_entry_point_detection,
    bench_circular_dependency_detection,
    bench_import_scanning
);
criterion_main!(benches);
