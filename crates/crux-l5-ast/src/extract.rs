//! Per-language extractors. Each language gets a function that turns a
//! `(content, file_path, project_root)` triple into a [`ParseResult`].
//!
//! Strategy: tree-sitter parse → walk top-level definitions to emit
//! `ParsedNode` rows, walk again to find calls / imports / inheritance
//! to emit `ParsedEdge` rows.
//!
//! v1 scope (deliberately conservative):
//! - Rust: pub/fn, impl blocks, struct, enum, trait, mod, use; CALLS via
//!   `(call_expression function: ...)`; IMPORTS_FROM via `use_declaration`.
//! - Python: class, def, async def, import / from-import; CALLS via
//!   `(call function: ...)`.
//! - TypeScript / JavaScript: function/class declarations, export
//!   const/let/var, ESM imports, call expressions.
//!
//! Edges that fail name resolution stay as `INFERRED`. The dispatcher
//! upgrades them to `RESOLVED` when the target qualified name actually
//! exists in the same project.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser};

use crate::types::{
    ConfidenceTier, EdgeKind, Language, NodeKind, ParseResult, ParsedEdge, ParsedNode,
};

pub fn parse(language: Language, content: &str, file_path: &Path) -> ParseResult {
    parse_with_project(language, content, file_path, None)
}

/// Same as [`parse`] but threads a project-wide signature aggregate
/// through the Rust receiver-typing pass so calls that depend on a
/// function / method / enum declared in *another* file can still
/// resolve. `project = None` gives the old file-local behaviour.
///
/// Non-Rust languages ignore the project argument today — L5.12 is
/// Rust-only because only the Rust extractor currently consumes
/// [`FileTypes`].
pub fn parse_with_project(
    language: Language,
    content: &str,
    file_path: &Path,
    project: Option<&ProjectFileTypes>,
) -> ParseResult {
    let mut parser = Parser::new();
    let _ = parser.set_language(&grammar(&language));
    let Some(tree) = parser.parse(content, None) else {
        return ParseResult::default();
    };
    let root = tree.root_node();
    let mod_qn = module_qn(&language, file_path);

    let mut out = ParseResult::default();
    out.nodes.push(ParsedNode {
        kind: NodeKind::File,
        name: file_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<file>")
            .to_string(),
        qualified_name: format!("{}:file", mod_qn),
        line_start: 1,
        line_end: content.lines().count() as u32,
        parent_qn: None,
        signature: None,
        is_test: false,
    });

    match language {
        Language::Rust => visit_rust_with_project(root, content, &mod_qn, &mut out, project),
        Language::Python => visit_python(root, content, &mod_qn, &mut out),
        Language::TypeScript | Language::JavaScript => {
            visit_js(root, content, &mod_qn, &mut out);
        }
        Language::Lua => visit_lua(root, content, &mod_qn, &mut out),
        Language::Bash => visit_bash(root, content, &mod_qn, &mut out),
    }
    crate::resolver::resolve_file_calls(&mut out);
    out
}

/// Collect just the [`FileTypes`] of a file, without running the full
/// extract pass. Rust only today; other languages produce an empty
/// `FileTypes`. Used by the indexer's phase-1 pass to aggregate
/// project-wide signatures before phase 2 re-parses with the cross-file
/// context in hand.
pub fn collect_file_signatures(language: Language, content: &str) -> FileTypes {
    if !matches!(language, Language::Rust) {
        return FileTypes::default();
    }
    let mut parser = Parser::new();
    let _ = parser.set_language(&grammar(&language));
    let Some(tree) = parser.parse(content, None) else {
        return FileTypes::default();
    };
    collect_rust_signatures(tree.root_node(), content)
}

fn grammar(lang: &Language) -> tree_sitter::Language {
    match lang {
        Language::Rust => tree_sitter_rust::language(),
        Language::Python => tree_sitter_python::language(),
        Language::TypeScript => tree_sitter_typescript::language_typescript(),
        Language::JavaScript => tree_sitter_javascript::language(),
        Language::Lua => tree_sitter_lua::language(),
        Language::Bash => tree_sitter_bash::language(),
    }
}

/// Compute the module-level qualified-name prefix for a file.
/// Strategy: project-relative path with extension stripped, separators
/// turned into the language's idiomatic delimiter.
fn module_qn(language: &Language, file_path: &Path) -> String {
    let stem_path: Vec<String> = file_path
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
        .collect();
    let mut joined = stem_path.join("/");
    if let Some(idx) = joined.rfind('.') {
        joined.truncate(idx);
    }
    match language {
        Language::Rust | Language::Python => joined.replace('/', "::"),
        // Lua's `require("foo.bar")` uses dots; mirror that for module
        // qualified names so `crux find` results read naturally.
        Language::Lua => joined.replace('/', "."),
        // Bash has no module concept — keep the file path as-is so the
        // qn round-trips back to a real on-disk file.
        _ => joined,
    }
}

fn slice<'a>(node: Node<'_>, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

fn line_of(node: Node<'_>) -> (u32, u32) {
    let start = node.start_position().row as u32 + 1;
    let end = node.end_position().row as u32 + 1;
    (start, end)
}

// ─────────────────────────────────────────────────────────────────────────
// Rust
// ─────────────────────────────────────────────────────────────────────────

fn visit_rust_with_project(
    root: Node<'_>,
    src: &str,
    mod_qn: &str,
    out: &mut ParseResult,
    project: Option<&ProjectFileTypes>,
) {
    // L5.8: collect a per-file map of fn / method return types so let
    // RHS inference can pin `let x = make_foo();` to `make_foo`'s
    // return type and chain through method calls.
    //
    // L5.12: if a project-wide aggregate is supplied, top up the local
    // map with non-ambiguous entries from every other file — so cross-
    // file calls like `let x = other_mod::make_foo(); x.bar();`
    // resolve even when `make_foo` isn't declared in this file.
    let mut file_types = collect_rust_signatures(root, src);
    if let Some(p) = project {
        p.fill_missing(&mut file_types);
    }
    let mut cursor = root.walk();
    walk_rust(root, &mut cursor, src, mod_qn, None, out, &file_types);
}

fn walk_rust<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
    file_types: &FileTypes,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "function_item" => {
                let name = field_name(&child, "name", src).unwrap_or_else(|| "<fn>".into());
                let qn = qn_join(mod_qn, parent_qn, &name);
                let (ls, le) = line_of(child);
                let sig = signature_rust(child, src);
                let is_test = is_rust_test(child, src);
                out.nodes.push(ParsedNode {
                    kind: if parent_qn.is_some() {
                        NodeKind::Method
                    } else {
                        NodeKind::Function
                    },
                    name,
                    qualified_name: qn.clone(),
                    line_start: ls,
                    line_end: le,
                    parent_qn: parent_qn.map(String::from),
                    signature: sig,
                    is_test,
                });
                if let Some(body) = child.child_by_field_name("body") {
                    // Build a Rust-only scope so method-call receivers like
                    // `self.foo()` / `Self::bar()` / `param.run()` get
                    // rewritten to the owning type's qualified name at
                    // emit time instead of staying bare.
                    let mut scope = RustScope::new(
                        parent_qn.map(String::from),
                        child.child_by_field_name("parameters"),
                        src,
                    );
                    let mut sub = body.walk();
                    collect_rust_calls(body, &mut sub, src, &qn, &mut scope, out, file_types);
                }
            }
            "struct_item" | "enum_item" | "trait_item" | "type_item" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: match child.kind() {
                            "trait_item" => NodeKind::Type,
                            _ => NodeKind::Class,
                        },
                        name,
                        qualified_name: qn,
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test: false,
                    });
                }
            }
            "impl_item" => {
                let target = child
                    .child_by_field_name("type")
                    .map(|n| slice(n, src).to_string())
                    .unwrap_or_default();
                let parent = qn_join(mod_qn, parent_qn, &target);
                if let Some(body) = child.child_by_field_name("body") {
                    let mut sub = body.walk();
                    walk_rust(body, &mut sub, src, mod_qn, Some(&parent), out, file_types);
                }
            }
            "mod_item" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let nested_mod = format!("{mod_qn}::{name}");
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Module,
                        name,
                        qualified_name: nested_mod.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: None,
                        is_test: false,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut sub = body.walk();
                        walk_rust(body, &mut sub, src, &nested_mod, None, out, file_types);
                    }
                }
            }
            "use_declaration" => {
                let path = slice(child, src);
                // A use_declaration may carry a visibility prefix
                // (`pub use ...`, `pub(crate) use ...`). Strip everything
                // up to the first `use ` token so downstream consumers see
                // only the path body.
                let target = match path.split_once("use ") {
                    Some((_, rest)) => rest.trim_end_matches(';').trim().to_string(),
                    None => path.trim_end_matches(';').trim().to_string(),
                };
                let (ls, _) = line_of(child);
                out.edges.push(ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: format!("{mod_qn}:file"),
                    target_qn: target,
                    line: ls,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                });
            }
            "const_item" | "static_item" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Constant,
                        name: name.clone(),
                        qualified_name: qn_join(mod_qn, parent_qn, &name),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test: false,
                    });
                }
            }
            _ => {
                // Recurse into containers we don't otherwise handle.
                let mut sub = child.walk();
                walk_rust(child, &mut sub, src, mod_qn, parent_qn, out, file_types);
            }
        }
    }
}

fn signature_rust(fn_node: Node<'_>, src: &str) -> Option<String> {
    fn_node
        .child_by_field_name("body")
        .map(|body| {
            let start = fn_node.start_byte();
            let stop = body.start_byte();
            src[start..stop].trim().to_string()
        })
        .or_else(|| Some(slice(fn_node, src).lines().next().unwrap_or("").to_string()))
}

fn is_rust_test(fn_node: Node<'_>, src: &str) -> bool {
    // Walk backwards in bytes — `src[..]` indexing would panic the moment
    // we land mid-codepoint, which happens often near doc-comments. The
    // attribute we care about is pure ASCII so byte-comparison is safe.
    let pre_start = fn_node.start_byte();
    let pre_window = pre_start.saturating_sub(80);
    let bytes = src.as_bytes();
    let end = pre_start.min(bytes.len());
    if pre_window >= end {
        return false;
    }
    let slice = &bytes[pre_window..end];
    contains_bytes(slice, b"#[test]") || contains_bytes(slice, b"#[tokio::test]")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ─────────────────────────────────────────────────────────────────────────
// Python
// ─────────────────────────────────────────────────────────────────────────

fn visit_python(root: Node<'_>, src: &str, mod_qn: &str, out: &mut ParseResult) {
    walk_python(root, src, mod_qn, None, out);
}

fn walk_python<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_definition" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Class,
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test: false,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        walk_python(body, src, mod_qn, Some(&qn), out);
                    }
                }
            }
            "function_definition" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(child);
                    let is_test = name.starts_with("test_");
                    out.nodes.push(ParsedNode {
                        kind: if parent_qn.is_some() {
                            NodeKind::Method
                        } else {
                            NodeKind::Function
                        },
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut sub = body.walk();
                        collect_calls(body, &mut sub, src, &qn, out);
                    }
                }
            }
            "import_statement" | "import_from_statement" => {
                let target = slice(child, src).trim().to_string();
                let (ls, _) = line_of(child);
                out.edges.push(ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: format!("{mod_qn}:file"),
                    target_qn: target,
                    line: ls,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                });
            }
            _ => {
                walk_python(child, src, mod_qn, parent_qn, out);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// JavaScript / TypeScript
// ─────────────────────────────────────────────────────────────────────────

fn visit_js(root: Node<'_>, src: &str, mod_qn: &str, out: &mut ParseResult) {
    walk_js(root, src, mod_qn, None, out);
}

fn walk_js<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Function,
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test: false,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut sub = body.walk();
                        collect_calls(body, &mut sub, src, &qn, out);
                    }
                }
            }
            "class_declaration" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Class,
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test: false,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        walk_js(body, src, mod_qn, Some(&qn), out);
                    }
                }
            }
            "method_definition" => {
                if let Some(name) = field_name(&child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Method,
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(slice(child, src).lines().next().unwrap_or("").to_string()),
                        is_test: false,
                    });
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut sub = body.walk();
                        collect_calls(body, &mut sub, src, &qn, out);
                    }
                }
            }
            "import_statement" => {
                let target = slice(child, src).trim().to_string();
                let (ls, _) = line_of(child);
                out.edges.push(ParsedEdge {
                    kind: EdgeKind::ImportsFrom,
                    source_qn: format!("{mod_qn}:file"),
                    target_qn: target,
                    line: ls,
                    confidence: 0.7,
                    tier: ConfidenceTier::Inferred,
                });
            }
            "export_statement" => {
                handle_js_export(child, src, mod_qn, parent_qn, out);
            }
            _ => {
                walk_js(child, src, mod_qn, parent_qn, out);
            }
        }
    }
}

/// L5.13e: handle JS/TS `export_statement`. Splits cases:
///
/// - `export default function bar() {}` / `export default class Bar {}`
///   emit the inner declaration as a normal node + an `ExportsDefault`
///   edge from the module to that node's FQN.
/// - `export default Foo` (identifier) emits an `ExportsDefault` edge
///   whose target is the bare identifier; [`crate::resolver::resolve_file_calls`]
///   later promotes it to the local FQN.
/// - `export default function() {}` / `export default <expression>`
///   (anonymous / value) synthesise a `{mod_qn}::default` Constant node
///   and emit an `ExportsDefault` edge pointing at it.
/// - `export function foo() {}` / `export class Bar {}` (named, not
///   default) recurse via the normal walker so the existing
///   declaration arms fire.
fn handle_js_export<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    let mut sub = node.walk();
    let mut has_default = false;
    let mut default_target: Option<String> = None;
    let mut handled_inner = false;
    for sub_child in node.children(&mut sub) {
        match sub_child.kind() {
            "default" => has_default = true,
            "function_declaration" | "generator_function_declaration" => {
                if let Some(name) = field_name(&sub_child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(sub_child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Function,
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(
                            slice(sub_child, src)
                                .lines()
                                .next()
                                .unwrap_or("")
                                .to_string(),
                        ),
                        is_test: false,
                    });
                    if let Some(body) = sub_child.child_by_field_name("body") {
                        let mut bsub = body.walk();
                        collect_calls(body, &mut bsub, src, &qn, out);
                    }
                    if has_default {
                        default_target = Some(qn);
                    }
                    handled_inner = true;
                }
            }
            "class_declaration" => {
                if let Some(name) = field_name(&sub_child, "name", src) {
                    let qn = qn_join(mod_qn, parent_qn, &name);
                    let (ls, le) = line_of(sub_child);
                    out.nodes.push(ParsedNode {
                        kind: NodeKind::Class,
                        name,
                        qualified_name: qn.clone(),
                        line_start: ls,
                        line_end: le,
                        parent_qn: parent_qn.map(String::from),
                        signature: Some(
                            slice(sub_child, src)
                                .lines()
                                .next()
                                .unwrap_or("")
                                .to_string(),
                        ),
                        is_test: false,
                    });
                    if let Some(body) = sub_child.child_by_field_name("body") {
                        walk_js(body, src, mod_qn, Some(&qn), out);
                    }
                    if has_default {
                        default_target = Some(qn);
                    }
                    handled_inner = true;
                }
            }
            "identifier" if has_default && default_target.is_none() => {
                // `export default Foo` — store the bare identifier; the
                // file-local resolver promotes it to the full FQN.
                default_target = Some(slice(sub_child, src).to_string());
                handled_inner = true;
            }
            _ => {}
        }
    }

    if has_default {
        let target = default_target.unwrap_or_else(|| {
            // Anonymous / expression default — synthesize a `default`
            // node so cross-file resolvers have something to land on.
            let synth_qn = qn_join(mod_qn, parent_qn, "default");
            let (ls, le) = line_of(node);
            out.nodes.push(ParsedNode {
                kind: NodeKind::Constant,
                name: "default".to_string(),
                qualified_name: synth_qn.clone(),
                line_start: ls,
                line_end: le,
                parent_qn: parent_qn.map(String::from),
                signature: Some(slice(node, src).lines().next().unwrap_or("").to_string()),
                is_test: false,
            });
            synth_qn
        });
        let (ls, _) = line_of(node);
        out.edges.push(ParsedEdge {
            kind: EdgeKind::ExportsDefault,
            source_qn: format!("{mod_qn}:file"),
            target_qn: target,
            line: ls,
            confidence: 0.9,
            tier: ConfidenceTier::Extracted,
        });
    } else if !handled_inner {
        // Named export wrapping forms we don't handle directly (e.g.
        // `export const foo = ...`, `export { a, b }`). Recurse so the
        // standard walker can pick up whatever it can.
        walk_js(node, src, mod_qn, parent_qn, out);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Lua (tree-sitter-lua 0.1)
//
// Node kinds we extract:
//   - `function_declaration` — `function foo(...)`, `local function foo`,
//     `function M:method(...)`, `function M.helper(...)`. The name is the
//     child between the `function` keyword and the `parameters` node and
//     can be an `identifier`, `dot_index_expression`, or
//     `method_index_expression`.
//   - `variable_declaration` — `local x = …`. We emit a `Constant` node
//     for each name in the variable list. When the RHS is a
//     `function_definition` (anonymous `function() … end`) we promote
//     the constant to a `Function`.
//   - `assignment_statement` (top-level, no `local`) — `M.helper = function() … end`
//     promotes to a `Method` on the table receiver.
//
// Edges:
//   - `function_call` whose function is `require` becomes an
//     `ImportsFrom` edge from the file to the required module name.
//   - All other `function_call` nodes inside a function body become
//     `Calls` edges via the shared `collect_calls` helper.
// ─────────────────────────────────────────────────────────────────────────

fn visit_lua(root: Node<'_>, src: &str, mod_qn: &str, out: &mut ParseResult) {
    walk_lua(root, src, mod_qn, None, out);
}

fn walk_lua<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                handle_lua_function_decl(child, src, mod_qn, parent_qn, out);
            }
            "variable_declaration" => {
                // `local x = …` (and friends). Always wraps an
                // `assignment_statement` we delegate to.
                let mut sub = child.walk();
                for inner in child.children(&mut sub) {
                    if inner.kind() == "assignment_statement" {
                        handle_lua_assignment(inner, src, mod_qn, parent_qn, true, out);
                    }
                }
            }
            "assignment_statement" => {
                handle_lua_assignment(child, src, mod_qn, parent_qn, false, out);
            }
            "function_call" => {
                // Top-level `require("foo")` not bound to a name still
                // counts as an import for the file's purposes.
                if let Some(target) = lua_require_target(child, src) {
                    let (ls, _) = line_of(child);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::ImportsFrom,
                        source_qn: format!("{mod_qn}:file"),
                        target_qn: target,
                        line: ls,
                        confidence: 0.7,
                        tier: ConfidenceTier::Inferred,
                    });
                }
            }
            _ => {
                walk_lua(child, src, mod_qn, parent_qn, out);
            }
        }
    }
}

