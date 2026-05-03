//! TOML configuration for CRUX.
//!
//! Two scopes:
//! - **Global**: `~/.crux/config.toml` (or `$CRUX_HOME/config.toml`)
//! - **Project**: `<project>/.crux/config.toml`
//!
//! Project values override global. Missing fields fall back to baked-in
//! defaults from [`Config::default`].
//!
//! Configuration design choices and the matrix of layer modes are described
//! in `docs/CRUX-DESIGN.md` Section 6.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CruxError, Result};
use crate::paths;

// ─────────────────────────────────────────────────────────────────────────
// Top-level Config
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub general: GeneralConfig,
    pub layers: LayerToggles,
    pub modes: LayerModes,
    pub layer: LayerConfigs,
    pub telemetry: TelemetryConfig,
    pub mcp: McpConfig,
    /// Patterns for `.contextignore`-style ignore (project-only typically).
    #[serde(default)]
    pub ignore: IgnoreConfig,
}

// ─────────────────────────────────────────────────────────────────────────
// General
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GeneralConfig {
    /// Override DB path. `None` resolves to `<crux_home>/db/crux.sqlite`.
    pub db_path: Option<PathBuf>,
    /// Override log file path. `None` resolves to `<crux_home>/logs/crux.log`.
    pub log_path: Option<PathBuf>,
    /// Log level: error/warn/info/debug/trace.
    pub log_level: String,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            db_path: None,
            log_path: None,
            log_level: "info".to_string(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Layer toggles (which layers are active)
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LayerToggles {
    pub l1_output: bool,
    pub l2_mcp_shrink: bool,
    pub l3_bash_filter: bool,
    pub l4_read_cache: bool,
    pub l5_ast_graph: bool,
    pub l6_hybrid_search: bool,
    /// Sandbox on-by-default as of 2026-05-03. The default isolation
    /// level is `IsolationLevel::Portable` (subprocess + timeout +
    /// `network_allowed=false` + project-root-only fs) which is safe
    /// without any system-level dependency. Users who want stronger
    /// isolation compile `crux-l7-sandbox` with the `seccomp` feature
    /// and pass `"isolation":"hard"` per-call.
    pub l7_sandbox: bool,
    pub l8_memory: bool,
    pub l9_coach: bool,
    pub l10_setup: bool,
    /// Layer 11 — conversation digest. Records every tool call as a
    /// `turn_event` and rolls them up into compact `turn_digests` so
    /// long sessions don't drag historical noise into context.
    pub l11_digest: bool,
}

impl Default for LayerToggles {
    fn default() -> Self {
        Self {
            l1_output: true,
            l2_mcp_shrink: true,
            l3_bash_filter: true,
            l4_read_cache: true,
            l5_ast_graph: true,
            l6_hybrid_search: true,
            l7_sandbox: true,
            l8_memory: true,
            l9_coach: true,
            l10_setup: true,
            l11_digest: true,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Layer modes (warn / block / shadow)
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum LayerMode {
    /// Layer runs but only logs telemetry; never blocks the agent.
    #[default]
    Warn,
    /// Layer can short-circuit the agent (e.g., return digest instead of file).
    Block,
    /// Layer is fully disabled but still records telemetry as if it had run.
    Shadow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct LayerModes {
    pub l3_bash_filter: LayerMode,
    pub l4_read_cache: LayerMode,
}

// ─────────────────────────────────────────────────────────────────────────
// Per-layer detailed configs
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct LayerConfigs {
    pub l1: L1Config,
    pub l4: L4Config,
    pub l5: L5Config,
    pub l6: L6Config,
    pub l7: L7Config,
    pub l8: L8Config,
    pub l9: L9Config,
    pub l11: L11Config,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L1Config {
    pub profile: String,
    pub intensity: String,
    pub auto_clarity: bool,
}

impl Default for L1Config {
    fn default() -> Self {
        Self {
            profile: "coding".to_string(),
            intensity: "full".to_string(),
            auto_clarity: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L4Config {
    pub delta_max_bytes: u64,
    pub delta_max_lines: u64,
    pub cache_max_entries: u64,
    pub contextignore_max_patterns: u64,
    /// Threshold in lines above which a full-file read auto-falls back
    /// to an L5 outline (symbol list + line ranges) instead of the
    /// whole body. `0` disables the behavior. Only fires when the
    /// caller asks for the full file (no offset/limit/symbol) AND the
    /// L5 graph has indexed at least one symbol for the file. Agents
    /// can opt out per-call via `force_full = true`.
    pub outline_above_lines: u64,
}

impl Default for L4Config {
    fn default() -> Self {
        Self {
            delta_max_bytes: 50 * 1024,
            delta_max_lines: 2000,
            cache_max_entries: 500,
            contextignore_max_patterns: 200,
            outline_above_lines: 1000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L5Config {
    pub languages: Vec<String>,
    pub bfs_engine: String,
    pub max_impact_nodes: u64,
    pub max_impact_depth: u64,
    pub daemon_enabled: bool,
}

impl Default for L5Config {
    fn default() -> Self {
        Self {
            languages: vec![
                "rust".into(),
                "python".into(),
                "javascript".into(),
                "typescript".into(),
                "go".into(),
                "java".into(),
            ],
            bfs_engine: "sql".into(),
            max_impact_nodes: 500,
            max_impact_depth: 2,
            daemon_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L6Config {
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dim: u32,
    pub vector_store: String,
    pub similarity_threshold: f64,
    pub top_k: u32,
    pub rrf_k: u32,
}

impl Default for L6Config {
    fn default() -> Self {
        // Defaults match what ships in the zero-deps build: a hash-based
        // baseline embedder so the dense path always works without a
        // network or extra runtime. Switch to `fastembed` after building
        // with `--features crux-l6-search/fastembed`.
        Self {
            embedding_provider: "hash".into(),
            embedding_model: "hash-256".into(),
            embedding_dim: 256,
            vector_store: "blob".into(),
            similarity_threshold: 0.7,
            top_k: 10,
            rrf_k: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L7Config {
    pub allowed_runtimes: Vec<String>,
    pub default_runtime: String,
    pub timeout_secs: u64,
    pub memory_limit_mb: u64,
    pub network_allowed: bool,
}

impl Default for L7Config {
    fn default() -> Self {
        Self {
            allowed_runtimes: vec![
                "lua".into(),
                "javascript".into(),
                "python".into(),
                "bash".into(),
            ],
            default_runtime: "lua".into(),
            timeout_secs: 30,
            memory_limit_mb: 256,
            network_allowed: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L8Config {
    pub auto_extract: bool,
    pub decay_check_interval_hours: u64,
    pub contradiction_check: bool,
    /// When true, `crux_read` / `crux_get_symbol_source` append a short
    /// footer listing past observations attached to the file/symbol they
    /// return. Zero new tool calls; pure context injection.
    pub auto_surface: bool,
    /// Cap on observations surfaced per call. Keep small (≤ 5) so the
    /// footer never dominates the payload.
    pub auto_surface_limit: usize,
}

impl Default for L8Config {
    fn default() -> Self {
        Self {
            auto_extract: true,
            decay_check_interval_hours: 24,
            contradiction_check: true,
            auto_surface: true,
            auto_surface_limit: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L9Config {
    pub score_target: u32,
    pub nudge_threshold_drop: u32,
    pub nudge_cooldown_minutes: u32,
    pub nudge_max_per_session: u32,
}

impl Default for L9Config {
    fn default() -> Self {
        Self {
            score_target: 80,
            nudge_threshold_drop: 15,
            nudge_cooldown_minutes: 5,
            nudge_max_per_session: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct L11Config {
    /// Auto-compact a session after this many pending events. `0`
    /// disables auto-compaction; manual `crux compact` still works.
    pub auto_compact_every_n: u32,
    /// Soft cap on summary tokens written into a digest row.
    pub max_summary_tokens: u32,
    /// When true, every compaction also writes the digest summary
    /// into the L8 `observations` table as a `convention` row.
    pub mirror_to_l8: bool,
    /// Importance assigned to mirrored observations (1..=10).
    pub mirror_importance: u8,
    /// Cap on events read in a single `summarize` call.
    pub render_max_events: u32,
}

impl Default for L11Config {
    fn default() -> Self {
        Self {
            auto_compact_every_n: 50,
            max_summary_tokens: 600,
            mirror_to_l8: true,
            mirror_importance: 4,
            render_max_events: 200,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Telemetry / MCP / ignore
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub retention_days: u32,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct McpConfig {
    /// "stdio" or "host:port". Defaults to stdio for safety.
    pub listen_addr: String,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            listen_addr: "stdio".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IgnoreConfig {
    pub patterns: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────
// Loading + saving
// ─────────────────────────────────────────────────────────────────────────

/// Resolve effective config: defaults → global TOML → project TOML.
///
/// Either or both files may be missing; defaults are always available.
/// Returns the merged config plus the paths that were consulted.
pub struct LoadedConfig {
    pub config: Config,
    pub global_path: PathBuf,
    pub project_path: Option<PathBuf>,
}

pub fn load(project_root: Option<&Path>) -> Result<LoadedConfig> {
    let global_path = paths::global_config_path()?;
    let mut cfg = Config::default();

    if global_path.is_file() {
        cfg = read_toml(&global_path)?.merge_into(cfg);
    }

    let project_path = project_root.map(|p| p.join(".crux").join("config.toml"));
    if let Some(ref pp) = project_path {
        if pp.is_file() {
            cfg = read_toml(pp)?.merge_into(cfg);
        }
    }

    Ok(LoadedConfig {
        config: cfg,
        global_path,
        project_path,
    })
}

/// Persist a config to a path, creating parent directories.
pub fn save(cfg: &Config, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CruxError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let s = toml::to_string_pretty(cfg)?;
    fs::write(path, s).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn read_toml(path: &Path) -> Result<PartialConfig> {
    let raw = fs::read_to_string(path).map_err(|e| CruxError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let parsed: PartialConfig = toml::from_str(&raw).map_err(|e| CruxError::ConfigInvalid {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    Ok(parsed)
}

// ─────────────────────────────────────────────────────────────────────────
// Partial / merge
//
// We use a fully-optional mirror of `Config` so that a project file can
// override only the fields it cares about without nuking unrelated sections.
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct PartialConfig {
    general: Option<GeneralConfig>,
    layers: Option<LayerToggles>,
    modes: Option<LayerModes>,
    layer: Option<LayerConfigs>,
    telemetry: Option<TelemetryConfig>,
    mcp: Option<McpConfig>,
    ignore: Option<IgnoreConfig>,
}

impl PartialConfig {
    fn merge_into(self, mut base: Config) -> Config {
        if let Some(g) = self.general {
            base.general = g;
        }
        if let Some(l) = self.layers {
            base.layers = l;
        }
        if let Some(m) = self.modes {
            base.modes = m;
        }
        if let Some(l) = self.layer {
            base.layer = l;
        }
        if let Some(t) = self.telemetry {
            base.telemetry = t;
        }
        if let Some(m) = self.mcp {
            base.mcp = m;
        }
        if let Some(i) = self.ignore {
            base.ignore = i;
        }
        base
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let cfg = Config::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn project_overrides_global() {
        // Project config explicitly disables L7 — this must win over the
        // default-on state.
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path();
        let proj_cfg_path = proj.join(".crux").join("config.toml");
        std::fs::create_dir_all(proj_cfg_path.parent().unwrap()).unwrap();
        std::fs::write(
            &proj_cfg_path,
            r#"[layers]
l7_sandbox = false
"#,
        )
        .unwrap();

        let loaded = load(Some(proj)).unwrap();
        assert!(
            !loaded.config.layers.l7_sandbox,
            "project override did not take effect"
        );
        assert!(loaded.config.layers.l4_read_cache); // default preserved
    }

    #[test]
    fn l7_sandbox_is_enabled_by_default() {
        let t = LayerToggles::default();
        assert!(
            t.l7_sandbox,
            "L7 sandbox must default to enabled (portable isolation, no system deps) \
             so crux_execute is usable out-of-the-box."
        );
    }

    #[test]
    fn save_then_load_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let cfg = Config::default();
        save(&cfg, &path).unwrap();
        let parsed = read_toml(&path).unwrap();
        let merged = parsed.merge_into(Config::default());
        assert_eq!(cfg, merged);
    }
}
