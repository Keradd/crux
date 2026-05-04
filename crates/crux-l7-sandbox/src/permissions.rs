//! Permission rule model + matcher shared by L7 sandbox callers and
//! the OpenClaw / Claude Code config loaders in [`crate::agent_perms`].
//!
//! The pattern format is the union of two real-world wirings:
//!
//! - **Claude Code** (`~/.claude/settings.json`):
//!   `permissions.{deny,allow}: ["Bash(rm -rf *)", "Read(.env)", ...]`.
//!   Each entry is a string of the shape `ToolName(<arg-pattern>)` or a
//!   bare `ToolName` that applies to any invocation.
//! - **OpenClaw** (`~/.openclaw/openclaw.json`):
//!   `tools.{deny,allow}: ["exec", "browser", "group:fs"]`. OpenClaw
//!   typically lists tool *identifiers* without an argument pattern,
//!   but accepts the same `Tool(pattern)` shape for parity.
//!
//! Either source can be merged into a single [`Permissions`] bundle
//! and then evaluated against an upcoming `ExecRequest` to decide
//! whether the runtime+code pair is allowed.
//!
//! The matcher is intentionally conservative:
//!
//! 1. `allow` rules win over `deny` rules (so a project-level allow can
//!    re-enable something a global deny disabled).
//! 2. Patterns are matched against the *raw code body* using a simple
//!    substring rule with leading / trailing `*` and `:` stripped. This
//!    is good enough to catch the patterns Claude Code users actually
//!    write (`Bash(rm -rf *)`, `Bash(sudo *)`, `Bash(npm install:*)`)
//!    without pulling in a glob crate.
//! 3. Tool ⇄ runtime mapping is explicit (`Bash` / `exec` →
//!    `RuntimeKind::Bash`, etc). Tools that don't map to an L7 runtime
//!    (`Read`, `Write`, `Edit`, MCP tools, …) are simply ignored — they
//!    belong to other layers.

use serde::{Deserialize, Serialize};

use crate::types::RuntimeKind;

/// Provenance of a permission rule. Surfaced in error messages so the
/// user knows *which* config file produced the deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermSource {
    ClaudeCode,
    OpenClaw,
}

impl PermSource {
    pub fn label(self) -> &'static str {
        match self {
            PermSource::ClaudeCode => "claude-code",
            PermSource::OpenClaw => "openclaw",
        }
    }
}

/// Whether a rule was sourced from a global agent config or from the
/// project-local override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermScope {
    Global,
    Project,
}

impl PermScope {
    pub fn label(self) -> &'static str {
        match self {
            PermScope::Global => "global",
            PermScope::Project => "project",
        }
    }
}

/// One parsed `Tool(arg_pattern)` rule. The original spec string is
/// preserved verbatim in `raw` so error messages can echo the user's
/// own wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermRule {
    /// Tool identifier as written by the user (`Bash`, `exec`, `Read`,
    /// `group:fs`, ...). Case-preserved; matching lowers it.
    pub tool: String,
    /// Argument pattern from inside the parens, or the empty string
    /// when the user wrote a bare `ToolName`.
    pub pattern: String,
    /// Original spec string, e.g. `"Bash(rm -rf *)"`. Echoed back in
    /// errors verbatim.
    pub raw: String,
    pub source: PermSource,
    pub scope: PermScope,
}

impl PermRule {
    /// Parse a single Claude-Code-style spec. Returns `None` for empty
    /// strings, `Some(_)` for everything else; malformed parens fall
    /// back to a bare-tool rule rather than dropping the entry, since
    /// dropping silently is worse than a too-broad rule.
    pub fn parse(spec: &str, source: PermSource, scope: PermScope) -> Option<Self> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return None;
        }
        let raw = trimmed.to_string();
        if let Some(open) = trimmed.find('(') {
            if let Some(close) = trimmed.rfind(')') {
                if close > open {
                    let tool = trimmed[..open].trim();
                    if tool.is_empty() {
                        return None;
                    }
                    let pattern = trimmed[open + 1..close].trim();
                    return Some(Self {
                        tool: tool.to_string(),
                        pattern: pattern.to_string(),
                        raw,
                        source,
                        scope,
                    });
                }
            }
            // Unclosed `(` — fall through to bare-tool fallback rather
            // than silently dropping the rule.
        }
        Some(Self {
            tool: trimmed.to_string(),
            pattern: String::new(),
            raw,
            source,
            scope,
        })
    }

    /// True when this rule's tool identifier covers the given runtime.
    /// Recognised aliases mirror the canonical names in Claude Code +
    /// OpenClaw docs:
    ///
    /// | Tool       | Runtimes |
    /// |------------|----------|
    /// | `bash`     | Bash |
    /// | `exec`     | Bash (OpenClaw shell exec) |
    /// | `python`/`py` | Python |
    /// | `node`/`js`/`javascript` | Node |
    /// | `*`        | all runtimes |
    pub fn maps_to_runtime(&self, runtime: RuntimeKind) -> bool {
        let t = self.tool.to_ascii_lowercase();
        if t == "*" {
            return true;
        }
        match runtime {
            RuntimeKind::Bash => matches!(t.as_str(), "bash" | "exec" | "shell" | "sh"),
            RuntimeKind::Python => matches!(t.as_str(), "python" | "py" | "python3"),
            RuntimeKind::Node => matches!(t.as_str(), "node" | "js" | "javascript" | "deno"),
        }
    }

    /// True when the rule's argument pattern matches the supplied code
    /// body. Empty / pure-wildcard patterns match anything.
    pub fn pattern_matches(&self, code: &str) -> bool {
        pattern_matches_code(&self.pattern, code)
    }
}

