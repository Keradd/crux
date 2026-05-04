//! OpenClaw context auditor.
//!
//! Ports the threshold rules from
//! `_refs/token-optimizer-alex/openclaw/src/context-audit.ts`'s
//! `generateRecommendations` into a CRUX-native, dependency-light
//! Rust module.
//!
//! The auditor is purely **read-only**: it scans an OpenClaw dir on
//! disk, estimates per-component token cost, and emits a list of
//! actionable recommendations. No side effects, no DB writes.
//!
//! Design choices that diverge from alex's TS reference:
//!
//! 1. **Token estimation** uses CRUX's `tokens::estimate` (chars/4) for
//!    consistency with every other layer's reporting. Numbers will be
//!    a few percent off the TS reference (which uses tiktoken).
//! 2. **MCP server counting** is a basename count of
//!    `openclaw.json` entries via a tiny ad-hoc JSON parse rather than
//!    a full schema-aware reader. We only need *count*, not contents.
//! 3. **Skills** are detected as immediate subdirectories of
//!    `<openclaw_dir>/skills/` containing a `SKILL.md`. Archived skills
//!    are subdirs whose name starts with `_` or that contain
//!    `.archived`.
//!
//! Public surface: [`audit`], [`AuditReport`], [`Component`],
//! [`ContextCategory`].

use std::path::{Path, PathBuf};

use serde::Serialize;

use crux_core::error::{CruxError, Result};
use crux_core::tokens;

// ─────────────────────────────────────────────────────────────────────────
// Tunables — mirror alex's thresholds, converted to const so a future
// crux.toml override is a one-line change.
// ─────────────────────────────────────────────────────────────────────────

/// Hard-coded estimate of the un-editable system prompt overhead.
/// Matches the TS reference (15k tokens). Surfaced so dashboards can
/// strip it from "user-controlled" totals.
pub const CORE_SYSTEM_TOKENS: u32 = 15_000;

/// Threshold above which SOUL.md should be trimmed.
pub const THRESHOLD_SOUL_TOKENS: u32 = 2_000;

/// Threshold above which MEMORY.md should have entries archived.
pub const THRESHOLD_MEMORY_TOKENS: u32 = 1_500;

/// Threshold above which TOOLS.md should defer rarely-used tools.
pub const THRESHOLD_TOOLS_TOKENS: u32 = 5_000;

/// Threshold above which we recommend archiving skills.
pub const THRESHOLD_ACTIVE_SKILLS: u32 = 20;

/// Threshold above which we recommend disabling MCP servers.
pub const THRESHOLD_MCP_SERVERS: u32 = 10;

/// Total overhead at which we start nagging about the 200k context budget.
pub const THRESHOLD_TOTAL_TOKENS: u32 = 30_000;

/// Reference context window used to compute "X% of window" in messages.
/// 200k matches Claude 3.5 Sonnet / 4 / Opus default; agents wanting a
/// different reference can ignore the textual percentage and use raw
/// `total_tokens` from the JSON output.
pub const CONTEXT_WINDOW_TOKENS: u32 = 200_000;

// ─────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextCategory {
    /// The estimated system prompt overhead (always present).
    System,
    /// Personality / SOUL.md.
    Personality,
    /// MEMORY.md.
    Memory,
    /// AGENTS.md.
    Agents,
    /// TOOLS.md.
    Tools,
    /// CLAUDE.md (Claude Code memory).
    ClaudeMemory,
    /// USER.md, IDENTITY.md, HEARTBEAT.md and other named bundle files.
    Identity,
    /// JSON / YAML / TOML config bundle (`openclaw.json` etc).
    Config,
    /// Aggregated skills bundle.
    Skills,
    /// Aggregated MCP server definition bundle.
    McpServers,
    /// Anything else we recognize but don't need to call out.
    Other,
}

