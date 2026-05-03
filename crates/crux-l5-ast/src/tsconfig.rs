//! L5.13g — TypeScript / JavaScript path-mapping resolver.
//!
//! Loads `tsconfig.json` (or `jsconfig.json`) at the project root,
//! follows any `extends` chain, and exposes a [`JsModuleResolver`] that
//! maps non-relative module specifiers (`@/foo`, `~components/Button`,
//! `@app/utils/x`) to project-relative module paths via
//! `compilerOptions.paths` and `compilerOptions.baseUrl`.
//!
//! The resolver is best-effort: a missing or unparseable config returns
//! `None` and the cross-file resolver falls back to the relative-only
//! L5.13e logic.
//!
//! Supported subset:
//!
//! - `compilerOptions.baseUrl` — anchors non-relative specs that match
//!   no alias.
//! - `compilerOptions.paths` — `{ pattern: [target, ...] }` with at
//!   most one `*` per pattern / target. Targets are tried in
//!   declaration order.
//! - `extends` — relative path to a parent tsconfig. Child
//!   `paths` / `baseUrl` fully replace the parent's (TypeScript
//!   semantics). `extends` chains terminate at depth 16 or on cycle.
//! - JSON-with-comments: `//` line comments, `/* ... */` block
//!   comments, and trailing commas before `}` / `]` are stripped
//!   before parsing.
//!
//! Out of scope (for now): npm-package `extends` (e.g.
//! `@tsconfig/node18`), wildcards beyond a single `*`, glob targets,
//! and `paths` resolution into `node_modules`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crux_core::error::{CruxError, Result};

