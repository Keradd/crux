use std::fs;
use std::path::Path;

use crux_core::error::{CruxError, Result};
use serde_yaml::{Mapping, Value};

pub fn read_or_empty(path: &Path) -> Result<Value> {
    if !path.exists() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let raw = fs::read_to_string(path).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if raw.trim().is_empty() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    serde_yaml::from_str(&raw).map_err(|e| {
        CruxError::other(format!(
            "{}: YAML parse error ({}). CRUX requires valid YAML 1.1 / 1.2; fix syntax and retry.",
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
    let mut s =
        serde_yaml::to_string(value).map_err(|e| CruxError::other(format!("serialize: {e}")))?;
    if !s.ends_with('\n') {
        s.push('\n');
    }
    fs::write(&tmp, s.as_bytes()).map_err(|e| CruxError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    fs::rename(&tmp, path).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn ensure_mapping(value: &mut Value) -> &mut Mapping {
    if !matches!(value, Value::Mapping(_)) {
        *value = Value::Mapping(Mapping::new());
    }
    value.as_mapping_mut().expect("ensured above")
}

fn ensure_mapping_at<'a>(map: &'a mut Mapping, key: &str) -> &'a mut Mapping {
    let k = Value::String(key.to_string());
    let entry = map
        .entry(k.clone())
        .or_insert(Value::Mapping(Mapping::new()));
    if !matches!(entry, Value::Mapping(_)) {
        *entry = Value::Mapping(Mapping::new());
    }
    entry.as_mapping_mut().expect("ensured above")
}

pub fn upsert_hermes_mcp_server(
    value: &mut Value,
    name: &str,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
) -> bool {
    let mut entry = Mapping::new();
    entry.insert(
        Value::String("command".into()),
        Value::String(command.to_string()),
    );
    entry.insert(
        Value::String("args".into()),
        Value::Sequence(vec![Value::String("mcp".to_string())]),
    );
    if !env.is_empty() {
        let mut env_map = Mapping::new();
        for (k, v) in env {
            env_map.insert(Value::String(k.clone()), Value::String(v.clone()));
        }
        entry.insert(Value::String("env".into()), Value::Mapping(env_map));
    }
    let entry = Value::Mapping(entry);

    let map = ensure_mapping(value);
    let servers = ensure_mapping_at(map, "mcp_servers");
    let name_key = Value::String(name.to_string());
    let prior = servers.get(&name_key).cloned();
    if prior.as_ref() == Some(&entry) {
        return false;
    }
    servers.insert(name_key, entry);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_env() -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::new()
    }

    #[test]
    fn upsert_into_empty_file() {
        let mut v = Value::Mapping(Mapping::new());
        let changed = upsert_hermes_mcp_server(&mut v, "crux", "/usr/local/bin/crux", &empty_env());
        assert!(changed);
        let ms = &v["mcp_servers"]["crux"];
        assert_eq!(ms["command"].as_str().unwrap(), "/usr/local/bin/crux");
        assert_eq!(ms["args"][0].as_str().unwrap(), "mcp");
        assert!(ms.get("env").is_none(), "env should be omitted when empty");
    }

    #[test]
    fn upsert_is_idempotent() {
        let mut v = Value::Mapping(Mapping::new());
        upsert_hermes_mcp_server(&mut v, "crux", "crux", &empty_env());
        let again = upsert_hermes_mcp_server(&mut v, "crux", "crux", &empty_env());
        assert!(!again, "second upsert should be a no-op");
    }

    #[test]
    fn upsert_preserves_other_servers() {
        let yaml = r#"
mcp_servers:
  filesystem:
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
"#;
        let mut v: Value = serde_yaml::from_str(yaml).unwrap();
        upsert_hermes_mcp_server(&mut v, "crux", "/usr/local/bin/crux", &empty_env());
        let servers = v["mcp_servers"].as_mapping().unwrap();
        assert!(servers.contains_key(Value::String("filesystem".into())));
        assert!(servers.contains_key(Value::String("crux".into())));
    }

    #[test]
    fn upsert_includes_env_when_non_empty() {
        let mut v = Value::Mapping(Mapping::new());
        let mut env = std::collections::BTreeMap::new();
        env.insert("CRUX_PROJECT".into(), "/p".into());
        upsert_hermes_mcp_server(&mut v, "crux", "crux", &env);
        assert_eq!(
            v["mcp_servers"]["crux"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/p"
        );
    }

    #[test]
    fn upsert_env_change_triggers_rewrite() {
        let mut v = Value::Mapping(Mapping::new());
        let mut env1 = std::collections::BTreeMap::new();
        env1.insert("CRUX_PROJECT".into(), "/old".into());
        upsert_hermes_mcp_server(&mut v, "crux", "crux", &env1);
        let mut env2 = std::collections::BTreeMap::new();
        env2.insert("CRUX_PROJECT".into(), "/new".into());
        let changed = upsert_hermes_mcp_server(&mut v, "crux", "crux", &env2);
        assert!(changed);
        assert_eq!(
            v["mcp_servers"]["crux"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/new"
        );
    }

    #[test]
    fn read_or_empty_missing_returns_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.yaml");
        let v = read_or_empty(&p).unwrap();
        assert!(v.is_mapping());
    }

    #[test]
    fn read_or_empty_blank_returns_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blank.yaml");
        std::fs::write(&p, "").unwrap();
        let v = read_or_empty(&p).unwrap();
        assert!(v.is_mapping());
    }

    #[test]
    fn write_atomic_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("out.yaml");
        let mut v = Value::Mapping(Mapping::new());
        upsert_hermes_mcp_server(&mut v, "crux", "crux", &empty_env());
        write_atomic(&p, &v).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("crux"));
        assert!(raw.contains("mcp_servers"));
        assert!(raw.ends_with('\n'));
        let parsed = read_or_empty(&p).unwrap();
        assert_eq!(parsed, v);
    }
}