fn handle_lua_function_decl<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    // Name is the first child of kind `identifier`,
    // `dot_index_expression`, or `method_index_expression` after the
    // `function` keyword. Iterate children until we hit `parameters`.
    let mut name_node: Option<Node<'a>> = None;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "identifier" | "dot_index_expression" | "method_index_expression" => {
                name_node = Some(c);
                break;
            }
            "parameters" | "block" | "end" => break,
            _ => {}
        }
    }
    let Some(name_node) = name_node else { return };

    let (kind, name, owner) = match name_node.kind() {
        "identifier" => (NodeKind::Function, slice(name_node, src).to_string(), None),
        "method_index_expression" | "dot_index_expression" => {
            // Both shapes wrap two identifiers. Take the last one as the
            // method name and the rest as the receiver.
            let raw = slice(name_node, src);
            let sep = if name_node.kind() == "method_index_expression" {
                ':'
            } else {
                '.'
            };
            match raw.rsplit_once(sep) {
                Some((recv, method)) => (
                    NodeKind::Method,
                    method.trim().to_string(),
                    Some(recv.trim().to_string()),
                ),
                // Unexpected shape — fall back to the whole literal so the
                // node still appears in the graph rather than going missing.
                None => (NodeKind::Function, raw.to_string(), None),
            }
        }
        _ => return,
    };

    let qn_parent = match owner {
        Some(o) => Some(qn_join(mod_qn, parent_qn, &o)),
        None => parent_qn.map(String::from),
    };
    let qn = qn_join(mod_qn, qn_parent.as_deref(), &name);
    let (ls, le) = line_of(node);
    let signature = lua_signature(node, src);
    let is_test = name.starts_with("test_") || name.starts_with("test");
    out.nodes.push(ParsedNode {
        kind,
        name,
        qualified_name: qn.clone(),
        line_start: ls,
        line_end: le,
        parent_qn: qn_parent,
        signature,
        is_test,
    });

    // Walk the body for nested calls + nested function definitions.
    if let Some(body) = lua_function_body(node) {
        let mut sub = body.walk();
        collect_calls(body, &mut sub, src, &qn, out);
    }
}

fn handle_lua_assignment<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    is_local: bool,
    out: &mut ParseResult,
) {
    let mut var_list: Option<Node<'a>> = None;
    let mut expr_list: Option<Node<'a>> = None;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "variable_list" => var_list = Some(c),
            "expression_list" => expr_list = Some(c),
            _ => {}
        }
    }
    let Some(var_list) = var_list else { return };

    // Pair each variable with its expression by index. Lua allows
    // `local a, b = 1, 2` so we walk both lists in lock-step.
    let vars: Vec<Node<'a>> = var_list
        .children(&mut var_list.walk())
        .filter(|c| c.kind() != ",")
        .collect();
    let exprs: Vec<Node<'a>> = expr_list
        .map(|e| {
            e.children(&mut e.walk())
                .filter(|c| c.kind() != ",")
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for (i, var) in vars.iter().enumerate() {
        let raw_name = match var.kind() {
            "identifier" | "dot_index_expression" | "method_index_expression" => {
                slice(*var, src).to_string()
            }
            _ => continue,
        };
        let rhs = exprs.get(i).copied();
        let is_func = rhs
            .map(|n| n.kind() == "function_definition")
            .unwrap_or(false);
        let kind = if is_func {
            if raw_name.contains('.') || raw_name.contains(':') {
                NodeKind::Method
            } else {
                NodeKind::Function
            }
        } else {
            NodeKind::Constant
        };

        let (display_name, qn_parent) = if raw_name.contains('.') || raw_name.contains(':') {
            let sep = if raw_name.contains(':') { ':' } else { '.' };
            match raw_name.rsplit_once(sep) {
                Some((recv, m)) => (
                    m.trim().to_string(),
                    Some(qn_join(mod_qn, parent_qn, recv.trim())),
                ),
                None => (raw_name.clone(), parent_qn.map(String::from)),
            }
        } else {
            (raw_name.clone(), parent_qn.map(String::from))
        };

        let qn = qn_join(mod_qn, qn_parent.as_deref(), &display_name);
        let (ls, le) = line_of(node);
        let mut sig_line = slice(node, src).lines().next().unwrap_or("").to_string();
        if is_local && !sig_line.starts_with("local ") {
            sig_line = format!("local {sig_line}");
        }
        out.nodes.push(ParsedNode {
            kind,
            name: display_name,
            qualified_name: qn.clone(),
            line_start: ls,
            line_end: le,
            parent_qn: qn_parent,
            signature: Some(sig_line),
            is_test: false,
        });

        // Body-level call extraction for the anonymous-fn case.
        if is_func {
            if let Some(rhs) = rhs {
                if let Some(body) = lua_function_body(rhs) {
                    let mut sub = body.walk();
                    collect_calls(body, &mut sub, src, &qn, out);
                }
            }
        }

        // `local x = require("foo")` → ImportsFrom edge.
        if let Some(rhs) = rhs {
            if rhs.kind() == "function_call" {
                if let Some(target) = lua_require_target(rhs, src) {
                    let (line, _) = line_of(rhs);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::ImportsFrom,
                        source_qn: format!("{mod_qn}:file"),
                        target_qn: target,
                        line,
                        confidence: 0.8,
                        tier: ConfidenceTier::Extracted,
                    });
                }
            }
        }
    }
}

/// Extract the inner `block` body of a `function_declaration` /
/// `function_definition` so we can run the call collector on it.
fn lua_function_body<'a>(node: Node<'a>) -> Option<Node<'a>> {
    // NB: the found Node must be bound to a local before the function
    // exits so the borrow-backed child iterator drops before `cursor`.
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == "block");
    found
}

/// Build the displayed signature for a Lua function: everything up to
/// (but not including) the body block.
fn lua_signature(fn_node: Node<'_>, src: &str) -> Option<String> {
    if let Some(block) = lua_function_body(fn_node) {
        let start = fn_node.start_byte();
        let stop = block.start_byte();
        Some(src[start..stop].trim().to_string())
    } else {
        Some(slice(fn_node, src).lines().next().unwrap_or("").to_string())
    }
}

/// If `node` is a `function_call` to `require("...")` return the
/// required module name.
fn lua_require_target(node: Node<'_>, src: &str) -> Option<String> {
    if node.kind() != "function_call" {
        return None;
    }
    let mut cursor = node.walk();
    let mut callee_is_require = false;
    let mut module: Option<String> = None;
    for c in node.children(&mut cursor) {
        if c.kind() == "identifier" && slice(c, src) == "require" {
            callee_is_require = true;
        }
        if c.kind() == "arguments" {
            // `require"foo"` and `require("foo")` shapes both end up as
            // an `arguments` child wrapping a string literal.
            let mut sub = c.walk();
            for arg in c.children(&mut sub) {
                if arg.kind() == "string" {
                    let raw = slice(arg, src);
                    let trimmed = raw.trim_matches(|ch: char| ch == '"' || ch == '\'');
                    module = Some(trimmed.to_string());
                    break;
                }
            }
        }
    }
    if callee_is_require {
        module
    } else {
        None
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Bash (tree-sitter-bash 0.21)
//
// Symbols we capture:
//   - `function_definition` — both `function name() {…}` and `name() {…}`.
//     Emitted as `Function` nodes; the body is walked for `command`
//     calls.
//   - `declaration_command` — `local x=…`, `declare x=…`,
//     `readonly x=…`. The `variable_assignment.variable_name` becomes a
//     `Constant` node with the original line as its signature.
//   - `command` whose first word is `alias` — `alias ll='ls -la'`. The
//     LHS of the `key=value` concatenation becomes a `Constant` whose
//     signature carries the full alias body. Anything assigned via
//     plain `x=...` at the file level surfaces the same way.
// ─────────────────────────────────────────────────────────────────────────

fn visit_bash(root: Node<'_>, src: &str, mod_qn: &str, out: &mut ParseResult) {
    walk_bash(root, src, mod_qn, None, out);
}

fn walk_bash<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_definition" => {
                handle_bash_function_decl(child, src, mod_qn, parent_qn, out);
            }
            "declaration_command" => {
                handle_bash_declaration(child, src, mod_qn, parent_qn, out);
            }
            "command" => {
                // Only `alias` invocations are interesting at the file
                // level; ordinary commands at the top of a script aren't
                // declarations of a symbol the agent can reference.
                if matches!(
                    bash_command_first_word(child, src).as_deref(),
                    Some("alias")
                ) {
                    handle_bash_alias(child, src, mod_qn, parent_qn, out);
                }
            }
            _ => walk_bash(child, src, mod_qn, parent_qn, out),
        }
    }
}

fn handle_bash_function_decl<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    // The name is the first `word` child (both `function name` and
    // `name()` forms).
    let mut cursor = node.walk();
    let name = node
        .children(&mut cursor)
        .find(|c| c.kind() == "word")
        .map(|n| slice(n, src).to_string());
    let Some(name) = name else { return };

    let qn = qn_join(mod_qn, parent_qn, &name);
    let (ls, le) = line_of(node);
    let signature = bash_signature(node, src);
    // Convention: bats `@test` and bash `test_*` functions are tests.
    let is_test = name.starts_with("test_") || name.starts_with("@test");
    out.nodes.push(ParsedNode {
        kind: NodeKind::Function,
        name,
        qualified_name: qn.clone(),
        line_start: ls,
        line_end: le,
        parent_qn: parent_qn.map(String::from),
        signature,
        is_test,
    });

    if let Some(body) = bash_function_body(node) {
        // Two passes over the body:
        //   1. `walk_bash` keeps emitting declaration_command / alias /
        //      nested function_definition nodes — owned by this fn's qn.
        //   2. `bash_collect_calls` emits a `Calls` edge per `command`
        //      node that runs inside the body.
        walk_bash(body, src, mod_qn, Some(&qn), out);
        bash_collect_calls(body, src, &qn, out);
    }
}

fn handle_bash_declaration<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    // declaration_command may carry one or many variable_assignments
    // (e.g. `declare -i a=1 b=2`). Emit one Constant per name.
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "variable_assignment" {
            if let Some(name_node) = c.child_by_field_name("name") {
                let name = slice(name_node, src).to_string();
                let qn = qn_join(mod_qn, parent_qn, &name);
                let (ls, le) = line_of(node);
                out.nodes.push(ParsedNode {
                    kind: NodeKind::Constant,
                    name,
                    qualified_name: qn,
                    line_start: ls,
                    line_end: le,
                    parent_qn: parent_qn.map(String::from),
                    signature: Some(slice(node, src).lines().next().unwrap_or("").to_string()),
                    is_test: false,
                });
            }
        }
    }
}

fn handle_bash_alias<'a>(
    node: Node<'a>,
    src: &str,
    mod_qn: &str,
    parent_qn: Option<&str>,
    out: &mut ParseResult,
) {
    // `alias ll='ls -la'` parses as:
    //   command
    //     command_name word=alias
    //     concatenation
    //       word=ll=
    //       raw_string='ls -la'
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() != "concatenation" {
            continue;
        }
        let mut sub = c.walk();
        let first = c.children(&mut sub).next();
        let Some(first) = first else { continue };
        if first.kind() != "word" {
            continue;
        }
        // Strip the trailing `=` that tree-sitter folds into the word.
        let raw = slice(first, src).trim_end_matches('=').to_string();
        if raw.is_empty() {
            continue;
        }
        let qn = qn_join(mod_qn, parent_qn, &raw);
        let (ls, le) = line_of(node);
        out.nodes.push(ParsedNode {
            kind: NodeKind::Constant,
            name: raw,
            qualified_name: qn,
            line_start: ls,
            line_end: le,
            parent_qn: parent_qn.map(String::from),
            signature: Some(slice(node, src).lines().next().unwrap_or("").to_string()),
            is_test: false,
        });
    }
}

fn bash_function_body<'a>(node: Node<'a>) -> Option<Node<'a>> {
    // Same borrow-ordering note as [`lua_function_body`].
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| c.kind() == "compound_statement" || c.kind() == "do_group");
    found
}

fn bash_signature(node: Node<'_>, src: &str) -> Option<String> {
    // Everything up to (but not including) the body block — usually
    // just the first line.
    if let Some(body) = bash_function_body(node) {
        let start = node.start_byte();
        let stop = body.start_byte();
        Some(src[start..stop].trim().to_string())
    } else {
        Some(slice(node, src).lines().next().unwrap_or("").to_string())
    }
}

fn bash_command_first_word(node: Node<'_>, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    let mut command_name: Option<Node<'_>> = None;
    for c in node.children(&mut cursor) {
        if c.kind() == "command_name" {
            command_name = Some(c);
            break;
        }
    }
    let cn = command_name?;
    let mut sub = cn.walk();
    for w in cn.children(&mut sub) {
        if w.kind() == "word" {
            return Some(slice(w, src).to_string());
        }
    }
    None
}

fn bash_collect_calls<'a>(node: Node<'a>, src: &str, source_qn: &str, out: &mut ParseResult) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "command" {
            if let Some(name) = bash_command_first_word(child, src) {
                if !name.is_empty() {
                    let (ls, _) = line_of(child);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::Calls,
                        source_qn: source_qn.to_string(),
                        target_qn: name,
                        line: ls,
                        confidence: 0.5,
                        tier: ConfidenceTier::Inferred,
                    });
                }
            }
        }
        bash_collect_calls(child, src, source_qn, out);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Common helpers
// ─────────────────────────────────────────────────────────────────────────

fn field_name(node: &Node<'_>, name: &str, src: &str) -> Option<String> {
    node.child_by_field_name(name)
        .map(|n| slice(n, src).to_string())
}

fn qn_join(mod_qn: &str, parent_qn: Option<&str>, name: &str) -> String {
    if let Some(p) = parent_qn {
        if mod_qn == p || p.starts_with(mod_qn) {
            format!("{p}::{name}")
        } else {
            format!("{mod_qn}::{p}::{name}")
        }
    } else {
        format!("{mod_qn}::{name}")
    }
}

fn collect_calls<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    src: &str,
    source_qn: &str,
    out: &mut ParseResult,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "call_expression" | "call" => {
                let target = child
                    .child_by_field_name("function")
                    .or_else(|| child.child_by_field_name("callee"))
                    .map(|n| slice(n, src).to_string());
                if let Some(target) = target {
                    let (ls, _) = line_of(child);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::Calls,
                        source_qn: source_qn.to_string(),
                        target_qn: target.trim().to_string(),
                        line: ls,
                        confidence: 0.6,
                        tier: ConfidenceTier::Inferred,
                    });
                }
            }
            // L5.13f: JS/TS `new Foo()` / `new Foo.Bar()` is a separate
            // tree-sitter node from `call_expression`. Treat it as a
            // call to the constructor so default-class exports +
            // imported classes light up the call graph.
            "new_expression" => {
                let target = child
                    .child_by_field_name("constructor")
                    .map(|n| slice(n, src).to_string());
                if let Some(target) = target {
                    let (ls, _) = line_of(child);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::Calls,
                        source_qn: source_qn.to_string(),
                        target_qn: target.trim().to_string(),
                        line: ls,
                        confidence: 0.6,
                        tier: ConfidenceTier::Inferred,
                    });
                }
            }
            // Phase 3 / Task G: Lua's `function_call` doesn't expose
            // a named field for the callee — it's the first child
            // before the `arguments` node. Iterate manually until we
            // hit an `identifier` / `dot_index_expression` /
            // `method_index_expression`.
            "function_call" => {
                let mut callee: Option<Node<'a>> = None;
                let mut sub2 = child.walk();
                for c in child.children(&mut sub2) {
                    match c.kind() {
                        "identifier" | "dot_index_expression" | "method_index_expression" => {
                            callee = Some(c);
                            break;
                        }
                        "arguments" => break,
                        _ => {}
                    }
                }
                if let Some(callee) = callee {
                    let (ls, _) = line_of(child);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::Calls,
                        source_qn: source_qn.to_string(),
                        target_qn: slice(callee, src).trim().to_string(),
                        line: ls,
                        confidence: 0.6,
                        tier: ConfidenceTier::Inferred,
                    });
                }
            }
            _ => {}
        }
        let mut sub = child.walk();
        collect_calls(child, &mut sub, src, source_qn, out);
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Rust-specific scoped call collector (L5.6 method-receiver typing).
//
// The generic `collect_calls` emits method-call receivers verbatim, so
// `self.run()` and `client.send()` land in the graph as `self.run` and
// `client.send` — the resolver then gives up because of the `.`. This
// collector carries a per-function [`RustScope`] mapping known receiver
// names to their type. When we see a `field_expression` as the call
// target and the value is `self` or a parameter we recognize, we emit
// `<type>::<method>` instead. Same trick for `Self::foo` and single
// `SomeType::method()` path calls.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct RustScope {
    /// Qualified name of the type owning the current method body, used
    /// to rewrite `self.foo()` / `Self::foo()`.
    self_type: Option<String>,
    /// Parameter (and `let` binding) name → type-name (short form as it
    /// appears in the source; the cross-file resolver promotes the head
    /// to an FQN if it matches an import).
    locals: HashMap<String, String>,
    /// L5.13b: Tuple-typed local name → element type heads.
    /// Tracks `let x = pair()` where `pair() -> (Foo, Bar)` so subsequent
    /// `let (a, b) = x` can bind each element from the stored tuple info.
    locals_tuple: HashMap<String, Vec<String>>,
}

impl RustScope {
    fn new(self_type: Option<String>, params: Option<Node<'_>>, src: &str) -> Self {
        let mut scope = Self {
            self_type,
            locals: HashMap::new(),
            locals_tuple: HashMap::new(),
        };
        if let Some(p) = params {
            scope.seed_from_parameters(p, src);
        }
        scope
    }

