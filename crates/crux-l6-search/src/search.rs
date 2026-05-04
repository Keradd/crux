use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection};

use crux_core::error::Result;

use crate::embed::{cosine_normalized, unpack_vector, Embedder};
use crate::types::{ContentType, HybridResult, StoredChunk};

const RRF_K: f64 = 60.0;
const FTS_FETCH_PER_TOKENIZER: usize = 50;
const VECTOR_FETCH_LIMIT: usize = 200;
const SNIPPET_WINDOW: usize = 80;
const PROXIMITY_ALPHA: f64 = 0.05;
const PROXIMITY_BETA: f64 = 40.0;
const FUZZY_MAX_DIST: u32 = 1;
const FUZZY_VOCAB_CAP: usize = 5_000;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    pub kinds: Vec<ContentType>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            kinds: Vec::new(),
        }
    }
}

pub struct SearchEngine<'c> {
    conn: &'c Connection,
    embedder: &'c dyn Embedder,
}

impl<'c> SearchEngine<'c> {
    pub fn new(conn: &'c Connection, embedder: &'c dyn Embedder) -> Self {
        Self { conn, embedder }
    }

    pub fn hybrid_search(
        &self,
        project_root: &str,
        query: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<HybridResult>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let out = self.hybrid_search_raw(project_root, query, opts)?;
        if !out.is_empty() {
            return Ok(out);
        }
        if let Some(corrected) = self.fuzzy_correct(project_root, query)? {
            if corrected != query {
                return self.hybrid_search_raw(project_root, &corrected, opts);
            }
        }
        Ok(out)
    }

