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
        Language::Lua => joined.replace('/', "."),
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

fn visit_rust_with_project(
    root: Node<'_>,
    src: &str,
    mod_qn: &str,
    out: &mut ParseResult,
    project: Option<&ProjectFileTypes>,
) {
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
                default_target = Some(slice(sub_child, src).to_string());
                handled_inner = true;
            }
            _ => {}
        }
    }

    if has_default {
        let target = default_target.unwrap_or_else(|| {
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
        walk_js(node, src, mod_qn, parent_qn, out);
    }
}

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

        if is_func {
            if let Some(rhs) = rhs {
                if let Some(body) = lua_function_body(rhs) {
                    let mut sub = body.walk();
                    collect_calls(body, &mut sub, src, &qn, out);
                }
            }
        }

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

fn lua_function_body<'a>(node: Node<'a>) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == "block");
    found
}

fn lua_signature(fn_node: Node<'_>, src: &str) -> Option<String> {
    if let Some(block) = lua_function_body(fn_node) {
        let start = fn_node.start_byte();
        let stop = block.start_byte();
        Some(src[start..stop].trim().to_string())
    } else {
        Some(slice(fn_node, src).lines().next().unwrap_or("").to_string())
    }
}

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
    let mut cursor = node.walk();
    let name = node
        .children(&mut cursor)
        .find(|c| c.kind() == "word")
        .map(|n| slice(n, src).to_string());
    let Some(name) = name else { return };

    let qn = qn_join(mod_qn, parent_qn, &name);
    let (ls, le) = line_of(node);
    let signature = bash_signature(node, src);
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
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .find(|c| c.kind() == "compound_statement" || c.kind() == "do_group");
    found
}