impl ContextCategory {
    fn label(self) -> &'static str {
        match self {
            ContextCategory::System => "system",
            ContextCategory::Personality => "personality",
            ContextCategory::Memory => "memory",
            ContextCategory::Agents => "agents",
            ContextCategory::Tools => "tools",
            ContextCategory::ClaudeMemory => "claude_memory",
            ContextCategory::Identity => "identity",
            ContextCategory::Config => "config",
            ContextCategory::Skills => "skills",
            ContextCategory::McpServers => "mcp_servers",
            ContextCategory::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Component {
    pub name: String,
    pub path: String,
    pub tokens: u32,
    pub category: ContextCategory,
    /// `false` for fixed costs like the core system prompt.
    pub is_optimizable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recommendation {
    pub kind: String,
    pub message: String,
    /// Action verb to surface in CLI output ("trim", "archive", ...).
    pub action: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    pub openclaw_dir: PathBuf,
    pub total_tokens: u32,
    pub editable_tokens: u32,
    pub components: Vec<Component>,
    pub active_skills: u32,
    pub archived_skills: u32,
    pub mcp_servers: u32,
    pub recommendations: Vec<Recommendation>,
}

// ─────────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────────

/// Scan the OpenClaw directory at `dir`, build the component breakdown,
/// and return the recommendations the user should act on. Returns a
/// CRUX error only on filesystem failures unrelated to "missing optional
/// file" — those are silently treated as zero-token components.
pub fn audit(dir: &Path) -> Result<AuditReport> {
    if !dir.exists() {
        return Err(CruxError::other(format!(
            "openclaw dir does not exist: {}",
            dir.display()
        )));
    }
    if !dir.is_dir() {
        return Err(CruxError::other(format!(
            "openclaw target is not a directory: {}",
            dir.display()
        )));
    }

    let mut components: Vec<Component> = Vec::new();
    components.push(Component {
        name: "Core system prompt (est.)".into(),
        path: "(built-in)".into(),
        tokens: CORE_SYSTEM_TOKENS,
        category: ContextCategory::System,
        is_optimizable: false,
    });

    // Per-file scans, mirroring alex's `fileScanners` list.
    push_optional(
        &mut components,
        dir,
        "SOUL.md",
        ContextCategory::Personality,
    );
    push_optional(&mut components, dir, "MEMORY.md", ContextCategory::Memory);
    push_optional(&mut components, dir, "AGENTS.md", ContextCategory::Agents);
    push_optional(&mut components, dir, "TOOLS.md", ContextCategory::Tools);
    push_optional(&mut components, dir, "USER.md", ContextCategory::Identity);
    push_optional(
        &mut components,
        dir,
        "IDENTITY.md",
        ContextCategory::Identity,
    );
    push_optional(
        &mut components,
        dir,
        "HEARTBEAT.md",
        ContextCategory::Identity,
    );
    push_optional(
        &mut components,
        dir,
        "CLAUDE.md",
        ContextCategory::ClaudeMemory,
    );
    push_optional(&mut components, dir, "config.json", ContextCategory::Config);
    push_optional(
        &mut components,
        dir,
        "openclaw.json",
        ContextCategory::Config,
    );

    // Aggregate scans — skills and MCP.
    let skills = scan_skills(&dir.join("skills"));
    let mcp = scan_mcp_servers(&dir.join("openclaw.json"));

    let active_skills_count = skills.active as u32;
    let archived_skills_count = skills.archived as u32;
    let mcp_count = mcp.0 as u32;

    if skills.active_tokens > 0 {
        components.push(Component {
            name: format!("Skills ({} active)", skills.active),
            path: "skills/".into(),
            tokens: skills.active_tokens,
            category: ContextCategory::Skills,
            is_optimizable: true,
        });
    }

    if mcp_count > 0 {
        // Each declared MCP server tends to introduce ~200 tokens of
        // tool descriptions in practice. Use a coarse heuristic: 200 *
        // count. Agents wanting precision should re-tally from runtime
        // tool-list output.
        let mcp_tokens = mcp_count.saturating_mul(200);
        components.push(Component {
            name: format!("MCP servers ({} active)", mcp_count),
            path: "openclaw.json".into(),
            tokens: mcp_tokens,
            category: ContextCategory::McpServers,
            is_optimizable: true,
        });
    }

    // Sort components: System on top, then by tokens desc.
    components.sort_by(|a, b| {
        if a.category == ContextCategory::System {
            std::cmp::Ordering::Less
        } else if b.category == ContextCategory::System {
            std::cmp::Ordering::Greater
        } else {
            b.tokens.cmp(&a.tokens)
        }
    });

    let total_tokens: u32 = components.iter().map(|c| c.tokens).sum();
    let editable_tokens: u32 = components
        .iter()
        .filter(|c| c.is_optimizable)
        .map(|c| c.tokens)
        .sum();

    let recommendations =
        generate_recommendations(&components, active_skills_count, mcp_count, total_tokens);

    Ok(AuditReport {
        openclaw_dir: dir.to_path_buf(),
        total_tokens,
        editable_tokens,
        components,
        active_skills: active_skills_count,
        archived_skills: archived_skills_count,
        mcp_servers: mcp_count,
        recommendations,
    })
}

// ─────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────

fn push_optional(out: &mut Vec<Component>, dir: &Path, name: &str, category: ContextCategory) {
    let path = dir.join(name);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return, // missing or unreadable optional file → skip
    };
    let text = std::str::from_utf8(&bytes).unwrap_or("");
    let toks = tokens::estimate(text) as u32;
    out.push(Component {
        name: name.to_string(),
        path: path.to_string_lossy().to_string(),
        tokens: toks,
        category,
        is_optimizable: true,
    });
}

#[derive(Debug, Default)]
struct SkillScan {
    active: usize,
    archived: usize,
    active_tokens: u32,
}

fn scan_skills(skills_dir: &Path) -> SkillScan {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return SkillScan::default();
    };
    let mut scan = SkillScan::default();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let archived = is_archived_skill(&path);
        let toks = std::fs::read_to_string(&skill_md)
            .map(|s| tokens::estimate(&s) as u32)
            .unwrap_or(0);
        if archived {
            scan.archived += 1;
        } else {
            scan.active += 1;
            scan.active_tokens = scan.active_tokens.saturating_add(toks);
        }
    }
    scan
}