    fn seed_from_parameters(&mut self, params: Node<'_>, src: &str) {
        let mut cursor = params.walk();
        for p in params.children(&mut cursor) {
            if p.kind() != "parameter" {
                continue;
            }
            let name = p
                .child_by_field_name("pattern")
                .map(|n| slice(n, src).trim().to_string());
            let ty = p
                .child_by_field_name("type")
                .map(|n| slice(n, src).trim().to_string());
            if let (Some(name), Some(ty)) = (name, ty) {
                let ty = normalize_rust_type(&ty);
                if !ty.is_empty() && !name.is_empty() {
                    self.locals.insert(name, ty);
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// L5.8 — per-file return-type table.
//
// `FileTypes` is built once per Rust file via [`collect_rust_signatures`]
// before the call-collector runs. It lets `let x = make_foo();` and
// `let x = obj.bar()` (and chained `.builder().build()`) infer the type
// of `x` from the file's own function and method signatures, which then
// flows back into the receiver-typing pass so subsequent calls on `x`
// resolve correctly.
//
// Cross-file inference (a method whose return type comes from another
// crate / module) is deliberately out of scope: it would need a
// project-wide pre-pass and a richer type model. The leaf-name fallback
// in `GraphStore::related` still covers those cases.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct FileTypes {
    /// Bare free-function name → return-type head. Examples:
    ///   `fn make_foo() -> Foo` ⇒ `make_foo → Foo`
    ///   `fn lookup() -> Option<Foo>` ⇒ `lookup → Option`
    fn_returns: HashMap<String, String>,
    /// `(impl_type_head, method_name) → return-type head`.
    /// `impl Foo { fn bar(&self) -> Baz {} }` ⇒ `(Foo, bar) → Baz`.
    method_returns: HashMap<(String, String), String>,
    /// L5.9: per-fn list of return-type generic param heads, in
    /// declaration order. Used by [`infer_let_inner_type`] to bind
    /// `Some(x) / Ok(x) / Err(e)` patterns to the matched value's
    /// inner generic param.
    ///
    ///   `fn lookup() -> Option<Foo>`         ⇒ `lookup → ["Foo"]`
    ///   `fn split() -> Result<Foo, Bar>`     ⇒ `split  → ["Foo", "Bar"]`
    fn_returns_generics: HashMap<String, Vec<String>>,
    /// L5.9: per-method list of return-type generic param heads.
    /// `Self` resolves to the impl type the same way `method_returns`
    /// already does.
    method_returns_generics: HashMap<(String, String), Vec<String>>,
    /// L5.9: struct field type lookup driving `let Foo { x, y } = ..`
    /// destructure. `(StructHead, FieldName) → FieldTypeHead`.
    struct_fields: HashMap<(String, String), String>,
    /// L5.10: user-enum variant field sources. `(EnumHead, VariantName)`
    /// → per-position field source, indexed in declaration order.
    /// Only tuple-style variants are modeled here (`enum E { V(T, U) }`).
    /// Struct-style variants are left for a future pass because they
    /// would need a per-variant struct-field table keyed by the
    /// enum-qualified variant name.
    ///
    ///   `enum MyResult<T, E> { Hit(T), Err(E) }` ⇒
    ///     `(MyResult, Hit) → [Generic(0)]`,
    ///     `(MyResult, Err) → [Generic(1)]`.
    ///   `enum Payload { Ping(Header) }` ⇒
    ///     `(Payload, Ping) → [Concrete("Header")]`.
    enum_variants: HashMap<(String, String), Vec<VariantFieldSource>>,
    /// L5.13a: struct-style enum variant fields. Parallel to
    /// [`enum_variants`] but keyed by named field instead of
    /// positional slot:
    ///   `enum Payload { Ping { header: Header } }` ⇒
    ///     `(Payload, Ping) → [("header", Concrete("Header"))]`.
    /// `Generic(idx)` + `Self` resolution mirror the tuple path.
    /// Field order follows the `field_declaration_list` sequence so
    /// `merge_unique` can dedupe deterministically across files.
    enum_struct_variants: HashMap<(String, String), Vec<(String, VariantFieldSource)>>,
    /// L5.10: tuple-typed return lists. `fn pair() -> (Foo, Bar)` ⇒
    /// `pair → ["Foo", "Bar"]`. Drives `let (a, b) = pair()`
    /// destructure when the RHS is a function call rather than a
    /// literal tuple expression.
    fn_returns_tuple: HashMap<String, Vec<String>>,
    /// L5.10: per-method tuple returns. `Self` entries are resolved to
    /// the owning impl type at collection time, mirroring
    /// `method_returns` / `method_returns_generics`.
    method_returns_tuple: HashMap<(String, String), Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum VariantFieldSource {
    /// Variant field that claims the enum's `idx`-th type parameter.
    /// `enum MyResult<T, E> { Hit(T) }` records `Hit → [Generic(0)]`
    /// so `if let Hit(x) = fn_returning_MyResult_A_B()` binds `x` to
    /// the matched value's first inner generic arg (A).
    Generic(usize),
    /// Variant field that uses a concrete (non-generic) type head.
    /// `enum Payload { Ping(Header) }` records
    /// `Ping → [Concrete("Header")]` so `if let Ping(x) = make()`
    /// binds `x` to `Header` regardless of the matched value's
    /// generic args.
    Concrete(String),
}

// ─────────────────────────────────────────────────────────────────────────
// L5.12 — project-wide signature aggregation.
//
// Per-file [`FileTypes`] only knows what's declared in its own source.
// `ProjectFileTypes` accumulates signatures from every file in the
// project with collision detection: when two files disagree on the
// return type of the same bare name (`fn make() -> Foo` in file_a,
// `fn make() -> Bar` in file_b), the key is flagged ambiguous and
// pulled from the aggregate so call-collection never resolves it.
//
// When processing a specific file, `fill_missing` tops up the local
// `FileTypes` with non-ambiguous project entries the local file never
// declared. Local declarations always win — the callers never see
// project-wide entries for names they defined themselves.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Default, Clone, Debug)]
pub struct ProjectFileTypes {
    types: FileTypes,
    fn_returns_ambig: std::collections::HashSet<String>,
    method_returns_ambig: std::collections::HashSet<(String, String)>,
    fn_returns_generics_ambig: std::collections::HashSet<String>,
    method_returns_generics_ambig: std::collections::HashSet<(String, String)>,
    struct_fields_ambig: std::collections::HashSet<(String, String)>,
    enum_variants_ambig: std::collections::HashSet<(String, String)>,
    enum_struct_variants_ambig: std::collections::HashSet<(String, String)>,
    fn_returns_tuple_ambig: std::collections::HashSet<String>,
    method_returns_tuple_ambig: std::collections::HashSet<(String, String)>,
}

impl ProjectFileTypes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold a single file's [`FileTypes`] into the project aggregate.
    /// Values that agree across files are kept; values that disagree
    /// flag the key as ambiguous and drop it from the aggregate so
    /// later `fill_missing` calls skip it.
    pub fn add(&mut self, file: &FileTypes) {
        merge_unique(
            &mut self.types.fn_returns,
            &mut self.fn_returns_ambig,
            &file.fn_returns,
        );
        merge_unique(
            &mut self.types.method_returns,
            &mut self.method_returns_ambig,
            &file.method_returns,
        );
        merge_unique(
            &mut self.types.fn_returns_generics,
            &mut self.fn_returns_generics_ambig,
            &file.fn_returns_generics,
        );
        merge_unique(
            &mut self.types.method_returns_generics,
            &mut self.method_returns_generics_ambig,
            &file.method_returns_generics,
        );
        merge_unique(
            &mut self.types.struct_fields,
            &mut self.struct_fields_ambig,
            &file.struct_fields,
        );
        merge_unique(
            &mut self.types.enum_variants,
            &mut self.enum_variants_ambig,
            &file.enum_variants,
        );
        merge_unique(
            &mut self.types.enum_struct_variants,
            &mut self.enum_struct_variants_ambig,
            &file.enum_struct_variants,
        );
        merge_unique(
            &mut self.types.fn_returns_tuple,
            &mut self.fn_returns_tuple_ambig,
            &file.fn_returns_tuple,
        );
        merge_unique(
            &mut self.types.method_returns_tuple,
            &mut self.method_returns_tuple_ambig,
            &file.method_returns_tuple,
        );
    }

    /// Top up `local` with project entries the file didn't declare
    /// itself. Local keys always win; ambiguous project keys are
    /// skipped entirely.
    pub fn fill_missing(&self, local: &mut FileTypes) {
        fill_missing_map(
            &mut local.fn_returns,
            &self.types.fn_returns,
            &self.fn_returns_ambig,
        );
        fill_missing_map(
            &mut local.method_returns,
            &self.types.method_returns,
            &self.method_returns_ambig,
        );
        fill_missing_map(
            &mut local.fn_returns_generics,
            &self.types.fn_returns_generics,
            &self.fn_returns_generics_ambig,
        );
        fill_missing_map(
            &mut local.method_returns_generics,
            &self.types.method_returns_generics,
            &self.method_returns_generics_ambig,
        );
        fill_missing_map(
            &mut local.struct_fields,
            &self.types.struct_fields,
            &self.struct_fields_ambig,
        );
        fill_missing_map(
            &mut local.enum_variants,
            &self.types.enum_variants,
            &self.enum_variants_ambig,
        );
        fill_missing_map(
            &mut local.enum_struct_variants,
            &self.types.enum_struct_variants,
            &self.enum_struct_variants_ambig,
        );
        fill_missing_map(
            &mut local.fn_returns_tuple,
            &self.types.fn_returns_tuple,
            &self.fn_returns_tuple_ambig,
        );
        fill_missing_map(
            &mut local.method_returns_tuple,
            &self.types.method_returns_tuple,
            &self.method_returns_tuple_ambig,
        );
    }
}

fn merge_unique<K, V>(
    target: &mut HashMap<K, V>,
    ambig: &mut std::collections::HashSet<K>,
    incoming: &HashMap<K, V>,
) where
    K: std::hash::Hash + Eq + Clone,
    V: Clone + PartialEq,
{
    for (k, v) in incoming {
        if ambig.contains(k) {
            continue;
        }
        match target.get(k) {
            Some(existing) if existing != v => {
                ambig.insert(k.clone());
                target.remove(k);
            }
            Some(_) => {}
            None => {
                target.insert(k.clone(), v.clone());
            }
        }
    }
}

fn fill_missing_map<K, V>(
    target: &mut HashMap<K, V>,
    source: &HashMap<K, V>,
    ambig: &std::collections::HashSet<K>,
) where
    K: std::hash::Hash + Eq + Clone,
    V: Clone,
{
    for (k, v) in source {
        if ambig.contains(k) {
            continue;
        }
        target.entry(k.clone()).or_insert_with(|| v.clone());
    }
}

fn collect_rust_signatures(root: Node<'_>, src: &str) -> FileTypes {
    let mut ft = FileTypes::default();
    walk_signatures(root, src, None, &mut ft);
    ft
}

fn walk_signatures(node: Node<'_>, src: &str, impl_type: Option<&str>, ft: &mut FileTypes) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_item" => {
                let name = field_name(&child, "name", src);
                let ret_raw = child
                    .child_by_field_name("return_type")
                    .map(|n| slice(n, src).to_string());
                if let (Some(name), Some(ret_raw)) = (name, ret_raw) {
                    let head = type_head(&ret_raw);
                    // L5.9: also collect each top-level generic param's
                    // head so `Some(x) = make_opt()` can pin `x` to the
                    // matched value's inner T (and `Err(e)` to E).
                    let generics = inner_generic_heads(&ret_raw);
                    // L5.10: tuple-typed return (`-> (Foo, Bar)`) so
                    // `let (a, b) = pair()` can destructure against a
                    // fn-call RHS rather than only literal tuples.
                    let tuple_parts = tuple_type_heads(&ret_raw);
                    if !head.is_empty() {
                        if let Some(it) = impl_type {
                            // `Self` resolves to this impl's type so
                            // `fn new() -> Self` on `impl Foo` records
                            // `(Foo, new) -> Foo` (instead of
                            // `(Foo, new) -> Self` which the resolver
                            // can't act on).
                            let resolved = if head == "Self" { it.to_string() } else { head };
                            ft.method_returns
                                .insert((it.to_string(), name.clone()), resolved);
                        } else {
                            ft.fn_returns.insert(name.clone(), head);
                        }
                    }
                    if !generics.is_empty() {
                        if let Some(it) = impl_type {
                            // Same Self-resolution trick for generics:
                            // `fn opt_self(&self) -> Option<Self>` on
                            // `impl Foo` records `(Foo, opt_self) → ["Foo"]`.
                            let resolved: Vec<String> = generics
                                .into_iter()
                                .map(|g| if g == "Self" { it.to_string() } else { g })
                                .collect();
                            ft.method_returns_generics
                                .insert((it.to_string(), name.clone()), resolved);
                        } else {
                            ft.fn_returns_generics.insert(name.clone(), generics);
                        }
                    }
                    if !tuple_parts.is_empty() {
                        if let Some(it) = impl_type {
                            // Same Self-resolution: `fn split(&self) ->
                            // (Self, Other)` on `impl Foo` records
                            // `(Foo, split) → ["Foo", "Other"]`.
                            let resolved: Vec<String> = tuple_parts
                                .into_iter()
                                .map(|t| if t == "Self" { it.to_string() } else { t })
                                .collect();
                            ft.method_returns_tuple
                                .insert((it.to_string(), name), resolved);
                        } else {
                            ft.fn_returns_tuple.insert(name, tuple_parts);
                        }
                    }
                }
                // Don't recurse into the body: nested fns are rare and
                // their signatures don't affect the outer block's scope.
            }
            "struct_item" => {
                // L5.9: collect each declared field's type so a
                // `let Foo { x, y } = ..` destructure can rebind `x`
                // and `y` to their field types.
                let struct_head = field_name(&child, "name", src)
                    .map(|n| type_head(&n))
                    .filter(|h| !h.is_empty());
                if let Some(struct_head) = struct_head {
                    if let Some(body) = child.child_by_field_name("body") {
                        let mut bcursor = body.walk();
                        for field in body.children(&mut bcursor) {
                            if field.kind() != "field_declaration" {
                                continue;
                            }
                            let fname = field
                                .child_by_field_name("name")
                                .map(|n| slice(n, src).trim().to_string());
                            let ftype = field
                                .child_by_field_name("type")
                                .map(|n| slice(n, src).to_string());
                            if let (Some(fname), Some(ftype)) = (fname, ftype) {
                                let fhead = type_head(&ftype);
                                if !fhead.is_empty() && !fname.is_empty() {
                                    ft.struct_fields.insert((struct_head.clone(), fname), fhead);
                                }
                            }
                        }
                    }
                }
            }
            "enum_item" => {
                // L5.10: collect variant → field-source mapping so
                // `if let MyHit(x) = make_my_result()` can bind `x` via
                // the enum's type-parameter list instead of falling
                // back to the leaf-name resolver.
                let enum_head = field_name(&child, "name", src)
                    .map(|n| type_head(&n))
                    .filter(|h| !h.is_empty());
                let Some(enum_head) = enum_head else {
                    walk_signatures(child, src, impl_type, ft);
                    continue;
                };
                let type_params = child
                    .child_by_field_name("type_parameters")
                    .map(|n| collect_type_param_names(n, src))
                    .unwrap_or_default();
                let Some(body) = child.child_by_field_name("body") else {
                    continue;
                };
                let mut bcursor = body.walk();
                for variant in body.children(&mut bcursor) {
                    if variant.kind() != "enum_variant" {
                        continue;
                    }
                    let Some(v_name) = field_name(&variant, "name", src) else {
                        continue;
                    };
                    // Unit variants (no body) stay unmodeled; there's
                    // nothing to bind. Everything else dispatches on
                    // the body kind: tuple-style → `enum_variants`
                    // (L5.10), struct-style → `enum_struct_variants`
                    // (L5.13a).
                    let Some(v_body) = variant.child_by_field_name("body") else {
                        continue;
                    };
                    match v_body.kind() {
                        "ordered_field_declaration_list" => {
                            // Tree-sitter-rust inlines tuple-variant
                            // fields directly inside the list (no
                            // per-field wrapper node), so we walk the
                            // positional `type` field children.
                            let mut fields = Vec::new();
                            let mut fcursor = v_body.walk();
                            for ftype_node in v_body.children_by_field_name("type", &mut fcursor) {
                                let field_head = type_head(slice(ftype_node, src));
                                if field_head.is_empty() {
                                    continue;
                                }
                                let source = resolve_variant_field_source(
                                    &field_head,
                                    &type_params,
                                    &enum_head,
                                );
                                fields.push(source);
                            }
                            if !fields.is_empty() {
                                ft.enum_variants.insert((enum_head.clone(), v_name), fields);
                            }
                        }
                        "field_declaration_list" => {
                            // Struct-style variant: each child is a
                            // `field_declaration` with `name` + `type`
                            // fields, same shape as a plain struct.
                            let mut fields: Vec<(String, VariantFieldSource)> = Vec::new();
                            let mut fcursor = v_body.walk();
                            for fd in v_body.children(&mut fcursor) {
                                if fd.kind() != "field_declaration" {
                                    continue;
                                }
                                let Some(fname) = field_name(&fd, "name", src) else {
                                    continue;
                                };
                                let Some(ftype_node) = fd.child_by_field_name("type") else {
                                    continue;
                                };
                                let field_head = type_head(slice(ftype_node, src));
                                if field_head.is_empty() || fname.is_empty() {
                                    continue;
                                }
                                let source = resolve_variant_field_source(
                                    &field_head,
                                    &type_params,
                                    &enum_head,
                                );
                                fields.push((fname, source));
                            }
                            if !fields.is_empty() {
                                ft.enum_struct_variants
                                    .insert((enum_head.clone(), v_name), fields);
                            }
                        }
                        _ => continue,
                    }
                }
            }
            "impl_item" => {
                let target = child
                    .child_by_field_name("type")
                    .map(|n| slice(n, src).to_string())
                    .unwrap_or_default();
                let head = type_head(&target);
                if head.is_empty() {
                    walk_signatures(child, src, impl_type, ft);
                    continue;
                }
                if let Some(body) = child.child_by_field_name("body") {
                    walk_signatures(body, src, Some(&head), ft);
                }
            }
            "mod_item" => {
                if let Some(body) = child.child_by_field_name("body") {
                    walk_signatures(body, src, impl_type, ft);
                }
            }
            _ => {
                walk_signatures(child, src, impl_type, ft);
            }
        }
    }
}

fn collect_rust_calls<'a>(
    node: Node<'a>,
    cursor: &mut tree_sitter::TreeCursor<'a>,
    src: &str,
    source_qn: &str,
    scope: &mut RustScope,
    out: &mut ParseResult,
    file_types: &FileTypes,
) {
    for child in node.children(cursor) {
        match child.kind() {
            "let_declaration" => {
                // Learn the binding's type before recursing so subsequent
                // sibling calls (`x.bar()` after `let x = Foo::new();`)
                // can see it. Then recurse into the RHS so any calls on
                // the value side still get emitted.
                learn_from_let(child, src, scope, file_types);
                let mut sub = child.walk();
                collect_rust_calls(child, &mut sub, src, source_qn, scope, out, file_types);
            }
            "call_expression" => {
                if let Some(func) = child.child_by_field_name("function") {
                    let target = resolve_rust_call_target(func, src, scope);
                    let (ls, _) = line_of(child);
                    out.edges.push(ParsedEdge {
                        kind: EdgeKind::Calls,
                        source_qn: source_qn.to_string(),
                        target_qn: target,
                        line: ls,
                        confidence: 0.6,
                        tier: ConfidenceTier::Inferred,
                    });
                }
                let mut sub = child.walk();
                collect_rust_calls(child, &mut sub, src, source_qn, scope, out, file_types);
            }
            // L5.8: `if let Pattern = expr { ... } else { ... }` — pattern
            // bindings live only in the consequence block. Walk the
            // condition itself with the *outer* scope so its calls don't
            // see the new binding either.
            "if_expression" => {
                let cond = child.child_by_field_name("condition");
                let conseq = child.child_by_field_name("consequence");
                let alt = child.child_by_field_name("alternative");
                let mut conseq_scope = scope.clone();
                if let Some(cond) = cond {
                    if cond.kind() == "let_condition" {
                        learn_from_let_condition(cond, src, &mut conseq_scope, file_types);
                    }
                    let mut sub = cond.walk();
                    collect_rust_calls(cond, &mut sub, src, source_qn, scope, out, file_types);
                }
                if let Some(conseq) = conseq {
                    let mut sub = conseq.walk();
                    collect_rust_calls(
                        conseq,
                        &mut sub,
                        src,
                        source_qn,
                        &mut conseq_scope,
                        out,
                        file_types,
                    );
                }
                if let Some(alt) = alt {
                    let mut alt_scope = scope.clone();
                    let mut sub = alt.walk();
                    collect_rust_calls(
                        alt,
                        &mut sub,
                        src,
                        source_qn,
                        &mut alt_scope,
                        out,
                        file_types,
                    );
                }
            }
            // L5.9: `match expr { Pat => ..., ... }` — each arm gets
            // its own scope clone seeded from the arm's pattern, so
            // `Some(x) => x.bar()` resolves inside that arm only. The
            // matched expression itself walks with the outer scope so
            // its calls don't accidentally see arm bindings.
            "match_expression" => {
                let matched = child.child_by_field_name("value");
                if let Some(matched) = matched {
                    let mut sub = matched.walk();
                    collect_rust_calls(matched, &mut sub, src, source_qn, scope, out, file_types);
                }
                if let Some(body) = child.child_by_field_name("body") {
                    let mut bcursor = body.walk();
                    for arm in body.children(&mut bcursor) {
                        if arm.kind() != "match_arm" {
                            continue;
                        }
                        let mut arm_scope = scope.clone();
                        if let (Some(pat), Some(matched)) =
                            (arm.child_by_field_name("pattern"), matched)
                        {
                            learn_from_pattern(pat, matched, src, &mut arm_scope, file_types);
                        }
                        if let Some(arm_value) = arm.child_by_field_name("value") {
                            let mut sub = arm_value.walk();
                            collect_rust_calls(
                                arm_value,
                                &mut sub,
                                src,
                                source_qn,
                                &mut arm_scope,
                                out,
                                file_types,
                            );
                        }
                    }
                }
            }
            // L5.8: `while let Pattern = expr { ... }` — body sees the
            // binding, condition does not.
            "while_expression" => {
                let cond = child.child_by_field_name("condition");
                let body = child.child_by_field_name("body");
                let mut body_scope = scope.clone();
                if let Some(cond) = cond {
                    if cond.kind() == "let_condition" {
                        learn_from_let_condition(cond, src, &mut body_scope, file_types);
                    }
                    let mut sub = cond.walk();
                    collect_rust_calls(cond, &mut sub, src, source_qn, scope, out, file_types);
                }
                if let Some(body) = body {
                    let mut sub = body.walk();
                    collect_rust_calls(
                        body,
                        &mut sub,
                        src,
                        source_qn,
                        &mut body_scope,
                        out,
                        file_types,
                    );
                }
            }
            // Constructs that introduce a new lexical scope: clone the
            // current bindings so `let` declarations inside the inner
            // block don't leak out to siblings of the parent block.
            "block" | "closure_expression" | "function_item" => {
                let mut inner = scope.clone();
                let mut sub = child.walk();
                collect_rust_calls(child, &mut sub, src, source_qn, &mut inner, out, file_types);
            }
            _ => {
                let mut sub = child.walk();
                collect_rust_calls(child, &mut sub, src, source_qn, scope, out, file_types);
            }
        }
    }
}

