//! L5.5 — symbol resolver.
//!
//! Raw extraction stores `target_qn` on `CALLS` edges as the bare callee
//! text emitted by tree-sitter (e.g. `compute_delta`). That leaves
//! downstream queries relying on a leaf-name fallback in [`crate::graph`]
//! which produces noise when multiple symbols share a short name.
//!
//! This module tightens precision in two passes:
//!
//! 1. **File-local** (`resolve_file_calls`) — runs at the end of
//!    [`crate::extract::parse`]. Builds a `leaf → FQN` table from the
//!    definitions seen in the file and from `use` / `import`
//!    statements, then rewrites each bare-name `CALLS` edge to the
//!    resolved FQN. Matches are only applied when unambiguous.
//!
//! 2. **Project-wide** ([`crate::graph::GraphStore::resolve_cross_file_calls`])
//!    — runs after the indexer has persisted every file. Upgrades any
//!    remaining bare-name edges whose target is unique across the
//!    project.
//!
//! The existing leaf-name fallback in `GraphStore::related` stays as a
//! safety net for cases neither pass can resolve.

use std::collections::HashMap;

use crate::types::{ConfidenceTier, EdgeKind, NodeKind, ParseResult};

/// Rewrite file-local `CALLS` edges whose `target_qn` is a bare leaf
/// name to the unique fully-qualified name of the matching local
/// definition or imported item. Ambiguous or unmatched targets are
/// left untouched.
pub(crate) fn resolve_file_calls(result: &mut ParseResult) {
    // Collect callable defs by bare name.
    let mut symbol_table: HashMap<String, Vec<String>> = HashMap::new();
    for n in &result.nodes {
        if matches!(
            n.kind,
            NodeKind::Function
                | NodeKind::Method
                | NodeKind::Class
                | NodeKind::Type
                | NodeKind::Constant
        ) {
            symbol_table
                .entry(n.name.clone())
                .or_default()
                .push(n.qualified_name.clone());
        }
    }

    // Collect imported symbols: leaf → full path.
    let mut imports: HashMap<String, String> = HashMap::new();
    // L5.13i: collect JS/TS namespace aliases (`import * as ns from
    // '<spec>'`) separately so we can distinguish them from named /
    // default imports that share the `imports` map — namespace values
    // are pure module specs, while named/default values carry an
    // appended `.<item>` segment.
    let mut namespace_imports: HashMap<String, String> = HashMap::new();
    for e in &result.edges {
        if matches!(e.kind, EdgeKind::ImportsFrom) {
            for (leaf, full) in parse_import_target(&e.target_qn) {
                imports.entry(leaf).or_insert(full);
            }
            if let Some((alias, module)) = parse_js_namespace_alias(&e.target_qn) {
                namespace_imports.entry(alias).or_insert(module);
            }
        }
    }

    for e in result.edges.iter_mut() {
        // L5.13e: `export default Foo` lands here as an ExportsDefault
        // edge with `target_qn = "Foo"`. Promote the bare identifier to
        // the local FQN so cross-file consumers can match it directly.
        if matches!(e.kind, EdgeKind::ExportsDefault)
            && !e.target_qn.contains("::")
            && !e.target_qn.contains('.')
        {
            if let Some(cands) = symbol_table.get(&e.target_qn) {
                if cands.len() == 1 {
                    e.target_qn = cands[0].clone();
                }
            }
            continue;
        }

        if !matches!(e.kind, EdgeKind::Calls) {
            continue;
        }

        // 1. Bare leaf: resolve against local defs, then imports.
        if !e.target_qn.contains("::") && !e.target_qn.contains('.') {
            let leaf = e.target_qn.clone();
            if let Some(cands) = symbol_table.get(&leaf) {
                if cands.len() == 1 {
                    e.target_qn = cands[0].clone();
                    e.tier = ConfidenceTier::Resolved;
                    e.confidence = (e.confidence + 0.2).min(1.0);
                    continue;
                }
            }
            if let Some(full) = imports.get(&leaf) {
                e.target_qn = full.clone();
                e.tier = ConfidenceTier::Resolved;
                e.confidence = (e.confidence + 0.15).min(1.0);
            }
            continue;
        }

        // 2. `Head::method` (single level): promote `Head` when it matches
        // an import or a unique local definition. Catches the output of
        // the Rust receiver-typing pass (`Foo::bar`, `Self::bar`) and
        // bare scoped calls like `SomeType::new()`.
        if !e.target_qn.contains('.') {
            if let Some((head, tail)) = e.target_qn.split_once("::") {
                // Keep Rust path keywords untouched — they're already
                // meaningful and further rewriting would corrupt them.
                if tail.contains("::") || head.is_empty() || is_rust_path_keyword(head) {
                    continue;
                }
                if let Some(full_head) = imports.get(head) {
                    e.target_qn = format!("{full_head}::{tail}");
                    e.tier = ConfidenceTier::Resolved;
                    e.confidence = (e.confidence + 0.15).min(1.0);
                    continue;
                }
                if let Some(cands) = symbol_table.get(head) {
                    if cands.len() == 1 {
                        e.target_qn = format!("{}::{tail}", cands[0]);
                        e.tier = ConfidenceTier::Resolved;
                        e.confidence = (e.confidence + 0.1).min(1.0);
                    }
                }
            }
            continue;
        }

        // 3. L5.13i: `import * as ns from '@/x'; ns.foo()` leaves
        // CALLS target as `ns.foo`. Rewrite the namespace head to
        // the imported module path so the cross-file L5.13h arm can
        // resolve it the same way named imports do (via
        // `split_js_modspec_leaf` + `js_resolve_module_candidates`).
        // Single-level member access only; chained `ns.sub.foo`
        // falls through unchanged.
        if let Some((head, tail)) = e.target_qn.split_once('.') {
            if !head.is_empty() && !tail.is_empty() && !tail.contains('.') {
                if let Some(module) = namespace_imports.get(head) {
                    e.target_qn = format!("{module}.{tail}");
                    // Tier / confidence left untouched — cross-file
                    // pass bumps it when the symbol resolves.
                }
            }
        }
    }
}