fn bash_signature(node: Node<'_>, src: &str) -> Option<String> {
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

#[derive(Clone, Default)]
struct RustScope {
    self_type: Option<String>,
    locals: HashMap<String, String>,
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

#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct FileTypes {
    fn_returns: HashMap<String, String>,
    method_returns: HashMap<(String, String), String>,
    fn_returns_generics: HashMap<String, Vec<String>>,
    method_returns_generics: HashMap<(String, String), Vec<String>>,
    struct_fields: HashMap<(String, String), String>,
    enum_variants: HashMap<(String, String), Vec<VariantFieldSource>>,
    enum_struct_variants: HashMap<(String, String), Vec<(String, VariantFieldSource)>>,
    fn_returns_tuple: HashMap<String, Vec<String>>,
    method_returns_tuple: HashMap<(String, String), Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum VariantFieldSource {
    Generic(usize),
    Concrete(String),
}

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
                    let generics = inner_generic_heads(&ret_raw);
                    let tuple_parts = tuple_type_heads(&ret_raw);
                    if !head.is_empty() {
                        if let Some(it) = impl_type {
                            let resolved = if head == "Self" { it.to_string() } else { head };
                            ft.method_returns
                                .insert((it.to_string(), name.clone()), resolved);
                        } else {
                            ft.fn_returns.insert(name.clone(), head);
                        }
                    }
                    if !generics.is_empty() {
                        if let Some(it) = impl_type {
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
            }
            "struct_item" => {
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
                    let Some(v_body) = variant.child_by_field_name("body") else {
                        continue;
                    };
                    match v_body.kind() {
                        "ordered_field_declaration_list" => {
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

fn learn_from_let(node: Node<'_>, src: &str, scope: &mut RustScope, file_types: &FileTypes) {
    let Some(pattern) = node.child_by_field_name("pattern") else {
        return;
    };

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
            if value.kind() == "tuple_expression" {
                let pat_children = named_children(pattern);
                let val_children = named_children(value);
                for (pc, vc) in pat_children.iter().zip(val_children.iter()) {
                    learn_from_pattern(*pc, *vc, src, scope, file_types);
                }
                return;
            }
            if value.kind() == "identifier" {
                let val_name = slice(value, src).trim();
                if let Some(types) = scope.locals_tuple.get(val_name).cloned() {
                    let pat_children = named_children(pattern);
                    destructure_tuple_elements(&pat_children, types, value, src, scope, file_types);
                    return;
                }
            }
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
            if let Some((first_locals, _)) = alt_results.first() {
                for (name, ty) in first_locals {
                    if alt_results.iter().all(|(l, _)| l.get(name) == Some(ty)) {
                        scope.locals.insert(name.clone(), ty.clone());
                    }
                }
            }
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
            if pc.kind() == "tuple_pattern" {
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
                let nested_types = tuple_type_heads(ty);
                if !nested_types.is_empty() {
                    scope.locals_tuple.insert(name, nested_types);
                }
            }
        } else if let Some(name) = pattern_to_simple_name(*pc, src) {
            if !ty.is_empty() {
                scope.locals.insert(name, ty.clone());
            }
        }
    }
}

fn pattern_to_simple_name(pattern: Node<'_>, src: &str) -> Option<String> {
    match pattern.kind() {
        "identifier" => Some(slice(pattern, src).trim().to_string()),
        "mutable_pattern" => {
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

fn infer_let_type(
    expr: Node<'_>,
    src: &str,
    scope: &RustScope,
    file_types: &FileTypes,
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
                    let name = slice(func, src).trim();
                    file_types.fn_returns.get(name).cloned()
                }
                "field_expression" => {
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
        "reference_expression"
        | "unary_expression"
        | "parenthesized_expression"
        | "try_expression" => infer_let_type(value, src, scope, file_types),
        "call_expression" | "struct_expression" => infer_let_type(value, src, scope, file_types),
        _ => None,
    }
}

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

fn tuple_type_heads(raw: &str) -> Vec<String> {
    let s = raw.trim();
    if !s.starts_with('(') || !s.ends_with(')') {
        return Vec::new();
    }
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
    parts
        .into_iter()
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let trimmed = seg.trim();
            if trimmed.starts_with('(') && trimmed.ends_with(')') {
                trimmed.to_string()
            } else {
                type_head(&seg)
            }
        })
        .filter(|h| !h.is_empty())
        .collect()
}

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
            _ => None,
        };
        if let Some(n) = name.filter(|s| !s.is_empty()) {
            names.push(n);
        }
    }
    names
}

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

    #[test]
    fn js_export_default_named_function_emits_node_and_edge() {
        let src = "export default function bar() { return 1; }\n";
        let r = parse(Language::TypeScript, src, &PathBuf::from("src/x.ts"));
        let bar = r.nodes.iter().find(|n| n.name == "bar").expect("bar node");
        assert_eq!(bar.qualified_name, "src/x::bar");
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
        let src = "function main() { const x = new Foo(); }\n";
        let r = parse(Language::JavaScript, src, &PathBuf::from("src/x.js"));
        assert!(r
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Calls) && e.target_qn == "Foo"));
    }

    #[test]
    fn js_new_expression_member_constructor_keeps_path() {
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
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Client::send")),
            "expected ...::Client::send, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_call_resolves_to_type() {
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
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected inner-arm Foo::bar, got {:?}",
            call_targets
        );
        assert!(
            call_targets.iter().any(|t| t == "x.bar"),
            "expected raw x.bar to survive outer block, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_binding_strips_reference_in_value() {
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
        let src = r#"
            fn run() {
                struct Inner;
                impl Inner { fn make() -> Self { Inner } }
                let _x = Inner::make();
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        assert!(r.edges.iter().all(|e| !e.target_qn.starts_with("Self::")));
    }

    #[test]
    fn rust_let_binding_bare_fn_call_resolves_via_return_type() {
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
        assert!(
            call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "expected ...::Foo::bar in consequence, got {:?}",
            call_targets
        );
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
        let src = r#"
            struct Foo;
            fn opt() -> Option<Foo> { None }
            fn run() {
                if let None = opt() {
                    let _ = 1;
                }
            }
        "#;
        let r = parse(Language::Rust, src, &PathBuf::from("src/lib.rs"));
        assert!(r.edges.iter().all(|e| !e.target_qn.starts_with("None::")));
    }

    #[test]
    fn rust_match_arm_binds_some_pattern_in_arm_only() {
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
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Foo::bar")),
            "unknown variant should not bind to Foo's inner, got {:?}",
            call_targets
        );
    }

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

        let hit = ft
            .enum_variants
            .get(&("MyResult".to_string(), "Hit".to_string()))
            .expect("MyResult::Hit should be indexed");
        assert_eq!(hit.len(), 1);
        assert!(matches!(hit[0], VariantFieldSource::Generic(0)));

        let err = ft
            .enum_variants
            .get(&("MyResult".to_string(), "Err".to_string()))
            .expect("MyResult::Err should be indexed");
        assert!(matches!(err[0], VariantFieldSource::Generic(1)));

        assert!(
            !ft.enum_variants
                .contains_key(&("MyResult".to_string(), "Miss".to_string())),
            "unit variants should not produce an entry"
        );

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

        let ping = ft
            .enum_variants
            .get(&("Payload".to_string(), "Ping".to_string()))
            .expect("Payload::Ping should be indexed");
        assert_eq!(ping.len(), 1);
        assert!(matches!(
            &ping[0],
            VariantFieldSource::Concrete(t) if t == "Header"
        ));

        let node = ft
            .enum_variants
            .get(&("Tree".to_string(), "Node".to_string()))
            .expect("Tree::Node should be indexed");
        assert!(matches!(
            &node[0],
            VariantFieldSource::Concrete(t) if t == "Tree"
        ));

        assert_eq!(
            ft.fn_returns_tuple.get("pair").map(Vec::as_slice),
            Some(["Foo".to_string(), "Bar".to_string()].as_slice())
        );

        assert_eq!(
            ft.method_returns_tuple
                .get(&("Wrapper".to_string(), "split".to_string()))
                .map(Vec::as_slice),
            Some(["Wrapper".to_string(), "Foo".to_string()].as_slice())
        );
    }

    #[test]
    fn rust_collect_type_param_names_skips_lifetimes_and_consts() {
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
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Bar::run")),
            "cross-enum binding leaked, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_let_tuple_destructure_from_fn_call_binds_each_element() {
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

    #[test]
    fn rust_collect_signatures_populates_enum_struct_variants() {
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

        assert!(!ft
            .enum_struct_variants
            .contains_key(&("Payload".to_string(), "Pong".to_string())));

        let some = ft
            .enum_struct_variants
            .get(&("Maybe".to_string(), "Some".to_string()))
            .expect("Maybe::Some struct variant should be indexed");
        assert_eq!(some.len(), 2);
        assert_eq!(some[0].0, "val");
        assert!(matches!(some[0].1, VariantFieldSource::Generic(0)));
        assert_eq!(some[1].0, "tag");
        assert!(matches!(&some[1].1, VariantFieldSource::Concrete(t) if t == "Header"));

        let err = ft
            .enum_struct_variants
            .get(&("Maybe".to_string(), "Err".to_string()))
            .expect("Maybe::Err struct variant should be indexed");
        assert!(matches!(err[0].1, VariantFieldSource::Generic(1)));

        let node = ft
            .enum_struct_variants
            .get(&("Tree".to_string(), "Node".to_string()))
            .expect("Tree::Node struct variant should be indexed");
        assert_eq!(node[0].0, "child");
        assert!(matches!(&node[0].1, VariantFieldSource::Concrete(t) if t == "Tree"));
    }

    #[test]
    fn rust_if_let_qualified_struct_variant_binds_concrete_field() {
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
        assert!(
            !call_targets.iter().any(|t| t.ends_with("::Foo::run")),
            "expected no Foo::run from non-intersecting or-pattern, got {:?}",
            call_targets
        );
    }

    #[test]
    fn rust_or_pattern_reversed_order_still_binds() {
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

    #[test]
    fn lua_top_level_function_extracted() {
        let src = "function add(a, b) return a + b end\n";
        let r = parse(Language::Lua, src, &PathBuf::from("calc.lua"));
        let n = r.nodes.iter().find(|n| n.name == "add").expect("add node");
        assert!(matches!(n.kind, NodeKind::Function));
        assert_eq!(n.qualified_name, "calc::add");
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
        assert!(n.signature.as_deref().unwrap_or("").starts_with("local "));
    }

    #[test]
    fn lua_anonymous_function_assigned_to_table_is_method() {
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
        let src = "function release {\n    echo 'release'\n}\n";
        let r = parse(Language::Bash, src, &PathBuf::from("ops.sh"));
        assert!(r
            .nodes
            .iter()
            .any(|n| n.name == "release" && matches!(n.kind, NodeKind::Function)));
    }

    #[test]
    fn bash_local_var_in_function_emits_constant() {
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
