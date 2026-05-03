use criterion::{black_box, criterion_group, criterion_main, Criterion};
use crux_l6_search::{
    Chunk, ContentType, Embedder, HashEmbedder, Indexer, SearchEngine, SearchOptions,
};
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

fn bench_embed(c: &mut Criterion) {
    let mut group = c.benchmark_group("l6_embed");

    let embedder = HashEmbedder::new(384);

    let small_text = "fn main() { println!(\"hello\"); }";
    group.bench_function("small", |b| {
        b.iter(|| embedder.embed(black_box(small_text)))
    });

    let medium_text: String = (0..100)
        .map(|i| format!("fn func_{}() {{ println!(\"{}\"); }}\n", i, i))
        .collect();
    group.bench_function("medium", |b| {
        b.iter(|| embedder.embed(black_box(&medium_text)))
    });

    let large_text: String = (0..1000)
        .map(|i| format!("fn func_{}() {{ println!(\"{}\"); }}\n", i, i))
        .collect();
    group.bench_function("large", |b| {
        b.iter(|| embedder.embed(black_box(&large_text)))
    });

    group.finish();
}

fn bench_index(c: &mut Criterion) {
    let mut group = c.benchmark_group("l6_index");

    let conn = setup_db();
    let embedder = HashEmbedder::new(384);
    let indexer = Indexer::new(&conn);

    let tmp = TempDir::new().unwrap();
    let content = "fn main() { println!(\"hello\"); }";
    let path = create_test_file(tmp.path(), "test.rs", content);

    let chunk = Chunk {
        project_root: tmp.path().to_string_lossy().to_string(),
        source_id: None,
        file_path: path.to_string_lossy().to_string(),
        language: Some("rust".to_string()),
        content_type: ContentType::Code,
        title: Some("main".to_string()),
        content: content.to_string(),
        line_start: 0,
        line_end: 3,
    };

    group.bench_function("single_chunk", |b| {
        b.iter(|| {
            indexer
                .index_chunks(
                    black_box(std::slice::from_ref(&chunk)),
                    black_box(&embedder),
                )
                .unwrap()
        })
    });

    // Multiple chunks
    let chunks: Vec<Chunk> = (0..10)
        .map(|i| Chunk {
            project_root: tmp.path().to_string_lossy().to_string(),
            source_id: None,
            file_path: path.to_string_lossy().to_string(),
            language: Some("rust".to_string()),
            content_type: ContentType::Code,
            title: Some(format!("func_{}", i)),
            content: format!("fn func_{}() {{ println!(\"{}\"); }}\n", i, i),
            line_start: i * 3,
            line_end: (i + 1) * 3,
        })
        .collect();

    group.bench_function("ten_chunks", |b| {
        b.iter(|| {
            indexer
                .index_chunks(black_box(&chunks), black_box(&embedder))
                .unwrap()
        })
    });

    group.finish();
}

fn bench_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("l6_search");

    let conn = setup_db();
    let embedder = HashEmbedder::new(384);
    let indexer = Indexer::new(&conn);
    let searcher = SearchEngine::new(&conn, &embedder);

    // Index some content
    let tmp = TempDir::new().unwrap();
    let code = r#"
fn main() {
    println!("hello world");
}

fn helper() -> i32 {
    42
}

fn another_function() {
    helper();
}
"#;
    let path = create_test_file(tmp.path(), "main.rs", code);
    let chunks = vec![
        Chunk {
            project_root: tmp.path().to_string_lossy().to_string(),
            source_id: None,
            file_path: path.to_string_lossy().to_string(),
            language: Some("rust".to_string()),
            content_type: ContentType::Code,
            title: Some("main".to_string()),
            content: "fn main() { println!(\"hello world\"); }".to_string(),
            line_start: 0,
            line_end: 3,
        },
        Chunk {
            project_root: tmp.path().to_string_lossy().to_string(),
            source_id: None,
            file_path: path.to_string_lossy().to_string(),
            language: Some("rust".to_string()),
            content_type: ContentType::Code,
            title: Some("helper".to_string()),
            content: "fn helper() -> i32 { 42 }".to_string(),
            line_start: 4,
            line_end: 7,
        },
    ];
    indexer.index_chunks(&chunks, &embedder).unwrap();

    let opts = SearchOptions {
        limit: 10,
        kinds: vec![],
    };

    group.bench_function("single_word", |b| {
        b.iter(|| {
            searcher
                .hybrid_search(
                    black_box("test_project"),
                    black_box("main"),
                    black_box(&opts),
                )
                .unwrap()
        })
    });

    group.bench_function("multi_word", |b| {
        b.iter(|| {
            searcher
                .hybrid_search(
                    black_box("test_project"),
                    black_box("hello world"),
                    black_box(&opts),
                )
                .unwrap()
        })
    });

    group.bench_function("code_query", |b| {
        b.iter(|| {
            searcher
                .hybrid_search(
                    black_box("test_project"),
                    black_box("fn helper"),
                    black_box(&opts),
                )
                .unwrap()
        })
    });

    group.finish();
}

criterion_group!(benches, bench_embed, bench_index, bench_search);
criterion_main!(benches);
