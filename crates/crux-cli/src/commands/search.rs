//! `crux reindex` / `crux search` — Layer 6 surface.

use std::collections::HashSet;

use anyhow::Result;
use clap::Args as ClapArgs;

use crux_core::merkle::{FileChangeSet, MerkleSync, SCOPE_CHUNKS};
use crux_core::{paths, Runtime};
use crux_l6_search::{
    build_embedder, chunks_from_ast_filtered, chunks_from_memory_filtered,
    chunks_from_prose_filtered, list_ast_files, list_memory_files, list_prose_files, ContentType,
    Indexer, SearchEngine, SearchOptions,
};

use super::resolve_project_root;
use crate::Cli;

#[derive(Debug, ClapArgs)]
pub struct ReindexArgs {
    /// Drop existing chunks before re-indexing.
    #[arg(long)]
    pub force: bool,
    /// Skip prose (markdown / text) — only re-chunk the AST graph.
    #[arg(long)]
    pub no_prose: bool,
    /// Skip code chunks — only re-chunk prose. Useful when only docs changed.
    #[arg(long)]
    pub no_code: bool,
    /// Skip the memory scanner (CLAUDE.md, MEMORY.md, .crux/memory/*.md,
    /// $CRUX_HOME/memory/*.md). Leave it on to keep agent rules/notes
    /// searchable via `crux search --kind memory`.
    #[arg(long)]
    pub no_memory: bool,
}

#[derive(Debug, ClapArgs)]
pub struct SearchArgs {
    #[arg(value_name = "QUERY")]
    pub query: String,
    /// Limit the result list.
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
    /// Filter by chunk kind: code | prose | symbol | memory.
    /// Repeat to allow multiple kinds.
    #[arg(long = "kind", value_name = "KIND")]
    pub kinds: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// crux reindex
// ─────────────────────────────────────────────────────────────────────────

pub fn run_reindex(cli: &Cli, args: &ReindexArgs) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let runtime = Runtime::open(Some(project.clone()))?;
    let key = project.display().to_string();

    let indexer = Indexer::new(&runtime.conn);
    let sync = MerkleSync::new(&runtime.conn, &project, SCOPE_CHUNKS);

    if args.force {
        // Rebuild from scratch: drop every chunk + snapshot so the diff
        // below treats every current file as `added`.
        indexer.purge_project(&key)?;
        sync.purge()?;
    }

    // Gather the union of paths that could produce chunks. AST paths
    // come from Layer 5 output; prose paths come from a filesystem walk;
    // memory paths come from well-known agent files under project + home.
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

    // Paths whose chunks we must (re)build. When `--force` was passed
    // the snapshot is empty, so every current path lands in `added`.
    let changed: HashSet<String> = changes.changed();

    // Purge chunks for files that disappeared from disk / were excluded
    // by --no-code / --no-prose now.
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

    // Commit the fresh snapshot so the next run sees today's state as
    // the baseline. Done after chunking so a mid-run crash leaves the
    // previous snapshot intact.
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

// ─────────────────────────────────────────────────────────────────────────
// crux search
// ─────────────────────────────────────────────────────────────────────────

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
    let engine = SearchEngine::new(&runtime.conn, embedder.as_ref());
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