fn is_rust_path_keyword(s: &str) -> bool {
    matches!(s, "crate" | "self" | "super" | "Self")
}

/// Parse a raw `ImportsFrom` edge target into `(leaf_name, full_path)`
/// pairs. Handles Rust `use` paths (already stripped of the `use ` prefix
/// by the extractor) plus Python and JS/TS `import` statements which are
/// stored verbatim.
///
/// Examples:
///   "foo::bar::Baz"              → [("Baz","foo::bar::Baz")]
///   "foo::{A, B as C}"           → [("A","foo::A"),("C","foo::B")]
///   "foo::bar as alias"          → [("alias","foo::bar")]
///   "import foo"                 → [("foo","foo")]
///   "import foo.bar as baz"      → [("baz","foo.bar")]
///   "from x.y import a, b as c"  → [("a","x.y.a"),("c","x.y.b")]
///   "import { a, b as c } from 'x'" → [("a","x.a"),("c","x.b")]
///   "import def from 'x'"        → [("def","x.default")]
///   "import * as ns from 'x'"    → [("ns","x")]
pub(crate) fn parse_import_target(raw: &str) -> Vec<(String, String)> {
    let t = raw.trim().trim_end_matches(';').trim();

    // Python: `from X import a, b as c`
    if let Some(rest) = t.strip_prefix("from ") {
        if let Some((module, items)) = rest.split_once(" import ") {
            let module = module.trim();
            return split_python_import_list(items)
                .into_iter()
                .map(|(leaf, orig)| (leaf, format!("{module}.{orig}")))
                .collect();
        }
    }

    // JS/TS: `import ... from 'mod'`
    if t.starts_with("import ") && (t.contains(" from ") || t.contains('\'') || t.contains('"')) {
        return parse_js_import(t.trim_start_matches("import ").trim());
    }

    // Python: `import X [as Y][, ...]`
    if let Some(rest) = t.strip_prefix("import ") {
        return rest
            .split(',')
            .filter_map(|part| {
                let part = part.trim();
                if part.is_empty() {
                    return None;
                }
                if let Some((path, alias)) = part.split_once(" as ") {
                    Some((alias.trim().to_string(), path.trim().to_string()))
                } else {
                    let leaf = part.rsplit('.').next().unwrap_or(part);
                    Some((leaf.to_string(), part.to_string()))
                }
            })
            .collect();
    }

    // Rust-style path: `foo::bar::Baz` or `foo::{...}` or `foo as bar`.
    parse_rust_use(t)
}

