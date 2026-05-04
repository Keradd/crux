use std::collections::HashMap;

use crate::types::{ConfidenceTier, EdgeKind, NodeKind, ParseResult};

pub(crate) fn resolve_file_calls(result: &mut ParseResult) {
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

    let mut imports: HashMap<String, String> = HashMap::new();
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

        if !e.target_qn.contains('.') {
            if let Some((head, tail)) = e.target_qn.split_once("::") {
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

        if let Some((head, tail)) = e.target_qn.split_once('.') {
            if !head.is_empty() && !tail.is_empty() && !tail.contains('.') {
                if let Some(module) = namespace_imports.get(head) {
                    e.target_qn = format!("{module}.{tail}");
                }
            }
        }
    }
}

fn is_rust_path_keyword(s: &str) -> bool {
    matches!(s, "crate" | "self" | "super" | "Self")
}

pub(crate) fn parse_import_target(raw: &str) -> Vec<(String, String)> {
    let t = raw.trim().trim_end_matches(';').trim();

    if let Some(rest) = t.strip_prefix("from ") {
        if let Some((module, items)) = rest.split_once(" import ") {
            let module = module.trim();
            return split_python_import_list(items)
                .into_iter()
                .map(|(leaf, orig)| (leaf, format!("{module}.{orig}")))
                .collect();
        }
    }

    if t.starts_with("import ") && (t.contains(" from ") || t.contains('\'') || t.contains('"')) {
        return parse_js_import(t.trim_start_matches("import ").trim());
    }

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

    parse_rust_use(t)
}

fn parse_rust_use(s: &str) -> Vec<(String, String)> {
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }

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

    if let Some((path, alias)) = s.rsplit_once(" as ") {
        return vec![(alias.trim().to_string(), path.trim().to_string())];
    }

    if s == "*" {
        return Vec::new();
    }
    let leaf = s.rsplit("::").next().unwrap_or(s);
    if leaf.is_empty() {
        return Vec::new();
    }
    vec![(leaf.to_string(), s.to_string())]
}

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

    let pre = match spec.rfind(" from ") {
        Some(i) => spec[..i].trim(),
        None => spec.trim(),
    };

    let mut out = Vec::new();

    if let Some(alias) = pre.strip_prefix("* as ") {
        out.push((alias.trim().to_string(), module));
        return out;
    }

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
        assert_eq!(parse_js_namespace_alias("import { foo } from 'mod'"), None);
        assert_eq!(parse_js_namespace_alias("import foo from 'mod'"), None);
        assert_eq!(parse_js_namespace_alias("import 'mod'"), None);
        assert_eq!(parse_js_namespace_alias(""), None);
        assert_eq!(parse_js_namespace_alias("use foo::bar"), None);
        assert_eq!(parse_js_namespace_alias("from x import y"), None);
        assert_eq!(parse_js_namespace_alias("import * as ns from mod"), None);
        assert_eq!(
            parse_js_namespace_alias("import * as my-ns from 'mod'"),
            None
        );
    }

    #[test]
    fn resolve_file_calls_rewrites_namespace_member_call() {
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
        assert!(matches!(call.tier, ConfidenceTier::Inferred));
    }

    #[test]
    fn resolve_file_calls_namespace_rewrite_skips_non_namespace_import() {
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
