//! Per-agent integration logic.
//!
//! All six supported agents share the same overall shape:
//! 1. Locate the right config file for the chosen scope.
//! 2. Read it (or fall back to an empty object if absent).
//! 3. Merge our MCP entry idempotently.
//! 4. Optionally apply agent-specific extras (Claude Code hooks +
//!    slash command).
//! 5. Write back atomically.

use std::path::{Path, PathBuf};

use crux_core::error::{CruxError, Result};

use super::json_merge::{
    read_or_empty, upsert_claude_code_hook, upsert_mcp_server_crux, upsert_openclaw_mcp_server,
    upsert_zed_context_server, write_atomic,
};
use super::skill::CLAUDE_CODE_SKILL;
use super::yaml_merge;
use super::{Action, AgentKind, IntegrateOptions, IntegrateReport, Scope};

/// Name CRUX uses inside agent MCP registries. Kept as a module-level
/// constant so the OpenClaw / Hermes helpers (which accept a name
/// parameter because their schemas take a server-name key path)
/// agree with the hard-coded `crux` used by Claude / Cursor /
/// Windsurf / Cline / Zed upserts.
const CRUX_SERVER_NAME: &str = "crux";

const HOOK_PRE_MATCHER: &str = "Read";
const HOOK_POST_MATCHER: &str = "Edit|Write|MultiEdit";

/// Cheap "is this agent installed?" probe based on canonical config
/// paths. False negatives are possible (a brand-new install with no
/// config dir yet); they only mean `auto_detect` skips the agent —
/// `crux setup <agent>` still works, it just creates the directory.
pub fn is_installed(kind: AgentKind) -> bool {
    let Ok(home) = super::home_dir() else {
        return false;
    };
    match kind {
        AgentKind::ClaudeCode => {
            path_any_exists(&[home.join(".claude"), home.join(".claude.json")])
                || which_in_path("claude")
        }
        AgentKind::ClaudeDesktop => path_any_exists(&claude_desktop_candidate_dirs(&home)),
        AgentKind::Cursor => path_any_exists(&[home.join(".cursor")]) || which_in_path("cursor"),
        AgentKind::Windsurf => {
            path_any_exists(&[home.join(".codeium").join("windsurf")]) || which_in_path("windsurf")
        }
        AgentKind::Cline => path_any_exists(&cline_candidate_dirs(&home)),
        AgentKind::Zed => path_any_exists(&zed_candidate_dirs(&home)) || which_in_path("zed"),
        AgentKind::OpenClaw => {
            path_any_exists(&[home.join(".openclaw")])
                || std::env::var_os("OPENCLAW_CONFIG_PATH").is_some()
                || which_in_path("openclaw")
        }
        AgentKind::Hermes => path_any_exists(&[home.join(".hermes")]) || which_in_path("hermes"),
    }
}

pub fn integrate(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    match opts.agent {
        AgentKind::ClaudeCode => integrate_claude_code(opts),
        AgentKind::ClaudeDesktop => integrate_claude_desktop(opts),
        AgentKind::Cursor => integrate_cursor(opts),
        AgentKind::Windsurf => integrate_windsurf(opts),
        AgentKind::Cline => integrate_cline(opts),
        AgentKind::Zed => integrate_zed(opts),
        AgentKind::OpenClaw => integrate_openclaw(opts),
        AgentKind::Hermes => integrate_hermes(opts),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Claude Code
// ─────────────────────────────────────────────────────────────────────────

fn integrate_claude_code(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);

    let settings = claude_code_settings_path(opts)?;
    apply_mcp_entry(
        &settings,
        &opts.crux_path,
        &opts.env,
        opts.dry_run,
        &mut report,
    )?;

    if opts.install_hooks {
        let pre_cmd = format!("{} hook pre-tool", quoted_if_needed(&opts.crux_path));
        let post_cmd = format!("{} hook post-tool", quoted_if_needed(&opts.crux_path));
        apply_hook(
            &settings,
            "PreToolUse",
            HOOK_PRE_MATCHER,
            &pre_cmd,
            opts.dry_run,
            &mut report,
        )?;
        apply_hook(
            &settings,
            "PostToolUse",
            HOOK_POST_MATCHER,
            &post_cmd,
            opts.dry_run,
            &mut report,
        )?;
    }

    if opts.install_skill {
        let skill_path = claude_code_skill_path(opts)?;
        let exists = skill_path.exists();
        if exists && !opts.force {
            report.actions.push(Action::Skipped {
                path: skill_path,
                reason: "exists (--force to overwrite)",
            });
        } else {
            if !opts.dry_run {
                if let Some(parent) = skill_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| CruxError::Io {
                        path: parent.to_path_buf(),
                        source: e,
                    })?;
                }
                std::fs::write(&skill_path, CLAUDE_CODE_SKILL).map_err(|e| CruxError::Io {
                    path: skill_path.clone(),
                    source: e,
                })?;
            }
            report.actions.push(if exists {
                Action::Updated(skill_path)
            } else {
                Action::Created(skill_path)
            });
        }
    }

    Ok(report)
}