fn parse_rust_use(s: &str) -> Vec<(String, String)> {
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }

    // Group form: `foo::bar::{A, B as C, self}` (only split when braces
    // are balanced at the top level — nested groups stay inside one item).
    if let Some(open) = s.find('{') {
        let close = s.rfind('}').unwrap_or(s.len());
        let prefix = s[..open].trim_end_matches("::").trim();
        let inner = &s[open + 1..close];
        let mut out = Vec::new();
        for item in split_top_level(inner, ',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            if item == "self" {
                if let Some(leaf) = prefix.rsplit("::").next() {
                    if !leaf.is_empty() {
                        out.push((leaf.to_string(), prefix.to_string()));
                    }
                }
                continue;
            }
            if item == "*" {
                continue;
            }
            if let Some((orig, alias)) = item.split_once(" as ") {
                out.push((
                    alias.trim().to_string(),
                    format!("{prefix}::{}", orig.trim()),
                ));
            } else {
                let leaf = item.rsplit("::").next().unwrap_or(item).to_string();
                out.push((leaf, format!("{prefix}::{item}")));
            }
        }
        return out;
    }

    // Alias: `foo::bar as baz`
    if let Some((path, alias)) = s.rsplit_once(" as ") {
        return vec![(alias.trim().to_string(), path.trim().to_string())];
    }

    // Plain path.
    if s == "*" {
        return Vec::new();
    }
    let leaf = s.rsplit("::").next().unwrap_or(s);
    if leaf.is_empty() {
        return Vec::new();
    }
    vec![(leaf.to_string(), s.to_string())]
}

/// L5.13i: detect `import * as <alias> from '<module>'` in a raw
/// `ImportsFrom` edge target. Returns `Some((alias, module))` only
/// for the namespace shape; named / default / side-effect imports
/// and non-JS forms return `None`. The raw edge text is the exact
/// slice tree-sitter captured, possibly with a trailing semicolon.
pub(crate) fn parse_js_namespace_alias(raw: &str) -> Option<(String, String)> {
    let t = raw.trim().trim_end_matches(';').trim();
    let rest = t.strip_prefix("import ")?;
    let from_idx = rest.rfind(" from ")?;
    let pre = rest[..from_idx].trim();
    let alias = pre.strip_prefix("* as ")?.trim();
    if alias.is_empty() || !alias.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let after_from = rest[from_idx + " from ".len()..].trim();
    let quote = after_from.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let inner = &after_from[quote.len_utf8()..];
    let end = inner.find(quote)?;
    let module = &inner[..end];
    if module.is_empty() {
        return None;
    }
    Some((alias.to_string(), module.to_string()))
}