    fn hybrid_search_raw(
        &self,
        project_root: &str,
        query: &str,
        opts: &SearchOptions,
    ) -> Result<Vec<HybridResult>> {
        let porter = self
            .bm25_search("chunks_fts_porter", project_root, query, &opts.kinds)
            .unwrap_or_default();
        let trigram = self
            .bm25_search("chunks_fts_trigram", project_root, query, &opts.kinds)
            .unwrap_or_default();
        let dense = self.dense_search(project_root, query, &opts.kinds)?;

        let mut accum: HashMap<i64, ResultAccum> = HashMap::new();

        merge_ranker(&mut accum, &porter, |acc, r| acc.bm25_porter_rank = Some(r));
        merge_ranker(&mut accum, &trigram, |acc, r| {
            acc.bm25_trigram_rank = Some(r)
        });
        merge_ranker(&mut accum, &dense, |acc, r| acc.vector_rank = Some(r));

        if accum.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = accum.keys().copied().collect();
        let chunks = self.fetch_chunks(&ids)?;

        let qtokens = tokenize_query(query);

        let mut out: Vec<HybridResult> = chunks
            .into_iter()
            .map(|c| {
                let acc = accum.get(&c.id).cloned().unwrap_or_default();
                let snippet = best_snippet(&c.content, query);
                let mut score = acc.score;
                if qtokens.len() >= 2 {
                    if let Some(window) = proximity_window(&c.content, &qtokens) {
                        score += PROXIMITY_ALPHA / (1.0 + (window as f64 / PROXIMITY_BETA));
                    }
                }
                HybridResult {
                    score,
                    bm25_porter_rank: acc.bm25_porter_rank,
                    bm25_trigram_rank: acc.bm25_trigram_rank,
                    vector_rank: acc.vector_rank,
                    snippet,
                    chunk: c,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(opts.limit);
        Ok(out)
    }

    fn fuzzy_correct(&self, project_root: &str, query: &str) -> Result<Option<String>> {
        let qtokens = tokenize_query(query);
        if qtokens.is_empty() {
            return Ok(None);
        }
        let vocab = self.fetch_title_vocab(project_root)?;
        if vocab.is_empty() {
            return Ok(None);
        }
        let mut corrected: Vec<String> = Vec::with_capacity(qtokens.len());
        let mut any_change = false;
        for t in &qtokens {
            if t.len() < 3 || vocab.contains(t) {
                corrected.push(t.clone());
                continue;
            }
            match nearest_vocab_term(t, &vocab, FUZZY_MAX_DIST) {
                Some(neighbor) if neighbor != *t => {
                    corrected.push(neighbor);
                    any_change = true;
                }
                _ => corrected.push(t.clone()),
            }
        }
        Ok(if any_change {
            Some(corrected.join(" "))
        } else {
            None
        })
    }

    fn fetch_title_vocab(&self, project_root: &str) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT title FROM chunks
              WHERE project_root = ? AND title IS NOT NULL AND title != ''
              LIMIT ?",
        )?;
        let rows = stmt.query_map(params![project_root, FUZZY_VOCAB_CAP as i64], |r| {
            r.get::<_, Option<String>>(0)
        })?;
        let mut vocab: HashSet<String> = HashSet::new();
        for row in rows {
            if let Some(title) = row? {
                for w in title
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .map(|w| w.to_ascii_lowercase())
                {
                    if w.len() >= 3 {
                        vocab.insert(w);
                    }
                }
            }
        }
        Ok(vocab)
    }

    fn bm25_search(
        &self,
        fts_table: &str,
        project_root: &str,
        query: &str,
        kinds: &[ContentType],
    ) -> Result<Vec<i64>> {
        let q = sanitize_fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let kind_filter = kind_clause(kinds);
        let sql = format!(
            "SELECT c.id
               FROM {fts_table} f
               JOIN chunks c ON c.id = f.rowid
              WHERE f.{fts_table} MATCH ?
                AND c.project_root = ?
                {kind_filter}
              ORDER BY bm25({fts_table})
              LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![q, project_root, FTS_FETCH_PER_TOKENIZER as i64],
                |r| r.get::<_, i64>(0),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn dense_search(
        &self,
        project_root: &str,
        query: &str,
        kinds: &[ContentType],
    ) -> Result<Vec<i64>> {
        let qv = self.embedder.embed(query)?;
        let dim = self.embedder.dim() as i64;
        let provider = self.embedder.provider();
        let model = self.embedder.model();
        let kind_filter = kind_clause(kinds);
        let sql = format!(
            "SELECT e.chunk_id, e.vector
               FROM chunk_embeddings e
               JOIN chunks c ON c.id = e.chunk_id
              WHERE c.project_root = ?
                AND e.dim = ? AND e.provider = ? AND e.model = ?
                {kind_filter}
              LIMIT ?"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<(i64, Vec<u8>)> = stmt
            .query_map(
                params![
                    project_root,
                    dim,
                    provider,
                    model,
                    VECTOR_FETCH_LIMIT as i64
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )?
            .collect::<rusqlite::Result<_>>()?;

        let mut scored: Vec<(i64, f32)> = rows
            .into_iter()
            .filter_map(|(id, blob)| {
                unpack_vector(&blob, self.embedder.dim()).map(|v| (id, cosine_normalized(&qv, &v)))
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scored.into_iter().map(|(id, _)| id).collect())
    }

    fn fetch_chunks(&self, ids: &[i64]) -> Result<Vec<StoredChunk>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT id, project_root, source_id, file_path, language,
                    content_type, title, content, line_start, line_end,
                    tokens_est, content_hash
               FROM chunks
              WHERE id IN ({placeholders})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params_vec: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|i| i as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(&*params_vec, |r| {
                let kind_s: String = r.get(5)?;
                let kind = ContentType::parse(&kind_s).unwrap_or(ContentType::Code);
                Ok(StoredChunk {
                    id: r.get(0)?,
                    project_root: r.get(1)?,
                    source_id: r.get(2)?,
                    file_path: r.get(3)?,
                    language: r.get(4)?,
                    content_type: kind,
                    title: r.get(6)?,
                    content: r.get(7)?,
                    line_start: r.get::<_, i64>(8)? as u32,
                    line_end: r.get::<_, i64>(9)? as u32,
                    tokens_est: r.get::<_, i64>(10)? as u32,
                    content_hash: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[derive(Default, Clone)]
struct ResultAccum {
    score: f64,
    bm25_porter_rank: Option<usize>,
    bm25_trigram_rank: Option<usize>,
    vector_rank: Option<usize>,
}

fn merge_ranker<F>(out: &mut HashMap<i64, ResultAccum>, ranked: &[i64], setter: F)
where
    F: Fn(&mut ResultAccum, usize),
{
    for (rank, id) in ranked.iter().enumerate() {
        let entry = out.entry(*id).or_default();
        let rank_1 = rank + 1;
        entry.score += 1.0 / (RRF_K + rank_1 as f64);
        setter(entry, rank_1);
    }
}

fn kind_clause(kinds: &[ContentType]) -> String {
    if kinds.is_empty() {
        return String::new();
    }
    let inner = kinds
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect::<Vec<_>>()
        .join(",");
    format!("AND c.content_type IN ({inner})")
}

fn sanitize_fts_query(q: &str) -> String {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect();
    if words.is_empty() {
        return String::new();
    }
    words.join(" OR ")
}

fn best_snippet(content: &str, query: &str) -> String {
    let qtokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    if qtokens.is_empty() {
        return content.chars().take(SNIPPET_WINDOW).collect();
    }
    let lower = content.to_ascii_lowercase();
    let mut best_pos = 0usize;
    let mut best_hits = 0usize;
    let mut start = 0usize;
    while start < lower.len() {
        let end = (start + SNIPPET_WINDOW).min(lower.len());
        let window = safe_slice(&lower, start, end);
        let hits = qtokens
            .iter()
            .filter(|t| window.contains(t.as_str()))
            .count();
        if hits > best_hits {
            best_hits = hits;
            best_pos = start;
        }
        start += SNIPPET_WINDOW / 2;
    }
    let lo = best_pos;
    let hi = (best_pos + SNIPPET_WINDOW).min(content.len());
    let mut snippet = String::new();
    if lo > 0 {
        snippet.push('…');
    }
    snippet.push_str(safe_slice(content, lo, hi));
    if hi < content.len() {
        snippet.push('…');
    }
    snippet
}

fn safe_slice(s: &str, lo: usize, hi: usize) -> &str {
    let mut a = lo;
    while a < s.len() && !s.is_char_boundary(a) {
        a += 1;
    }
    let mut b = hi.min(s.len());
    while b > a && !s.is_char_boundary(b) {
        b -= 1;
    }
    &s[a..b]
}

fn tokenize_query(query: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for w in query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
    {
        if seen.insert(w.clone()) {
            out.push(w);
        }
    }
    out
}

fn proximity_window(content: &str, qtokens: &[String]) -> Option<usize> {
    if qtokens.len() < 2 {
        return None;
    }
    let lower = content.to_ascii_lowercase();
    let mut occurrences: Vec<(usize, usize, usize)> = Vec::new();
    for (idx, t) in qtokens.iter().enumerate() {
        if t.is_empty() {
            continue;
        }
        let mut cursor = 0usize;
        while cursor < lower.len() {
            match lower[cursor..].find(t.as_str()) {
                Some(rel) => {
                    let abs = cursor + rel;
                    occurrences.push((idx, abs, t.len()));
                    cursor = abs + t.len().max(1);
                }
                None => break,
            }
        }
    }
    if occurrences.len() < 2 {
        return None;
    }
    occurrences.sort_by_key(|(_, pos, _)| *pos);

    let mut present: HashSet<usize> = HashSet::new();
    for (idx, _, _) in &occurrences {
        present.insert(*idx);
    }
    if present.len() < 2 {
        return None;
    }

    let mut counts: HashMap<usize, usize> = HashMap::with_capacity(present.len());
    let mut covered = 0usize;
    let mut best = usize::MAX;
    let mut left = 0usize;
    for right in 0..occurrences.len() {
        let (ridx, _, _) = occurrences[right];
        let c = counts.entry(ridx).or_insert(0);
        if *c == 0 {
            covered += 1;
        }
        *c += 1;
        while covered == present.len() {
            let (_, lpos, _) = occurrences[left];
            let (_, rpos, rlen) = occurrences[right];
            let window = (rpos + rlen).saturating_sub(lpos);
            if window < best {
                best = window;
            }
            let (lidx, _, _) = occurrences[left];
            let lc = counts.entry(lidx).or_insert(0);
            if *lc > 0 {
                *lc -= 1;
                if *lc == 0 {
                    covered -= 1;
                }
            }
            left += 1;
        }
    }
    if best == usize::MAX {
        None
    } else {
        Some(best)
    }
}

fn nearest_vocab_term(t: &str, vocab: &HashSet<String>, max_dist: u32) -> Option<String> {
    let mut best: Option<(String, u32)> = None;
    let tlen = t.chars().count();
    for w in vocab {
        let wlen = w.chars().count();
        let diff = (wlen as isize - tlen as isize).unsigned_abs() as u32;
        if diff > max_dist {
            continue;
        }
        let d = levenshtein_bounded(t, w, max_dist);
        if d > max_dist {
            continue;
        }
        match &best {
            None => best = Some((w.clone(), d)),
            Some((_, bd)) if d < *bd => best = Some((w.clone(), d)),
            _ => {}
        }
    }
    best.map(|(w, _)| w)
}

fn levenshtein_bounded(a: &str, b: &str, max_dist: u32) -> u32 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a == b {
        return 0;
    }
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n as u32;
    }
    if n == 0 {
        return m as u32;
    }
    let mut prev: Vec<u32> = (0..=n as u32).collect();
    let mut curr: Vec<u32> = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i as u32;
        let mut row_min = curr[0];
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            if curr[j] < row_min {
                row_min = curr[j];
            }
        }
        if row_min > max_dist {
            return max_dist + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;
    use crate::index::Indexer;
    use crate::types::Chunk;

    fn make_chunk(file: &str, title: &str, content: &str) -> Chunk {
        Chunk {
            project_root: "/tmp/p".into(),
            source_id: None,
            file_path: file.into(),
            language: Some("rust".into()),
            content_type: ContentType::Code,
            title: Some(title.into()),
            content: content.into(),
            line_start: 1,
            line_end: 5,
        }
    }

    fn seed_engine() -> rusqlite::Connection {
        let conn = crux_core::db::open_in_memory().unwrap();
        let emb = HashEmbedder::new(128);
        let idx = Indexer::new(&conn);
        let chunks = vec![
            make_chunk(
                "a.rs",
                "compute_delta",
                "compute delta over old and new strings",
            ),
            make_chunk(
                "b.rs",
                "render_html",
                "render html template into the response body",
            ),
            make_chunk(
                "c.rs",
                "cache_lookup",
                "look up an entry in the read cache by path",
            ),
        ];
        idx.index_chunks(&chunks, &emb).unwrap();
        conn
    }

    #[test]
    fn finds_relevant_chunk_for_keyword_query() {
        let conn = seed_engine();
        let emb = HashEmbedder::new(128);
        let engine = SearchEngine::new(&conn, &emb);
        let opts = SearchOptions {
            limit: 5,
            kinds: vec![],
        };
        let hits = engine
            .hybrid_search("/tmp/p", "delta strings", &opts)
            .unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].chunk.title.as_deref(), Some("compute_delta"));
    }

    #[test]
    fn empty_query_returns_no_results() {
        let conn = seed_engine();
        let emb = HashEmbedder::new(128);
        let engine = SearchEngine::new(&conn, &emb);
        let opts = SearchOptions::default();
        let hits = engine.hybrid_search("/tmp/p", "", &opts).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn kind_filter_excludes_other_types() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let emb = HashEmbedder::new(128);
        let idx = Indexer::new(&conn);
        idx.index_chunks(
            &[
                make_chunk("a.rs", "code one", "delta cache logic"),
                Chunk {
                    project_root: "/tmp/p".into(),
                    source_id: None,
                    file_path: "doc.md".into(),
                    language: None,
                    content_type: ContentType::Prose,
                    title: Some("note".into()),
                    content: "delta cache discussion in prose".into(),
                    line_start: 1,
                    line_end: 1,
                },
            ],
            &emb,
        )
        .unwrap();
        let engine = SearchEngine::new(&conn, &emb);
        let opts = SearchOptions {
            limit: 5,
            kinds: vec![ContentType::Prose],
        };
        let hits = engine
            .hybrid_search("/tmp/p", "delta cache", &opts)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].chunk.content_type, ContentType::Prose);
    }

    #[test]
    fn sanitize_fts_query_drops_punct() {
        let q = sanitize_fts_query("hello, world! foo:bar");
        assert!(q.contains("\"hello\""));
        assert!(q.contains(" OR "));
        assert!(!q.contains(","));
    }

    #[test]
    fn snippet_handles_short_content() {
        let s = best_snippet("hi", "anything");
        assert_eq!(s, "hi");
    }

    #[test]
    fn snippet_survives_multibyte_content() {
        let mut content = String::new();
        content.push_str(&"x".repeat(70));
        content.push_str(" — em-dash landing zone — ");
        content.push_str(&"y".repeat(200));
        content.push_str(" MerkleSync::commit at the tail end of the chunk");
        let s = best_snippet(&content, "merkle sync");
        assert!(
            s.contains("Merkle"),
            "expected snippet to include Merkle, got {s}"
        );
    }

    #[test]
    fn tokenize_query_dedups_and_lowers() {
        assert_eq!(
            tokenize_query("Delta, delta! DELTA cache"),
            vec!["delta".to_string(), "cache".to_string()]
        );
    }

    #[test]
    fn proximity_window_picks_smallest_span() {
        let content = "alpha one two three delta cache beta";
        let qt = tokenize_query("delta cache");
        let window = proximity_window(content, &qt).unwrap();
        assert_eq!(window, "delta cache".len());
    }

    #[test]
    fn proximity_window_none_when_only_one_token_present() {
        let content = "only delta here";
        let qt = tokenize_query("delta cache");
        assert!(proximity_window(content, &qt).is_none());
    }

    #[test]
    fn proximity_rerank_prefers_tighter_chunk() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let emb = HashEmbedder::new(128);
        let idx = Indexer::new(&conn);
        idx.index_chunks(
            &[
                make_chunk(
                    "far.rs",
                    "far apart",
                    "delta over something else very long and totally unrelated that drags context away then finally mentions cache much later",
                ),
                make_chunk("tight.rs", "tight match", "delta cache hit rate"),
            ],
            &emb,
        )
        .unwrap();
        let engine = SearchEngine::new(&conn, &emb);
        let opts = SearchOptions {
            limit: 5,
            kinds: vec![],
        };
        let hits = engine
            .hybrid_search("/tmp/p", "delta cache", &opts)
            .unwrap();
        assert!(hits.len() >= 2);
        assert_eq!(
            hits[0].chunk.file_path,
            "tight.rs",
            "tighter proximity should rank first, got {:?}",
            hits.iter().map(|h| &h.chunk.file_path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn levenshtein_bounded_reports_typo_distance() {
        assert_eq!(levenshtein_bounded("cache", "cache", 1), 0);
        assert_eq!(levenshtein_bounded("cache", "cacne", 1), 1);
        assert_eq!(levenshtein_bounded("cache", "caceh", 1), 2);
        assert!(levenshtein_bounded("alpha", "zzzzz", 1) > 1);
    }

    #[test]
    fn fuzzy_correct_swaps_typo_to_nearest_title_term() {
        let conn = seed_engine();
        let emb = HashEmbedder::new(128);
        let engine = SearchEngine::new(&conn, &emb);
        let corrected = engine.fuzzy_correct("/tmp/p", "dalta").unwrap();
        assert_eq!(
            corrected.as_deref(),
            Some("delta"),
            "expected `dalta` to be corrected to `delta`"
        );
    }

    #[test]
    fn fuzzy_correct_preserves_exact_match() {
        let conn = seed_engine();
        let emb = HashEmbedder::new(128);
        let engine = SearchEngine::new(&conn, &emb);
        assert!(engine.fuzzy_correct("/tmp/p", "delta").unwrap().is_none());
    }

    #[test]
    fn hybrid_search_falls_back_to_fuzzy_when_direct_hit_missing() {
        let conn = seed_engine();
        let emb = HashEmbedder::new(128);
        let engine = SearchEngine::new(&conn, &emb);
        let opts = SearchOptions {
            limit: 5,
            kinds: vec![],
        };
        let hits = engine.hybrid_search("/tmp/p", "dalta", &opts).unwrap();
        assert!(
            !hits.is_empty(),
            "fuzzy fallback should recover a hit for a 1-edit typo"
        );
    }
}