/// Substring-style matcher: trims leading / trailing `*` and `:`
/// segments (so `npm install:*` ⇒ `npm install`, `*rm -rf*` ⇒
/// `rm -rf`), then does a byte-substring lookup. Empty needle ⇒ match
/// everything.
pub(crate) fn pattern_matches_code(pattern: &str, code: &str) -> bool {
    let needle = pattern
        .trim()
        .trim_start_matches('*')
        .trim_end_matches('*')
        .trim_end_matches(':')
        .trim_start_matches(':')
        .trim();
    if needle.is_empty() {
        return true;
    }
    code.contains(needle)
}

/// Outcome of evaluating a runtime+code pair against a [`Permissions`]
/// bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermDecision {
    /// No deny rule matched (or an allow rule overrode the match).
    Allow,
    /// A deny rule matched — the caller should refuse to spawn the
    /// child. The matching rule is included verbatim so error messages
    /// can echo the user's spec back to them.
    Deny(PermRule),
}

/// Bundle of allow + deny rules, typically produced by
/// [`crate::agent_perms::load_unioned`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    pub deny: Vec<PermRule>,
    pub allow: Vec<PermRule>,
}

impl Permissions {
    pub fn new(deny: Vec<PermRule>, allow: Vec<PermRule>) -> Self {
        Self { deny, allow }
    }

    /// Apply the union of rules to a runtime+code pair. Allow first
    /// (so a project allow re-enables a global deny); deny second.
    pub fn evaluate(&self, runtime: RuntimeKind, code: &str) -> PermDecision {
        for rule in &self.allow {
            if rule.maps_to_runtime(runtime) && rule.pattern_matches(code) {
                return PermDecision::Allow;
            }
        }
        for rule in &self.deny {
            if rule.maps_to_runtime(runtime) && rule.pattern_matches(code) {
                return PermDecision::Deny(rule.clone());
            }
        }
        PermDecision::Allow
    }

    /// Total number of rules. Useful for L9 audit reporting.
    pub fn len(&self) -> usize {
        self.deny.len() + self.allow.len()
    }

