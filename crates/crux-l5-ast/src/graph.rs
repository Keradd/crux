//! `GraphStore` — SQLite-backed persistence + query API for the AST graph.

use rusqlite::{params, Connection};

use crux_core::error::Result;

use crate::tsconfig::JsModuleResolver;
use crate::types::{
    ConfidenceTier, EdgeKind, GraphNode, NodeKind, ParseResult, ParsedEdge, ParsedNode,
};

pub struct GraphStore<'c> {
    conn: &'c Connection,
}

impl<'c> GraphStore<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Drop everything for a single file (so re-indexing is idempotent).
    pub fn purge_file(&self, project_root: &str, file_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM ast_nodes WHERE project_root = ? AND file_path = ?",
            params![project_root, file_path],
        )?;
        self.conn.execute(
            "DELETE FROM ast_edges WHERE project_root = ? AND file_path = ?",
            params![project_root, file_path],
        )?;
        Ok(())
    }

    /// Drop every AST row for `project_root`. Used by `crux index
    /// --force` so the subsequent walk rebuilds from scratch.
    pub fn purge_project(&self, project_root: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM ast_nodes WHERE project_root = ?",
            params![project_root],
        )?;
        self.conn.execute(
            "DELETE FROM ast_edges WHERE project_root = ?",
            params![project_root],
        )?;
        Ok(())
    }

    /// Persist a parse result. Caller is expected to call `purge_file`
    /// first when re-indexing a file.
    pub fn write(
        &self,
        project_root: &str,
        file_path: &str,
        language: &str,
        file_hash: &str,
        result: &ParseResult,
    ) -> Result<(usize, usize)> {
        let now = chrono::Utc::now().timestamp();
        let mut nodes = 0usize;
        let mut edges = 0usize;
        for n in &result.nodes {
            self.upsert_node(project_root, file_path, language, file_hash, n, now)?;
            nodes += 1;
        }
        for e in &result.edges {
            self.insert_edge(project_root, file_path, e, now)?;
            edges += 1;
        }
        Ok((nodes, edges))
    }

    fn upsert_node(
        &self,
        project_root: &str,
        file_path: &str,
        language: &str,
        file_hash: &str,
        n: &ParsedNode,
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO ast_nodes
                 (project_root, kind, name, qualified_name, file_path,
                  line_start, line_end, language, parent_qn, signature,
                  is_test, file_hash, extra, updated_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '{}', ?)
               ON CONFLICT(project_root, qualified_name) DO UPDATE SET
                 kind            = excluded.kind,
                 name            = excluded.name,
                 file_path       = excluded.file_path,
                 line_start      = excluded.line_start,
                 line_end        = excluded.line_end,
                 language        = excluded.language,
                 parent_qn       = excluded.parent_qn,
                 signature       = excluded.signature,
                 is_test         = excluded.is_test,
                 file_hash       = excluded.file_hash,
                 updated_at_epoch = excluded.updated_at_epoch"#,
            params![
                project_root,
                n.kind.as_str(),
                n.name,
                n.qualified_name,
                file_path,
                n.line_start,
                n.line_end,
                language,
                n.parent_qn,
                n.signature,
                if n.is_test { 1 } else { 0 },
                file_hash,
                now,
            ],
        )?;
        Ok(())
    }

    fn insert_edge(
        &self,
        project_root: &str,
        file_path: &str,
        e: &ParsedEdge,
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"INSERT INTO ast_edges
                 (project_root, kind, source_qn, target_qn, file_path, line,
                  confidence, confidence_tier, extra, updated_at_epoch)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, '{}', ?)"#,
            params![
                project_root,
                e.kind.as_str(),
                e.source_qn,
                e.target_qn,
                file_path,
                e.line,
                e.confidence,
                e.tier.as_str(),
                now,
            ],
        )?;
        Ok(())
    }

    // ─────────────────────────────────────────────────────────────────
    // Queries
    // ─────────────────────────────────────────────────────────────────

    /// Find symbols by exact `name` match. Use `find_symbol_like` for
    /// substring matching.
    pub fn find_symbol(
        &self,
        project_root: &str,
        name: &str,
        kind: Option<NodeKind>,
    ) -> Result<Vec<GraphNode>> {
        let kind_filter: Option<String> = kind.map(|k| k.as_str().to_string());
        let sql = match &kind_filter {
            Some(_) => {
                "SELECT id, project_root, kind, name, qualified_name, file_path,
                        line_start, line_end, language, parent_qn, signature, is_test
                 FROM ast_nodes
                 WHERE project_root = ? AND name = ? AND kind = ?
                 ORDER BY name LIMIT 50"
            }
            None => {
                "SELECT id, project_root, kind, name, qualified_name, file_path,
                        line_start, line_end, language, parent_qn, signature, is_test
                 FROM ast_nodes
                 WHERE project_root = ? AND name = ?
                 ORDER BY name LIMIT 50"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows: Vec<GraphNode> = if let Some(k) = &kind_filter {
            stmt.query_map(params![project_root, name, k], map_node)?
                .collect::<rusqlite::Result<_>>()?
        } else {
            stmt.query_map(params![project_root, name], map_node)?
                .collect::<rusqlite::Result<_>>()?
        };
        Ok(rows)
    }

    pub fn find_symbol_like(
        &self,
        project_root: &str,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<GraphNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_root, kind, name, qualified_name, file_path,
                    line_start, line_end, language, parent_qn, signature, is_test
             FROM ast_nodes
             WHERE project_root = ?
               AND (name LIKE ?2 OR qualified_name LIKE ?2)
             ORDER BY name LIMIT ?",
        )?;
        let glob = format!("%{}%", pattern);
        let rows = stmt
            .query_map(params![project_root, glob, limit as i64], map_node)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Return every node anchored at `file_path` ordered by
    /// `line_start`. Used by the L4+L5 outline-first auto-mode in
    /// `crux_read`: when a full-file read is requested for a file
    /// larger than `[layer.l4] outline_above_lines`, the dispatcher
    /// pulls the symbol list here and returns it instead of the body.
    /// `limit` caps the number of rows returned (the outline format
    /// already truncates display, but we cap at the source so the
    /// SQLite query stays cheap on huge generated files).
    pub fn list_symbols_in_file(
        &self,
        project_root: &str,
        file_path: &str,
        limit: usize,
    ) -> Result<Vec<GraphNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_root, kind, name, qualified_name, file_path,
                    line_start, line_end, language, parent_qn, signature, is_test
             FROM ast_nodes
             WHERE project_root = ? AND file_path = ?
             ORDER BY line_start, line_end LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![project_root, file_path, limit as i64], map_node)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Cheap `COUNT(*)` helper — pairs with [`Self::list_symbols_in_file`]
    /// when the caller capped `limit` and needs to know the true total
    /// (e.g. the outline header reports "250 symbols, showing 200").
    pub fn count_symbols_in_file(&self, project_root: &str, file_path: &str) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ast_nodes
             WHERE project_root = ? AND file_path = ?",
            params![project_root, file_path],
            |row| row.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    pub fn get_by_qn(&self, project_root: &str, qn: &str) -> Result<Option<GraphNode>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, project_root, kind, name, qualified_name, file_path,
                        line_start, line_end, language, parent_qn, signature, is_test
                 FROM ast_nodes
                 WHERE project_root = ? AND qualified_name = ?",
                params![project_root, qn],
                map_node,
            )
            .ok();
        Ok(row)
    }

    pub fn callers_of(&self, project_root: &str, qn: &str) -> Result<Vec<GraphNode>> {
        self.related(project_root, qn, EdgeKind::Calls, /*incoming=*/ true)
    }

    pub fn callees_of(&self, project_root: &str, qn: &str) -> Result<Vec<GraphNode>> {
        self.related(project_root, qn, EdgeKind::Calls, /*incoming=*/ false)
    }

    fn related(
        &self,
        project_root: &str,
        qn: &str,
        kind: EdgeKind,
        incoming: bool,
    ) -> Result<Vec<GraphNode>> {
        let join_field = if incoming { "source_qn" } else { "target_qn" };
        let filter_field = if incoming { "target_qn" } else { "source_qn" };
        // Calls edges store `target_qn` as the bare callee text emitted by
        // tree-sitter (e.g. `compute_delta`), since cross-module resolution
        // is not performed during extraction. To recover useful caller
        // results when the user passes a fully-qualified name, also match
        // on the leaf segment of `qn`.
        let leaf = qn.rsplit("::").next().unwrap_or(qn);
        // The join also accepts a bare-name match against `n.name` so the
        // returned `GraphNode` row resolves even when the edge stores only
        // the leaf identifier.
        let sql = format!(
            "SELECT n.id, n.project_root, n.kind, n.name, n.qualified_name, n.file_path,
                    n.line_start, n.line_end, n.language, n.parent_qn, n.signature, n.is_test
             FROM ast_edges e
             JOIN ast_nodes n
               ON (n.qualified_name = e.{join_field} OR n.name = e.{join_field})
              AND n.project_root = e.project_root
             WHERE e.project_root = ?
               AND (e.{filter_field} = ? OR e.{filter_field} = ?)
               AND e.kind = ?
             GROUP BY n.id
             LIMIT 200"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![project_root, qn, leaf, kind.as_str()], map_node)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    /// Conservative blast-radius BFS. Visits up to `max_nodes` callers
    /// up to `max_depth` hops away. Returns nodes other than the seed.
    pub fn impact_radius(
        &self,
        project_root: &str,
        qn: &str,
        max_depth: u32,
        max_nodes: u32,
    ) -> Result<Vec<GraphNode>> {
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut frontier: Vec<String> = vec![qn.to_string()];
        let mut out: Vec<GraphNode> = Vec::new();
        for _ in 0..max_depth {
            let mut next: Vec<String> = Vec::new();
            for current in frontier.drain(..) {
                if !visited.insert(current.clone()) {
                    continue;
                }
                let callers = self.callers_of(project_root, &current)?;
                for n in callers {
                    if out.len() as u32 >= max_nodes {
                        return Ok(out);
                    }
                    if !visited.contains(&n.qualified_name) {
                        next.push(n.qualified_name.clone());
                        out.push(n);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        Ok(out)
    }

    /// Project-wide symbol resolution pass for `CALLS` edges.
    ///
    /// For every edge whose `target_qn` is not already a known FQN in the
    /// project, the last segment (split on `::` or `.`) is looked up
    /// against the symbol table. When exactly one symbol matches, the
    /// edge is rewritten to that symbol's fully-qualified name; if the
    /// edge was `INFERRED`, it is upgraded to `RESOLVED`. Ambiguous
    /// matches are left alone so the leaf-name fallback in
    /// `Self::related` can still surface them.
    ///
    /// Returns the number of edges rewritten.
    pub fn resolve_cross_file_calls(&self, project_root: &str) -> Result<u64> {
        // Candidates: every CALLS edge whose `target_qn` is not already a
        // known project FQN. That catches both bare leafs (`compute_delta`)
        // and partially-qualified paths (`delta::compute_delta`) produced
        // by file-local resolution of imports.
        let candidates: Vec<(i64, String, f64, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT e.id, e.target_qn, e.confidence, e.confidence_tier, e.file_path
                 FROM ast_edges e
                 WHERE e.project_root = ?
                   AND e.kind = 'CALLS'
                   AND e.confidence_tier <> 'EXTRACTED'
                   AND NOT EXISTS (
                         SELECT 1 FROM ast_nodes n
                         WHERE n.project_root = e.project_root
                           AND n.qualified_name = e.target_qn
                   )",
            )?;
            let rows = stmt
                .query_map(params![project_root], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            rows
        };

        let mut rewritten = 0u64;
        let mut lookup = self.conn.prepare(
            "SELECT qualified_name FROM ast_nodes
             WHERE project_root = ? AND name = ?
               AND kind IN ('Function','Method','Class','Type','Constant')
             LIMIT 2",
        )?;
        let mut default_lookup = self.conn.prepare(
            "SELECT target_qn FROM ast_edges
             WHERE project_root = ? AND kind = 'EXPORTS_DEFAULT' AND source_qn = ?
             LIMIT 2",
        )?;
        // L5.13h: exact-FQN lookup for named-import path-mapping. Given
        // a candidate FQN like `src/x::foo`, confirm it refers to a real
        // callable / type / constant node in the project graph.
        let mut fqn_lookup = self.conn.prepare(
            "SELECT qualified_name FROM ast_nodes
             WHERE project_root = ? AND qualified_name = ?
               AND kind IN ('Function','Method','Class','Type','Constant')
             LIMIT 1",
        )?;
        let mut update = self.conn.prepare(
            "UPDATE ast_edges
             SET target_qn = ?, confidence = ?, confidence_tier = 'RESOLVED'
             WHERE id = ?",
        )?;

        // L5.13g: load tsconfig.json (or jsconfig.json) once per
        // resolution pass so non-relative module specs (`@/foo`,
        // `~components/Button`, …) can be mapped to project-relative
        // module paths via `compilerOptions.paths` / `baseUrl`. Best
        // effort: missing/unparseable configs leave `js_resolver = None`
        // and the rest of the pipeline behaves as before.
        let js_resolver = JsModuleResolver::load(std::path::Path::new(project_root));

        for (id, target, conf, tier, file_path) in candidates {
            // L5.13e + L5.13g: `import Foo from './x'; Foo()` and
            // `import Foo from '@/x'; Foo()` both land here as
            // `target_qn = "<spec>.default"`. Resolve the spec against
            // the calling file (relative) or the tsconfig path-mapping
            // (alias / baseUrl), then look up the unique
            // `EXPORTS_DEFAULT` edge.
            if let Some(modspec) = target.strip_suffix(".default") {
                let mod_qns =
                    js_resolve_module_candidates(&file_path, modspec, js_resolver.as_ref());
                let mut final_hit: Option<String> = None;
                'outer: for mod_qn in &mod_qns {
                    let candidates_qn = js_module_qn_candidates(mod_qn);
                    let mut hit: Option<String> = None;
                    let mut ambiguous = false;
                    for cand in &candidates_qn {
                        let source = format!("{cand}:file");
                        let matches: Vec<String> = default_lookup
                            .query_map(params![project_root, source], |r| r.get::<_, String>(0))?
                            .collect::<rusqlite::Result<_>>()?;
                        if matches.len() == 1 {
                            if hit.is_some() {
                                ambiguous = true;
                                break;
                            }
                            hit = Some(matches.into_iter().next().unwrap());
                        } else if matches.len() > 1 {
                            ambiguous = true;
                            break;
                        }
                    }
                    if !ambiguous {
                        if let Some(real) = hit {
                            final_hit = Some(real);
                            break 'outer;
                        }
                    }
                }
                if let Some(real) = final_hit {
                    if real != target {
                        let new_conf = if tier == "RESOLVED" {
                            conf
                        } else {
                            (conf + 0.2).min(1.0)
                        };
                        update.execute(params![real, new_conf, id])?;
                        rewritten += 1;
                        continue;
                    }
                }
            }

            // L5.13h: `import { foo } from '@/x'; foo()` and
            // `import { foo } from './x'; foo()` both land here as
            // `target_qn = "<modspec>.<leaf>"` after L5.5 file-local
            // resolution rewrote the bare leaf through the `imports`
            // map. Resolve the modspec against the calling file
            // (relative) or tsconfig path-mapping (alias / baseUrl),
            // then probe `<mod_qn>::<leaf>` + `<mod_qn>/index::<leaf>`
            // as a real FQN in the project graph. Skip `.default`
            // (handled above) and anything that doesn't look like a
            // JS/TS module spec (so Python `x.y.foo` and Rust
            // `client.send` leaf-fallback paths stay untouched).
            if let Some((modspec, leaf_name)) = split_js_modspec_leaf(&target) {
                let mod_qns =
                    js_resolve_module_candidates(&file_path, modspec, js_resolver.as_ref());
                if !mod_qns.is_empty() {
                    let mut final_hit: Option<String> = None;
                    let mut ambiguous = false;
                    'outer: for mod_qn in &mod_qns {
                        for cand in js_module_qn_candidates(mod_qn) {
                            let fqn = format!("{cand}::{leaf_name}");
                            let matches: Vec<String> = fqn_lookup
                                .query_map(params![project_root, &fqn], |r| r.get::<_, String>(0))?
                                .collect::<rusqlite::Result<_>>()?;
                            if let Some(real) = matches.into_iter().next() {
                                match &final_hit {
                                    Some(existing) if existing != &real => {
                                        ambiguous = true;
                                        break 'outer;
                                    }
                                    Some(_) => {}
                                    None => final_hit = Some(real),
                                }
                            }
                        }
                    }
                    if !ambiguous {
                        if let Some(real) = final_hit {
                            if real != target {
                                let new_conf = if tier == "RESOLVED" {
                                    conf
                                } else {
                                    (conf + 0.2).min(1.0)
                                };
                                update.execute(params![real, new_conf, id])?;
                                rewritten += 1;
                                continue;
                            }
                        }
                    }
                }
            }

            let leaf = leaf_segment(&target);
            if leaf.is_empty() {
                continue;
            }
            let matches: Vec<String> = lookup
                .query_map(params![project_root, leaf], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<_>>()?;
            if matches.len() == 1 && matches[0] != target {
                let new_conf = if tier == "RESOLVED" {
                    conf
                } else {
                    (conf + 0.2).min(1.0)
                };
                update.execute(params![matches[0], new_conf, id])?;
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    pub fn count_nodes(&self, project_root: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM ast_nodes WHERE project_root = ?",
            params![project_root],
            |r| r.get(0),
        )?)
    }

    pub fn count_edges(&self, project_root: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM ast_edges WHERE project_root = ?",
            params![project_root],
            |r| r.get(0),
        )?)
    }
}

/// Return the last segment of a (possibly) qualified name. Splits on
/// whichever of `::` or `.` appears last. Empty string ⇒ empty result.
fn leaf_segment(qn: &str) -> &str {
    let a = qn.rfind("::").map(|i| i + 2);
    let b = qn.rfind('.').map(|i| i + 1);
    let idx = match (a, b) {
        (Some(x), Some(y)) => x.max(y),
        (Some(x), None) => x,
        (None, Some(y)) => y,
        (None, None) => 0,
    };
    &qn[idx..]
}

/// L5.13e + L5.13g: collect every candidate module path a JS/TS
/// import specifier could resolve to, in priority order.
///
/// Relative specifiers (`./x`, `../foo`) resolve against the
/// importer's file path and produce a single-element vec. Non-relative
/// specifiers run through the optional [`JsModuleResolver`] which
/// consults `tsconfig.json` `compilerOptions.paths` / `baseUrl` and
/// can produce zero or more project-relative module paths. When no
/// resolver is loaded, npm-style specifiers stay unresolved (the
/// project graph has no edge into `node_modules`).
fn js_resolve_module_candidates(
    importer_file: &str,
    spec: &str,
    resolver: Option<&JsModuleResolver>,
) -> Vec<String> {
    if let Some(rel) = js_resolve_relative_module(importer_file, spec) {
        return vec![rel];
    }
    if let Some(r) = resolver {
        return r.resolve(spec);
    }
    Vec::new()
}

/// L5.13e: resolve a relative JS/TS module specifier (`./x`, `../foo/x`)
/// against the importer's file path, returning the project-relative
/// module path with any leading `./` collapsed and known extensions
/// stripped. Non-relative specifiers (`react`, `@scope/pkg`) return
/// `None` because the project graph has no edge into npm packages.
fn js_resolve_relative_module(importer_file: &str, spec: &str) -> Option<String> {
    if !(spec.starts_with("./") || spec.starts_with("../")) {
        return None;
    }
    let importer = std::path::Path::new(importer_file);
    let parent = importer
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    let joined = parent.join(spec);
    let normalized = normalize_path_components(&joined);
    let mut s = normalized.to_string_lossy().replace('\\', "/");
    // Strip a known JS/TS extension if the spec carried one explicitly.
    if let Some(idx) = s.rfind('.') {
        if matches!(&s[idx..], ".ts" | ".tsx" | ".js" | ".jsx" | ".mjs" | ".cjs") {
            s.truncate(idx);
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Candidate module QNs to probe for a resolved relative path. Tries
/// the path itself first, then the `<path>/index` form so
/// `./folder` (resolving to `crates/foo/folder`) also matches a file
/// laid out as `crates/foo/folder/index.ts`.
fn js_module_qn_candidates(mod_qn: &str) -> Vec<String> {
    let mut out = vec![mod_qn.to_string()];
    out.push(format!("{mod_qn}/index"));
    out
}

/// L5.13h: split a post-file-local-resolve `CALLS` target into a
/// `(modspec, leaf)` pair when it looks like a JS/TS named-import
/// path-mapping hit. Returns `None` for anything that looks like a
/// Rust qualified path (`::`), a Python dotted module (`a.b.foo`),
/// a bare method receiver (`client.send`), or a default-import
/// sentinel (handled by the caller's `.default` arm).
///
/// The modspec gate matches genuine JS/TS module specifiers only:
/// relative (`./`, `../`), alias-prefixed (`@`, `~`), or
/// package-path-like (contains `/`). Everything else is rejected so
/// non-JS languages and dotted receivers fall through to the
/// leaf-name fallback unchanged.
fn split_js_modspec_leaf(target: &str) -> Option<(&str, &str)> {
    if target.contains("::") {
        return None;
    }
    let (modspec, leaf) = target.rsplit_once('.')?;
    if modspec.is_empty() || leaf.is_empty() || leaf == "default" {
        return None;
    }
    if !(modspec.starts_with("./")
        || modspec.starts_with("../")
        || modspec.starts_with('@')
        || modspec.starts_with('~')
        || modspec.contains('/'))
    {
        return None;
    }
    Some((modspec, leaf))
}

fn normalize_path_components(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::{Component, PathBuf};
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn map_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let kind_s: String = row.get(2)?;
    let kind = kind_s.parse::<NodeKind>().map_err(|e: String| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })?;
    Ok(GraphNode {
        id: row.get(0)?,
        project_root: row.get(1)?,
        kind,
        name: row.get(3)?,
        qualified_name: row.get(4)?,
        file_path: row.get(5)?,
        line_start: row.get::<_, i64>(6)? as u32,
        line_end: row.get::<_, i64>(7)? as u32,
        language: row.get(8)?,
        parent_qn: row.get(9)?,
        signature: row.get(10)?,
        is_test: row.get::<_, i64>(11)? != 0,
    })
}

#[allow(dead_code)]
const _USES_CONFIDENCE: ConfidenceTier = ConfidenceTier::Extracted;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ParseResult, ParsedEdge, ParsedNode};

    fn write_calls_fixture(conn: &Connection) {
        let store = GraphStore::new(conn);
        let project_root = "/tmp/proj";
        // Caller `main` lives in module `app::main_mod`; callee `compute_delta`
        // lives in module `lib::delta`. The Calls edge stores only the bare
        // leaf because cross-module resolution is not performed.
        let result = ParseResult {
            nodes: vec![
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "main".to_string(),
                    qualified_name: "app::main_mod::main".to_string(),
                    line_start: 1,
                    line_end: 3,
                    parent_qn: Some("app::main_mod".to_string()),
                    signature: Some("fn main()".to_string()),
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "compute_delta".to_string(),
                    qualified_name: "lib::delta::compute_delta".to_string(),
                    line_start: 10,
                    line_end: 20,
                    parent_qn: Some("lib::delta".to_string()),
                    signature: Some("fn compute_delta()".to_string()),
                    is_test: false,
                },
            ],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "app::main_mod::main".to_string(),
                target_qn: "compute_delta".to_string(),
                line: 2,
                confidence: 0.6,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, "src/main.rs", "rust", "deadbeef", &result)
            .unwrap();
    }

    #[test]
    fn callers_of_resolves_via_leaf_fallback() {
        let conn = crux_core::db::open_in_memory().unwrap();
        write_calls_fixture(&conn);
        let store = GraphStore::new(&conn);
        // Pass the FULLY-QUALIFIED name of the callee. The edge only stored
        // the leaf, so without the fallback this returns 0 rows.
        let callers = store
            .callers_of("/tmp/proj", "lib::delta::compute_delta")
            .unwrap();
        assert_eq!(
            callers.len(),
            1,
            "expected the leaf-name fallback to find the caller"
        );
        assert_eq!(callers[0].qualified_name, "app::main_mod::main");
    }

    #[test]
    fn impact_radius_walks_through_leaf_fallback() {
        let conn = crux_core::db::open_in_memory().unwrap();
        write_calls_fixture(&conn);
        let store = GraphStore::new(&conn);
        let radius = store
            .impact_radius("/tmp/proj", "lib::delta::compute_delta", 3, 50)
            .unwrap();
        assert!(radius.iter().any(|n| n.name == "main"));
    }

    #[test]
    fn resolve_cross_file_calls_upgrades_unique_match() {
        let conn = crux_core::db::open_in_memory().unwrap();
        write_calls_fixture(&conn);
        let store = GraphStore::new(&conn);

        // Before: edge stores bare `compute_delta`, tier INFERRED.
        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "compute_delta");
        assert_eq!(tier, "INFERRED");

        let rewritten = store.resolve_cross_file_calls("/tmp/proj").unwrap();
        assert_eq!(rewritten, 1);

        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "lib::delta::compute_delta");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn resolve_cross_file_calls_upgrades_partial_path() {
        // Edge target `delta::compute_delta` is partially qualified (the
        // file-local resolver produced it from a use import). Cross-file
        // resolution should promote it to the full FQN.
        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        let project_root = "/tmp/partial";
        let result = ParseResult {
            nodes: vec![
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "caller".to_string(),
                    qualified_name: "app::caller".to_string(),
                    line_start: 1,
                    line_end: 2,
                    parent_qn: None,
                    signature: None,
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "compute_delta".to_string(),
                    qualified_name: "crate::lib::delta::compute_delta".to_string(),
                    line_start: 10,
                    line_end: 12,
                    parent_qn: None,
                    signature: None,
                    is_test: false,
                },
            ],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "app::caller".to_string(),
                target_qn: "delta::compute_delta".to_string(),
                line: 1,
                confidence: 0.8,
                tier: ConfidenceTier::Resolved,
            }],
        };
        store
            .write(project_root, "src/app.rs", "rust", "h", &result)
            .unwrap();
        let rewritten = store.resolve_cross_file_calls(project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "crate::lib::delta::compute_delta");
    }

    #[test]
    fn resolve_cross_file_calls_skips_ambiguous() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        let project_root = "/tmp/ambig";
        // Two defs share the bare name `helper`.
        let result = ParseResult {
            nodes: vec![
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "caller".to_string(),
                    qualified_name: "a::caller".to_string(),
                    line_start: 1,
                    line_end: 2,
                    parent_qn: None,
                    signature: None,
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "helper".to_string(),
                    qualified_name: "b::helper".to_string(),
                    line_start: 1,
                    line_end: 2,
                    parent_qn: None,
                    signature: None,
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "helper".to_string(),
                    qualified_name: "c::helper".to_string(),
                    line_start: 1,
                    line_end: 2,
                    parent_qn: None,
                    signature: None,
                    is_test: false,
                },
            ],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "a::caller".to_string(),
                target_qn: "helper".to_string(),
                line: 1,
                confidence: 0.6,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, "src/a.rs", "rust", "h", &result)
            .unwrap();

        let rewritten = store.resolve_cross_file_calls(project_root).unwrap();
        assert_eq!(rewritten, 0, "ambiguous name should not be rewritten");
        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "helper");
        assert_eq!(tier, "INFERRED");
    }

    // ─── L5.13e: default-export aliasing ─────────────────────────────────

    fn write_default_export_fixture(conn: &Connection, project_root: &str) {
        // `src/x.ts`: exports `bar` as the default.
        let store = GraphStore::new(conn);
        let exporter = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "bar".to_string(),
                qualified_name: "src/x::bar".to_string(),
                line_start: 1,
                line_end: 1,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::ExportsDefault,
                source_qn: "src/x:file".to_string(),
                target_qn: "src/x::bar".to_string(),
                line: 1,
                confidence: 0.9,
                tier: ConfidenceTier::Extracted,
            }],
        };
        store
            .write(project_root, "src/x.ts", "typescript", "h", &exporter)
            .unwrap();

        // `src/y.ts`: imports the default and calls it.
        let importer = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "main".to_string(),
                qualified_name: "src/y::main".to_string(),
                line_start: 1,
                line_end: 3,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                // Mimics what the file-local resolver leaves behind
                // after `import Foo from './x'; Foo();`.
                source_qn: "src/y::main".to_string(),
                target_qn: "./x.default".to_string(),
                line: 2,
                confidence: 0.7,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, "src/y.ts", "typescript", "h", &importer)
            .unwrap();
    }

    #[test]
    fn resolve_cross_file_calls_upgrades_default_import_to_named_decl() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let project_root = "/tmp/default-named";
        write_default_export_fixture(&conn, project_root);
        let store = GraphStore::new(&conn);

        let rewritten = store.resolve_cross_file_calls(project_root).unwrap();
        assert_eq!(rewritten, 1);
        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/x::bar");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn resolve_cross_file_calls_default_import_via_index_file() {
        // `src/folder/index.ts` exports `helper` as default; `src/y.ts`
        // does `import H from './folder'`. The relative spec resolves
        // to `src/folder` which doesn't exist as a module, so the
        // resolver must fall back to `src/folder/index`.
        let conn = crux_core::db::open_in_memory().unwrap();
        let project_root = "/tmp/default-index";
        let store = GraphStore::new(&conn);

        let exporter = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "helper".to_string(),
                qualified_name: "src/folder/index::helper".to_string(),
                line_start: 1,
                line_end: 1,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::ExportsDefault,
                source_qn: "src/folder/index:file".to_string(),
                target_qn: "src/folder/index::helper".to_string(),
                line: 1,
                confidence: 0.9,
                tier: ConfidenceTier::Extracted,
            }],
        };
        store
            .write(
                project_root,
                "src/folder/index.ts",
                "typescript",
                "h",
                &exporter,
            )
            .unwrap();

        let importer = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "main".to_string(),
                qualified_name: "src/y::main".to_string(),
                line_start: 1,
                line_end: 3,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "src/y::main".to_string(),
                target_qn: "./folder.default".to_string(),
                line: 2,
                confidence: 0.7,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, "src/y.ts", "typescript", "h", &importer)
            .unwrap();

        let rewritten = store.resolve_cross_file_calls(project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "src/folder/index::helper");
    }

    #[test]
    fn resolve_cross_file_calls_skips_default_import_for_unknown_module() {
        // No exporter file → the `./missing.default` target stays put.
        let conn = crux_core::db::open_in_memory().unwrap();
        let project_root = "/tmp/default-missing";
        let store = GraphStore::new(&conn);
        let importer = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "main".to_string(),
                qualified_name: "src/y::main".to_string(),
                line_start: 1,
                line_end: 3,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "src/y::main".to_string(),
                target_qn: "./missing.default".to_string(),
                line: 2,
                confidence: 0.7,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, "src/y.ts", "typescript", "h", &importer)
            .unwrap();
        let rewritten = store.resolve_cross_file_calls(project_root).unwrap();
        assert_eq!(rewritten, 0);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "./missing.default");
    }

    #[test]
    fn js_resolve_relative_module_handles_dot_and_dotdot() {
        assert_eq!(
            js_resolve_relative_module("src/y.ts", "./x"),
            Some("src/x".to_string())
        );
        assert_eq!(
            js_resolve_relative_module("src/sub/y.ts", "../x"),
            Some("src/x".to_string())
        );
        assert_eq!(
            js_resolve_relative_module("src/y.ts", "./x.ts"),
            Some("src/x".to_string()),
            "explicit extension stripped"
        );
        assert_eq!(
            js_resolve_relative_module("src/y.ts", "react"),
            None,
            "bare specifier rejected"
        );
        assert_eq!(
            js_resolve_relative_module("src/y.ts", "@scope/pkg"),
            None,
            "scoped package rejected"
        );
    }

    // ─── L5.13g: tsconfig path-mapping ───────────────────────────────────

    fn write_default_export_with_module(
        store: &GraphStore,
        project_root: &str,
        module_qn: &str,
        file_path: &str,
        symbol: &str,
    ) {
        let exporter = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: symbol.to_string(),
                qualified_name: format!("{module_qn}::{symbol}"),
                line_start: 1,
                line_end: 1,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::ExportsDefault,
                source_qn: format!("{module_qn}:file"),
                target_qn: format!("{module_qn}::{symbol}"),
                line: 1,
                confidence: 0.9,
                tier: ConfidenceTier::Extracted,
            }],
        };
        store
            .write(project_root, file_path, "typescript", "h", &exporter)
            .unwrap();
    }

    fn write_default_call_edge(
        store: &GraphStore,
        project_root: &str,
        importer_module: &str,
        importer_file: &str,
        spec: &str,
    ) {
        let importer = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "main".to_string(),
                qualified_name: format!("{importer_module}::main"),
                line_start: 1,
                line_end: 3,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: format!("{importer_module}::main"),
                target_qn: format!("{spec}.default"),
                line: 2,
                confidence: 0.7,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, importer_file, "typescript", "h", &importer)
            .unwrap();
    }

    #[test]
    fn resolve_cross_file_calls_default_import_via_alias() {
        // `tsconfig.json` declares `"@/*": ["src/*"]`. An importer doing
        // `import Foo from '@/x'` should land on `src/x:file`'s default
        // export the same way `./x` does.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_default_export_with_module(&store, &project_root, "src/x", "src/x.ts", "bar");
        write_default_call_edge(&store, &project_root, "src/y", "src/y.ts", "@/x");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/x::bar");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn resolve_cross_file_calls_default_import_via_baseurl() {
        // `baseUrl: "src"` with no `paths` — bare `utils/x` resolves to
        // `src/utils/x:file`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
        )
        .unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_default_export_with_module(
            &store,
            &project_root,
            "src/utils/x",
            "src/utils/x.ts",
            "bar",
        );
        write_default_call_edge(&store, &project_root, "src/y", "src/y.ts", "utils/x");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "src/utils/x::bar");
    }

    #[test]
    fn resolve_cross_file_calls_alias_falls_back_to_index_file() {
        // `@/folder` aliases `src/folder` which itself doesn't have a
        // module — only `src/folder/index.ts`. The `<path>/index`
        // candidate must still be probed once the alias has been
        // applied.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_default_export_with_module(
            &store,
            &project_root,
            "src/folder/index",
            "src/folder/index.ts",
            "helper",
        );
        write_default_call_edge(&store, &project_root, "src/y", "src/y.ts", "@/folder");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "src/folder/index::helper");
    }

    #[test]
    fn resolve_cross_file_calls_alias_priority_picks_first_existing_target() {
        // Multi-target `paths` — first existing module wins; the
        // second is only probed when the first yields nothing.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "~/*": ["primary/*", "fallback/*"] }
              }
            }"#,
        )
        .unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        // Only the fallback module exists.
        write_default_export_with_module(&store, &project_root, "fallback/x", "fallback/x.ts", "f");
        write_default_call_edge(&store, &project_root, "src/y", "src/y.ts", "~/x");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "fallback/x::f");
    }

    #[test]
    fn resolve_cross_file_calls_no_tsconfig_leaves_alias_unresolved() {
        // Without a tsconfig, `@/x.default` stays untouched — the
        // pre-L5.13g behaviour.
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_default_export_with_module(&store, &project_root, "src/x", "src/x.ts", "bar");
        write_default_call_edge(&store, &project_root, "src/y", "src/y.ts", "@/x");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 0);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "@/x.default");
    }

    #[test]
    fn js_resolve_module_candidates_prefers_relative_then_alias() {
        // Relative spec short-circuits the resolver entirely.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let resolver = JsModuleResolver::load(dir.path());
        assert!(resolver.is_some());

        let cands = js_resolve_module_candidates("src/y.ts", "./x", resolver.as_ref());
        assert_eq!(cands, vec!["src/x".to_string()]);

        let cands = js_resolve_module_candidates("src/y.ts", "@/x", resolver.as_ref());
        assert_eq!(cands, vec!["src/x".to_string()]);

        // No resolver → npm-style specifiers stay unresolved.
        let cands = js_resolve_module_candidates("src/y.ts", "@/x", None);
        assert!(cands.is_empty());
    }

    // ─── L5.13h: named-import path-mapping ───────────────────────────────

    #[test]
    fn split_js_modspec_leaf_accepts_js_shapes() {
        assert_eq!(split_js_modspec_leaf("@/x.foo"), Some(("@/x", "foo")));
        assert_eq!(split_js_modspec_leaf("./x.foo"), Some(("./x", "foo")));
        assert_eq!(
            split_js_modspec_leaf("../foo/bar.baz"),
            Some(("../foo/bar", "baz"))
        );
        assert_eq!(
            split_js_modspec_leaf("~components/Button.click"),
            Some(("~components/Button", "click"))
        );
        assert_eq!(
            split_js_modspec_leaf("my-lib/sub.bar"),
            Some(("my-lib/sub", "bar"))
        );
    }

    #[test]
    fn split_js_modspec_leaf_rejects_non_js_shapes() {
        // Rust qualified path.
        assert_eq!(split_js_modspec_leaf("foo::bar::baz"), None);
        // Python dotted module (no slash/alias).
        assert_eq!(split_js_modspec_leaf("x.y.foo"), None);
        // Rust receiver method-call leftover.
        assert_eq!(split_js_modspec_leaf("client.send"), None);
        // Default-import sentinel — handled by the `.default` arm.
        assert_eq!(split_js_modspec_leaf("@/x.default"), None);
        // Empty shapes.
        assert_eq!(split_js_modspec_leaf(""), None);
        assert_eq!(split_js_modspec_leaf(".foo"), None);
        assert_eq!(split_js_modspec_leaf("@/x."), None);
        // Leaf with no separator.
        assert_eq!(split_js_modspec_leaf("foo"), None);
    }

    /// Write a JS/TS module that exports `symbol` as a named function,
    /// anchored at `module_qn` / `file_path`.
    fn write_named_export_module(
        store: &GraphStore,
        project_root: &str,
        module_qn: &str,
        file_path: &str,
        symbol: &str,
    ) {
        let exporter = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: symbol.to_string(),
                qualified_name: format!("{module_qn}::{symbol}"),
                line_start: 1,
                line_end: 1,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![],
        };
        store
            .write(project_root, file_path, "typescript", "h", &exporter)
            .unwrap();
    }

    /// Write an importer whose single `CALLS` edge mimics what the
    /// file-local resolver leaves after `import { <leaf> } from '<spec>'`
    /// promotes the bare leaf through the `imports` map.
    fn write_named_call_edge(
        store: &GraphStore,
        project_root: &str,
        importer_module: &str,
        importer_file: &str,
        spec: &str,
        leaf: &str,
    ) {
        let importer = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Function,
                name: "main".to_string(),
                qualified_name: format!("{importer_module}::main"),
                line_start: 1,
                line_end: 3,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: format!("{importer_module}::main"),
                target_qn: format!("{spec}.{leaf}"),
                line: 2,
                confidence: 0.7,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project_root, importer_file, "typescript", "h", &importer)
            .unwrap();
    }

    #[test]
    fn resolve_cross_file_calls_named_import_relative() {
        // `import { bar } from './x'; bar()` resolves to `src/x::bar`
        // even though the project has no tsconfig.
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_named_export_module(&store, &project_root, "src/x", "src/x.ts", "bar");
        write_named_call_edge(&store, &project_root, "src/y", "src/y.ts", "./x", "bar");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let (target, tier): (String, String) = conn
            .query_row(
                "SELECT target_qn, confidence_tier FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(target, "src/x::bar");
        assert_eq!(tier, "RESOLVED");
    }

    #[test]
    fn resolve_cross_file_calls_named_import_via_tsconfig_alias() {
        // `import { bar } from '@/x'; bar()` resolves via tsconfig
        // `paths: { "@/*": ["src/*"] }` to `src/x::bar`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tsconfig.json"),
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )
        .unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_named_export_module(&store, &project_root, "src/x", "src/x.ts", "bar");
        write_named_call_edge(&store, &project_root, "src/y", "src/y.ts", "@/x", "bar");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "src/x::bar");
    }

    #[test]
    fn resolve_cross_file_calls_named_import_via_index_file() {
        // `import { helper } from './folder'` should fall back to
        // `./folder/index::helper` when no `./folder` module exists.
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_named_export_module(
            &store,
            &project_root,
            "src/folder/index",
            "src/folder/index.ts",
            "helper",
        );
        write_named_call_edge(
            &store,
            &project_root,
            "src/y",
            "src/y.ts",
            "./folder",
            "helper",
        );

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "src/folder/index::helper");
    }

    #[test]
    fn resolve_cross_file_calls_named_import_disambiguates_by_modspec() {
        // Two functions in the project share the bare name `foo`. The
        // leaf-name fallback would be ambiguous; the modspec arm must
        // disambiguate by consulting the `./x` spec → `src/x`.
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_named_export_module(&store, &project_root, "src/x", "src/x.ts", "foo");
        // Ambiguator: a different module also exposes `foo`.
        write_named_export_module(&store, &project_root, "src/other", "src/other.ts", "foo");
        write_named_call_edge(&store, &project_root, "src/y", "src/y.ts", "./x", "foo");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 1);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS' AND source_qn='src/y::main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "src/x::foo");
    }

    #[test]
    fn resolve_cross_file_calls_named_import_skips_when_module_missing() {
        // No exporter for `./missing.bar` → target stays untouched so
        // downstream consumers can still see the unresolved leaf.
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_named_call_edge(
            &store,
            &project_root,
            "src/y",
            "src/y.ts",
            "./missing",
            "bar",
        );

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 0);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "./missing.bar");
    }

    #[test]
    fn resolve_cross_file_calls_named_import_no_tsconfig_leaves_alias_unresolved() {
        // Without a tsconfig, `@/x.bar` can't be mapped — the edge
        // falls through to the leaf-name fallback (which picks the
        // unique `bar` def if one exists; otherwise stays put).
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        // Two `bar`s so the leaf-name fallback also gives up.
        write_named_export_module(&store, &project_root, "src/x", "src/x.ts", "bar");
        write_named_export_module(&store, &project_root, "src/other", "src/other.ts", "bar");
        write_named_call_edge(&store, &project_root, "src/y", "src/y.ts", "@/x", "bar");

        let rewritten = store.resolve_cross_file_calls(&project_root).unwrap();
        assert_eq!(rewritten, 0);
        let target: String = conn
            .query_row(
                "SELECT target_qn FROM ast_edges WHERE kind='CALLS' AND source_qn='src/y::main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(target, "@/x.bar");
    }
}
