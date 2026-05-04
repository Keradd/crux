use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use crux_l4_readcache::{compute_delta, CheckOptions, ReadCacheManager, ReadEvent};
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

fn bench_compute_delta(c: &mut Criterion) {
    let mut group = c.benchmark_group("l4_delta");

    let old_small: String = (0..100).map(|i| format!("line {}\n", i)).collect();
    let mut new_small = old_small.clone();
    new_small.push_str("added line\n");
    group.bench_with_input(
        BenchmarkId::new("small", "100_lines"),
        &(old_small, new_small),
        |b, (old, new)| b.iter(|| compute_delta(black_box(old), black_box(new))),
    );

    let old_medium: String = (0..1000).map(|i| format!("line {}\n", i)).collect();
    let mut new_medium = old_medium.clone();
    new_medium.push_str("added line\n");
    group.bench_with_input(
        BenchmarkId::new("medium", "1000_lines"),
        &(old_medium, new_medium),
        |b, (old, new)| b.iter(|| compute_delta(black_box(old), black_box(new))),
    );

    let old_large: String = (0..5000).map(|i| format!("line {}\n", i)).collect();
    let mut new_large = old_large.clone();
    new_large.push_str("added line\n");
    group.bench_with_input(
        BenchmarkId::new("large", "5000_lines"),
        &(old_large, new_large),
        |b, (old, new)| b.iter(|| compute_delta(black_box(old), black_box(new))),
    );

    group.finish();
}

fn bench_check_with(c: &mut Criterion) {
    let mut group = c.benchmark_group("l4_check_with");

    let conn = setup_db();
    let manager = ReadCacheManager::new(&conn);
    let tmp = TempDir::new().unwrap();
    let content = "fn main() {\n    println!(\"hello\");\n}\n";
    let path = create_test_file(tmp.path(), "test.rs", content);

    let ev = ReadEvent {
        agent_id: "bench-agent",
        session_id: "bench-session",
        project_root: tmp.path(),
        file_path: &path,
        offset: 0,
        limit: 10000,
    };

    group.bench_function("first_read", |b| {
        b.iter(|| {
            manager
                .check_with(black_box(&ev), &CheckOptions::default())
                .unwrap()
        })
    });

    manager.check_with(&ev, &CheckOptions::default()).unwrap();
    group.bench_function("cached_read", |b| {
        b.iter(|| {
            manager
                .check_with(black_box(&ev), &CheckOptions::default())
                .unwrap()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_compute_delta, bench_check_with);
criterion_main!(benches);