/// Learn a single `let_declaration` into [`RustScope::locals`].
///
/// Strategy (in priority order):
///   1. For simple-identifier / `let mut` patterns, an explicit type
///      annotation wins: `let x: Foo = expr;` → `Foo`.
///   2. Otherwise dispatch to [`learn_from_pattern`] which handles
///      simple identifiers, tuple destructure (`let (a, b) = ..`),
///      struct destructure (`let Foo { x } = ..`), and the L5.9
///      enum-unwrap shapes via `if let` / `while let` / match arms.
///   3. Inside [`infer_let_type`] (called by `learn_from_pattern`):
///      - `Foo::new(...)` / `Module::Foo::new(...)` → leading head.
///      - `Foo { .. }` / `Module::Foo { .. }` → leading head.
///      - `&expr` / `&mut expr` / `(expr)` / `expr?` → unwrap & recurse.
///      - L5.8: bare-fn call `foo()` → `file_types.fn_returns[foo]`.
///      - L5.8: method call `obj.bar()` → resolve `obj` type then look
///        up `(type, bar)` in `file_types.method_returns`. Chained
///        builders work via the same path because the value side is
///        itself a `call_expression`.
///
/// Anything else is left alone (no entry created), which keeps the
/// leaf-name fallback in `GraphStore::related` as the safety net.
fn learn_from_let(node: Node<'_>, src: &str, scope: &mut RustScope, file_types: &FileTypes) {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };

    // Type annotation only short-circuits for simple-identifier /
    // `let mut x` patterns. Destructure patterns route straight to
    // `learn_from_pattern` so each binding picks up its own type.
    if let Some(name) = pattern_to_simple_name(pattern, src) {
        if let Some(ty_node) = node.child_by_field_name("type") {
            let ty = type_head(slice(ty_node, src));
            if !ty.is_empty() {
                scope.locals.insert(name, ty);
                return;
            }
        }
    }

    if let Some(value) = node.child_by_field_name("value") {
        learn_from_pattern(pattern, value, src, scope, file_types);
    }
}

/// L5.9: bind every name introduced by `pattern` against the type of
/// `value`. Drives `let`, `if let` / `while let`, and `match` arms via
/// a single dispatcher.
///
/// Supported pattern kinds:
///   - `identifier` / `mutable_pattern` — bind to the value's inferred
///     head (delegates to [`infer_let_type`]).
///   - `tuple_pattern` — handles literal `tuple_expression` RHS,
///     tuple-typed local identifiers, and fn/method tuple returns.
///     L5.13d: recursively destructures nested tuples like
///     `let ((a, b), c) = pair_pair()` via `destructure_tuple_elements`.
///   - `struct_pattern` — looks up each `field_pattern`'s field type in
///     `FileTypes::struct_fields` so `let Foo { x, y: alias } = ..`
///     binds `x` and `alias` to their declared field types.
///   - `tuple_struct_pattern` — Rust standard library variants only:
///     `Some(x)` / `Ok(v)` bind to inner generic param 0,
///     `Err(e)` binds to inner generic param 1. Other constructors are
///     intentionally skipped — without a per-enum variant table we
///     can't tell which generic position each variant claims.
///   - `match_pattern` / `or_pattern` — wrappers; transparently drill
///     into alternatives. `or_pattern` takes the intersection of bindings
///     across all alternatives (L5.13c).
fn learn_from_pattern(
    pattern: Node<'_>,
    value: Node<'_>,
    src: &str,
    scope: &mut RustScope,
    file_types: &FileTypes,
) {
    match pattern.kind() {
        "identifier" => {
            let name = slice(pattern, src).trim().to_string();
            if name.is_empty() {
                return;
            }
            // L5.13b: tuple-typed RHS records the per-element heads in
            // `locals_tuple` so a later `let (a, b) = x` destructure can
            // recover the bindings. Tuple-typed fns never populate
            // `fn_returns` (the surface type starts with `(`, which
            // `normalize_rust_type` rejects), so this probe must run
            // independently of `infer_let_type`'s success.
            if let Some(tuple_types) = tuple_return_of(value, src, scope, file_types) {
                scope.locals_tuple.insert(name.clone(), tuple_types);
            }
            if let Some(ty) = infer_let_type(value, src, scope, file_types) {
                scope.locals.insert(name, ty);
            }
        }
        "mutable_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                if child.kind() == "identifier" {
                    let name = slice(child, src).trim().to_string();
                    if name.is_empty() {
                        return;
                    }
                    if let Some(ty) = infer_let_type(value, src, scope, file_types) {
                        scope.locals.insert(name, ty);
                    }
                    return;
                }
            }
        }
        "tuple_pattern" => {
            // Literal tuple RHS: pair up children and recurse so nested
            // destructures (`let ((a, b), c) = ((..), ..)`) still work.
            if value.kind() == "tuple_expression" {
                let pat_children = named_children(pattern);
                let val_children = named_children(value);
                for (pc, vc) in pat_children.iter().zip(val_children.iter()) {
                    learn_from_pattern(*pc, *vc, src, scope, file_types);
                }
                return;
            }
            // L5.13b: tuple-typed local identifier RHS.
            // `let x = pair(); let (a, b) = x` — look up x in locals_tuple.
            // L5.13d: nested tuples like `let ((a, b), c) = x` where x
            // has type `((Foo, Bar), Baz)` are recursively destructured.
            if value.kind() == "identifier" {
                let val_name = slice(value, src).trim();
                if let Some(types) = scope.locals_tuple.get(val_name).cloned() {
                    let pat_children = named_children(pattern);
                    destructure_tuple_elements(&pat_children, types, value, src, scope, file_types);
                    return;
                }
            }
            // L5.10: fall back to the file's fn/method tuple-return
            // table. `let (a, b) = pair()` where `pair() -> (Foo, Bar)`
            // binds each pattern child to its positional type head.
            // L5.13d: nested tuple returns are recursively destructured.
            if let Some(types) = tuple_return_of(value, src, scope, file_types) {
                let pat_children = named_children(pattern);
                destructure_tuple_elements(&pat_children, types, value, src, scope, file_types);
            }
        }
        "struct_pattern" => {
            let Some(ty_node) = pattern.child_by_field_name("type") else {
                return;
            };
            let raw_type = slice(ty_node, src).trim().to_string();
            if raw_type.is_empty() {
                return;
            }
            // L5.13a: try the struct-style-enum-variant path first. The
            // pattern can reach a variant in two shapes:
            //   1. Qualified: `Payload::Ping { header }` — the prefix
            //      before the last `::` is the enum head candidate.
            //   2. Bare: `Ping { header }` when the variant is brought
            //      into scope via `use` — we fall back to the matched
            //      value's inferred type head for the enum candidate.
            // Either candidate that lands in `enum_struct_variants` wins
            // and short-circuits the plain-struct fallback.
            let (enum_prefix, variant_leaf) = match raw_type.rsplit_once("::") {
                Some((prefix, last)) => (Some(prefix.to_string()), last.trim().to_string()),
                None => (None, raw_type.clone()),
            };
            let mut enum_candidates: Vec<String> = Vec::new();
            if let Some(prefix) = &enum_prefix {
                let head = type_head(prefix);
                if !head.is_empty() {
                    enum_candidates.push(head);
                }
            }
            if let Some(v_type) = infer_let_type(value, src, scope, file_types) {
                if !enum_candidates.contains(&v_type) {
                    enum_candidates.push(v_type);
                }
            }

            for enum_head in &enum_candidates {
                let key = (enum_head.clone(), variant_leaf.clone());
                let Some(fields) = file_types.enum_struct_variants.get(&key).cloned() else {
                    continue;
                };
                let mut cursor = pattern.walk();
                for child in pattern.children(&mut cursor) {
                    if child.kind() != "field_pattern" {
                        continue;
                    }
                    let Some(fname_node) = child.child_by_field_name("name") else {
                        continue;
                    };
                    let fname = slice(fname_node, src).trim().to_string();
                    if fname.is_empty() {
                        continue;
                    }
                    let bind_name = match child.child_by_field_name("pattern") {
                        Some(ip) => match pattern_to_simple_name(ip, src) {
                            Some(n) => n,
                            None => continue,
                        },
                        None => fname.clone(),
                    };
                    let Some((_, source)) = fields.iter().find(|(f, _)| f == &fname) else {
                        continue;
                    };
                    let ty = match source {
                        VariantFieldSource::Generic(idx) => {
                            infer_let_inner_type(value, src, scope, file_types, *idx)
                        }
                        VariantFieldSource::Concrete(t) => Some(t.clone()),
                    };
                    if let Some(ty) = ty {
                        scope.locals.insert(bind_name, ty);
                    }
                }
                return;
            }

            // L5.9 fallback: plain struct destructure. `type_head` keeps
            // only the trailing `::` segment so `Mod::Foo { .. }` still
            // looks up `(Foo, field)` in `struct_fields`.
            let struct_head = type_head(&raw_type);
            if struct_head.is_empty() {
                return;
            }
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                if child.kind() != "field_pattern" {
                    continue;
                }
                let Some(fname_node) = child.child_by_field_name("name") else {
                    continue;
                };
                let fname = slice(fname_node, src).trim().to_string();
                if fname.is_empty() {
                    continue;
                }
                let bind_name = match child.child_by_field_name("pattern") {
                    Some(ip) => match pattern_to_simple_name(ip, src) {
                        Some(n) => n,
                        // Wildcards / nested patterns: skip without
                        // emitting a bogus binding.
                        None => continue,
                    },
                    None => fname.clone(),
                };
                if let Some(ftype) = file_types.struct_fields.get(&(struct_head.clone(), fname)) {
                    scope.locals.insert(bind_name, ftype.clone());
                }
            }
        }
        "tuple_struct_pattern" => {
            // The variant constructor's identifier sits in the `type`
            // field; bare-or-scoped paths both end up handled because
            // we keep only the trailing leaf segment.
            let Some(variant_node) = pattern.child_by_field_name("type") else {
                return;
            };
            let raw_variant = slice(variant_node, src).trim().to_string();
            let variant_leaf = raw_variant
                .rsplit("::")
                .next()
                .unwrap_or(&raw_variant)
                .to_string();
            let type_id = pattern.child_by_field_name("type").map(|n| n.id());
            // L5.9 fast path: Rust standard library variants. `Some(x)`
            // / `Ok(v)` / `Err(e)` don't need the user-enum table
            // because `FileTypes::{fn,method}_returns_generics` already
            // carries the matched value's generic args.
            let std_idx = match variant_leaf.as_str() {
                "Some" | "Ok" => Some(0_usize),
                "Err" => Some(1),
                _ => None,
            };
            if let Some(idx) = std_idx {
                let mut cursor = pattern.walk();
                for child in pattern.children(&mut cursor) {
                    if !child.is_named() {
                        continue;
                    }
                    if Some(child.id()) == type_id {
                        continue;
                    }
                    if let Some(name) = pattern_to_simple_name(child, src) {
                        if let Some(ty) = infer_let_inner_type(value, src, scope, file_types, idx) {
                            scope.locals.insert(name, ty);
                        }
                    }
                    return;
                }
                return;
            }
            // L5.10: user-enum variant unwrap. Disambiguate by the
            // matched value's type head: `if let Hit(x) = make()`
            // where `make() -> MyResult<Foo>` looks up
            // `(MyResult, Hit) → [Generic(0)]` and resolves `x` via
            // the matched value's inner generic arg. Unknown variants
            // (typo, cross-file enum not indexed here, tuple-struct
            // patterns that aren't actually enum variants) silently
            // skip, keeping the leaf-name fallback in the resolver.
            let Some(value_head) = infer_let_type(value, src, scope, file_types) else {
                return;
            };
            let Some(variant_fields) = file_types
                .enum_variants
                .get(&(value_head, variant_leaf))
                .cloned()
            else {
                return;
            };
            let mut cursor = pattern.walk();
            let mut field_iter = variant_fields.iter();
            for child in pattern.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                if Some(child.id()) == type_id {
                    continue;
                }
                let Some(field_src) = field_iter.next() else {
                    break;
                };
                let Some(name) = pattern_to_simple_name(child, src) else {
                    continue;
                };
                let ty = match field_src {
                    VariantFieldSource::Generic(idx) => {
                        infer_let_inner_type(value, src, scope, file_types, *idx)
                    }
                    VariantFieldSource::Concrete(t) => Some(t.clone()),
                };
                if let Some(ty) = ty {
                    scope.locals.insert(name, ty);
                }
            }
        }
        "match_pattern" => {
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                if !child.is_named() {
                    continue;
                }
                if child.kind() == "match_arm_guard" {
                    continue;
                }
                learn_from_pattern(child, value, src, scope, file_types);
                return;
            }
        }
        "or_pattern" => {
            // L5.13c: process all alternatives and take the intersection
            // of bindings. Only bindings present in ALL alternatives are
            // promoted to the scope. This handles `None | Some(x)` correctly
            // (no common bindings) and `Ok(x) | Err(x)` correctly (x bound
            // from both arms).
            let alts: Vec<Node<'_>> = {
                let mut cursor = pattern.walk();
                pattern
                    .children(&mut cursor)
                    .filter(|c| c.is_named())
                    .collect()
            };
            if alts.is_empty() {
                return;
            }
            // Collect bindings from each alternative into separate scopes.
            let alt_results: Vec<_> = alts
                .iter()
                .map(|alt| {
                    let mut tmp = RustScope {
                        self_type: scope.self_type.clone(),
                        locals: HashMap::new(),
                        locals_tuple: HashMap::new(),
                    };
                    learn_from_pattern(*alt, value, src, &mut tmp, file_types);
                    (tmp.locals, tmp.locals_tuple)
                })
                .collect();
            // Intersection: only keep bindings present in ALL alternatives.
            if let Some((first_locals, _)) = alt_results.first() {
                for (name, ty) in first_locals {
                    if alt_results.iter().all(|(l, _)| l.get(name) == Some(ty)) {
                        scope.locals.insert(name.clone(), ty.clone());
                    }
                }
            }
            // Same for locals_tuple intersection.
            if let Some((_, first_tuples)) = alt_results.first() {
                for (name, tys) in first_tuples {
                    if alt_results.iter().all(|(_, t)| t.get(name) == Some(tys)) {
                        scope.locals_tuple.insert(name.clone(), tys.clone());
                    }
                }
            }
        }
        _ => {}
    }
}

