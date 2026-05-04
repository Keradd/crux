use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::permissions::{PermRule, PermScope, PermSource, Permissions};

const CLAUDE_HOME_DIRNAME: &str = ".claude";
const CLAUDE_SETTINGS: &str = "settings.json";
const OPENCLAW_HOME_DIRNAME: &str = ".openclaw";
const OPENCLAW_CONFIG: &str = "openclaw.json";
const ENV_OPENCLAW_CONFIG: &str = "OPENCLAW_CONFIG_PATH";

pub fn load_unioned(project_root: Option<&Path>, home_dir: Option<&Path>) -> Permissions {
    let mut deny: Vec<PermRule> = Vec::new();
    let mut allow: Vec<PermRule> = Vec::new();

    if let Some(home) = home_dir {
        load_claude_into(
            &home.join(CLAUDE_HOME_DIRNAME).join(CLAUDE_SETTINGS),
            PermScope::Global,
            &mut deny,
            &mut allow,
        );
    }
    if let Some(root) = project_root {
        load_claude_into(
            &root.join(CLAUDE_HOME_DIRNAME).join(CLAUDE_SETTINGS),
            PermScope::Project,
            &mut deny,
            &mut allow,
        );
    }
    if let Some(home) = home_dir {
        load_openclaw_into(
            &home.join(OPENCLAW_HOME_DIRNAME).join(OPENCLAW_CONFIG),
            PermScope::Global,
            &mut deny,
            &mut allow,
        );
    }
    if let Ok(path) = std::env::var(ENV_OPENCLAW_CONFIG) {
        if !path.is_empty() {
            load_openclaw_into(Path::new(&path), PermScope::Global, &mut deny, &mut allow);
        }
    }
    if let Some(root) = project_root {
        load_openclaw_into(
            &root.join(OPENCLAW_HOME_DIRNAME).join(OPENCLAW_CONFIG),
            PermScope::Project,
            &mut deny,
            &mut allow,
        );
    }

    Permissions::new(deny, allow)
}

fn load_claude_into(
    path: &Path,
    scope: PermScope,
    deny: &mut Vec<PermRule>,
    allow: &mut Vec<PermRule>,
) {
    let Some(json) = read_json_silent(path) else {
        return;
    };
    let perms = json.pointer("/permissions");
    let denies = perms.and_then(|v| v.get("deny"));
    let allows = perms.and_then(|v| v.get("allow"));
    push_string_array(denies, PermSource::ClaudeCode, scope, deny);
    push_string_array(allows, PermSource::ClaudeCode, scope, allow);
}

fn load_openclaw_into(
    path: &Path,
    scope: PermScope,
    deny: &mut Vec<PermRule>,
    allow: &mut Vec<PermRule>,
) {
    let Some(json) = read_json_silent(path) else {
        return;
    };
    for ptr in &["/tools/deny", "/deny"] {
        push_string_array(json.pointer(ptr), PermSource::OpenClaw, scope, deny);
    }
    for ptr in &["/tools/allow", "/allow"] {
        push_string_array(json.pointer(ptr), PermSource::OpenClaw, scope, allow);
    }
}

fn push_string_array(
    node: Option<&Value>,
    source: PermSource,
    scope: PermScope,
    out: &mut Vec<PermRule>,
) {
    let Some(arr) = node.and_then(|v| v.as_array()) else {
        return;
    };
    for entry in arr {
        if let Some(s) = entry.as_str() {
            if let Some(rule) = PermRule::parse(s, source, scope) {
                out.push(rule);
            }
        }
    }
}

fn read_json_silent(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Value>(&raw) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "ignoring malformed agent config"
            );
            None
        }
    }
}

pub fn load_for_project(project_root: Option<&Path>) -> Permissions {
    load_unioned(project_root, dirs::home_dir().as_deref())
}

#[cfg(test)]
pub(crate) fn load_from_paths(
    claude_global: Option<&Path>,
    claude_project: Option<&Path>,
    openclaw_global: Option<&Path>,
    openclaw_project: Option<&Path>,
) -> Permissions {
    let mut deny = Vec::new();
    let mut allow = Vec::new();
    if let Some(p) = claude_global {
        load_claude_into(p, PermScope::Global, &mut deny, &mut allow);
    }
    if let Some(p) = claude_project {
        load_claude_into(p, PermScope::Project, &mut deny, &mut allow);
    }
    if let Some(p) = openclaw_global {
        load_openclaw_into(p, PermScope::Global, &mut deny, &mut allow);
    }
    if let Some(p) = openclaw_project {
        load_openclaw_into(p, PermScope::Project, &mut deny, &mut allow);
    }
    Permissions::new(deny, allow)
}