const KNOWN_EXTENSIONS: &[&str] = &[".d.ts", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
const MAX_EXTENDS_DEPTH: usize = 16;

/// Resolved tsconfig view exposing path-mapping queries.
#[derive(Debug, Clone, Default)]
pub struct JsModuleResolver {
    project_root: PathBuf,
    base_url: Option<PathBuf>,
    aliases: Vec<AliasEntry>,
}

#[derive(Debug, Clone)]
struct AliasEntry {
    pattern: AliasPart,
    targets: Vec<AliasPart>,
    base_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct AliasPart {
    prefix: String,
    suffix: String,
    has_star: bool,
}

#[derive(Default, Deserialize)]
struct TsConfigFile {
    extends: Option<String>,
    #[serde(rename = "compilerOptions")]
    compiler_options: Option<CompilerOptions>,
}

#[derive(Default, Deserialize)]
struct CompilerOptions {
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    paths: Option<BTreeMap<String, Vec<String>>>,
}

impl JsModuleResolver {
    /// Try to load `tsconfig.json` (then `jsconfig.json`) from the
    /// project root. Returns `None` when neither exists, parsing fails,
    /// or the config is empty of path-mapping data.
    pub fn load(project_root: &Path) -> Option<Self> {
        for name in &["tsconfig.json", "jsconfig.json"] {
            let p = project_root.join(name);
            if p.is_file() {
                if let Ok(r) = load_chain(project_root, &p, &mut HashSet::new(), 0) {
                    if r.aliases.is_empty() && r.base_url.is_none() {
                        return None;
                    }
                    return Some(r);
                }
            }
        }
        None
    }

    /// Resolve a non-relative module spec via path mappings + baseUrl.
    /// Returns project-relative module paths (no extension,
    /// forward-slash separated) in TypeScript priority order. Returns
    /// an empty vec for relative specs (`./foo`, `../bar`) and for
    /// specs no rule covers.
    pub fn resolve(&self, spec: &str) -> Vec<String> {
        if spec.starts_with("./") || spec.starts_with("../") {
            return Vec::new();
        }
        let mut out = Vec::new();
        for alias in &self.aliases {
            let Some(matched) = match_pattern(&alias.pattern, spec) else {
                continue;
            };
            let prev_len = out.len();
            for target in &alias.targets {
                let substituted = substitute_target(target, matched);
                let abs = alias.base_dir.join(&substituted);
                if let Some(rel) = make_project_relative(&self.project_root, &abs) {
                    if !out.contains(&rel) {
                        out.push(rel);
                    }
                }
            }
            if out.len() > prev_len {
                // TypeScript's `paths` matches the most-specific pattern
                // and stops; sort earlier in `load_chain` ensures
                // concrete patterns are probed before wildcards.
                break;
            }
        }
        if out.is_empty() {
            if let Some(base) = &self.base_url {
                let abs = base.join(spec);
                if let Some(rel) = make_project_relative(&self.project_root, &abs) {
                    out.push(rel);
                }
            }
        }
        out
    }

    /// True iff the resolver carries any usable path-mapping data.
    pub fn is_active(&self) -> bool {
        !self.aliases.is_empty() || self.base_url.is_some()
    }
}

fn load_chain(
    project_root: &Path,
    config_path: &Path,
    seen: &mut HashSet<PathBuf>,
    depth: usize,
) -> Result<JsModuleResolver> {
    if depth > MAX_EXTENDS_DEPTH {
        return Err(CruxError::other("tsconfig extends chain too deep"));
    }
    let canon = path_clean::clean(config_path);
    if !seen.insert(canon.clone()) {
        return Err(CruxError::other("tsconfig extends cycle"));
    }
    let raw = fs::read_to_string(&canon).map_err(|e| CruxError::Io {
        path: canon.clone(),
        source: e,
    })?;
    let stripped = strip_jsonc(&raw);
    let parsed: TsConfigFile = serde_json::from_str(&stripped)
        .map_err(|e| CruxError::other(format!("parse {}: {}", canon.display(), e)))?;

    let mut effective = if let Some(extends) = parsed.extends.as_deref() {
        let parent = resolve_extends_path(&canon, extends);
        if parent.is_file() {
            load_chain(project_root, &parent, seen, depth + 1).unwrap_or_else(|_| {
                JsModuleResolver {
                    project_root: project_root.to_path_buf(),
                    ..Default::default()
                }
            })
        } else {
            JsModuleResolver {
                project_root: project_root.to_path_buf(),
                ..Default::default()
            }
        }
    } else {
        JsModuleResolver {
            project_root: project_root.to_path_buf(),
            ..Default::default()
        }
    };
    effective.project_root = project_root.to_path_buf();

    let config_dir = canon
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(""));

    if let Some(co) = parsed.compiler_options {
        if let Some(bu) = co.base_url {
            effective.base_url = Some(path_clean::clean(config_dir.join(&bu)));
        }
        if let Some(paths) = co.paths {
            // Anchor `paths` targets at `baseUrl` if set, otherwise at
            // the directory holding this tsconfig — matching the
            // TypeScript compiler's behaviour for `paths` without
            // baseUrl since TS 4.1.
            let alias_base = effective
                .base_url
                .clone()
                .unwrap_or_else(|| config_dir.clone());
            let mut aliases = Vec::new();
            for (pattern_str, targets_raw) in paths {
                let pattern = parse_alias_part(&pattern_str);
                let targets: Vec<AliasPart> =
                    targets_raw.iter().map(|t| parse_alias_part(t)).collect();
                aliases.push(AliasEntry {
                    pattern,
                    targets,
                    base_dir: alias_base.clone(),
                });
            }
            // More-specific patterns (longer prefix, no star) win first.
            aliases.sort_by_key(|a| std::cmp::Reverse(pattern_priority(&a.pattern)));
            effective.aliases = aliases;
        }
    }
    Ok(effective)
}

fn pattern_priority(p: &AliasPart) -> usize {
    // Concrete patterns rank above wildcards; longer prefixes outrank
    // shorter ones so `@app/foo/*` is tried before `@app/*`.
    let star_penalty = if p.has_star { 0 } else { 1_000_000 };
    star_penalty + p.prefix.len() + p.suffix.len()
}

fn resolve_extends_path(canon: &Path, extends: &str) -> PathBuf {
    let parent = canon.parent().unwrap_or_else(|| Path::new(""));
    let mut p = parent.join(extends);
    if !p.is_file() && p.extension().and_then(|e| e.to_str()) != Some("json") {
        let with_json = p.with_extension("json");
        if with_json.is_file() {
            p = with_json;
        }
    }
    p
}

fn parse_alias_part(s: &str) -> AliasPart {
    if let Some(idx) = s.find('*') {
        AliasPart {
            prefix: s[..idx].to_string(),
            suffix: s[idx + 1..].to_string(),
            has_star: true,
        }
    } else {
        AliasPart {
            prefix: s.to_string(),
            suffix: String::new(),
            has_star: false,
        }
    }
}

fn match_pattern<'a>(p: &AliasPart, spec: &'a str) -> Option<&'a str> {
    if p.has_star {
        // TypeScript requires `*` to consume at least one character so
        // `@/` doesn't accidentally match `@/*`.
        if spec.len() > p.prefix.len() + p.suffix.len()
            && spec.starts_with(&p.prefix)
            && spec.ends_with(&p.suffix)
        {
            Some(&spec[p.prefix.len()..spec.len() - p.suffix.len()])
        } else {
            None
        }
    } else if spec == p.prefix {
        Some("")
    } else {
        None
    }
}