fn named_children<'a>(node: Node<'a>) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|c| c.is_named())
        .collect()
}

/// L5.13d: Recursively destructure tuple elements, handling nested tuples.
/// For each pattern child paired with its type:
/// - If the type is a nested tuple (starts with `(`) and the pattern child
///   is a `tuple_pattern`, recursively destructure the nested tuple.
/// - If the type is a nested tuple but the pattern child is a simple name,
///   store the nested tuple info in `locals_tuple` for later destructure.
/// - Otherwise, bind the pattern child to the type head.
#[allow(clippy::only_used_in_recursion)]
fn destructure_tuple_elements(
    pat_children: &[Node<'_>],
    types: Vec<String>,
    value: Node<'_>,
    src: &str,
    scope: &mut RustScope,
    file_types: &FileTypes,
) {
    for (pc, ty) in pat_children.iter().zip(types.iter()) {
        if ty.starts_with('(') && ty.ends_with(')') {
            // Nested tuple type: e.g. "(Foo, Bar)"
            if pc.kind() == "tuple_pattern" {
                // Pattern is also a tuple: `let ((a, b), c) = ...`
                // Parse the nested tuple type and recurse.
                let nested_types = tuple_type_heads(ty);
                let nested_pat_children = named_children(*pc);
                destructure_tuple_elements(
                    &nested_pat_children,
                    nested_types,
                    value,
                    src,
                    scope,
                    file_types,
                );
            } else if let Some(name) = pattern_to_simple_name(*pc, src) {
                // Pattern is a simple name: `let x = ...` where the type
                // is a nested tuple. Store in locals_tuple for later
                // destructure via `let (a, b) = x`.
                let nested_types = tuple_type_heads(ty);
                if !nested_types.is_empty() {
                    scope.locals_tuple.insert(name, nested_types);
                }
            }
        } else {
            // Flat type: bind directly.
            if let Some(name) = pattern_to_simple_name(*pc, src) {
                if !ty.is_empty() {
                    scope.locals.insert(name, ty.clone());
                }
            }
        }
    }
}

/// Turn a `let_declaration`'s `pattern` field into a simple variable
/// name. Returns `None` for tuple / struct / wildcard patterns we don't
/// model.
fn pattern_to_simple_name(pattern: Node<'_>, src: &str) -> Option<String> {
    match pattern.kind() {
        "identifier" => Some(slice(pattern, src).trim().to_string()),
        "mutable_pattern" => {
            // `let mut x = ...`: the inner identifier carries the name.
            let mut cursor = pattern.walk();
            for child in pattern.children(&mut cursor) {
                if child.kind() == "identifier" {
                    return Some(slice(child, src).trim().to_string());
                }
            }
            None
        }
        _ => None,
    }
}

/// Infer the type of a `let` RHS expression, returning a single-segment
/// type head suitable for [`RustScope::locals`].
fn infer_let_type(
    expr: Node<'_>,
    src: &str,
    scope: &RustScope,
    file_types: &FileTypes,
) -> Option<String> {
    match expr.kind() {
        "reference_expression" | "unary_expression" => {
            // Strip leading `&`, `&mut`, `*` etc., recurse on the inner
            // expression. The grammar exposes the operand as the last
            // non-token child.
            let mut cursor = expr.walk();
            let mut last = None;
            for c in expr.children(&mut cursor) {
                if !c.is_named() {
                    continue;
                }
                last = Some(c);
            }
            last.and_then(|c| infer_let_type(c, src, scope, file_types))
        }
        "parenthesized_expression" => {
            let mut cursor = expr.walk();
            for c in expr.children(&mut cursor) {
                if c.is_named() {
                    return infer_let_type(c, src, scope, file_types);
                }
            }
            None
        }
        "try_expression" => {
            let inner = expr.child_by_field_name("value").or_else(|| {
                let mut cursor = expr.walk();
                let mut found = None;
                for c in expr.children(&mut cursor) {
                    if c.is_named() {
                        found = Some(c);
                        break;
                    }
                }
                found
            });
            inner.and_then(|c| infer_let_type(c, src, scope, file_types))
        }
        "call_expression" => {
            let func = expr.child_by_field_name("function")?;
            match func.kind() {
                "scoped_identifier" => {
                    // `Foo::new(..)` / `Module::Foo::method(..)`.
                    // Prefer the explicit (Type, method) return; fall
                    // back to the type head so associated-fn calls like
                    // `Foo::new()` (`-> Self`, mapped to Foo in
                    // walk_signatures) plus unknown methods still pin
                    // to Foo — matching the prior behaviour while
                    // letting non-`new` methods (e.g. `Foo::builder()`)
                    // resolve to their actual return type.
                    let path = func.child_by_field_name("path")?;
                    let name_node = func.child_by_field_name("name")?;
                    let path_text = slice(path, src).trim();
                    let name_text = slice(name_node, src).trim();
                    if is_rust_path_keyword_let(path_text) {
                        return None;
                    }
                    let head = type_head(path_text);
                    if head.is_empty() {
                        return None;
                    }
                    if let Some(ret) = file_types
                        .method_returns
                        .get(&(head.clone(), name_text.to_string()))
                    {
                        return Some(ret.clone());
                    }
                    Some(head)
                }
                "identifier" => {
                    // L5.8: bare-fn call `make_foo()` → look up the
                    // file's fn_returns map.
                    let name = slice(func, src).trim();
                    file_types.fn_returns.get(name).cloned()
                }
                "field_expression" => {
                    // L5.8: method call `obj.bar()` — resolve receiver
                    // type, then look up `(type, bar)` in the file's
                    // method_returns map. Chained calls work because
                    // `receiver_type_of` recurses into nested
                    // `call_expression` nodes.
                    let recv = func.child_by_field_name("value")?;
                    let field = func.child_by_field_name("field")?;
                    let method_name = slice(field, src).trim().to_string();
                    let recv_type = receiver_type_of(recv, src, scope, file_types)?;
                    file_types
                        .method_returns
                        .get(&(recv_type, method_name))
                        .cloned()
                }
                _ => None,
            }
        }
        "struct_expression" => {
            let name = expr.child_by_field_name("name")?;
            let raw = slice(name, src).trim();
            let head = type_head(raw);
            (!head.is_empty()).then_some(head)
        }
        _ => None,
    }
}

/// Best-effort return of a method-call's *receiver* type. Used by
/// [`infer_let_type`] when chasing chained method calls
/// (`obj.builder().build()`).
fn receiver_type_of(
    value: Node<'_>,
    src: &str,
    scope: &RustScope,
    file_types: &FileTypes,
) -> Option<String> {
    match value.kind() {
        "self" => scope.self_type.as_deref().map(type_head),
        "identifier" => {
            let name = slice(value, src).trim();
            if name == "self" {
                return scope.self_type.as_deref().map(type_head);
            }
            scope.locals.get(name).cloned()
        }
        // Wrappers strip transparently.
        "reference_expression"
        | "unary_expression"
        | "parenthesized_expression"
        | "try_expression" => infer_let_type(value, src, scope, file_types),
        // Chained method call or constructor — let `infer_let_type`
        // figure it out (it already understands `Foo::new(..)` and
        // method-call return-type lookup).
        "call_expression" | "struct_expression" => infer_let_type(value, src, scope, file_types),
        _ => None,
    }
}

/// Learn a `let_condition` (the `let pat = expr` head of `if let` /
/// `while let`) into the consequence/body scope. Dispatches to
/// [`learn_from_pattern`] so the L5.9 destructure shapes
/// (`Some(x)` / `Ok(v)` / `Err(e)`, struct destructure, tuple
/// destructure) all light up here too.
fn learn_from_let_condition(
    cond: Node<'_>,
    src: &str,
    scope: &mut RustScope,
    file_types: &FileTypes,
) {
    let Some(pattern) = cond.child_by_field_name("pattern") else {
        return;
    };
    let Some(value) = cond.child_by_field_name("value") else {
        return;
    };
    learn_from_pattern(pattern, value, src, scope, file_types);
}

/// L5.9: like [`infer_let_type`] but returns the `idx`-th inner generic
/// parameter of the value's type rather than the type's own head.
/// Drives `Some(x) = make_opt()` (idx 0) and `Err(e) = make_res()`
/// (idx 1) bindings.
///
/// Returns `None` when the value's type isn't a generic wrapper, when
/// the wrapper has fewer than `idx + 1` parameters, or when the value
/// expression isn't one of the recognised producer kinds. The producer
/// list mirrors [`infer_let_type`] exactly so any expression that
/// works for `let x = expr` also works under `Some(x) = expr`.
fn infer_let_inner_type(
    expr: Node<'_>,
    src: &str,
    scope: &RustScope,
    file_types: &FileTypes,
    idx: usize,
) -> Option<String> {
    match expr.kind() {
        "reference_expression" | "unary_expression" => {
            let mut cursor = expr.walk();
            let mut last = None;
            for c in expr.children(&mut cursor) {
                if !c.is_named() {
                    continue;
                }
                last = Some(c);
            }
            last.and_then(|c| infer_let_inner_type(c, src, scope, file_types, idx))
        }
        "parenthesized_expression" => {
            let mut cursor = expr.walk();
            for c in expr.children(&mut cursor) {
                if c.is_named() {
                    return infer_let_inner_type(c, src, scope, file_types, idx);
                }
            }
            None
        }
        "try_expression" => {
            let inner = expr.child_by_field_name("value").or_else(|| {
                let mut cursor = expr.walk();
                let mut found = None;
                for c in expr.children(&mut cursor) {
                    if c.is_named() {
                        found = Some(c);
                        break;
                    }
                }
                found
            });
            inner.and_then(|c| infer_let_inner_type(c, src, scope, file_types, idx))
        }
        "call_expression" => {
            let func = expr.child_by_field_name("function")?;
            match func.kind() {
                "scoped_identifier" => {
                    let path = func.child_by_field_name("path")?;
                    let name_node = func.child_by_field_name("name")?;
                    let path_text = slice(path, src).trim();
                    let name_text = slice(name_node, src).trim();
                    if is_rust_path_keyword_let(path_text) {
                        return None;
                    }
                    let head = type_head(path_text);
                    if head.is_empty() {
                        return None;
                    }
                    file_types
                        .method_returns_generics
                        .get(&(head, name_text.to_string()))
                        .and_then(|g| g.get(idx))
                        .cloned()
                }
                "identifier" => {
                    let name = slice(func, src).trim();
                    file_types
                        .fn_returns_generics
                        .get(name)
                        .and_then(|g| g.get(idx))
                        .cloned()
                }
                "field_expression" => {
                    let recv = func.child_by_field_name("value")?;
                    let field = func.child_by_field_name("field")?;
                    let method_name = slice(field, src).trim().to_string();
                    let recv_type = receiver_type_of(recv, src, scope, file_types)?;
                    file_types
                        .method_returns_generics
                        .get(&(recv_type, method_name))
                        .and_then(|g| g.get(idx))
                        .cloned()
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn is_rust_path_keyword_let(s: &str) -> bool {
    matches!(s, "self" | "Self" | "super" | "crate")
}

/// Reduce a Rust type expression to a single bare segment usable as a
/// resolver head. Strips refs / lifetimes / generics via
/// [`normalize_rust_type`], then keeps only the trailing `::` segment so
/// downstream `Head::method` promotion can match a single import.
fn type_head(raw: &str) -> String {
    let normalized = normalize_rust_type(raw);
    if normalized.is_empty() {
        return String::new();
    }
    normalized
        .rsplit("::")
        .next()
        .unwrap_or(&normalized)
        .to_string()
}

/// L5.9: extract the head of every top-level generic parameter from a
/// Rust type expression. Splits the outermost `<...>` group on commas
/// while respecting nested `<...>` so multi-arg wrappers stay intact.
///
///   "Option<Foo>"              → ["Foo"]
///   "Result<Foo, Bar>"         → ["Foo", "Bar"]
///   "HashMap<String, Vec<u8>>" → ["String", "Vec"]
///   "Foo"                      → []
fn inner_generic_heads(raw: &str) -> Vec<String> {
    let s = raw.trim();
    let Some(lt) = s.find('<') else {
        return Vec::new();
    };
    let Some(gt) = s.rfind('>') else {
        return Vec::new();
    };
    if gt <= lt {
        return Vec::new();
    }
    let inner = &s[lt + 1..gt];

    let mut depth: i32 = 0;
    let mut start = 0_usize;
    let mut parts: Vec<String> = Vec::new();
    for (i, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(inner[start..i].trim().to_string());
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(inner[start..].trim().to_string());
    parts
        .into_iter()
        .map(|seg| type_head(&seg))
        .filter(|h| !h.is_empty())
        .collect()
}

/// L5.10: parse a parenthesised tuple type like `(Foo, Bar, Vec<u8>)`
/// into a list of type heads. Returns an empty vec for non-tuple
/// expressions so callers can treat it as a cheap probe.
///
///   "(Foo, Bar)"               → ["Foo", "Bar"]
///   "(Foo,)"                   → ["Foo"]
///   "(Vec<u8>, HashMap<K, V>)" → ["Vec", "HashMap"]
///   "Foo"                      → []
///   "()"                       → []
fn tuple_type_heads(raw: &str) -> Vec<String> {
    let s = raw.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return Vec::new();
    }
    // Strip the outermost parens. Byte-indexing is safe: `(` and `)`
    // are single-byte ASCII.
    let inner = &s[1..s.len() - 1];
    if inner.trim().is_empty() {
        return Vec::new();
    }
    let mut depth: i32 = 0;
    let mut start = 0_usize;
    let mut parts: Vec<String> = Vec::new();
    for (i, ch) in inner.char_indices() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(inner[start..i].trim().to_string());
                start = i + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(inner[start..].trim().to_string());
    // L5.13d: preserve nested tuples as-is (e.g. "(Foo, Bar)") so
    // `tuple_pattern` can recursively destructure them. For flat
    // types, extract the type head as before.
    parts
        .into_iter()
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let trimmed = seg.trim();
            if trimmed.starts_with('(') && trimmed.ends_with(')') {
                // Nested tuple: preserve the full string for recursive
                // destructure in `tuple_pattern`.
                trimmed.to_string()
            } else {
                type_head(&seg)
            }
        })
        .filter(|h| !h.is_empty())
        .collect()
}

/// L5.10: extract the generic type-parameter names of a
/// `type_parameters` node in declaration order, skipping lifetimes and
/// const parameters (which don't participate in the generic-arg list
/// parsed by [`inner_generic_heads`]).
///
///   `<T>`                         → ["T"]
///   `<T, E>`                      → ["T", "E"]
///   `<'a, T: Clone, const N: usize>` → ["T"]
fn collect_type_param_names(type_params: Node<'_>, src: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = type_params.walk();
    for child in type_params.children(&mut cursor) {
        if !child.is_named() {
            continue;
        }
        let name = match child.kind() {
            "type_identifier" => Some(slice(child, src).trim().to_string()),
            "constrained_type_parameter" => child
                .child_by_field_name("left")
                .map(|n| slice(n, src).trim().to_string()),
            "optional_type_parameter" => child
                .child_by_field_name("name")
                .map(|n| slice(n, src).trim().to_string()),
            // Lifetimes, const_parameter, metavariable — not counted
            // against the generic-arg index.
            _ => None,
        };
        if let Some(n) = name.filter(|s| !s.is_empty()) {
            names.push(n);
        }
    }
    names
}

/// L5.10/L5.13a: classify a variant-field type head against the
/// enclosing enum's type-parameter list.
///
/// - `Self` resolves to the enum's own name so `enum Tree { Node(Self) }`
///   records `Node → Concrete("Tree")` instead of a literal "Self".
/// - Heads matching a declared type param → `Generic(idx)`.
/// - Anything else → `Concrete(head)`.
///
/// Shared between the tuple-style (`ordered_field_declaration_list`)
/// and struct-style (`field_declaration_list`) collection paths so
/// both stay in lockstep.
fn resolve_variant_field_source(
    field_head: &str,
    type_params: &[String],
    enum_head: &str,
) -> VariantFieldSource {
    if field_head == "Self" {
        VariantFieldSource::Concrete(enum_head.to_string())
    } else if let Some(idx) = type_params.iter().position(|p| p == field_head) {
        VariantFieldSource::Generic(idx)
    } else {
        VariantFieldSource::Concrete(field_head.to_string())
    }
}

/// L5.10: if `value` is a call whose return type is a tuple, return
/// the list of tuple-element type heads. Drives the
/// `let (a, b) = pair()` destructure path when the RHS isn't a
/// literal `tuple_expression`.
///
/// Transparently strips `&` / `&mut` / `(..)` / `?` wrappers before
/// inspecting the call, matching the shape of [`infer_let_type`].
fn tuple_return_of(
    value: Node<'_>,
    src: &str,
    scope: &RustScope,
    file_types: &FileTypes,
) -> Option<Vec<String>> {
    match value.kind() {
        "reference_expression" | "unary_expression" => {
            let mut cursor = value.walk();
            let mut last = None;
            for c in value.children(&mut cursor) {
                if !c.is_named() {
                    continue;
                }
                last = Some(c);
            }
            last.and_then(|c| tuple_return_of(c, src, scope, file_types))
        }
        "parenthesized_expression" => {
            let mut cursor = value.walk();
            for c in value.children(&mut cursor) {
                if c.is_named() {
                    return tuple_return_of(c, src, scope, file_types);
                }
            }
            None
        }
        "try_expression" => {
            let inner = value.child_by_field_name("value").or_else(|| {
                let mut cursor = value.walk();
                let mut found = None;
                for c in value.children(&mut cursor) {
                    if c.is_named() {
                        found = Some(c);
                        break;
                    }
                }
                found
            });
            inner.and_then(|c| tuple_return_of(c, src, scope, file_types))
        }
        "call_expression" => {
            let func = value.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => {
                    let name = slice(func, src).trim();
                    file_types.fn_returns_tuple.get(name).cloned()
                }
                "scoped_identifier" => {
                    let path = func.child_by_field_name("path")?;
                    let name_node = func.child_by_field_name("name")?;
                    let path_text = slice(path, src).trim();
                    let name_text = slice(name_node, src).trim();
                    if is_rust_path_keyword_let(path_text) {
                        return None;
                    }
                    let head = type_head(path_text);
                    if head.is_empty() {
                        return None;
                    }
                    file_types
                        .method_returns_tuple
                        .get(&(head, name_text.to_string()))
                        .cloned()
                }
                "field_expression" => {
                    let recv = func.child_by_field_name("value")?;
                    let field = func.child_by_field_name("field")?;
                    let method_name = slice(field, src).trim().to_string();
                    let recv_type = receiver_type_of(recv, src, scope, file_types)?;
                    file_types
                        .method_returns_tuple
                        .get(&(recv_type, method_name))
                        .cloned()
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn resolve_rust_call_target(func: Node<'_>, src: &str, scope: &RustScope) -> String {
    match func.kind() {
        "field_expression" => {
            let value = func.child_by_field_name("value");
            let field = func.child_by_field_name("field");
            let (Some(value), Some(field)) = (value, field) else {
                return slice(func, src).trim().to_string();
            };
            let value_text = slice(value, src).trim();
            let field_text = slice(field, src).trim();
            if value_text == "self" {
                if let Some(ty) = &scope.self_type {
                    return format!("{ty}::{field_text}");
                }
            }
            if let Some(ty) = scope.locals.get(value_text) {
                return format!("{ty}::{field_text}");
            }
            // Fall back to the raw slice so existing behaviour (and the
            // leaf-name fallback) still produces something searchable.
            slice(func, src).trim().to_string()
        }
        "scoped_identifier" => {
            let path = func.child_by_field_name("path");
            let name = func.child_by_field_name("name");
            let (Some(path), Some(name)) = (path, name) else {
                return slice(func, src).trim().to_string();
            };
            let path_text = slice(path, src).trim();
            let name_text = slice(name, src).trim();
            if path_text == "Self" {
                if let Some(ty) = &scope.self_type {
                    return format!("{ty}::{name_text}");
                }
            }
            format!("{path_text}::{name_text}")
        }
        _ => slice(func, src).trim().to_string(),
    }
}

/// Normalise a Rust type expression to its leading type name.
///
/// Strips leading `&`, `mut`, a lifetime (e.g. `'a`), and any trailing
/// generic suffix so `&'a mut Vec<u8>` → `Vec`. This keeps the call
/// target compact enough for the resolver's single-`::` import rewrite
/// to fire on the next pass.
fn normalize_rust_type(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        if let Some(rest) = s.strip_prefix('&') {
            s = rest.trim_start();
            continue;
        }
        if let Some(rest) = s.strip_prefix("mut ") {
            s = rest.trim_start();
            continue;
        }
        if let Some(rest) = s.strip_prefix('\'') {
            // lifetime + whitespace
            if let Some(idx) = rest.find(char::is_whitespace) {
                s = rest[idx..].trim_start();
                continue;
            }
            return String::new();
        }
        break;
    }
    if let Some(idx) = s.find('<') {
        s = s[..idx].trim_end();
    }
    // For tuple / fn / impl / dyn and other non-nominal types, skip.
    if s.starts_with('(')
        || s.starts_with("fn ")
        || s.starts_with("fn(")
        || s.starts_with("impl ")
        || s.starts_with("dyn ")
        || s.starts_with('[')
        || s.is_empty()
    {
        return String::new();
    }
    // Reject anything that still carries weird punctuation we don't model.
    if s.contains(' ') || s.contains('(') || s.contains('[') {
        return String::new();
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rust_function_extracted() {
        let src = "fn answer() -> i32 { 42 }\n";
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        assert!(r
            .nodes
            .iter()
            .any(|n| n.name == "answer" && matches!(n.kind, NodeKind::Function)));
    }

    #[test]
    fn rust_test_attr_marks_is_test() {
        let src = "#[test]\nfn it_works() { assert!(true); }\n";
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let n = r.nodes.iter().find(|n| n.name == "it_works").unwrap();
        assert!(n.is_test);
    }

    #[test]
    fn python_class_and_method() {
        let src = "class Foo:\n    def bar(self):\n        return 1\n";
        let r = parse(Language::Python, src, &PathBuf::from("foo.py"));
        assert!(r.nodes.iter().any(|n| n.name == "Foo"));
        assert!(r
            .nodes
            .iter()
            .any(|n| n.name == "bar" && matches!(n.kind, NodeKind::Method)));
    }

    #[test]
    fn js_function_call_emits_edge() {
        let src = "function main() { greet('hi'); }\n";
        let r = parse(Language::JavaScript, src, &PathBuf::from("a.js"));
        assert!(r.nodes.iter().any(|n| n.name == "main"));
        assert!(r
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Calls) && e.target_qn == "greet"));
    }

    // ─── L5.13e: JS/TS default-export aliasing ───────────────────────────

    #[test]
    fn js_export_default_named_function_emits_node_and_edge() {
        let src = "export default function bar() { return 1; }\n";
        let r = parse(Language::TypeScript, src, &PathBuf::from("src/x.ts"));
        // The inner function declaration should still be extracted as
        // `src/x::bar` so the symbol is reachable.
        let bar = r.nodes.iter().find(|n| n.name == "bar").expect("bar node");
        assert_eq!(bar.qualified_name, "src/x::bar");
        // ExportsDefault edge points at the real symbol.
        let default_edge = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::ExportsDefault))
            .expect("ExportsDefault edge");
        assert_eq!(default_edge.source_qn, "src/x:file");
        assert_eq!(default_edge.target_qn, "src/x::bar");
    }

    #[test]
    fn js_export_default_class_emits_node_and_edge() {
        let src = "export default class Bar {}\n";
        let r = parse(Language::TypeScript, src, &PathBuf::from("src/x.ts"));
        assert!(r
            .nodes
            .iter()
            .any(|n| n.qualified_name == "src/x::Bar" && matches!(n.kind, NodeKind::Class)));
        let default_edge = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::ExportsDefault))
            .expect("ExportsDefault edge");
        assert_eq!(default_edge.target_qn, "src/x::Bar");
    }

    #[test]
    fn js_export_default_identifier_resolves_to_local_fqn() {
        // `function foo() {}; export default foo` — the identifier
        // form means the file-local resolver has to upgrade `foo` to
        // `src/x::foo`.
        let src = "function foo() {}\nexport default foo;\n";
        let r = parse(Language::TypeScript, src, &PathBuf::from("src/x.ts"));
        let default_edge = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::ExportsDefault))
            .expect("ExportsDefault edge");
        assert_eq!(default_edge.target_qn, "src/x::foo");
    }

    #[test]
    fn js_export_default_anonymous_synthesizes_default_node() {
        // Anonymous default expression → synthesize `{mod_qn}::default`
        // so the edge has somewhere to land.
        let src = "export default function () { return 1; };\n";
        let r = parse(Language::JavaScript, src, &PathBuf::from("src/x.js"));
        assert!(r
            .nodes
            .iter()
            .any(|n| n.qualified_name == "src/x::default" && n.name == "default"));
        let default_edge = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::ExportsDefault))
            .expect("ExportsDefault edge");
        assert_eq!(default_edge.target_qn, "src/x::default");
    }

    #[test]
    fn js_export_named_function_emits_no_default_edge() {
        // `export function foo()` is a named export — the existing
        // walker should still extract `foo` as a normal node and we
        // must not emit a stray ExportsDefault edge.
        let src = "export function foo() { return 1; }\n";
        let r = parse(Language::TypeScript, src, &PathBuf::from("src/x.ts"));
        assert!(r.nodes.iter().any(|n| n.qualified_name == "src/x::foo"));
        assert!(!r
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::ExportsDefault)));
    }

    #[test]
    fn js_new_expression_emits_calls_edge() {
        // L5.13f: `new Foo()` should land in the graph as a CALLS
        // edge so constructor invocations participate in callgraph
        // queries the same way function calls do.
        let src = "function main() { const x = new Foo(); }\n";
        let r = parse(Language::JavaScript, src, &PathBuf::from("src/x.js"));
        assert!(r
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Calls) && e.target_qn == "Foo"));
    }

    #[test]
    fn js_new_expression_member_constructor_keeps_path() {
        // `new ns.Foo()` should preserve the dotted constructor as
        // the call target so the resolver / leaf-segment fallback
        // can still match it.
        let src = "function main() { return new ns.Foo(); }\n";
        let r = parse(Language::TypeScript, src, &PathBuf::from("src/x.ts"));
        assert!(r
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Calls) && e.target_qn == "ns.Foo"));
    }

    #[test]
    fn rust_impl_block_methods_attach_to_type() {
        let src = "struct Foo;\nimpl Foo { fn bar(&self) -> i32 { 1 } }\n";
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let m = r.nodes.iter().find(|n| n.name == "bar").unwrap();
        assert!(matches!(m.kind, NodeKind::Method));
        assert!(m.qualified_name.contains("Foo"));
    }

    #[test]
    fn rust_self_call_rewrites_receiver_to_type() {
        let src = r#"
            struct Foo;
            impl Foo {
                fn bar(&self) -> i32 { self.baz() }
                fn baz(&self) -> i32 { 1 }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::baz")),
            "expected self.baz() to resolve to Foo::baz, got {:?}",
            call_targets
        );
        assert!(!call_targets.contains(&"self.baz"));
    }

    #[test]
    #[allow(non_snake_case)] // mirrors the `Self` keyword under test
    fn rust_Self_path_call_rewrites_to_type() {
        let src = r#"
            struct Foo;
            impl Foo {
                fn bar(x: i32) -> i32 { Self::baz(x) + 1 }
                fn baz(x: i32) -> i32 { x }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        assert!(r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .any(|e| e.target_qn.ends_with("::Foo::baz")));
    }

    #[test]
    fn rust_param_method_call_rewrites_via_scope() {
        // `client.send()` should resolve to `Client::send` because the
        // function's parameter list binds `client: &mut Client`.
        let src = r#"
            struct Client;
            impl Client { fn send(&mut self, msg: &str) {} }
            fn dispatch(client: &mut Client, msg: &str) {
                client.send(msg);
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        // The extractor emits `Client::send` and the resolver's
        // Head-qualification pass then promotes `Client` to its local
        // FQN, so the final target ends with `::Client::send`.
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Client::send")),
            "expected ...::Client::send, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_call_resolves_to_type() {
        // `let client = Client::new(); client.send();` should emit a
        // CALLS edge pointing at `Client::send`, not `client.send`.
        let src = r#"
            struct Client;
            impl Client {
                fn new() -> Self { Client }
                fn send(&self) {}
            }
            fn dispatch() {
                let client = Client::new();
                client.send();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Client::send")),
            "expected ...::Client::send, got {:?}",
            call_targets
        );
        assert!(
            !call_targets.contains(&"client.send"),
            "raw client.send should have been rewritten, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_struct_literal_resolves_to_type() {
        // `let cfg = Config { x: 1 }; cfg.run();` → `Config::run`.
        let src = r#"
            struct Config { x: i32 }
            impl Config { fn run(&self) {} }
            fn main() {
                let cfg = Config { x: 1 };
                cfg.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Config::run")),
            "expected ...::Config::run, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_with_type_annotation_resolves() {
        // `let x: Foo = make(); x.bar();` → `Foo::bar`. Type annotation
        // wins even though the RHS is a bare-name call we can't infer.
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn make() -> Foo { Foo }
            fn run() {
                let x: Foo = make();
                x.bar();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_mut_binding_resolves() {
        // `let mut x = Foo::new();` should still bind `x → Foo`.
        let src = r#"
            struct Foo;
            impl Foo {
                fn new() -> Self { Foo }
                fn poke(&mut self) {}
            }
            fn run() {
                let mut x = Foo::new();
                x.poke();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::poke")),
            "expected ...::Foo::poke, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_does_not_leak_out_of_inner_block() {
        // Binding `x` lives only inside the `if` arm. After the arm,
        // the same name should NOT carry the inferred type — so a
        // shadow `x.bar()` outside stays as `x.bar` (handled by the
        // leaf-name fallback, not the receiver-typing pass).
        let src = r#"
            struct Foo;
            impl Foo {
                fn new() -> Self { Foo }
                fn bar(&self) {}
            }
            fn run(cond: bool) {
                if cond {
                    let x = Foo::new();
                    x.bar();
                }
                x.bar();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<String> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.clone())
            .collect();
        // The inner-arm call resolved.
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected inner-arm Foo::bar, got {:?}",
            call_targets
        );
        // The outer call kept the bare `x.bar` form (no leak).
        assert!(
            call_targets.iter().any(|t| t == "x.bar"),
            "expected raw x.bar to survive outer block, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_strips_reference_in_value() {
        // `let x = &Foo::new(); x.bar();` — leading `&` should be
        // stripped while inferring the type.
        let src = r#"
            struct Foo;
            impl Foo {
                fn new() -> Self { Foo }
                fn bar(&self) {}
            }
            fn run() {
                let x = &Foo::new();
                x.bar();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_ignores_self_constructor() {
        // `let x = Self::new();` — Self isn't a usable type head from
        // outside an `impl`, but here the let learner is called inside
        // a free fn so `Self` shouldn't cause a bogus `Self::method`
        // entry to leak.
        let src = r#"
            fn run() {
                struct Inner;
                impl Inner { fn make() -> Self { Inner } }
                let _x = Inner::make();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        // We just want this to parse + resolve cleanly without panic
        // and without producing a `Self::method` synthetic call edge.
        assert!(r.edges.iter().all(|e| !e.target_qn.starts_with("Self::")));
    }

    // ─────────────────────────────────────────────────────────────────
    // L5.8: RHS method-call & free-fn return-type inference + if-let.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn rust_let_binding_bare_fn_call_resolves_via_return_type() {
        // `fn make() -> Foo {}; let x = make(); x.bar();` should now
        // resolve to `Foo::bar` thanks to the per-file fn_returns map.
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn make() -> Foo { Foo }
            fn run() {
                let x = make();
                x.bar();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar, got {:?}",
            call_targets
        );
        assert!(
            !call_targets.contains(&"x.bar"),
            "raw x.bar should have been rewritten, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_method_call_resolves_via_return_type() {
        // `let bar = foo.make_bar(); bar.process();` — receiver `foo`
        // typed via parameter binding, method's return type pulled
        // from the method_returns map.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn make_bar(&self) -> Bar { Bar } }
            impl Bar { fn process(&self) {} }
            fn run(foo: &Foo) {
                let bar = foo.make_bar();
                bar.process();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::process")),
            "expected ...::Bar::process, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_chained_method_call_resolves() {
        // `let built = Foo::builder().build();` — chained method call.
        // `Foo::builder()` returns Builder, `Builder::build()` returns
        // Built. So `built: Built` and `built.run()` → `Built::run`.
        let src = r#"
            struct Foo;
            struct Builder;
            struct Built;
            impl Foo { fn builder() -> Builder { Builder } }
            impl Builder { fn build(self) -> Built { Built } }
            impl Built { fn run(&self) {} }
            fn run() {
                let built = Foo::builder().build();
                built.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Built::run")),
            "expected ...::Built::run, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_simple_identifier_binds_in_consequence() {
        // `if let foo = make_foo() { foo.bar(); }` — simple identifier
        // pattern (uncommon but valid) binds inside the consequence.
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn make_foo() -> Foo { Foo }
            fn run() {
                if let foo = make_foo() {
                    foo.bar();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar inside consequence, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_binding_does_not_leak_to_alternative() {
        // The if-let binding is visible only inside the consequence.
        // The else branch references a same-named free `foo` (here a
        // fn) so the call should NOT be rewritten via the (absent)
        // local binding.
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn make_foo() -> Foo { Foo }
            fn foo() {}
            fn run(cond: bool) {
                if cond {
                    if let foo = make_foo() {
                        foo.bar();
                    }
                } else {
                    foo();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        // Inner binding fired.
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar in consequence, got {:?}",
            call_targets
        );
        // The else-branch `foo()` resolved to the free fn, NOT to
        // `Foo::foo` (which would only happen if the binding leaked
        // and was misinterpreted).
        assert!(
            !call_targets.iter().any(|t| t.contains("Foo::foo")),
            "else-branch foo() should not promote via inner binding, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_while_let_simple_identifier_binds_in_body() {
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn make_foo() -> Foo { Foo }
            fn run() {
                while let foo = make_foo() {
                    foo.bar();
                    break;
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar inside while-let body, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_collect_signatures_populates_fn_and_method_returns() {
        // White-box: confirm the FileTypes pre-pass actually picks up
        // free fn and impl-method return types so the rest of the
        // pipeline has data to work with.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo {
                fn make_bar(&self) -> Bar { Bar }
                fn helper(&self) {}
            }
            fn make_foo() -> Foo { Foo }
            fn no_return() {}
        "#;
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::language());
        let tree = parser.parse(src, None).expect("parse");
        let ft = collect_rust_signatures(tree.root_node(), src);
        assert_eq!(
            ft.fn_returns.get("make_foo").map(String::as_str),
            Some("Foo")
        );
        // Methods without an explicit return type stay out of the map.
        assert!(!ft.fn_returns.contains_key("no_return"));
        assert_eq!(
            ft.method_returns
                .get(&("Foo".to_string(), "make_bar".to_string()))
                .map(String::as_str),
            Some("Bar")
        );
        assert!(!ft
            .method_returns
            .contains_key(&("Foo".to_string(), "helper".to_string())));
    }

    #[test]
    fn normalize_rust_type_strips_refs_lifetimes_generics() {
        assert_eq!(normalize_rust_type("Foo"), "Foo");
        assert_eq!(normalize_rust_type("&Foo"), "Foo");
        assert_eq!(normalize_rust_type("&mut Foo"), "Foo");
        assert_eq!(normalize_rust_type("&'a mut Foo"), "Foo");
        assert_eq!(normalize_rust_type("Vec<u8>"), "Vec");
        assert_eq!(normalize_rust_type("HashMap<String, T>"), "HashMap");
        assert_eq!(normalize_rust_type("  &'static  str "), "str");
        assert_eq!(normalize_rust_type(""), "");
        assert_eq!(normalize_rust_type("(i32, i32)"), "");
        assert_eq!(normalize_rust_type("fn() -> i32"), "");
        assert_eq!(normalize_rust_type("impl Iterator<Item = i32>"), "");
        assert_eq!(normalize_rust_type("dyn Debug"), "");
        assert_eq!(normalize_rust_type("[u8; 32]"), "");
    }

    // ─────────────────────────────────────────────────────────────────
    // L5.9: pattern bindings — generic enum unwrap (Some/Ok/Err),
    // tuple destructure, struct destructure, match-arm patterns.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn rust_inner_generic_heads_handles_nested_and_multi_param() {
        assert_eq!(inner_generic_heads("Option<Foo>"), vec!["Foo"]);
        assert_eq!(inner_generic_heads("Result<Foo, Bar>"), vec!["Foo", "Bar"]);
        assert_eq!(
            inner_generic_heads("HashMap<String, Vec<u8>>"),
            vec!["String", "Vec"]
        );
        assert!(inner_generic_heads("Foo").is_empty());
        assert!(inner_generic_heads("").is_empty());
    }

    #[test]
    fn rust_collect_signatures_populates_generics_and_fields() {
        // White-box: confirm the FileTypes pre-pass populates the new
        // L5.9 maps for both free fns and methods, plus struct fields.
        let src = r#"
            struct Inner;
            struct Outer { inner: Inner, count: i32 }
            impl Outer { fn lookup(&self) -> Option<Inner> { None } }
            fn maybe_pair() -> Result<Inner, Outer> { Err(Outer { inner: Inner, count: 0 }) }
        "#;
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::language());
        let tree = parser.parse(src, None).expect("parse");
        let ft = collect_rust_signatures(tree.root_node(), src);

        assert_eq!(
            ft.fn_returns_generics.get("maybe_pair").map(Vec::as_slice),
            Some(["Inner".to_string(), "Outer".to_string()].as_slice())
        );
        assert_eq!(
            ft.method_returns_generics
                .get(&("Outer".to_string(), "lookup".to_string()))
                .map(Vec::as_slice),
            Some(["Inner".to_string()].as_slice())
        );
        assert_eq!(
            ft.struct_fields
                .get(&("Outer".to_string(), "inner".to_string()))
                .map(String::as_str),
            Some("Inner")
        );
        assert_eq!(
            ft.struct_fields
                .get(&("Outer".to_string(), "count".to_string()))
                .map(String::as_str),
            Some("i32")
        );
    }

    #[test]
    fn rust_if_let_some_binds_inner_type_via_option_return() {
        // `if let Some(x) = opt_foo()` should bind `x` to Foo via
        // Option<Foo>'s generic param, so `x.bar()` resolves to
        // `Foo::bar` rather than the raw `x.bar`.
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn opt_foo() -> Option<Foo> { Some(Foo) }
            fn run() {
                if let Some(x) = opt_foo() {
                    x.bar();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar via Option<Foo> unwrap, got {:?}",
            call_targets
        );
        assert!(
            !call_targets.contains(&"x.bar"),
            "raw x.bar should have been rewritten, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_ok_binds_first_result_param() {
        let src = r#"
            struct Foo;
            struct Err1;
            impl Foo { fn run(&self) {} }
            fn maybe() -> Result<Foo, Err1> { Ok(Foo) }
            fn handler() {
                if let Ok(v) = maybe() {
                    v.run();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run via Result<Foo, _> Ok unwrap, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_err_binds_second_result_param() {
        let src = r#"
            struct Foo;
            struct Err1;
            impl Err1 { fn report(&self) {} }
            fn maybe() -> Result<Foo, Err1> { Ok(Foo) }
            fn handler() {
                if let Err(e) = maybe() {
                    e.report();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Err1::report")),
            "expected ...::Err1::report via Err binding, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_none_does_not_bind_anything() {
        // `None` carries no inner data, so the variant branch should
        // exit cleanly without inventing a binding.
        let src = r#"
            struct Foo;
            fn opt() -> Option<Foo> { None }
            fn run() {
                if let None = opt() {
                    let _ = 1;
                }
            }
        "#;
        // Just make sure parsing doesn't panic and no spurious edges
        // get attached to a phantom binding.
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        assert!(r.edges.iter().all(|e| !e.target_qn.starts_with("None::")));
    }

    #[test]
    fn rust_match_arm_binds_some_pattern_in_arm_only() {
        // Some(x) arm should bind x to Foo. None arm has its own local
        // y bound from a separate fn — we want both arms' bindings to
        // resolve correctly and to NOT contaminate each other.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn bar(&self) {} }
            impl Bar { fn baz(&self) {} }
            fn opt() -> Option<Foo> { Some(Foo) }
            fn other() -> Bar { Bar }
            fn run() {
                match opt() {
                    Some(x) => { x.bar(); }
                    None => { let y = other(); y.baz(); }
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar in Some arm, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::baz")),
            "expected ...::Bar::baz in None arm, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_match_arm_binding_does_not_leak_to_sibling_arm() {
        // The Some-arm `x` binding must not promote the None-arm's
        // free fn call `x()` to `Foo::x` via accidental scope leakage.
        let src = r#"
            struct Foo;
            impl Foo { fn bar(&self) {} }
            fn opt() -> Option<Foo> { Some(Foo) }
            fn x() {}
            fn run() {
                match opt() {
                    Some(x) => { x.bar(); }
                    None => { x(); }
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            !call_targets.iter().any(|t| t.contains("Foo::x")),
            "None-arm x() leaked into Some arm's binding, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_destructure_binds_each_element() {
        // `let (a, b) = (Foo::new(), Bar::new());` should bind `a → Foo`
        // and `b → Bar` so subsequent method calls resolve.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo {
                fn new() -> Self { Foo }
                fn run(&self) {}
            }
            impl Bar {
                fn new() -> Self { Bar }
                fn run(&self) {}
            }
            fn driver() {
                let (a, b) = (Foo::new(), Bar::new());
                a.run();
                b.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from tuple element 0, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run from tuple element 1, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_destructure_with_wildcard_skips_silently() {
        // Wildcard slots are skipped by pattern_to_simple_name, so
        // only the named element binds.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo {
                fn new() -> Self { Foo }
                fn run(&self) {}
            }
            impl Bar { fn new() -> Self { Bar } }
            fn driver() {
                let (a, _) = (Foo::new(), Bar::new());
                a.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_struct_destructure_binds_field_types() {
        // `let Outer { inner, count: _ } = ..` should bind `inner` to
        // its declared field type Inner.
        let src = r#"
            struct Inner;
            impl Inner { fn run(&self) {} }
            struct Outer { inner: Inner, count: i32 }
            impl Outer { fn new() -> Self { Outer { inner: Inner, count: 0 } } }
            fn driver() {
                let Outer { inner, count: _ } = Outer::new();
                inner.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Inner::run")),
            "expected ...::Inner::run via struct destructure, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_struct_destructure_with_renamed_field() {
        // `let Outer { inner: renamed } = ..` — `renamed` should still
        // pick up the field type even though the binding name differs.
        let src = r#"
            struct Inner;
            impl Inner { fn run(&self) {} }
            struct Outer { inner: Inner }
            impl Outer { fn new() -> Self { Outer { inner: Inner } } }
            fn driver() {
                let Outer { inner: renamed } = Outer::new();
                renamed.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Inner::run")),
            "expected ...::Inner::run via field rename, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_unknown_tuple_struct_variant_does_not_bind() {
        // `MyVariant(x)` with no FileTypes inner-generics entry must
        // NOT silently bind `x` to a wrong type. Confirm by checking
        // the call survives in its raw form.
        let src = r#"
            struct Foo;
            fn make() -> Foo { Foo }
            fn run() {
                if let MyVariant(x) = make() {
                    x.bar();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        // No bogus `Foo::bar` from misinterpreting the variant.
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "unknown variant should not bind to Foo's inner, got {:?}",
            call_targets
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // L5.10: user-enum variant tracking + tuple-typed return
    // destructure.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn rust_tuple_type_heads_splits_parenthesised_tuples() {
        assert_eq!(tuple_type_heads("(Foo, Bar)"), vec!["Foo", "Bar"]);
        assert_eq!(tuple_type_heads("(Foo,)"), vec!["Foo"]);
        assert_eq!(
            tuple_type_heads("(Vec<u8>, HashMap<K, V>)"),
            vec!["Vec", "HashMap"]
        );
        assert!(tuple_type_heads("Foo").is_empty());
        assert!(tuple_type_heads("").is_empty());
        assert!(tuple_type_heads("()").is_empty());
    }

    #[test]
    fn rust_collect_signatures_populates_enum_variants_and_tuple_returns() {
        // White-box: confirm enum variants (generic + concrete + Self)
        // and tuple return types land in the FileTypes pre-pass.
        let src = r#"
            struct Foo;
            struct Bar;
            struct Header;
            enum MyResult<T, E> { Hit(T), Err(E), Miss, Pair(Foo, E) }
            enum Payload { Ping(Header), Pong }
            enum Tree { Leaf, Node(Self) }
            fn pair() -> (Foo, Bar) { (Foo, Bar) }
            struct Wrapper;
            impl Wrapper {
                fn split(&self) -> (Self, Foo) { (Wrapper, Foo) }
            }
        "#;
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::language());
        let tree = parser.parse(src, None).expect("parse");
        let ft = collect_rust_signatures(tree.root_node(), src);

        // Generic variant: Hit(T) picks generic param 0.
        let hit = ft
            .enum_variants
            .get(&("MyResult".to_string(), "Hit".to_string()))
            .expect("MyResult::Hit should be indexed");
        assert_eq!(hit.len(), 1);
        assert!(matches!(hit[0], VariantFieldSource::Generic(0)));

        // Generic variant at idx 1: Err(E).
        let err = ft
            .enum_variants
            .get(&("MyResult".to_string(), "Err".to_string()))
            .expect("MyResult::Err should be indexed");
        assert!(matches!(err[0], VariantFieldSource::Generic(1)));

        // Unit variant is NOT indexed (no fields).
        assert!(
            !ft.enum_variants
                .contains_key(&("MyResult".to_string(), "Miss".to_string())),
            "unit variants should not produce an entry"
        );

        // Mixed variant Pair(Foo, E): concrete Foo at 0, Generic(1) at 1.
        let pair_fields = ft
            .enum_variants
            .get(&("MyResult".to_string(), "Pair".to_string()))
            .expect("MyResult::Pair should be indexed");
        assert_eq!(pair_fields.len(), 2);
        assert!(matches!(
            &pair_fields[0],
            VariantFieldSource::Concrete(t) if t == "Foo"
        ));
        assert!(matches!(pair_fields[1], VariantFieldSource::Generic(1)));

        // Concrete-only enum.
        let ping = ft
            .enum_variants
            .get(&("Payload".to_string(), "Ping".to_string()))
            .expect("Payload::Ping should be indexed");
        assert_eq!(ping.len(), 1);
        assert!(matches!(
            &ping[0],
            VariantFieldSource::Concrete(t) if t == "Header"
        ));

        // `Self` inside a variant resolves to the enum's own name.
        let node = ft
            .enum_variants
            .get(&("Tree".to_string(), "Node".to_string()))
            .expect("Tree::Node should be indexed");
        assert!(matches!(
            &node[0],
            VariantFieldSource::Concrete(t) if t == "Tree"
        ));

        // Tuple-return free fn.
        assert_eq!(
            ft.fn_returns_tuple.get("pair").map(Vec::as_slice),
            Some(["Foo".to_string(), "Bar".to_string()].as_slice())
        );

        // Tuple-return method with Self, normalized to the impl type.
        assert_eq!(
            ft.method_returns_tuple
                .get(&("Wrapper".to_string(), "split".to_string()))
                .map(Vec::as_slice),
            Some(["Wrapper".to_string(), "Foo".to_string()].as_slice())
        );
    }

    #[test]
    fn rust_collect_type_param_names_skips_lifetimes_and_consts() {
        // White-box via collect_rust_signatures — it's easier to drive
        // through an enum than to synthesize a `type_parameters` node
        // directly, and the net effect (indexing Generic slots by
        // position among type params only) is what we actually care
        // about.
        let src = "enum E<'a, T: Clone, const N: usize, U> { A(T), B(U) }";
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::language());
        let tree = parser.parse(src, None).expect("parse");
        let ft = collect_rust_signatures(tree.root_node(), src);
        let a = ft
            .enum_variants
            .get(&("E".to_string(), "A".to_string()))
            .expect("E::A indexed");
        assert!(
            matches!(a[0], VariantFieldSource::Generic(0)),
            "T is the first type param (lifetime + const skipped), got {:?}",
            a[0]
        );
        let b = ft
            .enum_variants
            .get(&("E".to_string(), "B".to_string()))
            .expect("E::B indexed");
        assert!(
            matches!(b[0], VariantFieldSource::Generic(1)),
            "U is the second type param, got {:?}",
            b[0]
        );
    }

    #[test]
    fn rust_if_let_user_enum_variant_binds_inner_generic() {
        // `if let Hit(x) = make()` where `make() -> MyResult<Foo>`
        // should bind `x: Foo` so `x.run()` resolves to `Foo::run`.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn run(&self) {} }
            enum MyResult<T, E> { Hit(T), Err(E), Miss }
            fn make() -> MyResult<Foo, Bar> { MyResult::Hit(Foo) }
            fn handler() {
                if let Hit(x) = make() {
                    x.run();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run via MyResult::Hit generic, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_user_enum_variant_binds_second_generic() {
        // `if let Err(e) = make()` — E is the second type param.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Bar { fn report(&self) {} }
            enum MyResult<T, E> { Hit(T), Err(E) }
            fn make() -> MyResult<Foo, Bar> { MyResult::Err(Bar) }
            fn handler() {
                if let Err(e) = make() {
                    e.report();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::report")),
            "expected ...::Bar::report via MyResult::Err idx 1, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_user_enum_variant_binds_concrete_field() {
        // `if let Ping(h) = make()` where `Ping(Header)` binds
        // `h: Header` regardless of the enum having no generics.
        let src = r#"
            struct Header;
            impl Header { fn parse(&self) {} }
            enum Payload { Ping(Header), Pong }
            fn make() -> Payload { Payload::Ping(Header) }
            fn handler() {
                if let Ping(h) = make() {
                    h.parse();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Header::parse")),
            "expected ...::Header::parse via Payload::Ping concrete, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_match_arm_user_enum_variants_isolate_per_arm() {
        // Each arm's pattern binding lives only inside that arm.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn ff(&self) {} }
            impl Bar { fn bb(&self) {} }
            enum Either { L(Foo), R(Bar) }
            fn make() -> Either { Either::L(Foo) }
            fn run() {
                match make() {
                    L(a) => { a.ff(); }
                    R(b) => { b.bb(); }
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::ff")),
            "expected ...::Foo::ff in L arm, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::bb")),
            "expected ...::Bar::bb in R arm, got {:?}",
            call_targets
        );
        // Siblings must not cross-contaminate: no ...::Foo::bb or
        // ...::Bar::ff.
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Foo::bb")),
            "a (Foo) leaked into R arm, got {:?}",
            call_targets
        );
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Bar::ff")),
            "b (Bar) leaked into L arm, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_user_enum_variant_only_binds_for_matching_value_head() {
        // `make()` returns Foo, not Either — so even if Either has a
        // variant `Some`, we shouldn't misbind because the matched
        // value's type head is Foo (and Foo has no variant table).
        // Guards against an accidental global variant lookup.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Bar { fn run(&self) {} }
            enum Either { Custom(Bar) }
            fn make() -> Foo { Foo }
            fn run() {
                if let Custom(x) = make() {
                    x.run();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        // Should NOT resolve to Bar::run because the matched value is
        // Foo, not Either.
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "cross-enum binding leaked, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_destructure_from_fn_call_binds_each_element() {
        // `let (a, b) = pair();` where `pair() -> (Foo, Bar)` should
        // bind both elements so `a.run()` / `b.run()` resolve.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn run(&self) {} }
            impl Bar { fn run(&self) {} }
            fn pair() -> (Foo, Bar) { (Foo, Bar) }
            fn driver() {
                let (a, b) = pair();
                a.run();
                b.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from tuple-return idx 0, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run from tuple-return idx 1, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_destructure_from_method_call_binds_each_element() {
        // Method call returning a tuple — driver's parameter binding
        // `p: Pair` seeds RustScope so `p.split()` resolves via
        // method_returns_tuple.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn run(&self) {} }
            impl Bar { fn run(&self) {} }
            struct Pair;
            impl Pair {
                fn split(&self) -> (Foo, Bar) { (Foo, Bar) }
            }
            fn driver(p: &Pair) {
                let (a, b) = p.split();
                a.run();
                b.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run via Pair::split, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run via Pair::split, got {:?}",
            call_targets
        );
    }

    // ─────────────────────────────────────────────────────────────────
    // L5.13a: struct-style enum variant tracking.
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn rust_collect_signatures_populates_enum_struct_variants() {
        // White-box: confirm struct-style variants (concrete,
        // generic, Self) land in `enum_struct_variants` with their
        // field ordering preserved.
        let src = r#"
            struct Header;
            struct Body;
            enum Payload {
                Ping { header: Header, body: Body },
                Pong,
            }
            enum Maybe<T, E> {
                Some { val: T, tag: Header },
                None,
                Err { kind: E },
            }
            enum Tree {
                Node { child: Self, depth: Header },
                Leaf,
            }
        "#;
        let mut parser = Parser::new();
        let _ = parser.set_language(&tree_sitter_rust::language());
        let tree = parser.parse(src, None).expect("parse");
        let ft = collect_rust_signatures(tree.root_node(), src);

        let ping = ft
            .enum_struct_variants
            .get(&("Payload".to_string(), "Ping".to_string()))
            .expect("Payload::Ping struct variant should be indexed");
        assert_eq!(ping.len(), 2);
        assert_eq!(ping[0].0, "header");
        assert!(matches!(&ping[0].1, VariantFieldSource::Concrete(t) if t == "Header"));
        assert_eq!(ping[1].0, "body");
        assert!(matches!(&ping[1].1, VariantFieldSource::Concrete(t) if t == "Body"));

        // Unit variant must NOT appear in the map.
        assert!(!ft
            .enum_struct_variants
            .contains_key(&("Payload".to_string(), "Pong".to_string())));

        // Generic + concrete mix: `Some { val: T, tag: Header }` →
        // Generic(0) for val, Concrete("Header") for tag.
        let some = ft
            .enum_struct_variants
            .get(&("Maybe".to_string(), "Some".to_string()))
            .expect("Maybe::Some struct variant should be indexed");
        assert_eq!(some.len(), 2);
        assert_eq!(some[0].0, "val");
        assert!(matches!(some[0].1, VariantFieldSource::Generic(0)));
        assert_eq!(some[1].0, "tag");
        assert!(matches!(&some[1].1, VariantFieldSource::Concrete(t) if t == "Header"));

        // Second type param: `Err { kind: E }` → Generic(1).
        let err = ft
            .enum_struct_variants
            .get(&("Maybe".to_string(), "Err".to_string()))
            .expect("Maybe::Err struct variant should be indexed");
        assert!(matches!(err[0].1, VariantFieldSource::Generic(1)));

        // `Self` inside a struct variant resolves to the enum head.
        let node = ft
            .enum_struct_variants
            .get(&("Tree".to_string(), "Node".to_string()))
            .expect("Tree::Node struct variant should be indexed");
        assert_eq!(node[0].0, "child");
        assert!(matches!(&node[0].1, VariantFieldSource::Concrete(t) if t == "Tree"));
    }

    #[test]
    fn rust_if_let_qualified_struct_variant_binds_concrete_field() {
        // `if let Payload::Ping { header } = make()` should bind
        // `header: Header` so `header.parse()` resolves to
        // `Header::parse` via the new enum_struct_variants lookup.
        let src = r#"
            struct Header;
            impl Header { fn parse(&self) {} }
            enum Payload {
                Ping { header: Header },
                Pong,
            }
            fn make() -> Payload { Payload::Ping { header: Header } }
            fn handler() {
                if let Payload::Ping { header } = make() {
                    header.parse();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Header::parse")),
            "expected ...::Header::parse via Payload::Ping struct variant, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_struct_variant_with_renamed_binding() {
        // `if let Payload::Ping { header: h } = make()` — the alias
        // `h` should pick up `Header`, not the original `header`.
        let src = r#"
            struct Header;
            impl Header { fn parse(&self) {} }
            enum Payload { Ping { header: Header } }
            fn make() -> Payload { Payload::Ping { header: Header } }
            fn handler() {
                if let Payload::Ping { header: h } = make() {
                    h.parse();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Header::parse")),
            "expected ...::Header::parse via alias `h`, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_struct_variant_binds_generic_field() {
        // `if let Maybe::Some { val } = make()` where
        // `make() -> Maybe<Foo, Bar>` and `Some { val: T }` → val: Foo.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn run(&self) {} }
            enum Maybe<T, E> {
                Some { val: T },
                Err { kind: E },
            }
            fn make() -> Maybe<Foo, Bar> { Maybe::Some { val: Foo } }
            fn handler() {
                if let Maybe::Some { val } = make() {
                    val.run();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run via Maybe::Some generic idx 0, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_if_let_bare_struct_variant_binds_via_value_head() {
        // `if let Ping { header } = make()` — no enum prefix in the
        // pattern. The matched value's inferred type head (`Payload`)
        // drives the enum_struct_variants lookup.
        let src = r#"
            struct Header;
            impl Header { fn parse(&self) {} }
            enum Payload { Ping { header: Header } }
            fn make() -> Payload { Payload::Ping { header: Header } }
            fn handler() {
                if let Ping { header } = make() {
                    header.parse();
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Header::parse")),
            "expected ...::Header::parse via bare Ping + value head, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_match_arm_struct_variants_isolate_per_arm() {
        // Struct-style match arms — bindings must stay inside their
        // own arm.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn ff(&self) {} }
            impl Bar { fn bb(&self) {} }
            enum Either {
                L { a: Foo },
                R { b: Bar },
            }
            fn make() -> Either { Either::L { a: Foo } }
            fn run() {
                match make() {
                    Either::L { a } => { a.ff(); }
                    Either::R { b } => { b.bb(); }
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::ff")),
            "expected ::Foo::ff in L arm, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::bb")),
            "expected ::Bar::bb in R arm, got {:?}",
            call_targets
        );
        // Cross-contamination guard.
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Foo::bb")),
            "L binding leaked into R arm: {:?}",
            call_targets
        );
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Bar::ff")),
            "R binding leaked into L arm: {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_plain_struct_destructure_still_works_after_enum_probe() {
        // Regression guard: a regular struct destructure should still
        // go through the `struct_fields` fallback when the enum probe
        // misses.
        let src = r#"
            struct Inner;
            impl Inner { fn poke(&self) {} }
            struct Outer { inner: Inner }
            fn make() -> Outer { Outer { inner: Inner } }
            fn handler() {
                let Outer { inner } = make();
                inner.poke();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Inner::poke")),
            "expected ::Inner::poke via plain-struct fallback, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_typed_local_destructure_binds_elements() {
        // L5.13b: `let x = pair(); let (a, b) = x` should bind `a → Foo`
        // and `b → Bar` from the stored tuple info in locals_tuple.
        let src = r#"
            struct Foo;
            struct Bar;
            impl Foo { fn run(&self) {} }
            impl Bar { fn run(&self) {} }
            fn pair() -> (Foo, Bar) { (Foo, Bar) }
            fn driver() {
                let x = pair();
                let (a, b) = x;
                a.run();
                b.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from tuple-typed local destructure, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run from tuple-typed local destructure, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_typed_local_method_call_destructure() {
        // L5.13b: `let x = p.split(); let (a, b) = x` where `split()`
        // returns a tuple — bindings should resolve from locals_tuple.
        // Note: p must be &Pair (reference) for receiver_type_of to work
        // because the method signature uses &self.
        let src = r#"
            struct Foo;
            struct Bar;
            struct Pair;
            impl Foo { fn run(&self) {} }
            impl Bar { fn run(&self) {} }
            impl Pair {
                fn split(&self) -> (Foo, Bar) { (Foo, Bar) }
            }
            fn driver(p: &Pair) {
                let x = p.split();
                let (a, b) = x;
                a.run();
                b.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from method-call tuple-return, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run from method-call tuple-return, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_or_pattern_intersection_binds_common_names() {
        // L5.13c: `Ok(x) | Err(x)` should bind `x` from both alternatives.
        // Only bindings present in ALL alternatives are promoted.
        let src = r#"
            struct Foo;
            impl Foo { fn run(&self) {} }
            fn make() -> Result<Foo, Foo> { Ok(Foo) }
            fn driver() {
                match make() {
                    Ok(x) | Err(x) => { x.run(); },
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from or-pattern intersection, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_or_pattern_no_intersection_skips_binding() {
        // L5.13c: `Some(x) | None` has no common bindings (x only in first
        // alternative, none in second), so nothing should be bound.
        let src = r#"
            struct Foo;
            impl Foo { fn run(&self) {} }
            fn make() -> Option<Foo> { Some(Foo) }
            fn driver() {
                match make() {
                    Some(x) | None => { x.run(); },
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        // x should NOT be bound to Foo because None doesn't bind x.
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected no Foo::run from non-intersecting or-pattern, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_or_pattern_reversed_order_still_binds() {
        // L5.13c: `Err(x) | Ok(x)` — reversed order should still work.
        let src = r#"
            struct Foo;
            impl Foo { fn run(&self) {} }
            fn make() -> Result<Foo, Foo> { Ok(Foo) }
            fn driver() {
                match make() {
                    Err(x) | Ok(x) => { x.run(); },
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from reversed or-pattern, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_nested_tuple_destructure_from_fn_call() {
        // L5.13d: `let ((a, b), c) = pair_pair()` where
        // `pair_pair() -> ((Foo, Bar), Baz)` should bind a, b, c.
        let src = r#"
            struct Foo;
            struct Bar;
            struct Baz;
            impl Foo { fn run(&self) {} }
            impl Bar { fn run(&self) {} }
            impl Baz { fn run(&self) {} }
            fn pair_pair() -> ((Foo, Bar), Baz) { ((Foo, Bar), Baz) }
            fn driver() {
                let ((a, b), c) = pair_pair();
                a.run();
                b.run();
                c.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from nested tuple element 0, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run from nested tuple element 1, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Baz::run")),
            "expected ...::Baz::run from nested tuple element 2, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_nested_tuple_typed_local_destructure() {
        // L5.13d: `let x = pair_pair(); let ((a, b), c) = x` should
        // resolve via locals_tuple with nested tuple types.
        let src = r#"
            struct Foo;
            struct Bar;
            struct Baz;
            impl Foo { fn run(&self) {} }
            impl Bar { fn run(&self) {} }
            impl Baz { fn run(&self) {} }
            fn pair_pair() -> ((Foo, Bar), Baz) { ((Foo, Bar), Baz) }
            fn driver() {
                let x = pair_pair();
                let ((a, b), c) = x;
                a.run();
                b.run();
                c.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from nested tuple-typed local, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "expected ...::Bar::run from nested tuple-typed local, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Baz::run")),
            "expected ...::Baz::run from nested tuple-typed local, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_nested_tuple_partial_destructure() {
        // L5.13d: `let ((a, b), c) = pair_pair()` where only a, c are
        // used — b should still be bound but we only check a, c resolve.
        let src = r#"
            struct Foo;
            struct Bar;
            struct Baz;
            impl Foo { fn run(&self) {} }
            impl Baz { fn run(&self) {} }
            fn pair_pair() -> ((Foo, Bar), Baz) { ((Foo, Bar), Baz) }
            fn driver() {
                let ((a, _b), c) = pair_pair();
                a.run();
                c.run();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        let call_targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected ...::Foo::run from partial nested destructure, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Baz::run")),
            "expected ...::Baz::run from partial nested destructure, got {:?}",
            call_targets
        );
    }

    // ─── Lua (Phase 3 / Task G) ──────────────────────────────────────

    #[test]
    fn lua_top_level_function_extracted() {
        let src = "function add(a, b) return a + b end\n";
        let r = parse(Language::Lua, src, &PathBuf::from("calc.lua"));
        let n = r.nodes.iter().find(|n| n.name == "add").expect("add node");
        assert!(matches!(n.kind, NodeKind::Function));
        assert_eq!(n.qualified_name, "calc::add");
        // Signature carries everything before the body block.
        assert!(n
            .signature
            .as_deref()
            .unwrap_or("")
            .starts_with("function add(a, b)"));
    }

    #[test]
    fn lua_local_function_extracted() {
        let src = "local function helper(x) return x * 2 end\n";
        let r = parse(Language::Lua, src, &PathBuf::from("util.lua"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "helper")
            .expect("helper node");
        assert!(matches!(n.kind, NodeKind::Function));
    }

    #[test]
    fn lua_method_index_emits_method_node() {
        // `function M:method(arg) ... end` → Method on receiver M.
        let src = "local M = {}\nfunction M:greet(name) return 'hi ' .. name end\nreturn M\n";
        let r = parse(Language::Lua, src, &PathBuf::from("mod.lua"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "greet")
            .expect("greet method");
        assert!(matches!(n.kind, NodeKind::Method));
        assert_eq!(n.parent_qn.as_deref(), Some("mod::M"));
        assert_eq!(n.qualified_name, "mod::M::greet");
    }

    #[test]
    fn lua_dot_table_function_emits_method_node() {
        // `function M.helper(x) ... end` → also a Method via dot syntax.
        let src = "local M = {}\nfunction M.helper(x) return x end\nreturn M\n";
        let r = parse(Language::Lua, src, &PathBuf::from("mod.lua"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "helper")
            .expect("helper method");
        assert!(matches!(n.kind, NodeKind::Method));
        assert_eq!(n.parent_qn.as_deref(), Some("mod::M"));
    }

    #[test]
    fn lua_local_variable_extracted_as_constant() {
        let src = "local count = 42\n";
        let r = parse(Language::Lua, src, &PathBuf::from("data.lua"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "count")
            .expect("count node");
        assert!(matches!(n.kind, NodeKind::Constant));
        // The signature is prefixed with `local` so callers can tell the
        // declaration is local even when reading the symbol in isolation.
        assert!(n.signature.as_deref().unwrap_or("").starts_with("local "));
    }

    #[test]
    fn lua_anonymous_function_assigned_to_table_is_method() {
        // `M.helper = function(z) ... end` — common Lua module pattern.
        let src = "local M = {}\nM.helper = function(z) return z + 1 end\nreturn M\n";
        let r = parse(Language::Lua, src, &PathBuf::from("mod.lua"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "helper")
            .expect("helper assignment");
        assert!(matches!(n.kind, NodeKind::Method));
        assert_eq!(n.parent_qn.as_deref(), Some("mod::M"));
    }

    #[test]
    fn lua_require_emits_imports_edge() {
        let src = "local utils = require('utils')\n";
        let r = parse(Language::Lua, src, &PathBuf::from("app.lua"));
        let edge = r
            .edges
            .iter()
            .find(|e| matches!(e.kind, EdgeKind::ImportsFrom))
            .expect("ImportsFrom edge");
        assert_eq!(edge.target_qn, "utils");
        assert_eq!(edge.source_qn, "app:file");
        assert!(matches!(edge.tier, ConfidenceTier::Extracted));
    }

    #[test]
    fn lua_calls_inside_function_body_are_collected() {
        let src = "local function caller() return helper(1, 2) end\n";
        let r = parse(Language::Lua, src, &PathBuf::from("a.lua"));
        let calls: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            calls.contains(&"helper"),
            "expected `helper` in call targets, got {:?}",
            calls
        );
    }

    // ─── Bash (Phase 3 / Task G) ─────────────────────────────────────

    #[test]
    fn bash_function_definition_extracted() {
        let src = "deploy() {\n    echo 'go'\n}\n";
        let r = parse(Language::Bash, src, &PathBuf::from("scripts/deploy.sh"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "deploy")
            .expect("deploy node");
        assert!(matches!(n.kind, NodeKind::Function));
        assert_eq!(n.qualified_name, "scripts/deploy::deploy");
    }

    #[test]
    fn bash_function_keyword_form_extracted() {
        // `function name { ... }` — same node kind as `name() { ... }`.
        let src = "function release {\n    echo 'release'\n}\n";
        let r = parse(Language::Bash, src, &PathBuf::from("ops.sh"));
        assert!(r
            .nodes
            .iter()
            .any(|n| n.name == "release" && matches!(n.kind, NodeKind::Function)));
    }

    #[test]
    fn bash_local_var_in_function_emits_constant() {
        // The declaration is inside the function body, so the walker
        // recurses through `compound_statement`.
        let src = "deploy() {\n    local target=prod\n}\n";
        let r = parse(Language::Bash, src, &PathBuf::from("d.sh"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "target")
            .expect("target const");
        assert!(matches!(n.kind, NodeKind::Constant));
    }

    #[test]
    fn bash_top_level_declare_emits_constant() {
        let src = "declare -i COUNT=42\n";
        let r = parse(Language::Bash, src, &PathBuf::from("a.sh"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "COUNT")
            .expect("COUNT const");
        assert!(matches!(n.kind, NodeKind::Constant));
    }

    #[test]
    fn bash_alias_emits_constant() {
        let src = "alias ll='ls -la'\n";
        let r = parse(Language::Bash, src, &PathBuf::from("rc.sh"));
        let n = r.nodes.iter().find(|n| n.name == "ll").expect("ll alias");
        assert!(matches!(n.kind, NodeKind::Constant));
        assert_eq!(n.qualified_name, "rc::ll");
        // Signature preserves the alias body so a `crux find` reader can
        // see what the alias actually does without re-reading the file.
        assert!(n.signature.as_deref().unwrap_or("").contains("ls -la"));
    }

    #[test]
    fn bash_calls_inside_function_emit_edges() {
        let src = "deploy() {\n    upload\n    notify_team\n}\n";
        let r = parse(Language::Bash, src, &PathBuf::from("d.sh"));
        let targets: Vec<&str> = r
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Calls))
            .map(|e| e.target_qn.as_str())
            .collect();
        assert!(
            targets.contains(&"upload"),
            "missing `upload` in {:?}",
            targets
        );
        assert!(
            targets.contains(&"notify_team"),
            "missing `notify_team` in {:?}",
            targets
        );
    }

    #[test]
    fn bash_test_function_naming_marks_is_test() {
        let src = "test_login_flow() {\n    return 0\n}\n";
        let r = parse(Language::Bash, src, &PathBuf::from("t.sh"));
        let n = r
            .nodes
            .iter()
            .find(|n| n.name == "test_login_flow")
            .expect("test fn");
        assert!(n.is_test);
    }

    #[test]
    fn bash_top_level_unknown_command_does_not_create_symbol() {
        // `echo hi` at the file root is not a declaration — make sure
        // we don't accidentally promote arbitrary commands to nodes
        // (only `alias` is special-cased).
        let src = "echo hello\n";
        let r = parse(Language::Bash, src, &PathBuf::from("a.sh"));
        let non_file_nodes: Vec<&str> = r
            .nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::File))
            .map(|n| n.name.as_str())
            .collect();
        assert!(
            non_file_nodes.is_empty(),
            "expected only the File node, got extras: {:?}",
            non_file_nodes
        );
    }

    #[test]
    fn lua_and_bash_extension_routing() {
        assert!(matches!(
            Language::from_extension("lua"),
            Some(Language::Lua)
        ));
        assert!(matches!(
            Language::from_extension("sh"),
            Some(Language::Bash)
        ));
        assert!(matches!(
            Language::from_extension("bash"),
            Some(Language::Bash)
        ));
        assert!(Language::from_extension("rs").is_some());
        assert!(Language::from_extension("md").is_none());
    }
}