fn parse_js_import(spec: &str) -> Vec<(String, String)> {
    // Extract module string (quoted).
    let (quote_start, quote_char) = match spec.char_indices().find(|(_, c)| *c == '\'' || *c == '"')
    {
        Some((i, c)) => (i, c),
        None => return Vec::new(),
    };
    let after = &spec[quote_start + quote_char.len_utf8()..];
    let end = match after.find(quote_char) {
        Some(i) => i,
        None => return Vec::new(),
    };
    let module = after[..end].to_string();

    // Part before `from 'mod'`.
    let pre = match spec.rfind(" from ") {
        Some(i) => spec[..i].trim(),
        None => spec.trim(),
    };

    let mut out = Vec::new();

    // Namespace: `* as ns`
    if let Some(alias) = pre.strip_prefix("* as ") {
        out.push((alias.trim().to_string(), module));
        return out;
    }

    // Split default + named.
    let (default_part, named_part) = if let Some(brace) = pre.find('{') {
        let before = pre[..brace].trim().trim_end_matches(',').trim();
        let close = pre.rfind('}').unwrap_or(pre.len());
        let named = &pre[brace + 1..close];
        let def = if before.is_empty() {
            None
        } else {
            Some(before)
        };
        (def, Some(named))
    } else if pre.is_empty() {
        (None, None)
    } else {
        (Some(pre), None)
    };

    if let Some(def) = default_part {
        let def = def.trim();
        if !def.is_empty() {
            out.push((def.to_string(), format!("{module}.default")));
        }
    }
    if let Some(named) = named_part {
        for item in named.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            if let Some((orig, alias)) = item.split_once(" as ") {
                out.push((
                    alias.trim().to_string(),
                    format!("{module}.{}", orig.trim()),
                ));
            } else {
                out.push((item.to_string(), format!("{module}.{item}")));
            }
        }
    }
    out
}

fn split_python_import_list(items: &str) -> Vec<(String, String)> {
    items
        .split(',')
        .filter_map(|item| {
            let item = item.trim();
            if item.is_empty() {
                return None;
            }
            if let Some((orig, alias)) = item.split_once(" as ") {
                Some((alias.trim().to_string(), orig.trim().to_string()))
            } else {
                Some((item.to_string(), item.to_string()))
            }
        })
        .collect()
}