fn is_archived_skill(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if name.starts_with('_') {
        return true;
    }
    path.join(".archived").exists()
}

fn scan_mcp_servers(openclaw_json: &Path) -> (usize,) {
    let Ok(text) = std::fs::read_to_string(openclaw_json) else {
        return (0,);
    };
    // We only need the *count* of declared servers under
    // `mcp.servers.<name>` (or top-level `mcp_servers.<name>`). A
    // schema-light count via `serde_json::Value` is enough — we don't
    // care about content.
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return (0,);
    };
    let from_path = |v: Option<&serde_json::Value>| -> usize {
        v.and_then(|x| x.as_object()).map(|o| o.len()).unwrap_or(0)
    };
    let count = from_path(json.pointer("/mcp/servers"))
        .max(from_path(json.pointer("/mcp_servers")))
        .max(from_path(json.pointer("/servers")));
    (count,)
}

fn generate_recommendations(
    components: &[Component],
    active_skills: u32,
    mcp_servers: u32,
    total_tokens: u32,
) -> Vec<Recommendation> {
    let mut recs = Vec::new();

    let by_name = |n: &str| -> Option<&Component> { components.iter().find(|c| c.name == n) };

    if let Some(c) = by_name("SOUL.md") {
        if c.tokens > THRESHOLD_SOUL_TOKENS {
            recs.push(Recommendation {
                kind: "soul_too_large".into(),
                action: "trim",
                message: format!(
                    "SOUL.md is {} tokens (threshold {}). Move verbose instructions into focused skills.",
                    c.tokens, THRESHOLD_SOUL_TOKENS
                ),
            });
        }
    }

    if let Some(c) = by_name("MEMORY.md") {
        if c.tokens > THRESHOLD_MEMORY_TOKENS {
            recs.push(Recommendation {
                kind: "memory_too_large".into(),
                action: "archive",
                message: format!(
                    "MEMORY.md is {} tokens (threshold {}). Archive stale entries (`crux memory archive <id>`).",
                    c.tokens, THRESHOLD_MEMORY_TOKENS
                ),
            });
        }
    }

    if let Some(c) = by_name("TOOLS.md") {
        if c.tokens > THRESHOLD_TOOLS_TOKENS {
            recs.push(Recommendation {
                kind: "tools_too_large".into(),
                action: "defer",
                message: format!(
                    "TOOLS.md is {} tokens (threshold {}). Move rarely-used tools to a deferred-load section.",
                    c.tokens, THRESHOLD_TOOLS_TOKENS
                ),
            });
        }
    }

    if active_skills > THRESHOLD_ACTIVE_SKILLS {
        let aggregate = components
            .iter()
            .find(|c| c.category == ContextCategory::Skills)
            .map(|c| c.tokens)
            .unwrap_or(0);
        recs.push(Recommendation {
            kind: "too_many_skills".into(),
            action: "archive",
            message: format!(
                "{} active skills loaded (~{} tokens). Archive unused skills (rename dir with leading `_`).",
                active_skills, aggregate
            ),
        });
    }

    if mcp_servers > THRESHOLD_MCP_SERVERS {
        recs.push(Recommendation {
            kind: "too_many_mcp_servers".into(),
            action: "disable",
            message: format!(
                "{} MCP servers configured. Disable unused servers in `openclaw.json` to drop their tool descriptions.",
                mcp_servers
            ),
        });
    }

    if total_tokens > THRESHOLD_TOTAL_TOKENS {
        let pct = (total_tokens as f64 / CONTEXT_WINDOW_TOKENS as f64) * 100.0;
        recs.push(Recommendation {
            kind: "total_overhead_high".into(),
            action: "reduce",
            message: format!(
                "Total context overhead is {} tokens (~{:.1}% of {}-token window). Target under {} tokens.",
                total_tokens,
                pct,
                CONTEXT_WINDOW_TOKENS,
                THRESHOLD_TOTAL_TOKENS - 5_000
            ),
        });
    }

    if recs.is_empty() {
        recs.push(Recommendation {
            kind: "healthy".into(),
            action: "noop",
            message: "Context overhead looks healthy. No immediate optimizations needed.".into(),
        });
    }

    recs
}