fn substitute_target(t: &AliasPart, matched: &str) -> String {
    if t.has_star {
        format!("{}{matched}{}", t.prefix, t.suffix)
    } else {
        t.prefix.clone()
    }
}

fn make_project_relative(root: &Path, abs: &Path) -> Option<String> {
    let cleaned = path_clean::clean(abs);
    let rel = cleaned.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        return None;
    }
    Some(strip_known_ext(&s))
}

fn strip_known_ext(s: &str) -> String {
    for ext in KNOWN_EXTENSIONS {
        if let Some(stripped) = s.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    s.to_string()
}

/// Strip `//` line comments and `/* ... */` block comments while
/// honouring quoted strings. Trailing commas inside arrays / objects
/// are then turned into spaces so `serde_json` accepts the document.
pub(crate) fn strip_jsonc(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_str = false;
    let mut quote: char = '"';
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
                continue;
            }
            if c == quote {
                in_str = false;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            in_str = true;
            quote = c;
            out.push(c);
            continue;
        }
        if c == '/' {
            if let Some(&n) = chars.peek() {
                if n == '/' {
                    chars.next();
                    while let Some(&c2) = chars.peek() {
                        if c2 == '\n' {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                }
                if n == '*' {
                    chars.next();
                    while let Some(c2) = chars.next() {
                        if c2 == '*' {
                            if let Some(&c3) = chars.peek() {
                                if c3 == '/' {
                                    chars.next();
                                    break;
                                }
                            }
                        }
                    }
                    continue;
                }
            }
        }
        out.push(c);
    }
    strip_trailing_commas(&out)
}

fn strip_trailing_commas(src: &str) -> String {
    let mut chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut in_str = false;
    let mut quote: char = '"';
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if in_str {
            if c == '\\' && i + 1 < n {
                i += 2;
                continue;
            }
            if c == quote {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' || c == '\'' {
            in_str = true;
            quote = c;
            i += 1;
            continue;
        }
        if c == '}' || c == ']' {
            let mut j = i;
            while j > 0 {
                j -= 1;
                if !chars[j].is_whitespace() {
                    break;
                }
            }
            if j < n && chars[j] == ',' {
                chars[j] = ' ';
            }
        }
        i += 1;
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mkdir(p: &Path) {
        std::fs::create_dir_all(p).unwrap();
    }

    fn write(p: &Path, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (rel, contents) in files {
            let p = dir.path().join(rel);
            if let Some(parent) = p.parent() {
                mkdir(parent);
            }
            write(&p, contents);
        }
        dir
    }

    #[test]
    fn missing_config_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(JsModuleResolver::load(dir.path()).is_none());
    }

    #[test]
    fn empty_config_returns_none() {
        let dir = fixture(&[("tsconfig.json", "{}")]);
        assert!(JsModuleResolver::load(dir.path()).is_none());
    }

    #[test]
    fn baseurl_only_resolves_bare_specifier() {
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("foo/bar"), vec!["src/foo/bar".to_string()]);
        assert!(r.resolve("./relative").is_empty());
    }

    #[test]
    fn paths_alias_resolves_with_wildcard() {
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": {
                  "@/*": ["src/*"]
                }
              }
            }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@/foo"), vec!["src/foo".to_string()]);
        assert_eq!(
            r.resolve("@/foo/bar/baz"),
            vec!["src/foo/bar/baz".to_string()]
        );
        // `@/` (empty wildcard match) does not satisfy the alias; it
        // can still be picked up by the baseUrl fallback, which is
        // harmless because no real module will be named "@".
    }

    #[test]
    fn paths_concrete_alias_overrides_wildcard() {
        // TypeScript probes more-specific patterns first; we sort by
        // prefix length and `*`-presence to mirror that.
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": {
                  "@/*": ["src/*"],
                  "@/special": ["lib/special-case"]
                }
              }
            }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@/special"), vec!["lib/special-case".to_string()]);
        assert_eq!(r.resolve("@/foo"), vec!["src/foo".to_string()]);
    }

    #[test]
    fn paths_multi_target_returns_priority_order() {
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": {
                  "~/*": ["src/*", "vendor/*"]
                }
              }
            }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(
            r.resolve("~/foo"),
            vec!["src/foo".to_string(), "vendor/foo".to_string()]
        );
    }

    #[test]
    fn paths_without_baseurl_anchors_at_config_dir() {
        // TS 4.1+: `paths` works without an explicit `baseUrl`.
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{
              "compilerOptions": {
                "paths": { "@/*": ["src/*"] }
              }
            }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@/foo"), vec!["src/foo".to_string()]);
    }

    #[test]
    fn extends_chain_inherits_baseurl() {
        let dir = fixture(&[
            (
                "base.json",
                r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
            ),
            ("tsconfig.json", r#"{ "extends": "./base.json" }"#),
        ]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("foo"), vec!["src/foo".to_string()]);
    }

    #[test]
    fn extends_chain_child_paths_replace_parent() {
        // TS semantics: child `paths` fully overrides parent `paths`.
        let dir = fixture(&[
            (
                "base.json",
                r#"{
                  "compilerOptions": {
                    "baseUrl": ".",
                    "paths": { "@/*": ["base-src/*"] }
                  }
                }"#,
            ),
            (
                "tsconfig.json",
                r#"{
                  "extends": "./base.json",
                  "compilerOptions": {
                    "paths": { "@/*": ["app-src/*"] }
                  }
                }"#,
            ),
        ]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@/foo"), vec!["app-src/foo".to_string()]);
    }

    #[test]
    fn extends_without_explicit_json_extension_is_appended() {
        let dir = fixture(&[
            (
                "base.json",
                r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
            ),
            ("tsconfig.json", r#"{ "extends": "./base" }"#),
        ]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("foo"), vec!["src/foo".to_string()]);
    }

    #[test]
    fn jsonc_comments_and_trailing_commas_are_stripped() {
        // No `baseUrl` so the alias target stays project-relative; the
        // point of this test is the JSONC stripper, not TS
        // baseUrl semantics.
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{
              // line comment
              "compilerOptions": {
                /* block comment */
                "paths": {
                  "@/*": ["lib/*",],
                },
              },
            }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@/foo"), vec!["lib/foo".to_string()]);
    }

    #[test]
    fn jsconfig_json_is_used_when_tsconfig_absent() {
        let dir = fixture(&[(
            "jsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("foo"), vec!["src/foo".to_string()]);
    }

    #[test]
    fn unmatched_specifier_returns_empty() {
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@/*": ["src/*"] } } }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        // No baseUrl so bare specifiers fall through.
        assert!(r.resolve("react").is_empty());
    }

    #[test]
    fn relative_specifier_short_circuits() {
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "baseUrl": "src" } }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert!(r.resolve("./x").is_empty());
        assert!(r.resolve("../y").is_empty());
    }

    #[test]
    fn target_with_known_extension_is_stripped() {
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{ "compilerOptions": { "paths": { "@types": ["lib/types.d.ts"] } } }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@types"), vec!["lib/types".to_string()]);
    }

    #[test]
    fn extends_cycle_does_not_loop() {
        let dir = fixture(&[
            ("a.json", r#"{ "extends": "./b.json" }"#),
            (
                "b.json",
                r#"{ "extends": "./a.json", "compilerOptions": { "baseUrl": "src" } }"#,
            ),
            ("tsconfig.json", r#"{ "extends": "./a.json" }"#),
        ]);
        // We don't care about the resolved paths here, only that the
        // load terminates without a stack overflow.
        let _ = JsModuleResolver::load(dir.path());
    }

    #[test]
    fn ordering_independent_of_btree_iteration() {
        // BTreeMap iterates alphabetically. Confirm specificity sort
        // wins regardless of key order.
        let dir = fixture(&[(
            "tsconfig.json",
            r#"{
              "compilerOptions": {
                "baseUrl": ".",
                "paths": {
                  "@/special": ["lib/concrete"],
                  "@/*": ["src/*"]
                }
              }
            }"#,
        )]);
        let r = JsModuleResolver::load(dir.path()).unwrap();
        assert_eq!(r.resolve("@/special"), vec!["lib/concrete".to_string()]);
        assert_eq!(r.resolve("@/other"), vec!["src/other".to_string()]);
    }
}
