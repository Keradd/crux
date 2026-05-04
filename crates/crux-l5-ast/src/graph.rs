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
        kind: Option<NodeKind>,
        limit: usize,
    ) -> Result<Vec<GraphNode>> {
        let kind_filter: Option<String> = kind.map(|k| k.as_str().to_string());
        let sql = match &kind_filter {
            Some(_) => {
                "SELECT id, project_root, kind, name, qualified_name, file_path,
                        line_start, line_end, language, parent_qn, signature, is_test
                 FROM ast_nodes
                 WHERE project_root = ?1
                   AND (name LIKE ?2 OR qualified_name LIKE ?2)
                   AND kind = ?3
                 ORDER BY name LIMIT ?4"
            }
            None => {
                "SELECT id, project_root, kind, name, qualified_name, file_path,
                        line_start, line_end, language, parent_qn, signature, is_test
                 FROM ast_nodes
                 WHERE project_root = ?1
                   AND (name LIKE ?2 OR qualified_name LIKE ?2)
                 ORDER BY name LIMIT ?3"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let glob = format!("%{}%", pattern);
        let rows: Vec<GraphNode> = if let Some(k) = &kind_filter {
            stmt.query_map(params![project_root, glob, k, limit as i64], map_node)?
                .collect::<rusqlite::Result<_>>()?
        } else {
            stmt.query_map(params![project_root, glob, limit as i64], map_node)?
                .collect::<rusqlite::Result<_>>()?
        };
        Ok(rows)
    }

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
        let is_type_like = match self.get_by_qn(project_root, qn)? {
            Some(n) => matches!(n.kind, NodeKind::Class | NodeKind::Type),
            None => false,
        };
        if is_type_like {
            self.callers_of_type(project_root, qn)
        } else {
            self.related(project_root, qn, EdgeKind::Calls, /*incoming=*/ true)
        }
    }

    pub fn callees_of(&self, project_root: &str, qn: &str) -> Result<Vec<GraphNode>> {
        self.related(project_root, qn, EdgeKind::Calls, /*incoming=*/ false)
    }

    fn callers_of_type(&self, project_root: &str, qn: &str) -> Result<Vec<GraphNode>> {
        let leaf = qn.rsplit("::").next().unwrap_or(qn);
        let method_prefix_full = format!("{qn}::%");
        let method_prefix_leaf = format!("{leaf}::%");
        let sql = "SELECT n.id, n.project_root, n.kind, n.name, n.qualified_name, n.file_path,
                          n.line_start, n.line_end, n.language, n.parent_qn, n.signature, n.is_test
                   FROM ast_edges e
                   JOIN ast_nodes n
                     ON (n.qualified_name = e.source_qn OR n.name = e.source_qn)
                    AND n.project_root = e.project_root
                   WHERE e.project_root = ?1
                     AND e.kind = 'CALLS'
                     AND (e.target_qn = ?2
                          OR e.target_qn = ?3
                          OR e.target_qn LIKE ?4
                          OR e.target_qn LIKE ?5)
                   GROUP BY n.id
                   LIMIT 200";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map(
                params![
                    project_root,
                    qn,
                    leaf,
                    method_prefix_full,
                    method_prefix_leaf
                ],
                map_node,
            )?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
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
        let leaf = qn.rsplit("::").next().unwrap_or(qn);
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

    pub fn resolve_cross_file_calls(&self, project_root: &str) -> Result<u64> {
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

        let js_resolver = JsModuleResolver::load(std::path::Path::new(project_root));

        for (id, target, conf, tier, file_path) in candidates {
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

fn js_module_qn_candidates(mod_qn: &str) -> Vec<String> {
    let mut out = vec![mod_qn.to_string()];
    out.push(format!("{mod_qn}/index"));
    out
}

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
    fn callers_of_type_catches_method_calls_on_struct() {
        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        let project_root = "/tmp/type-proj";
        let result = ParseResult {
            nodes: vec![
                ParsedNode {
                    kind: NodeKind::Type,
                    name: "LayerToggles".to_string(),
                    qualified_name: "core::config::LayerToggles".to_string(),
                    line_start: 1,
                    line_end: 5,
                    parent_qn: Some("core::config".to_string()),
                    signature: Some("struct LayerToggles".to_string()),
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "dispatch".to_string(),
                    qualified_name: "mcp::dispatch::dispatch".to_string(),
                    line_start: 10,
                    line_end: 20,
                    parent_qn: Some("mcp::dispatch".to_string()),
                    signature: Some("fn dispatch()".to_string()),
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "audit".to_string(),
                    qualified_name: "cli::audit::audit".to_string(),
                    line_start: 30,
                    line_end: 40,
                    parent_qn: Some("cli::audit".to_string()),
                    signature: Some("fn audit()".to_string()),
                    is_test: false,
                },
            ],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::Calls,
                    source_qn: "mcp::dispatch::dispatch".to_string(),
                    target_qn: "core::config::LayerToggles::default".to_string(),
                    line: 12,
                    confidence: 0.9,
                    tier: ConfidenceTier::Resolved,
                },
                ParsedEdge {
                    kind: EdgeKind::Calls,
                    source_qn: "cli::audit::audit".to_string(),
                    target_qn: "LayerToggles::new".to_string(),
                    line: 33,
                    confidence: 0.6,
                    tier: ConfidenceTier::Inferred,
                },
            ],
        };
        store
            .write(project_root, "src/config.rs", "rust", "cafebabe", &result)
            .unwrap();

        let callers = store
            .callers_of(project_root, "core::config::LayerToggles")
            .unwrap();
        let names: Vec<_> = callers.iter().map(|n| n.name.as_str()).collect();
        assert!(
            names.contains(&"dispatch"),
            "fully-qualified Type::method caller must resolve; got {names:?}"
        );
        assert!(
            names.contains(&"audit"),
            "leaf-prefixed Type::method caller must resolve; got {names:?}"
        );

        let radius = store
            .impact_radius(project_root, "core::config::LayerToggles", 2, 50)
            .unwrap();
        assert!(
            radius.iter().any(|n| n.name == "dispatch"),
            "impact radius must surface type field users via method calls"
        );
    }

    #[test]
    fn resolve_cross_file_calls_upgrades_unique_match() {
        let conn = crux_core::db::open_in_memory().unwrap();
        write_calls_fixture(&conn);
        let store = GraphStore::new(&conn);

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

    fn write_default_export_fixture(conn: &Connection, project_root: &str) {
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

        let cands = js_resolve_module_candidates("src/y.ts", "@/x", None);
        assert!(cands.is_empty());
    }

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
        assert_eq!(split_js_modspec_leaf("foo::bar::baz"), None);
        assert_eq!(split_js_modspec_leaf("x.y.foo"), None);
        assert_eq!(split_js_modspec_leaf("client.send"), None);
        assert_eq!(split_js_modspec_leaf("@/x.default"), None);
        assert_eq!(split_js_modspec_leaf(""), None);
        assert_eq!(split_js_modspec_leaf(".foo"), None);
        assert_eq!(split_js_modspec_leaf("@/x."), None);
        assert_eq!(split_js_modspec_leaf("foo"), None);
    }

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
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
        write_named_export_module(&store, &project_root, "src/x", "src/x.ts", "foo");
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
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_string_lossy().to_string();

        let conn = crux_core::db::open_in_memory().unwrap();
        let store = GraphStore::new(&conn);
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
