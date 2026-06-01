use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args as ClapArgs;

use crux_core::merkle::{FileChangeSet, MerkleSync, SCOPE_CHUNKS};
use crux_core::{paths, Runtime};
use crux_l6_search::{
    build_embedder, chunks_from_ast_filtered, chunks_from_memory_filtered,
    chunks_from_prose_filtered, list_ast_files, list_memory_files, list_prose_files, ContentType,
    Indexer, SearchConfig, SearchEngine, SearchOptions,
};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, Default, ClapArgs)]
pub struct ReindexArgs {
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub no_prose: bool,
    #[arg(long)]
    pub no_code: bool,
    #[arg(long)]
    pub no_memory: bool,
    #[arg(long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
}

#[derive(Debug, ClapArgs)]
pub struct SearchArgs {
    #[arg(value_name = "QUERY")]
    pub query: String,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    #[arg(long = "kind", value_name = "KIND")]
    pub kinds: Vec<String>,
}

pub fn run_reindex(cli: &Cli, args: &ReindexArgs) -> Result<()> {
    let project = args
        .dir
        .clone()
        .unwrap_or_else(|| resolve_project_root(cli.project.as_deref()));
    let runtime = Runtime::open(Some(project.clone()))?;
    let key = project.display().to_string();

    let indexer = Indexer::new(&runtime.conn);
    let sync = MerkleSync::new(&runtime.conn, &project, SCOPE_CHUNKS);

    if args.force {
        indexer.purge_project(&key)?;
        sync.purge()?;
    }

    let crux_home = paths::crux_home().ok();
    let mut tracked: HashSet<String> = HashSet::new();
    if !args.no_code {
        for p in list_ast_files(&runtime.conn, &project)? {
            tracked.insert(p);
        }
    }
    if !args.no_prose {
        for p in list_prose_files(&project)? {
            tracked.insert(p);
        }
    }
    if !args.no_memory {
        for p in list_memory_files(&project, crux_home.as_deref())? {
            tracked.insert(p);
        }
    }

    let current = sync.scan(&project, tracked.iter())?;
    let stored = sync.load()?;
    let changes = MerkleSync::diff(&current, &stored);

    let changed: HashSet<String> = changes.changed();

    let chunks_removed = indexer.purge_files(&key, &changes.removed)?;
    sync.remove(&changes.removed)?;

    let embedder = build_embedder(&runtime.config.layer.l6)?;
    let dim = embedder.dim();
    let provider = embedder.provider().to_string();

    let mut total_inserted: u64 = 0;
    let mut total_skipped: u64 = 0;

    if !args.no_code {
        let filter = Some(&changed);
        let code_chunks = chunks_from_ast_filtered(&runtime.conn, &project, filter)?;
        let stats = indexer.index_chunks(&code_chunks, embedder.as_ref())?;
        total_inserted += stats.chunks_inserted;
        total_skipped += stats.chunks_skipped_unchanged;
        if !cli.json {
            println!(
                "indexed code: {} chunks (+{} new, {} unchanged)",
                code_chunks.len(),
                stats.chunks_inserted,
                stats.chunks_skipped_unchanged
            );
        }
    }

    if !args.no_prose {
        let filter = Some(&changed);
        let prose_chunks = chunks_from_prose_filtered(&project, filter)?;
        let stats = indexer.index_chunks(&prose_chunks, embedder.as_ref())?;
        total_inserted += stats.chunks_inserted;
        total_skipped += stats.chunks_skipped_unchanged;
        if !cli.json {
            println!(
                "indexed prose: {} chunks (+{} new, {} unchanged)",
                prose_chunks.len(),
                stats.chunks_inserted,
                stats.chunks_skipped_unchanged
            );
        }
    }

    if !args.no_memory {
        let filter = Some(&changed);
        let memory_chunks = chunks_from_memory_filtered(&project, crux_home.as_deref(), filter)?;
        let stats = indexer.index_chunks(&memory_chunks, embedder.as_ref())?;
        total_inserted += stats.chunks_inserted;
        total_skipped += stats.chunks_skipped_unchanged;
        if !cli.json {
            println!(
                "indexed memory: {} chunks (+{} new, {} unchanged)",
                memory_chunks.len(),
                stats.chunks_inserted,
                stats.chunks_skipped_unchanged
            );
        }
    }

    sync.commit(&current)?;

    let total_chunks = indexer.count_chunks(&key)?;
    print_merkle_summary(cli, &changes, chunks_removed);

    if cli.json {
        let payload = serde_json::json!({
            "project": key,
            "chunks_inserted": total_inserted,
            "chunks_unchanged": total_skipped,
            "chunks_removed":   chunks_removed,
            "chunks_total":     total_chunks,
            "files_added":      changes.added.len(),
            "files_modified":   changes.modified.len(),
            "files_removed":    changes.removed.len(),
            "files_unchanged":  changes.unchanged.len(),
            "embedder_provider": provider,
            "embedder_dim":     dim,
            "forced":           args.force,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("total chunks for {}: {}", key, total_chunks);
        println!("embedder: {} (dim {})", provider, dim);
    }
    Ok(())
}

fn print_merkle_summary(cli: &Cli, changes: &FileChangeSet, chunks_removed: u64) {
    if cli.json {
        return;
    }
    println!(
        "merkle diff: +{} added, ~{} modified, -{} removed, ={} unchanged ({} chunks purged)",
        changes.added.len(),
        changes.modified.len(),
        changes.removed.len(),
        changes.unchanged.len(),
        chunks_removed,
    );
}

pub fn run_search(cli: &Cli, args: &SearchArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let key = project.display().to_string();

    let kinds: Vec<ContentType> = args
        .kinds
        .iter()
        .filter_map(|s| ContentType::parse(s.as_str()))
        .collect();
    if !args.kinds.is_empty() && kinds.len() != args.kinds.len() {
        return Err(anyhow::anyhow!(
            "unknown --kind value (want code|prose|symbol|memory)"
        ));
    }

    let opts = SearchOptions {
        limit: args.limit,
        kinds,
    };

    let embedder = build_embedder(&runtime.config.layer.l6)?;
    let search_cfg = SearchConfig::from(&runtime.config.layer.l6);
    let engine = SearchEngine::with_config(&runtime.conn, embedder.as_ref(), search_cfg);
    let hits = engine.hybrid_search(&key, &args.query, &opts)?;

    if cli.json {
        let arr: Vec<_> = hits
            .iter()
            .map(|h| serde_json::to_value(h).unwrap())
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }

    if hits.is_empty() {
        println!(
            "(no matches for {:?} — run `crux reindex` if the chunk store is empty)",
            args.query
        );
        return Ok(());
    }

    println!("hybrid search for {:?} ({} hits):", args.query, hits.len());
    for (i, h) in hits.iter().enumerate() {
        let title = h.chunk.title.as_deref().unwrap_or("(no title)");
        println!(
            "{:>2}. [{:.3}] {} {} ({}:{}-{})",
            i + 1,
            h.score,
            h.chunk.content_type.as_str(),
            title,
            h.chunk.file_path,
            h.chunk.line_start,
            h.chunk.line_end,
        );
        let provenance: Vec<String> = [
            ("porter", h.bm25_porter_rank),
            ("trigram", h.bm25_trigram_rank),
            ("vector", h.vector_rank),
        ]
        .iter()
        .filter_map(|(name, r)| r.map(|n| format!("{}=#{}", name, n)))
        .collect();
        if !provenance.is_empty() {
            println!("    ranks: {}", provenance.join(", "));
        }
        println!("    {}", h.snippet);
    }
    Ok(())
}
