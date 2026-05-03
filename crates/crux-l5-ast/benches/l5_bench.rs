use criterion::{black_box, criterion_group, criterion_main, Criterion};
use crux_l5_ast::{parse, GraphStore, Language};
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn setup_db() -> Connection {
    crux_core::db::open_in_memory().unwrap()
}

fn create_test_file(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, content).unwrap();
    path
}

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("l5_parse");

    // Small Rust file
    let small_rust = r#"
fn main() {
    println!("hello");
}

fn helper() -> i32 {
    42
}
"#;
    group.bench_function("rust_small", |b| {
        b.iter(|| {
            parse(
                black_box(Language::Rust),
                black_box(small_rust),
                Path::new("test.rs"),
            )
        })
    });

    // Medium Rust file (100 lines)
    let medium_rust: String = (0..100)
        .map(|i| format!("fn func_{}() {{ println!(\"{}\"); }}\n", i, i))
        .collect();
    group.bench_function("rust_medium", |b| {
        b.iter(|| {
            parse(
                black_box(Language::Rust),
                black_box(&medium_rust),
                Path::new("test.rs"),
            )
        })
    });

    // Python file
    let python_code = r#"
def hello():
    print("hello")

class MyClass:
    def __init__(self):
        self.x = 1
    
    def method(self):
        return self.x
"#;
    group.bench_function("python", |b| {
        b.iter(|| {
            parse(
                black_box(Language::Python),
                black_box(python_code),
                Path::new("test.py"),
            )
        })
    });

    group.finish();
}

fn bench_find_symbol(c: &mut Criterion) {
    let mut group = c.benchmark_group("l5_find_symbol");

    let conn = setup_db();
    let store = GraphStore::new(&conn);
    let tmp = TempDir::new().unwrap();

    // Index a medium Rust file
    let rust_code: String = (0..50)
        .map(|i| format!("pub fn func_{}() {{ println!(\"{}\"); }}\n", i, i))
        .collect();
    let path = create_test_file(tmp.path(), "lib.rs", &rust_code);
    let result = parse(Language::Rust, &rust_code, &path);
    store
        .write("test_project", "lib.rs", "rust", "hash123", &result)
        .unwrap();

    group.bench_function("exact_match", |b| {
        b.iter(|| {
            store.find_symbol(
                black_box("test_project"),
                black_box("func_25"),
                black_box(None),
            )
        })
    });

    group.bench_function("prefix_search", |b| {
        b.iter(|| {
            store.find_symbol_like(
                black_box("test_project"),
                black_box("func_2%"),
                black_box(10),
            )
        })
    });

    group.finish();
}

fn bench_impact_radius(c: &mut Criterion) {
    let mut group = c.benchmark_group("l5_impact_radius");

    let conn = setup_db();
    let store = GraphStore::new(&conn);

    // Index code with call relationships
    let rust_code = r#"
fn caller1() {
    callee1();
    callee2();
}

fn caller2() {
    callee1();
}

fn callee1() {
    println!("hello");
}

fn callee2() {
    println!("world");
}
"#;
    let tmp = TempDir::new().unwrap();
    let path = create_test_file(tmp.path(), "main.rs", rust_code);
    let result = parse(Language::Rust, rust_code, &path);
    store
        .write("test_project", "main.rs", "rust", "hash456", &result)
        .unwrap();

    group.bench_function("depth_1", |b| {
        b.iter(|| {
            store.impact_radius(
                black_box("test_project"),
                black_box("test_project::callee1"),
                black_box(1),
                black_box(100),
            )
        })
    });

    group.bench_function("depth_3", |b| {
        b.iter(|| {
            store.impact_radius(
                black_box("test_project"),
                black_box("test_project::callee1"),
                black_box(3),
                black_box(100),
            )
        })
    });

    group.finish();
}

criterion_group!(benches, bench_parse, bench_find_symbol, bench_impact_radius);
criterion_main!(benches);