/// Split `s` on `sep`, but keep pieces surrounded by balanced `{ }` as a
/// single item. Needed so nested use-group items don't get chopped up.
fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ParsedEdge, ParsedNode};

    fn call_edge(target: &str) -> ParsedEdge {
        ParsedEdge {
            kind: EdgeKind::Calls,
            source_qn: "mod::main".into(),
            target_qn: target.into(),
            line: 1,
            confidence: 0.6,
            tier: ConfidenceTier::Inferred,
        }
    }

    fn fn_node(qn: &str, name: &str) -> ParsedNode {
        ParsedNode {
            kind: NodeKind::Function,
            name: name.into(),
            qualified_name: qn.into(),
            line_start: 1,
            line_end: 2,
            parent_qn: None,
            signature: None,
            is_test: false,
        }
    }

    #[test]
    fn parse_rust_plain_path() {
        assert_eq!(
            parse_rust_use("foo::bar::Baz"),
            vec![("Baz".into(), "foo::bar::Baz".into())]
        );
    }

    #[test]
    fn parse_rust_alias() {
        assert_eq!(
            parse_rust_use("foo::bar as baz"),
            vec![("baz".into(), "foo::bar".into())]
        );
    }

    #[test]
    fn parse_rust_group_with_alias_and_self() {
        let pairs = parse_rust_use("foo::bar::{A, B as C, self}");
        assert!(pairs.contains(&("A".into(), "foo::bar::A".into())));
        assert!(pairs.contains(&("C".into(), "foo::bar::B".into())));
        assert!(pairs.contains(&("bar".into(), "foo::bar".into())));
    }

    #[test]
    fn parse_python_from_import() {
        let pairs = parse_import_target("from x.y import a, b as c");
        assert!(pairs.contains(&("a".into(), "x.y.a".into())));
        assert!(pairs.contains(&("c".into(), "x.y.b".into())));
    }

    #[test]
    fn parse_python_plain_import() {
        assert_eq!(
            parse_import_target("import foo.bar as baz"),
            vec![("baz".into(), "foo.bar".into())]
        );
    }

    #[test]
    fn parse_js_named_and_default() {
        let pairs = parse_import_target("import def, { a, b as c } from 'mod'");
        assert!(pairs.contains(&("def".into(), "mod.default".into())));
        assert!(pairs.contains(&("a".into(), "mod.a".into())));
        assert!(pairs.contains(&("c".into(), "mod.b".into())));
    }

    #[test]
    fn parse_js_namespace() {
        assert_eq!(
            parse_import_target("import * as ns from 'mod'"),
            vec![("ns".into(), "mod".into())]
        );
    }

    // ─── L5.13i: namespace-import detection ─────────────────────────

    #[test]
    fn parse_js_namespace_alias_happy() {
        assert_eq!(
            parse_js_namespace_alias("import * as ns from 'mod'"),
            Some(("ns".into(), "mod".into()))
        );
    }

    #[test]
    fn parse_js_namespace_alias_with_semicolon_and_double_quotes() {
        assert_eq!(
            parse_js_namespace_alias("import * as ns from \"mod\";"),
            Some(("ns".into(), "mod".into()))
        );
    }

    #[test]
    fn parse_js_namespace_alias_keeps_alias_module_spec() {
        assert_eq!(
            parse_js_namespace_alias("import * as utils from '@/utils/x'"),
            Some(("utils".into(), "@/utils/x".into()))
        );
        assert_eq!(
            parse_js_namespace_alias("import * as helpers from '../helpers'"),
            Some(("helpers".into(), "../helpers".into()))
        );
    }

    #[test]
    fn parse_js_namespace_alias_rejects_non_namespace_shapes() {
        // Named import.
        assert_eq!(parse_js_namespace_alias("import { foo } from 'mod'"), None);
        // Default import.
        assert_eq!(parse_js_namespace_alias("import foo from 'mod'"), None);
        // Side-effect import.
        assert_eq!(parse_js_namespace_alias("import 'mod'"), None);
        // Empty.
        assert_eq!(parse_js_namespace_alias(""), None);
        // Non-import (Rust use, Python from-import).
        assert_eq!(parse_js_namespace_alias("use foo::bar"), None);
        assert_eq!(parse_js_namespace_alias("from x import y"), None);
        // Unquoted module spec.
        assert_eq!(parse_js_namespace_alias("import * as ns from mod"), None);
        // Alias with non-identifier char.
        assert_eq!(
            parse_js_namespace_alias("import * as my-ns from 'mod'"),
            None
        );
    }

    #[test]
    fn resolve_file_calls_rewrites_namespace_member_call() {
        // `import * as ns from '@/x'; ns.foo();` — file-local resolver
        // rewrites the CALLS target so the cross-file L5.13h arm picks
        // it up the same way named imports do.
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: "mymod:file".into(),
                    target_qn: "import * as ns from '@/x';".into(),
                    line: 1,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                },
                call_edge("ns.foo"),
            ],
        };
        resolve_file_calls(&mut r);
        let call = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(call.target_qn, "@/x.foo");
        // Tier untouched; cross-file pass bumps it on resolution.
        assert!(matches!(call.tier, ConfidenceTier::Inferred));
    }

    #[test]
    fn resolve_file_calls_namespace_rewrite_skips_non_namespace_import() {
        // `import { ns } from 'mod'; ns.foo();` — `ns` is a named
        // binding, not a namespace alias. The CALLS target stays as
        // `ns.foo` so the leaf-name fallback still has a shot. (The
        // value in `imports` would be `mod.ns`, not a bare module
        // spec, so the namespace arm correctly ignores it.)
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: "mymod:file".into(),
                    target_qn: "import { ns } from 'mod';".into(),
                    line: 1,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                },
                call_edge("ns.foo"),
            ],
        };
        resolve_file_calls(&mut r);
        let call = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(call.target_qn, "ns.foo");
    }

    #[test]
    fn resolve_file_calls_namespace_rewrite_skips_chained_member_access() {
        // `ns.sub.foo()` is a multi-level access. We don't try to
        // figure out what `ns.sub` is — leave the target untouched so
        // downstream consumers keep their existing fallback chain.
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: "mymod:file".into(),
                    target_qn: "import * as ns from '@/x';".into(),
                    line: 1,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                },
                call_edge("ns.sub.foo"),
            ],
        };
        resolve_file_calls(&mut r);
        let call = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(call.target_qn, "ns.sub.foo");
    }

    #[test]
    fn resolve_file_calls_namespace_rewrite_skips_unknown_alias() {
        // No matching `import * as foo` — `foo.bar` falls through.
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![call_edge("foo.bar")],
        };
        resolve_file_calls(&mut r);
        assert_eq!(r.edges[0].target_qn, "foo.bar");
    }

    #[test]
    fn resolves_local_call_unambiguously() {
        let mut r = ParseResult {
            nodes: vec![fn_node("mymod::helper", "helper")],
            edges: vec![call_edge("helper")],
        };
        resolve_file_calls(&mut r);
        assert_eq!(r.edges[0].target_qn, "mymod::helper");
        assert!(matches!(r.edges[0].tier, ConfidenceTier::Resolved));
    }

    #[test]
    fn leaves_ambiguous_local_match_alone() {
        let mut r = ParseResult {
            nodes: vec![
                fn_node("a::helper", "helper"),
                fn_node("b::helper", "helper"),
            ],
            edges: vec![call_edge("helper")],
        };
        resolve_file_calls(&mut r);
        assert_eq!(r.edges[0].target_qn, "helper");
        assert!(matches!(r.edges[0].tier, ConfidenceTier::Inferred));
    }

    #[test]
    fn resolves_via_rust_use_import() {
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: "mymod:file".into(),
                    target_qn: "crate::util::compute_delta".into(),
                    line: 1,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                },
                call_edge("compute_delta"),
            ],
        };
        resolve_file_calls(&mut r);
        let call = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(call.target_qn, "crate::util::compute_delta");
        assert!(matches!(call.tier, ConfidenceTier::Resolved));
    }

    #[test]
    fn skips_already_qualified_targets() {
        let mut r = ParseResult {
            nodes: vec![fn_node("mymod::helper", "helper")],
            edges: vec![call_edge("other::helper")],
        };
        resolve_file_calls(&mut r);
        assert_eq!(r.edges[0].target_qn, "other::helper");
        assert!(matches!(r.edges[0].tier, ConfidenceTier::Inferred));
    }

    #[test]
    fn resolves_head_qualified_via_import() {
        // `Foo::new` with `use crate::util::Foo` → `crate::util::Foo::new`.
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: "mymod:file".into(),
                    target_qn: "crate::util::Foo".into(),
                    line: 1,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                },
                call_edge("Foo::new"),
            ],
        };
        resolve_file_calls(&mut r);
        let call = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(call.target_qn, "crate::util::Foo::new");
        assert!(matches!(call.tier, ConfidenceTier::Resolved));
    }

    #[test]
    fn resolves_head_qualified_via_local_symbol() {
        // `Foo::bar` with a local `struct Foo` defined in the same file.
        let mut r = ParseResult {
            nodes: vec![ParsedNode {
                kind: NodeKind::Class,
                name: "Foo".into(),
                qualified_name: "mymod::Foo".into(),
                line_start: 1,
                line_end: 1,
                parent_qn: None,
                signature: None,
                is_test: false,
            }],
            edges: vec![call_edge("Foo::bar")],
        };
        resolve_file_calls(&mut r);
        assert_eq!(r.edges[0].target_qn, "mymod::Foo::bar");
        assert!(matches!(r.edges[0].tier, ConfidenceTier::Resolved));
    }

    #[test]
    fn keeps_rust_path_keywords_as_is() {
        for head in &["self", "Self", "super", "crate"] {
            let target = format!("{head}::foo");
            let mut r = ParseResult {
                nodes: vec![],
                edges: vec![call_edge(&target)],
            };
            resolve_file_calls(&mut r);
            assert_eq!(r.edges[0].target_qn, target, "head={head}");
        }
    }

    #[test]
    fn leaves_deeply_qualified_targets_alone() {
        // Already has two segments after head — don't touch.
        let mut r = ParseResult {
            nodes: vec![],
            edges: vec![
                ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: "mymod:file".into(),
                    target_qn: "crate::util::Foo".into(),
                    line: 1,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                },
                call_edge("Foo::inner::deep"),
            ],
        };
        resolve_file_calls(&mut r);
        let call = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::Calls))
            .unwrap();
        assert_eq!(call.target_qn, "Foo::inner::deep");
        assert!(matches!(call.tier, ConfidenceTier::Inferred));
    }
}
