use serde::{Deserialize, Serialize};

use crate::types::RuntimeKind;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermRule {
    pub tool: String,
    pub pattern: String,
    pub raw: String,
    pub source: PermSource,
    pub scope: PermScope,
}

impl PermRule {
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
        }
        Some(Self {
            tool: trimmed.to_string(),
            pattern: String::new(),
            raw,
            source,
            scope,
        })
    }

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

    pub fn pattern_matches(&self, code: &str) -> bool {
        pattern_matches_code(&self.pattern, code)
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermDecision {
    Allow,
    Deny(PermRule),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    pub deny: Vec<PermRule>,
    pub allow: Vec<PermRule>,
}

impl Permissions {
    pub fn new(deny: Vec<PermRule>, allow: Vec<PermRule>) -> Self {
        Self { deny, allow }
    }

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

    pub fn has_unknown_tool_rules(&self) -> bool {
        let known_tools = [
            "bash",
            "exec",
            "shell",
            "sh",
            "python",
            "py",
            "python3",
            "node",
            "js",
            "javascript",
            "deno",
            "*",
        ];
        for rule in &self.allow {
            let t = rule.tool.to_ascii_lowercase();
            if !known_tools.contains(&t.as_str()) {
                return true;
            }
        }
        for rule in &self.deny {
            let t = rule.tool.to_ascii_lowercase();
            if !known_tools.contains(&t.as_str()) {
                return true;
            }
        }
        false
    }

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
        assert_eq!(dec, PermDecision::Allow);
    }

    #[test]
    fn allow_overrides_deny() {
        let perms = Permissions::new(
            vec![rule("Bash(rm *)")],             // deny rm in bash
            vec![rule("Bash(rm -rf project/*)")], // but allow this specific
        );
        assert_eq!(
            perms.evaluate(RuntimeKind::Bash, "rm -rf project/scratch"),
            PermDecision::Allow
        );
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
