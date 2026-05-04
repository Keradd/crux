use std::fs;
use std::path::Path;

use crux_core::error::{CruxError, Result};
use serde_json::{Map, Value};

pub fn read_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let raw = fs::read_to_string(path).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&raw).map_err(|e| {
        CruxError::other(format!(
            "{}: parse error ({}). CRUX requires plain JSON; strip JSONC comments and trailing commas, then retry.",
            path.display(),
            e
        ))
    })
}

pub fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CruxError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let mut tmp = path.to_path_buf();
    let stem = path.file_name().and_then(|s| s.to_str()).unwrap_or("crux");
    tmp.set_file_name(format!(".{stem}.crux.tmp"));
    let s = serde_json::to_string_pretty(value)
        .map_err(|e| CruxError::other(format!("serialize: {e}")))?;
    let mut bytes = s.into_bytes();
    bytes.push(b'\n');
    fs::write(&tmp, &bytes).map_err(|e| CruxError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    fs::rename(&tmp, path).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

pub fn ensure_object(value: &mut Value) -> &mut Map<String, Value> {
    if !matches!(value, Value::Object(_)) {
        *value = Value::Object(Map::new());
    }
    value.as_object_mut().expect("ensured above")
}

pub fn ensure_object_at<'a>(
    map: &'a mut Map<String, Value>,
    key: &str,
) -> &'a mut Map<String, Value> {
    let entry = map
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !matches!(entry, Value::Object(_)) {
        *entry = Value::Object(Map::new());
    }
    entry.as_object_mut().expect("ensured above")
}

pub fn ensure_array_at<'a>(map: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let entry = map
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !matches!(entry, Value::Array(_)) {
        *entry = Value::Array(Vec::new());
    }
    entry.as_array_mut().expect("ensured above")
}