fn claude_code_settings_path(opts: &IntegrateOptions) -> Result<PathBuf> {
    match opts.scope {
        Scope::Global | Scope::Auto => Ok(super::home_dir()?.join(".claude").join("settings.json")),
        Scope::Project => Ok(opts.project_root.join(".claude").join("settings.json")),
    }
}

fn claude_code_skill_path(opts: &IntegrateOptions) -> Result<PathBuf> {
    match opts.scope {
        Scope::Global | Scope::Auto => Ok(super::home_dir()?
            .join(".claude")
            .join("commands")
            .join("crux.md")),
        Scope::Project => Ok(opts
            .project_root
            .join(".claude")
            .join("commands")
            .join("crux.md")),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Claude Desktop
// ─────────────────────────────────────────────────────────────────────────

fn integrate_claude_desktop(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let path = claude_desktop_config_path()?;
    apply_mcp_entry(&path, &opts.crux_path, &opts.env, opts.dry_run, &mut report)?;
    Ok(report)
}

fn claude_desktop_candidate_dirs(home: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/Claude")]
    } else if cfg!(target_os = "windows") {
        vec![dirs::config_dir()
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Claude")]
    } else {
        // Linux is unofficial but Claude Desktop (Electron) writes here.
        vec![home.join(".config").join("Claude")]
    }
}

fn claude_desktop_config_path() -> Result<PathBuf> {
    let home = super::home_dir()?;
    Ok(claude_desktop_candidate_dirs(&home)
        .into_iter()
        .next()
        .expect("candidate list never empty")
        .join("claude_desktop_config.json"))
}

// ─────────────────────────────────────────────────────────────────────────
// Cursor
// ─────────────────────────────────────────────────────────────────────────

fn integrate_cursor(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let path = cursor_config_path(opts)?;
    apply_mcp_entry(&path, &opts.crux_path, &opts.env, opts.dry_run, &mut report)?;
    Ok(report)
}

fn cursor_config_path(opts: &IntegrateOptions) -> Result<PathBuf> {
    match opts.scope {
        Scope::Global | Scope::Auto => Ok(super::home_dir()?.join(".cursor").join("mcp.json")),
        Scope::Project => Ok(opts.project_root.join(".cursor").join("mcp.json")),
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Windsurf (Cascade)
// ─────────────────────────────────────────────────────────────────────────

fn integrate_windsurf(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let path = super::home_dir()?
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    apply_mcp_entry(&path, &opts.crux_path, &opts.env, opts.dry_run, &mut report)?;
    Ok(report)
}

// ─────────────────────────────────────────────────────────────────────────
// Cline (VS Code extension `saoudrizwan.claude-dev`)
// ─────────────────────────────────────────────────────────────────────────

fn integrate_cline(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let candidates = cline_candidate_dirs(&super::home_dir()?);
    let dir = candidates
        .into_iter()
        .find(|p| p.exists())
        .or_else(|| {
            // Fall back to the OS-default path even if it doesn't exist
            // yet — the user may be running setup before opening Cline.
            cline_candidate_dirs(&super::home_dir().ok()?)
                .into_iter()
                .next()
        })
        .ok_or_else(|| CruxError::other("could not resolve Cline settings dir"))?;
    let path = dir.join("settings").join("cline_mcp_settings.json");
    apply_mcp_entry(&path, &opts.crux_path, &opts.env, opts.dry_run, &mut report)?;
    Ok(report)
}

fn cline_candidate_dirs(home: &Path) -> Vec<PathBuf> {
    let leaf = "globalStorage/saoudrizwan.claude-dev";
    if cfg!(target_os = "macos") {
        vec![home
            .join("Library/Application Support/Code/User")
            .join(leaf)]
    } else if cfg!(target_os = "windows") {
        vec![dirs::config_dir()
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Code/User")
            .join(leaf)]
    } else {
        vec![home.join(".config/Code/User").join(leaf)]
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Zed
// ─────────────────────────────────────────────────────────────────────────

fn integrate_zed(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let candidates = zed_candidate_dirs(&super::home_dir()?);
    let dir = candidates
        .into_iter()
        .find(|p| p.exists())
        .or_else(|| {
            zed_candidate_dirs(&super::home_dir().ok()?)
                .into_iter()
                .next()
        })
        .ok_or_else(|| CruxError::other("could not resolve Zed config dir"))?;
    let path = dir.join("settings.json");

    let mut value = read_or_empty(&path)?;
    let changed = upsert_zed_context_server(&mut value, &opts.crux_path, &opts.env);
    finalize_change(&path, &value, changed, opts.dry_run, &mut report)?;
    Ok(report)
}

fn zed_candidate_dirs(home: &Path) -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        vec![home.join(".config/zed"), home.join(".zed")]
    } else if cfg!(target_os = "windows") {
        vec![dirs::config_dir()
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Zed")]
    } else {
        vec![home.join(".config/zed")]
    }
}

// ─────────────────────────────────────────────────────────────────────────
// OpenClaw (docs.openclaw.ai)
// ─────────────────────────────────────────────────────────────────────────

fn integrate_openclaw(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let path = openclaw_config_path(opts)?;
    apply_openclaw_mcp_entry(&path, &opts.crux_path, &opts.env, opts.dry_run, &mut report)?;
    Ok(report)
}

/// Honor `$OPENCLAW_CONFIG_PATH` first (matches the Gateway's own
/// lookup rule), then fall back to `~/.openclaw/openclaw.json` for
/// the global scope or the project-local equivalent for `--scope
/// project`.
fn openclaw_config_path(opts: &IntegrateOptions) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("OPENCLAW_CONFIG_PATH") {
        return Ok(PathBuf::from(p));
    }
    match opts.scope {
        Scope::Global | Scope::Auto => {
            Ok(super::home_dir()?.join(".openclaw").join("openclaw.json"))
        }
        Scope::Project => Ok(opts.project_root.join(".openclaw").join("openclaw.json")),
    }
}

fn apply_openclaw_mcp_entry(
    path: &Path,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
    dry_run: bool,
    report: &mut IntegrateReport,
) -> Result<()> {
    let mut value = read_or_empty(path)?;
    let changed = upsert_openclaw_mcp_server(&mut value, CRUX_SERVER_NAME, command, env);
    finalize_change(path, &value, changed, dry_run, report)
}

// ─────────────────────────────────────────────────────────────────────────
// Hermes Agent (hermes-agent.nousresearch.com)
// ─────────────────────────────────────────────────────────────────────────

fn integrate_hermes(opts: &IntegrateOptions) -> Result<IntegrateReport> {
    let mut report = IntegrateReport::new(opts.agent);
    let path = hermes_config_path(opts)?;

    let mut value = yaml_merge::read_or_empty(&path)?;
    let changed = yaml_merge::upsert_hermes_mcp_server(
        &mut value,
        CRUX_SERVER_NAME,
        &opts.crux_path,
        &opts.env,
    );
    finalize_yaml_change(&path, &value, changed, opts.dry_run, &mut report)?;
    Ok(report)
}

fn hermes_config_path(opts: &IntegrateOptions) -> Result<PathBuf> {
    match opts.scope {
        Scope::Global | Scope::Auto => Ok(super::home_dir()?.join(".hermes").join("config.yaml")),
        Scope::Project => Ok(opts.project_root.join(".hermes").join("config.yaml")),
    }
}

fn finalize_yaml_change(
    path: &Path,
    value: &serde_yaml::Value,
    changed: bool,
    dry_run: bool,
    report: &mut IntegrateReport,
) -> Result<()> {
    if !changed {
        report.actions.push(Action::Skipped {
            path: path.to_path_buf(),
            reason: "already configured",
        });
        return Ok(());
    }
    let exists_before = path.exists();
    if !dry_run {
        yaml_merge::write_atomic(path, value)?;
    }
    report.actions.push(if exists_before {
        Action::Updated(path.to_path_buf())
    } else {
        Action::Created(path.to_path_buf())
    });
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Shared write helpers
// ─────────────────────────────────────────────────────────────────────────

fn apply_mcp_entry(
    path: &Path,
    command: &str,
    env: &std::collections::BTreeMap<String, String>,
    dry_run: bool,
    report: &mut IntegrateReport,
) -> Result<()> {
    let mut value = read_or_empty(path)?;
    let changed = upsert_mcp_server_crux(&mut value, command, env);
    finalize_change(path, &value, changed, dry_run, report)
}

fn apply_hook(
    path: &Path,
    event: &str,
    matcher: &str,
    command: &str,
    dry_run: bool,
    report: &mut IntegrateReport,
) -> Result<()> {
    let mut value = read_or_empty(path)?;
    let changed = upsert_claude_code_hook(&mut value, event, matcher, command);
    finalize_change(path, &value, changed, dry_run, report)
}

fn finalize_change(
    path: &Path,
    value: &serde_json::Value,
    changed: bool,
    dry_run: bool,
    report: &mut IntegrateReport,
) -> Result<()> {
    if !changed {
        report.actions.push(Action::Skipped {
            path: path.to_path_buf(),
            reason: "already configured",
        });
        return Ok(());
    }
    let exists_before = path.exists();
    if !dry_run {
        write_atomic(path, value)?;
    }
    report.actions.push(if exists_before {
        Action::Updated(path.to_path_buf())
    } else {
        Action::Created(path.to_path_buf())
    });
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────
// Tiny utilities
// ─────────────────────────────────────────────────────────────────────────

fn path_any_exists(paths: &[PathBuf]) -> bool {
    paths.iter().any(|p| p.exists())
}

fn which_in_path(bin: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(sep) {
        if dir.is_empty() {
            continue;
        }
        let p = Path::new(dir).join(bin);
        if p.is_file() {
            return true;
        }
        #[cfg(windows)]
        if Path::new(dir).join(format!("{bin}.exe")).is_file() {
            return true;
        }
    }
    false
}

/// Quote a path with embedded spaces for safe interpolation into the
/// `command` string of a Claude Code hook (which is run via the shell).
fn quoted_if_needed(s: &str) -> String {
    if s.contains(' ') && !s.starts_with('"') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{IntegrateOptions, Scope};

    fn make_opts(agent: AgentKind, dir: &Path) -> IntegrateOptions {
        IntegrateOptions {
            agent,
            scope: Scope::Project,
            project_root: dir.to_path_buf(),
            crux_path: "crux".to_string(),
            env: std::collections::BTreeMap::new(),
            install_hooks: agent.supports_hooks(),
            install_skill: agent.supports_slash_command(),
            dry_run: false,
            force: false,
        }
    }

    #[test]
    fn cursor_integrates_into_project_scope() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::Cursor, dir.path());
        let report = integrate(&opts).unwrap();
        let path = dir.path().join(".cursor").join("mcp.json");
        assert!(path.is_file(), "expected {} to exist", path.display());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"crux\""));
        assert!(raw.contains("\"mcp\""));
        assert!(report.changed());
    }

    #[test]
    fn cursor_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::Cursor, dir.path());
        let r1 = integrate(&opts).unwrap();
        let r2 = integrate(&opts).unwrap();
        assert!(r1.changed());
        assert!(!r2.changed(), "second run should be a no-op");
    }

    #[test]
    fn claude_code_writes_settings_hooks_and_skill() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::ClaudeCode, dir.path());
        integrate(&opts).unwrap();
        let settings = dir.path().join(".claude").join("settings.json");
        let skill = dir.path().join(".claude").join("commands").join("crux.md");
        assert!(settings.is_file());
        assert!(skill.is_file());
        let raw = std::fs::read_to_string(&settings).unwrap();
        assert!(raw.contains("\"crux\""));
        assert!(raw.contains("PreToolUse"));
        assert!(raw.contains("PostToolUse"));
        assert!(raw.contains("crux hook pre-tool"));
    }

    #[test]
    fn claude_code_dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = make_opts(AgentKind::ClaudeCode, dir.path());
        opts.dry_run = true;
        let report = integrate(&opts).unwrap();
        assert!(
            report.changed(),
            "report should still describe planned changes"
        );
        assert!(!dir.path().join(".claude").exists());
    }

    #[test]
    fn claude_code_hooks_can_be_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = make_opts(AgentKind::ClaudeCode, dir.path());
        opts.install_hooks = false;
        opts.install_skill = false;
        integrate(&opts).unwrap();
        let raw =
            std::fs::read_to_string(dir.path().join(".claude").join("settings.json")).unwrap();
        assert!(raw.contains("\"crux\""));
        assert!(!raw.contains("PreToolUse"));
    }

    #[test]
    fn claude_code_skill_force_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::ClaudeCode, dir.path());
        integrate(&opts).unwrap();
        let skill = dir.path().join(".claude").join("commands").join("crux.md");
        std::fs::write(&skill, "tampered").unwrap();
        let mut opts2 = opts.clone();
        opts2.force = true;
        integrate(&opts2).unwrap();
        let raw = std::fs::read_to_string(&skill).unwrap();
        assert!(!raw.contains("tampered"));
        assert!(raw.contains("CRUX integration helper"));
    }

    #[test]
    fn parse_agent_kind_aliases() {
        assert_eq!(AgentKind::parse("claude-code"), Some(AgentKind::ClaudeCode));
        assert_eq!(AgentKind::parse("cline"), Some(AgentKind::Cline));
        assert_eq!(AgentKind::parse("Cascade"), Some(AgentKind::Windsurf));
        assert_eq!(AgentKind::parse("desktop"), Some(AgentKind::ClaudeDesktop));
        assert_eq!(AgentKind::parse("openclaw"), Some(AgentKind::OpenClaw));
        assert_eq!(AgentKind::parse("Open-Claw"), Some(AgentKind::OpenClaw));
        assert_eq!(AgentKind::parse("hermes"), Some(AgentKind::Hermes));
        assert_eq!(AgentKind::parse("HERMES-AGENT"), Some(AgentKind::Hermes));
        assert_eq!(AgentKind::parse("nous"), Some(AgentKind::Hermes));
        assert_eq!(AgentKind::parse("nope"), None);
    }

    #[test]
    fn all_kinds_have_unique_slugs_and_labels() {
        let slugs: std::collections::HashSet<_> =
            AgentKind::all().iter().map(|k| k.slug()).collect();
        assert_eq!(slugs.len(), AgentKind::all().len(), "duplicate slug");
        let labels: std::collections::HashSet<_> =
            AgentKind::all().iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), AgentKind::all().len(), "duplicate label");
    }

    #[test]
    fn openclaw_writes_project_scope_config() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::OpenClaw, dir.path());
        let report = integrate(&opts).unwrap();
        let path = dir.path().join(".openclaw").join("openclaw.json");
        assert!(path.is_file(), "expected {} to exist", path.display());
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["mcp"]["servers"]["crux"]["command"]
                .as_str()
                .unwrap(),
            "crux"
        );
        assert_eq!(
            parsed["mcp"]["servers"]["crux"]["args"][0]
                .as_str()
                .unwrap(),
            "mcp"
        );
        assert!(report.changed());
    }

    #[test]
    fn openclaw_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::OpenClaw, dir.path());
        let r1 = integrate(&opts).unwrap();
        let r2 = integrate(&opts).unwrap();
        assert!(r1.changed());
        assert!(!r2.changed(), "second openclaw run should be a no-op");
    }

    #[test]
    fn openclaw_preserves_existing_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".openclaw").join("openclaw.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let seed = serde_json::json!({
            "mcp": {
                "servers": {
                    "context7": {
                        "command": "uvx",
                        "args": ["context7-mcp"]
                    }
                }
            }
        });
        std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

        let opts = make_opts(AgentKind::OpenClaw, dir.path());
        integrate(&opts).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["mcp"]["servers"]["context7"]["command"]
                .as_str()
                .unwrap(),
            "uvx"
        );
        assert!(parsed["mcp"]["servers"].get("crux").is_some());
    }

    #[test]
    fn hermes_writes_project_scope_config() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::Hermes, dir.path());
        let report = integrate(&opts).unwrap();
        let path = dir.path().join(".hermes").join("config.yaml");
        assert!(path.is_file(), "expected {} to exist", path.display());
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("mcp_servers"));
        assert!(raw.contains("crux"));
        assert!(raw.contains("mcp"));
        assert!(report.changed());
    }

    #[test]
    fn hermes_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let opts = make_opts(AgentKind::Hermes, dir.path());
        let r1 = integrate(&opts).unwrap();
        let r2 = integrate(&opts).unwrap();
        assert!(r1.changed());
        assert!(!r2.changed(), "second hermes run should be a no-op");
    }

    #[test]
    fn hermes_preserves_existing_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".hermes").join("config.yaml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let seed = r#"mcp_servers:
  filesystem:
    command: "npx"
    args:
      - "-y"
      - "@modelcontextprotocol/server-filesystem"
      - "/tmp"
"#;
        std::fs::write(&path, seed).unwrap();

        let opts = make_opts(AgentKind::Hermes, dir.path());
        integrate(&opts).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["filesystem"]["command"]
                .as_str()
                .unwrap(),
            "npx"
        );
        assert!(parsed["mcp_servers"].get("crux").is_some());
    }

    #[test]
    fn hermes_env_block_written_when_non_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut opts = make_opts(AgentKind::Hermes, dir.path());
        opts.env.insert("CRUX_PROJECT".into(), "/p".into());
        integrate(&opts).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(".hermes").join("config.yaml")).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
        assert_eq!(
            parsed["mcp_servers"]["crux"]["env"]["CRUX_PROJECT"]
                .as_str()
                .unwrap(),
            "/p"
        );
    }
}