// Convenience accessor exposed for CLI formatting — keeps the public
// surface tiny without leaking the enum's exact label string.
pub fn category_label(c: ContextCategory) -> &'static str {
    c.label()
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn missing_dir_errors() {
        let err = audit(Path::new("/definitely/not/a/dir/123abc")).unwrap_err();
        assert!(format!("{}", err).contains("does not exist"));
    }

    #[test]
    fn empty_dir_reports_only_system_overhead() {
        let dir = tempfile::tempdir().unwrap();
        let r = audit(dir.path()).unwrap();
        assert_eq!(r.total_tokens, CORE_SYSTEM_TOKENS);
        assert_eq!(r.editable_tokens, 0);
        assert_eq!(r.active_skills, 0);
        assert_eq!(r.mcp_servers, 0);
        assert_eq!(r.recommendations.len(), 1);
        assert_eq!(r.recommendations[0].kind, "healthy");
    }

    #[test]
    fn bloated_soul_md_fires_trim_recommendation() {
        let dir = tempfile::tempdir().unwrap();
        // 10k chars / 4 ≈ 2500 tokens — comfortably above 2000.
        let body = "x".repeat(10_000);
        std::fs::write(dir.path().join("SOUL.md"), &body).unwrap();
        let r = audit(dir.path()).unwrap();
        assert!(r
            .components
            .iter()
            .any(|c| c.name == "SOUL.md" && c.tokens > THRESHOLD_SOUL_TOKENS));
        assert!(r.recommendations.iter().any(|x| x.kind == "soul_too_large"));
    }

    #[test]
    fn bloated_memory_md_fires_archive_recommendation() {
        let dir = tempfile::tempdir().unwrap();
        // 7k chars / 4 ≈ 1750 — above 1500 threshold.
        std::fs::write(dir.path().join("MEMORY.md"), "y".repeat(7_000)).unwrap();
        let r = audit(dir.path()).unwrap();
        assert!(r
            .recommendations
            .iter()
            .any(|x| x.kind == "memory_too_large"));
    }

    #[test]
    fn many_skills_fires_too_many_skills() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(THRESHOLD_ACTIVE_SKILLS + 2) {
            let p = dir
                .path()
                .join("skills")
                .join(format!("s{i}"))
                .join("SKILL.md");
            write(&p, "skill body");
        }
        let r = audit(dir.path()).unwrap();
        assert!(r.active_skills > THRESHOLD_ACTIVE_SKILLS);
        assert!(r
            .recommendations
            .iter()
            .any(|x| x.kind == "too_many_skills"));
    }

    #[test]
    fn underscore_prefixed_skill_dir_counts_as_archived() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("skills/_old/SKILL.md"), "old");
        write(&dir.path().join("skills/active/SKILL.md"), "new");
        let r = audit(dir.path()).unwrap();
        assert_eq!(r.active_skills, 1);
        assert_eq!(r.archived_skills, 1);
    }

    #[test]
    fn mcp_count_from_openclaw_json_at_either_path() {
        let dir = tempfile::tempdir().unwrap();
        // Use `mcp.servers` shape (the canonical OpenClaw layout).
        let body = serde_json::json!({
            "mcp": {
                "servers": {
                    "crux": {"command": "crux"},
                    "git": {"command": "git-mcp"},
                }
            }
        });
        std::fs::write(
            dir.path().join("openclaw.json"),
            serde_json::to_string(&body).unwrap(),
        )
        .unwrap();
        let r = audit(dir.path()).unwrap();
        assert_eq!(r.mcp_servers, 2);
    }

    #[test]
    fn many_mcp_servers_triggers_recommendation() {
        let dir = tempfile::tempdir().unwrap();
        let mut servers = serde_json::Map::new();
        for i in 0..(THRESHOLD_MCP_SERVERS + 2) {
            servers.insert(format!("srv{i}"), serde_json::json!({"command": "x"}));
        }
        let body = serde_json::json!({"mcp": {"servers": servers}});
        std::fs::write(
            dir.path().join("openclaw.json"),
            serde_json::to_string(&body).unwrap(),
        )
        .unwrap();
        let r = audit(dir.path()).unwrap();
        assert!(r
            .recommendations
            .iter()
            .any(|x| x.kind == "too_many_mcp_servers"));
    }

    #[test]
    fn components_sorted_system_then_tokens_desc() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("MEMORY.md"), "m".repeat(8_000)).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "a".repeat(2_000)).unwrap();
        let r = audit(dir.path()).unwrap();
        assert_eq!(r.components[0].category, ContextCategory::System);
        // After system, the larger MEMORY.md must precede AGENTS.md.
        let memory_idx = r
            .components
            .iter()
            .position(|c| c.name == "MEMORY.md")
            .unwrap();
        let agents_idx = r
            .components
            .iter()
            .position(|c| c.name == "AGENTS.md")
            .unwrap();
        assert!(memory_idx < agents_idx);
    }
}