    pub fn is_empty(&self) -> bool {
        self.deny.is_empty() && self.allow.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(spec: &str) -> PermRule {
        PermRule::parse(spec, PermSource::ClaudeCode, PermScope::Global)
            .expect("parse should succeed for non-empty spec")
    }

    #[test]
    fn parse_tool_with_pattern() {
        let r = rule("Bash(rm -rf *)");
        assert_eq!(r.tool, "Bash");
        assert_eq!(r.pattern, "rm -rf *");
        assert_eq!(r.raw, "Bash(rm -rf *)");
    }

    #[test]
    fn parse_bare_tool_has_empty_pattern() {
        let r = rule("Bash");
        assert_eq!(r.tool, "Bash");
        assert!(r.pattern.is_empty());
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(PermRule::parse("", PermSource::ClaudeCode, PermScope::Global).is_none());
        assert!(PermRule::parse("   ", PermSource::ClaudeCode, PermScope::Global).is_none());
    }

    #[test]
    fn parse_unclosed_paren_falls_back_to_bare_tool() {
        // Better to over-block than silently drop the rule. The whole
        // `Bash(rm` becomes the tool identifier (which won't match any
        // real runtime), so it's a no-op rather than a security hole.
        let r = PermRule::parse("Bash(rm", PermSource::ClaudeCode, PermScope::Global).unwrap();
        assert_eq!(r.tool, "Bash(rm");
        assert!(r.pattern.is_empty());
    }

    #[test]
    fn parse_handles_openclaw_group_syntax() {
        let r = PermRule::parse("group:fs", PermSource::OpenClaw, PermScope::Project).unwrap();
        assert_eq!(r.tool, "group:fs");
        assert_eq!(r.source, PermSource::OpenClaw);
        assert_eq!(r.scope, PermScope::Project);
    }

    #[test]
    fn maps_bash_aliases_to_bash_runtime() {
        for alias in &["Bash", "bash", "exec", "shell", "sh"] {
            let r = rule(&format!("{}(*)", alias));
            assert!(
                r.maps_to_runtime(RuntimeKind::Bash),
                "{alias} should map to Bash"
            );
            assert!(!r.maps_to_runtime(RuntimeKind::Python));
        }
    }

    #[test]
    fn maps_python_and_node_aliases() {
        assert!(rule("python(*)").maps_to_runtime(RuntimeKind::Python));
        assert!(rule("py(*)").maps_to_runtime(RuntimeKind::Python));
        assert!(rule("node(*)").maps_to_runtime(RuntimeKind::Node));
        assert!(rule("js(*)").maps_to_runtime(RuntimeKind::Node));
    }

    #[test]
    fn star_tool_covers_every_runtime() {
        let r = rule("*(rm -rf *)");
        assert!(r.maps_to_runtime(RuntimeKind::Bash));
        assert!(r.maps_to_runtime(RuntimeKind::Python));
        assert!(r.maps_to_runtime(RuntimeKind::Node));
    }

    #[test]
    fn unrelated_tool_does_not_map() {
        // `Read(.env)` is a Claude Code file-read deny — L7 should
        // ignore it, not silently translate it to a runtime block.
        assert!(!rule("Read(.env)").maps_to_runtime(RuntimeKind::Bash));
        assert!(!rule("Edit(*)").maps_to_runtime(RuntimeKind::Python));
        assert!(!rule("Write(.git/**)").maps_to_runtime(RuntimeKind::Node));
    }

    #[test]
    fn pattern_substring_match() {
        assert!(pattern_matches_code("rm -rf", "rm -rf /tmp/x"));
        assert!(pattern_matches_code("rm -rf *", "rm -rf /tmp/x"));
        assert!(pattern_matches_code("npm install:*", "npm install --foo"));
        assert!(!pattern_matches_code("rm -rf", "echo hi"));
    }

    #[test]
    fn empty_or_wildcard_pattern_matches_everything() {
        assert!(pattern_matches_code("", "anything"));
        assert!(pattern_matches_code("*", "anything"));
        assert!(pattern_matches_code("**", "anything"));
        assert!(pattern_matches_code(":*", "anything"));
    }

    #[test]
    fn evaluate_denies_matching_bash_code() {
        let perms = Permissions::new(vec![rule("Bash(rm -rf *)")], vec![]);
        let dec = perms.evaluate(RuntimeKind::Bash, "rm -rf /tmp/x");
        match dec {
            PermDecision::Deny(r) => assert_eq!(r.raw, "Bash(rm -rf *)"),
            _ => panic!("expected deny"),
        }
    }

    #[test]
    fn evaluate_allows_python_when_only_bash_is_denied() {
        let perms = Permissions::new(vec![rule("Bash(rm -rf *)")], vec![]);
        let dec = perms.evaluate(RuntimeKind::Python, "import os; os.system('rm -rf /')");
        // Bash deny must NOT translate to Python runtime — the user
        // would have to write `python(*)` for that.
        assert_eq!(dec, PermDecision::Allow);
    }

    #[test]
    fn allow_overrides_deny() {
        let perms = Permissions::new(
            vec![rule("Bash(rm *)")],          // deny rm in bash
            vec![rule("Bash(rm -rf project/*)")], // but allow this specific
        );
        // The allow list is checked first, so even though the deny rule
        // matches, the allow path wins.
        assert_eq!(
            perms.evaluate(RuntimeKind::Bash, "rm -rf project/scratch"),
            PermDecision::Allow
        );
        // Anything not covered by the allow still falls through to deny.
        match perms.evaluate(RuntimeKind::Bash, "rm /etc/passwd") {
            PermDecision::Deny(r) => assert_eq!(r.raw, "Bash(rm *)"),
            _ => panic!("expected deny"),
        }
    }

    #[test]
    fn evaluate_with_empty_perms_always_allows() {
        let perms = Permissions::default();
        assert!(perms.is_empty());
        assert_eq!(
            perms.evaluate(RuntimeKind::Bash, "rm -rf /"),
            PermDecision::Allow
        );
    }

    #[test]
    fn unclosed_paren_rule_is_a_noop_not_a_universal_block() {
        // Regression: parse fallback turns `Bash(rm` into a tool whose
        // name doesn't match any runtime, so it must NOT block real
        // bash code.
        let perms = Permissions::new(
            vec![PermRule::parse("Bash(rm", PermSource::ClaudeCode, PermScope::Global).unwrap()],
            vec![],
        );
        assert_eq!(
            perms.evaluate(RuntimeKind::Bash, "rm -rf /"),
            PermDecision::Allow
        );
    }
}