pub fn upsert_mcp_server_crux(
    value: &mut Value,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> bool {
    let mut entry = serde_json::Map::new();
    entry.insert("command".into(), Value::String(command.to_string()));
    entry.insert(
        "args".into(),
        Value::Array(vec![Value::String("mcp".to_string())]),
    );
    if !env.is_empty() {
        let mut env_map = serde_json::Map::new();
        for (k, v) in env {
            env_map.insert(k.clone(), Value::String(v.clone()));
        }
        entry.insert("env".into(), Value::Object(env_map));
    }
    let entry = Value::Object(entry);
    let map = ensure_object(value);
    let servers = ensure_object_at(map, "mcpServers");
    let prior = servers.get("crux").cloned();
    if prior.as_ref() == Some(&entry) {
        return false;
    }
    servers.insert("crux".into(), entry);
    true
}

pub fn upsert_openclaw_mcp_server(
    value: &mut Value,
    name: &str,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> bool {
    let mut entry = serde_json::Map::new();
    entry.insert("command".into(), Value::String(command.to_string()));
    entry.insert(
        "args".into(),
        Value::Array(vec![Value::String("mcp".to_string())]),
    );
    if !env.is_empty() {
        let mut env_map = serde_json::Map::new();
        for (k, v) in env {
            env_map.insert(k.clone(), Value::String(v.clone()));
        }
        entry.insert("env".into(), Value::Object(env_map));
    }
    let entry = Value::Object(entry);
    let map = ensure_object(value);
    let mcp = ensure_object_at(map, "mcp");
    let servers = ensure_object_at(mcp, "servers");
    let prior = servers.get(name).cloned();
    if prior.as_ref() == Some(&entry) {
        return false;
    }
    servers.insert(name.to_string(), entry);
    true
}

pub fn upsert_zed_context_server(
    value: &mut Value,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> bool {
    let mut env_map = serde_json::Map::new();
    for (k, v) in env {
        env_map.insert(k.clone(), Value::String(v.clone()));
    }
    let entry = serde_json::json!({
        "command": {
            "path": command,
            "args": ["mcp"],
            "env": Value::Object(env_map),
        }
    });
    let map = ensure_object(value);
    let servers = ensure_object_at(map, "context_servers");
    let prior = servers.get("crux").cloned();
    if prior.as_ref() == Some(&entry) {
        return false;
    }
    servers.insert("crux".into(), entry);
    true
}

pub fn remove_claude_code_hook(
    value: &mut Value,
    event: &str,
    matcher: &str,
    command: &str,
) -> bool {
    let Some(root_map) = value.as_object_mut() else {
        return false;
    };
    let Some(hooks_root) = root_map.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        return false;
    };
    let Some(event_arr) = hooks_root.get_mut(event).and_then(|v| v.as_array_mut()) else {
        return false;
    };

    let mut changed = false;
    for entry in event_arr.iter_mut() {
        if entry.get("matcher").and_then(|v| v.as_str()) != Some(matcher) {
            continue;
        }
        let Some(inner) = entry.get_mut("hooks").and_then(|v| v.as_array_mut()) else {
            continue;
        };
        let before = inner.len();
        inner.retain(|h| h.get("command").and_then(|v| v.as_str()) != Some(command));
        if inner.len() != before {
            changed = true;
        }
    }

    let before_entries = event_arr.len();
    event_arr.retain(|entry| {
        entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| !arr.is_empty())
    });
    if event_arr.len() != before_entries {
        changed = true;
    }

    if event_arr.is_empty() {
        hooks_root.remove(event);
    }
    if hooks_root.is_empty() {
        root_map.remove("hooks");
    }

    changed
}

pub fn upsert_claude_code_hook(
    value: &mut Value,
    event: &str, // "PreToolUse" | "PostToolUse"
    matcher: &str,
    command: &str,
) -> bool {
    let map = ensure_object(value);
    let hooks_root = ensure_object_at(map, "hooks");
    let event_arr = ensure_array_at(hooks_root, event);

    for entry in event_arr.iter_mut() {
        let matcher_match = entry.get("matcher").and_then(|v| v.as_str()) == Some(matcher);
        if matcher_match {
            let inner = match entry.get_mut("hooks") {
                Some(Value::Array(v)) => v,
                _ => {
                    let map = entry
                        .as_object_mut()
                        .expect("hook entry should be an object");
                    map.insert("hooks".to_string(), Value::Array(Vec::new()));
                    map.get_mut("hooks").unwrap().as_array_mut().unwrap()
                }
            };
            let already = inner
                .iter()
                .any(|h| h.get("command").and_then(|v| v.as_str()) == Some(command));
            if already {
                return false;
            }
            inner.push(serde_json::json!({
                "type": "command",
                "command": command
            }));
            return true;
        }
    }
    event_arr.push(serde_json::json!({
        "matcher": matcher,
        "hooks": [{ "type": "command", "command": command }]
    }));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env() -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::new()
    }

    #[test]
    fn upsert_mcp_into_empty() {
        let mut v = Value::Object(Map::new());
        let changed = upsert_mcp_server_crux(&mut v, "/usr/local/bin/crux", &empty_env());
        assert!(changed);
        assert_eq!(
            v["mcpServers"]["crux"]["command"].as_str().unwrap(),
            "/usr/local/bin/crux"
        );
        assert_eq!(v["mcpServers"]["crux"]["args"][0].as_str().unwrap(), "mcp");
        assert!(
            v["mcpServers"]["crux"].get("env").is_none(),
            "env should be omitted when empty"
        );
    }

    #[test]
    fn upsert_mcp_idempotent() {
        let mut v = Value::Object(Map::new());
        upsert_mcp_server_crux(&mut v, "crux", &empty_env());
        let again = upsert_mcp_server_crux(&mut v, "crux", &empty_env());
        assert!(!again, "second upsert should be a no-op");
    }

    #[test]
    fn upsert_mcp_preserves_other_servers() {
        let mut v = serde_json::json!({
            "mcpServers": {
                "other": { "command": "other-bin", "args": [] }
            }
        });
        upsert_mcp_server_crux(&mut v, "crux", &empty_env());
        assert!(v["mcpServers"].get("other").is_some());
        assert!(v["mcpServers"].get("crux").is_some());
    }

    #[test]
    fn upsert_mcp_includes_env_when_non_empty() {
        let mut v = Value::Object(Map::new());
        let mut env = std::collections::BTreeMap::new();
        env.insert("CRUX_PROJECT".into(), "/path/to/project".into());
        env.insert("RUST_LOG".into(), "debug".into());
        upsert_mcp_server_crux(&mut v, "crux", &env);
        assert_eq!(
            v["mcpServers"]["crux"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/path/to/project"
        );
        assert_eq!(
            v["mcpServers"]["crux"]["env"]["RUST_LOG"].as_str().unwrap(),
            "debug"
        );
    }

    #[test]
    fn upsert_mcp_env_is_idempotent() {
        let mut v = Value::Object(Map::new());
        let mut env = std::collections::BTreeMap::new();
        env.insert("CRUX_PROJECT".into(), "/x".into());
        upsert_mcp_server_crux(&mut v, "crux", &env);
        let again = upsert_mcp_server_crux(&mut v, "crux", &env);
        assert!(!again, "matching env should be idempotent");
    }

    #[test]
    fn upsert_mcp_env_change_triggers_update() {
        let mut v = Value::Object(Map::new());
        let mut env1 = std::collections::BTreeMap::new();
        env1.insert("CRUX_PROJECT".into(), "/old".into());
        upsert_mcp_server_crux(&mut v, "crux", &env1);
        let mut env2 = std::collections::BTreeMap::new();
        env2.insert("CRUX_PROJECT".into(), "/new".into());
        let changed = upsert_mcp_server_crux(&mut v, "crux", &env2);
        assert!(changed, "env value change should re-write");
        assert_eq!(
            v["mcpServers"]["crux"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/new"
        );
    }

    #[test]
    fn upsert_openclaw_into_empty() {
        let mut v = Value::Object(Map::new());
        let changed =
            upsert_openclaw_mcp_server(&mut v, "crux", "/usr/local/bin/crux", &empty_env());
        assert!(changed);
        assert_eq!(
            v["mcp"]["servers"]["crux"]["command"].as_str().unwrap(),
            "/usr/local/bin/crux"
        );
        assert_eq!(
            v["mcp"]["servers"]["crux"]["args"][0].as_str().unwrap(),
            "mcp"
        );
        assert!(
            v["mcp"]["servers"]["crux"].get("env").is_none(),
            "env should be omitted when empty"
        );
    }

    #[test]
    fn upsert_openclaw_idempotent() {
        let mut v = Value::Object(Map::new());
        upsert_openclaw_mcp_server(&mut v, "crux", "crux", &empty_env());
        let again = upsert_openclaw_mcp_server(&mut v, "crux", "crux", &empty_env());
        assert!(!again, "matching entry should be no-op");
    }

    #[test]
    fn upsert_openclaw_preserves_peer_servers() {
        let mut v = serde_json::json!({
            "mcp": {
                "servers": {
                    "context7": { "command": "uvx", "args": ["context7-mcp"] }
                }
            }
        });
        upsert_openclaw_mcp_server(&mut v, "crux", "crux", &empty_env());
        assert!(v["mcp"]["servers"].get("context7").is_some());
        assert!(v["mcp"]["servers"].get("crux").is_some());
    }

    #[test]
    fn upsert_openclaw_env_round_trip() {
        let mut v = Value::Object(Map::new());
        let mut env = std::collections::BTreeMap::new();
        env.insert("CRUX_PROJECT".into(), "/x".into());
        upsert_openclaw_mcp_server(&mut v, "crux", "crux", &env);
        assert_eq!(
            v["mcp"]["servers"]["crux"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/x"
        );
    }

    #[test]
    fn upsert_zed_context_server_into_empty() {
        let mut v = Value::Object(Map::new());
        let changed = upsert_zed_context_server(&mut v, "crux", &empty_env());
        assert!(changed);
        assert_eq!(
            v["context_servers"]["crux"]["command"]["path"]
                .as_str()
                .unwrap(),
            "crux"
        );
        assert!(
            v["context_servers"]["crux"]["command"]["env"].is_object(),
            "Zed always carries an env object (possibly empty)"
        );
    }

    #[test]
    fn upsert_zed_context_server_with_env() {
        let mut v = Value::Object(Map::new());
        let mut env = std::collections::BTreeMap::new();
        env.insert("CRUX_PROJECT".into(), "/p".into());
        upsert_zed_context_server(&mut v, "crux", &env);
        assert_eq!(
            v["context_servers"]["crux"]["command"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/p"
        );
    }

    #[test]
    fn upsert_hook_into_empty() {
        let mut v = Value::Object(Map::new());
        let changed = upsert_claude_code_hook(&mut v, "PreToolUse", "Read", "crux hook pre-tool");
        assert!(changed);
        let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["matcher"].as_str().unwrap(), "Read");
    }

    #[test]
    fn upsert_hook_idempotent() {
        let mut v = Value::Object(Map::new());
        upsert_claude_code_hook(&mut v, "PreToolUse", "Read", "crux hook pre-tool");
        let again = upsert_claude_code_hook(&mut v, "PreToolUse", "Read", "crux hook pre-tool");
        assert!(!again, "duplicate matcher+command should be skipped");
    }

    #[test]
    fn upsert_hook_appends_to_existing_matcher() {
        let mut v = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Read",
                        "hooks": [{ "type": "command", "command": "other" }]
                    }
                ]
            }
        });
        let changed = upsert_claude_code_hook(&mut v, "PreToolUse", "Read", "crux hook pre-tool");
        assert!(changed);
        let inner = v["hooks"]["PreToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 2);
    }

    #[test]
    fn upsert_hook_preserves_other_events() {
        let mut v = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    { "matcher": "X", "hooks": [{ "type": "command", "command": "y" }] }
                ]
            }
        });
        upsert_claude_code_hook(&mut v, "PreToolUse", "Read", "crux hook pre-tool");
        assert!(v["hooks"].get("PostToolUse").is_some());
        assert!(v["hooks"].get("PreToolUse").is_some());
    }

    #[test]
    fn remove_hook_noop_when_absent() {
        let mut v = Value::Object(Map::new());
        let changed = remove_claude_code_hook(&mut v, "PostToolUse", "Edit", "crux hygiene");
        assert!(!changed);

        let mut v2 = serde_json::json!({ "hooks": {} });
        let changed = remove_claude_code_hook(&mut v2, "PostToolUse", "Edit", "crux hygiene");
        assert!(!changed);
    }

    #[test]
    fn remove_hook_removes_only_matching_command() {
        let mut v = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    {
                        "matcher": "Edit|Write|MultiEdit",
                        "hooks": [
                            { "type": "command", "command": "crux hook post-tool" },
                            { "type": "command", "command": "crux hygiene comments --check --changed-from-stdin" }
                        ]
                    }
                ]
            }
        });
        let changed = remove_claude_code_hook(
            &mut v,
            "PostToolUse",
            "Edit|Write|MultiEdit",
            "crux hygiene comments --check --changed-from-stdin",
        );
        assert!(changed);
        let inner = v["hooks"]["PostToolUse"][0]["hooks"].as_array().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0]["command"].as_str().unwrap(), "crux hook post-tool");
    }

    #[test]
    fn remove_hook_drops_empty_matcher_and_event() {
        let mut v = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    {
                        "matcher": "Edit|Write|MultiEdit",
                        "hooks": [
                            { "type": "command", "command": "crux hygiene" }
                        ]
                    }
                ]
            }
        });
        let changed = remove_claude_code_hook(
            &mut v,
            "PostToolUse",
            "Edit|Write|MultiEdit",
            "crux hygiene",
        );
        assert!(changed);
        assert!(
            v.get("hooks").is_none(),
            "empty hooks tree should be removed"
        );
    }

    #[test]
    fn remove_hook_is_idempotent() {
        let mut v = serde_json::json!({
            "hooks": {
                "PostToolUse": [
                    {
                        "matcher": "Edit|Write|MultiEdit",
                        "hooks": [
                            { "type": "command", "command": "crux hygiene" }
                        ]
                    }
                ]
            }
        });
        let first = remove_claude_code_hook(
            &mut v,
            "PostToolUse",
            "Edit|Write|MultiEdit",
            "crux hygiene",
        );
        let second = remove_claude_code_hook(
            &mut v,
            "PostToolUse",
            "Edit|Write|MultiEdit",
            "crux hygiene",
        );
        assert!(first);
        assert!(
            !second,
            "second remove on already-clean tree should be a no-op"
        );
    }

    #[test]
    fn remove_hook_preserves_sibling_event() {
        let mut v = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "Read", "hooks": [{ "type": "command", "command": "crux hook pre-tool" }] }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Edit|Write|MultiEdit",
                        "hooks": [
                            { "type": "command", "command": "crux hygiene" }
                        ]
                    }
                ]
            }
        });
        remove_claude_code_hook(
            &mut v,
            "PostToolUse",
            "Edit|Write|MultiEdit",
            "crux hygiene",
        );
        assert!(v["hooks"].get("PreToolUse").is_some());
        assert!(
            v["hooks"].get("PostToolUse").is_none(),
            "emptied event should drop"
        );
    }

    #[test]
    fn read_or_empty_missing_returns_object() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.json");
        let v = read_or_empty(&p).unwrap();
        assert!(v.is_object());
    }

    #[test]
    fn read_or_empty_blank_returns_object() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blank.json");
        std::fs::write(&p, "").unwrap();
        let v = read_or_empty(&p).unwrap();
        assert!(v.is_object());
    }

    #[test]
    fn read_or_empty_invalid_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.json");
        std::fs::write(&p, "// comment\n{ \"x\": 1 }").unwrap();
        assert!(read_or_empty(&p).is_err());
    }

    #[test]
    fn write_atomic_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("out.json");
        let mut v = Value::Object(Map::new());
        upsert_mcp_server_crux(&mut v, "crux", &empty_env());
        write_atomic(&p, &v).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"crux\""));
        assert!(raw.ends_with('\n'));
        let parsed = read_or_empty(&p).unwrap();
        assert_eq!(parsed, v);
    }
}