pub fn claude_settings_path(dir: &Path) -> PathBuf {
    dir.join(CLAUDE_HOME_DIRNAME).join(CLAUDE_SETTINGS)
}

pub fn openclaw_config_path(dir: &Path) -> PathBuf {
    dir.join(OPENCLAW_HOME_DIRNAME).join(OPENCLAW_CONFIG)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::PermDecision;
    use crate::types::RuntimeKind;
    use std::fs;

    fn write(p: &Path, body: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn missing_files_yield_empty_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let perms = load_from_paths(
            Some(&dir.path().join("nope1")),
            Some(&dir.path().join("nope2")),
            Some(&dir.path().join("nope3")),
            Some(&dir.path().join("nope4")),
        );
        assert!(perms.is_empty(), "missing files should be no-op");
    }

    #[test]
    fn parses_claude_permissions_deny_and_allow() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write(
            &p,
            r#"{
                "permissions": {
                    "deny":  ["Bash(rm -rf *)", "Read(.env)"],
                    "allow": ["Bash(git:*)"]
                }
            }"#,
        );
        let perms = load_from_paths(Some(&p), None, None, None);
        assert_eq!(perms.deny.len(), 2);
        assert_eq!(perms.allow.len(), 1);
        assert!(perms
            .deny
            .iter()
            .all(|r| r.source == PermSource::ClaudeCode));
        assert!(perms.deny.iter().all(|r| r.scope == PermScope::Global));
        assert_eq!(perms.allow[0].raw, "Bash(git:*)");
    }

    #[test]
    fn parses_openclaw_tools_deny_and_allow() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("openclaw.json");
        write(
            &p,
            r#"{
                "tools": {
                    "deny":  ["exec", "Bash(sudo *)"],
                    "allow": ["browser"]
                }
            }"#,
        );
        let perms = load_from_paths(None, None, Some(&p), None);
        assert_eq!(perms.deny.len(), 2);
        assert_eq!(perms.allow.len(), 1);
        assert!(perms.deny.iter().all(|r| r.source == PermSource::OpenClaw));
    }

    #[test]
    fn parses_openclaw_top_level_deny_for_legacy_configs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("openclaw.json");
        write(&p, r#"{ "deny": ["exec"], "allow": ["bash"] }"#);
        let perms = load_from_paths(None, None, Some(&p), None);
        assert_eq!(perms.deny.len(), 1);
        assert_eq!(perms.allow.len(), 1);
        assert_eq!(perms.deny[0].tool, "exec");
    }

    #[test]
    fn project_scope_is_recorded_separately_from_global() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path().join("global.json");
        let l = dir.path().join("local.json");
        write(&g, r#"{ "permissions": { "deny": ["Bash(rm *)"] } }"#);
        write(&l, r#"{ "permissions": { "allow": ["Bash(rm tmp/*)"] } }"#);
        let perms = load_from_paths(Some(&g), Some(&l), None, None);
        assert_eq!(perms.deny.len(), 1);
        assert_eq!(perms.allow.len(), 1);
        assert_eq!(perms.deny[0].scope, PermScope::Global);
        assert_eq!(perms.allow[0].scope, PermScope::Project);
    }

    #[test]
    fn malformed_json_is_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write(&p, "{ this is not json");
        let perms = load_from_paths(Some(&p), None, None, None);
        assert!(perms.is_empty());
    }

    #[test]
    fn unioned_decision_blocks_rm_rf() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(".claude").join("settings.json");
        write(
            &settings,
            r#"{ "permissions": { "deny": ["Bash(rm -rf *)"] } }"#,
        );
        let perms = load_unioned(Some(dir.path()), None);
        assert_eq!(perms.deny.len(), 1);
        match perms.evaluate(RuntimeKind::Bash, "rm -rf /tmp/scratch") {
            PermDecision::Deny(r) => assert_eq!(r.raw, "Bash(rm -rf *)"),
            _ => panic!("expected deny"),
        }
    }

    #[test]
    fn read_rule_is_ignored_for_runtime_evaluation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        write(&p, r#"{ "permissions": { "deny": ["Read(.env)"] } }"#);
        let perms = load_from_paths(Some(&p), None, None, None);
        assert_eq!(perms.deny.len(), 1, "Read rule still loaded for audit");
        assert_eq!(
            perms.evaluate(RuntimeKind::Bash, "cat .env"),
            PermDecision::Allow
        );
    }

    #[test]
    fn helper_paths_compose_correctly() {
        let dir = Path::new("/tmp/proj");
        assert_eq!(
            claude_settings_path(dir),
            PathBuf::from("/tmp/proj/.claude/settings.json")
        );
        assert_eq!(
            openclaw_config_path(dir),
            PathBuf::from("/tmp/proj/.openclaw/openclaw.json")
        );
    }
}
