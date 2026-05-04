//! `crux audit` — health snapshot via Layer 9 Coach.
//!
//! Thin wrapper around [`crux_l9_coach::CoachEngine::snapshot`] that
//! renders the result as a human-readable or JSON report alongside the
//! raw per-layer telemetry table. `crux coach snapshot` is equivalent.
//!
//! `--watch` polls the snapshot on a fixed interval (default 5s) and
//! re-renders. In `--json` mode each iteration emits one compact JSON
//! object terminated by a newline (NDJSON / JSON Lines), so consumers
//! can stream the snapshots into a dashboard or `jq` pipeline without
//! waiting for the process to exit.

use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args as ClapArgs;

use crux_core::{telemetry, Runtime};
use crux_l9_coach::CoachEngine;

use super::resolve_project_root;
use crate::Cli;

/// Default polling interval when `--watch` is given without an explicit
/// `--interval-ms`. 5s is the same cadence `htop` defaults to.
const DEFAULT_WATCH_INTERVAL_MS: u64 = 5000;
/// Floor on the polling interval so a runaway shell loop can't spin the
/// CPU. 200ms is enough headroom for the L9 snapshot pass even on a
/// debug build.
const MIN_WATCH_INTERVAL_MS: u64 = 200;

#[derive(Debug, Default, ClapArgs)]
pub struct Args {
    /// Re-render the audit on a fixed interval until the process is
    /// interrupted. Combine with `--json` for NDJSON output suitable
    /// for streaming into `jq` or a dashboard.
    #[arg(long)]
    pub watch: bool,

    /// Polling interval in milliseconds when `--watch` is set.
    /// Floored at 200ms; defaults to 5000ms.
    #[arg(long = "interval-ms", value_name = "MS")]
    pub interval_ms: Option<u64>,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    if args.watch {
        return run_watch(cli, args);
    }
    run_once(cli)
}

/// Clamp a user-supplied poll interval to the project-wide floor. Pulled
/// out so the same logic is unit-testable without invoking `clap`.
pub(crate) fn clamp_interval_ms(raw: Option<u64>) -> u64 {
    raw.unwrap_or(DEFAULT_WATCH_INTERVAL_MS)
        .max(MIN_WATCH_INTERVAL_MS)
}

fn run_once(cli: &Cli) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;

    let coach = CoachEngine::new(&runtime.conn, &runtime.config, project_opt.as_deref());
    let data = coach.snapshot()?;

    let pr_str = project_opt.as_ref().map(|p| p.display().to_string());
    let stats = telemetry::stats_by_layer(&runtime.conn, pr_str.as_deref())?;

    if cli.json {
        let payload = build_payload(
            pr_str.as_deref(),
            &runtime.config.layers,
            &data,
            &stats,
        );
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("CRUX audit — score {} ({})", data.health_score, data.grade);
    match &project_opt {
        Some(p) => println!("project: {}", p.display()),
        None => println!("project: (none — running outside a CRUX-initialized project)"),
    }
    println!();
    println!("active layers:");
    print_layer(&runtime.config.layers.l1_output, "L1  output compression");
    print_layer(
        &runtime.config.layers.l2_mcp_shrink,
        "L2  MCP description shrinker",
    );
    print_layer(&runtime.config.layers.l3_bash_filter, "L3  bash filter");
    print_layer(&runtime.config.layers.l4_read_cache, "L4  read cache");
    print_layer(&runtime.config.layers.l5_ast_graph, "L5  AST graph");
    print_layer(&runtime.config.layers.l6_hybrid_search, "L6  hybrid search");
    print_layer(&runtime.config.layers.l7_sandbox, "L7  sandbox");
    print_layer(&runtime.config.layers.l8_memory, "L8  memory");
    print_layer(&runtime.config.layers.l9_coach, "L9  coach");
    print_layer(&runtime.config.layers.l10_setup, "L10 setup");
    print_layer(&runtime.config.layers.l11_digest, "L11 digest");
    println!();

    println!("snapshot:");
    println!("  ctx window   : {}", data.snapshot.context_window);
    println!(
        "  CLAUDE.md    : {} tok ({:.2}% of ctx)",
        data.snapshot.claude_md_tokens, data.snapshot.claude_md_pct
    );
    println!(
        "  telemetry    : {} events, {} tok saved ({:.1}%)",
        data.snapshot.telemetry_events,
        data.snapshot.total_savings_tokens,
        data.snapshot.savings_pct
    );
    println!("  observations : {}", data.snapshot.memory_observations);
    println!();

    if !data.patterns_good.is_empty() {
        println!("good:");
        for p in &data.patterns_good {
            println!("  + {} — {}", p.name, p.detail);
        }
    }
    if !data.patterns_bad.is_empty() {
        println!("bad:");
        for p in &data.patterns_bad {
            let sev = p
                .severity
                .map(|s| format!("{:?}", s).to_lowercase())
                .unwrap_or_default();
            println!("  - [{}] {} — {}", sev, p.name, p.detail);
            if let Some(fix) = &p.fix {
                println!("      fix: {}", fix);
            }
            if let Some(sv) = &p.savings {
                println!("      savings: {}", sv);
            }
        }
    }

    if !stats.is_empty() {
        println!();
        println!("telemetry by layer:");
        println!(
            "  {:<8} {:>10} {:>16} {:>14}",
            "layer", "events", "original tok", "saved tok"
        );
        for s in &stats {
            println!(
                "  {:<8} {:>10} {:>16} {:>14}",
                s.layer, s.events, s.original_tokens, s.savings
            );
        }
    }

    Ok(())
}

fn print_layer(active: &bool, label: &str) {
    let marker = if *active { "ON " } else { "off" };
    println!("  [{}] {}", marker, label);
}

fn active_layer_summary(t: &crux_core::config::LayerToggles) -> serde_json::Value {
    serde_json::json!({
        "l1_output": t.l1_output,
        "l2_mcp_shrink": t.l2_mcp_shrink,
        "l3_bash_filter": t.l3_bash_filter,
        "l4_read_cache": t.l4_read_cache,
        "l5_ast_graph": t.l5_ast_graph,
        "l6_hybrid_search": t.l6_hybrid_search,
        "l7_sandbox": t.l7_sandbox,
        "l8_memory": t.l8_memory,
        "l9_coach": t.l9_coach,
        "l10_setup": t.l10_setup,
        "l11_digest": t.l11_digest,
    })
}

/// Pure builder for the `--json` payload. Decoupled from I/O so both the
/// one-shot `run_once` path and the streaming `run_watch` path can share
/// the same shape, and so unit tests can exercise it without spinning a
/// runtime.
pub(crate) fn build_payload(
    project: Option<&str>,
    layers: &crux_core::config::LayerToggles,
    data: &crux_l9_coach::CoachData,
    stats: &[telemetry::LayerStat],
) -> serde_json::Value {
    let perms = build_perms_summary(project);
    serde_json::json!({
        "project": project,
        "coach": data,
        "layers_toggled": active_layer_summary(layers),
        "telemetry": stats.iter().map(|s| serde_json::json!({
            "layer": s.layer,
            "events": s.events,
            "original_tokens": s.original_tokens,
            "compressed_tokens": s.compressed_tokens,
            "savings": s.savings,
        })).collect::<Vec<_>>(),
        "agent_permissions": perms,
        "captured_at_epoch": chrono::Utc::now().timestamp(),
    })
}

/// Scrape Claude Code + OpenClaw configs for a project and return a
/// compact summary of the unioned deny / allow lists. Surfaced in the
/// audit JSON payload so `crux audit --json --watch` consumers can
/// alert on rule drift without re-implementing the loader.
fn build_perms_summary(project: Option<&str>) -> serde_json::Value {
    let project_path = project.map(std::path::PathBuf::from);
    let perms = crux_l7_sandbox::agent_perms::load_for_project(project_path.as_deref());
    serde_json::json!({
        "deny_count": perms.deny.len(),
        "allow_count": perms.allow.len(),
        "rules": perms
            .deny
            .iter()
            .chain(perms.allow.iter())
            .map(|r| serde_json::json!({
                "raw": r.raw,
                "tool": r.tool,
                "pattern": r.pattern,
                "source": r.source.label(),
                "scope": r.scope.label(),
                "kind": if perms.deny.iter().any(|x| x.raw == r.raw && x.scope == r.scope) {
                    "deny"
                } else {
                    "allow"
                },
            }))
            .collect::<Vec<_>>(),
    })
}

/// `crux audit --watch` — re-render on a fixed cadence until the
/// process is interrupted. JSON mode emits NDJSON (one compact object
/// per line) so a downstream `jq` / dashboard pipeline gets a clean
/// stream. Text mode clears the screen between iterations and adds a
/// short footer telling the user how often it polls.
fn run_watch(cli: &Cli, args: &Args) -> Result<()> {
    let interval = Duration::from_millis(clamp_interval_ms(args.interval_ms));
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    loop {
        let tick_start = Instant::now();
        watch_step(cli, &mut handle, interval)?;
        // Sleep the *remainder* of the interval so heavy snapshots don't
        // accumulate drift across iterations.
        let elapsed = tick_start.elapsed();
        if elapsed < interval {
            std::thread::sleep(interval - elapsed);
        }
    }
}

/// One iteration of the watch loop: collect the coach snapshot + render
/// either NDJSON or text. Lives behind a generic `Write` so tests can
/// drive it against a `Vec<u8>` without spawning a subprocess.
#[allow(dead_code)]
pub(crate) fn watch_step<W: Write>(
    cli: &Cli,
    writer: &mut W,
    interval: Duration,
) -> Result<()> {
    let project = resolve_project_root(cli.project.as_deref());
    let project_opt = if project.join(".crux").is_dir() {
        Some(project.clone())
    } else {
        None
    };
    let runtime = Runtime::open(project_opt.clone())?;
    let coach = CoachEngine::new(&runtime.conn, &runtime.config, project_opt.as_deref());
    let data = coach.snapshot()?;

    let pr_str = project_opt.as_ref().map(|p| p.display().to_string());
    let stats = telemetry::stats_by_layer(&runtime.conn, pr_str.as_deref())?;
    let payload = build_payload(pr_str.as_deref(), &runtime.config.layers, &data, &stats);

    if cli.json {
        // Compact NDJSON — one line per snapshot. `jq -c` friendly.
        let line = serde_json::to_string(&payload)?;
        writeln!(writer, "{}", line)?;
        writer.flush()?;
    } else {
        // Clear-screen ANSI escape + cursor home, then re-render the
        // human report. Falls back gracefully on terminals that don't
        // honor it (the escape just shows up as garbage but doesn't
        // crash anything).
        write!(writer, "\x1b[2J\x1b[H")?;
        write_text(writer, project_opt.as_deref(), &runtime.config.layers, &data, &stats)?;
        writeln!(
            writer,
            "(audit refresh every {}ms — Ctrl-C to exit)",
            interval.as_millis()
        )?;
        writer.flush()?;
    }
    Ok(())
}

/// Render the human-readable audit body to a generic writer. Lives
/// alongside the `println!`-based one-shot rendering in `run_once` so
/// the watch path doesn't have to fight stdout locking semantics.
fn write_text<W: Write>(
    writer: &mut W,
    project_opt: Option<&std::path::Path>,
    layers: &crux_core::config::LayerToggles,
    data: &crux_l9_coach::CoachData,
    stats: &[telemetry::LayerStat],
) -> Result<()> {
    writeln!(
        writer,
        "CRUX audit — score {} ({})",
        data.health_score, data.grade
    )?;
    match project_opt {
        Some(p) => writeln!(writer, "project: {}", p.display())?,
        None => writeln!(
            writer,
            "project: (none — running outside a CRUX-initialized project)"
        )?,
    }
    writeln!(writer)?;
    writeln!(writer, "active layers:")?;
    write_layer(writer, layers.l1_output, "L1  output compression")?;
    write_layer(writer, layers.l2_mcp_shrink, "L2  MCP description shrinker")?;
    write_layer(writer, layers.l3_bash_filter, "L3  bash filter")?;
    write_layer(writer, layers.l4_read_cache, "L4  read cache")?;
    write_layer(writer, layers.l5_ast_graph, "L5  AST graph")?;
    write_layer(writer, layers.l6_hybrid_search, "L6  hybrid search")?;
    write_layer(writer, layers.l7_sandbox, "L7  sandbox")?;
    write_layer(writer, layers.l8_memory, "L8  memory")?;
    write_layer(writer, layers.l9_coach, "L9  coach")?;
    write_layer(writer, layers.l10_setup, "L10 setup")?;
    write_layer(writer, layers.l11_digest, "L11 digest")?;
    writeln!(writer)?;

    writeln!(writer, "snapshot:")?;
    writeln!(writer, "  ctx window   : {}", data.snapshot.context_window)?;
    writeln!(
        writer,
        "  CLAUDE.md    : {} tok ({:.2}% of ctx)",
        data.snapshot.claude_md_tokens, data.snapshot.claude_md_pct
    )?;
    writeln!(
        writer,
        "  telemetry    : {} events, {} tok saved ({:.1}%)",
        data.snapshot.telemetry_events,
        data.snapshot.total_savings_tokens,
        data.snapshot.savings_pct
    )?;
    writeln!(writer, "  observations : {}", data.snapshot.memory_observations)?;
    writeln!(writer)?;

    if !data.patterns_good.is_empty() {
        writeln!(writer, "good:")?;
        for p in &data.patterns_good {
            writeln!(writer, "  + {} — {}", p.name, p.detail)?;
        }
    }
    if !data.patterns_bad.is_empty() {
        writeln!(writer, "bad:")?;
        for p in &data.patterns_bad {
            let sev = p
                .severity
                .map(|s| format!("{:?}", s).to_lowercase())
                .unwrap_or_default();
            writeln!(writer, "  - [{}] {} — {}", sev, p.name, p.detail)?;
            if let Some(fix) = &p.fix {
                writeln!(writer, "      fix: {}", fix)?;
            }
            if let Some(sv) = &p.savings {
                writeln!(writer, "      savings: {}", sv)?;
            }
        }
    }

    if !stats.is_empty() {
        writeln!(writer)?;
        writeln!(writer, "telemetry by layer:")?;
        writeln!(
            writer,
            "  {:<8} {:>10} {:>16} {:>14}",
            "layer", "events", "original tok", "saved tok"
        )?;
        for s in stats {
            writeln!(
                writer,
                "  {:<8} {:>10} {:>16} {:>14}",
                s.layer, s.events, s.original_tokens, s.savings
            )?;
        }
    }
    Ok(())
}

fn write_layer<W: Write>(writer: &mut W, active: bool, label: &str) -> Result<()> {
    let marker = if active { "ON " } else { "off" };
    writeln!(writer, "  [{}] {}", marker, label)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crux_core::config::LayerToggles;
    use crux_core::telemetry::LayerStat;
    use crux_l9_coach::{score_to_grade, CoachData, Snapshot};

    fn dummy_coach() -> CoachData {
        CoachData {
            health_score: 80,
            grade: score_to_grade(80),
            patterns_good: vec![],
            patterns_bad: vec![],
            snapshot: Snapshot {
                context_window: 200_000,
                claude_md_tokens: 100,
                claude_md_pct: 0.05,
                total_savings_tokens: 0,
                total_original_tokens: 0,
                savings_pct: 0.0,
                telemetry_events: 0,
                l4_cache_hits: 0,
                memory_observations: 0,
                active_layers: 11,
                unused_layers: 0,
            },
        }
    }

    #[test]
    fn clamp_interval_uses_default_when_unset() {
        assert_eq!(clamp_interval_ms(None), DEFAULT_WATCH_INTERVAL_MS);
    }

    #[test]
    fn clamp_interval_floors_too_aggressive_polling() {
        // Anything below the 200ms floor must round up so a runaway
        // shell loop can't peg the CPU.
        assert_eq!(clamp_interval_ms(Some(0)), MIN_WATCH_INTERVAL_MS);
        assert_eq!(clamp_interval_ms(Some(50)), MIN_WATCH_INTERVAL_MS);
        assert_eq!(clamp_interval_ms(Some(199)), MIN_WATCH_INTERVAL_MS);
        assert_eq!(clamp_interval_ms(Some(200)), 200);
    }

    #[test]
    fn clamp_interval_passes_through_normal_values() {
        assert_eq!(clamp_interval_ms(Some(1000)), 1000);
        assert_eq!(clamp_interval_ms(Some(60_000)), 60_000);
    }

    #[test]
    fn build_payload_shape_is_stable() {
        let layers = LayerToggles::default();
        let data = dummy_coach();
        let stats: Vec<LayerStat> = vec![LayerStat {
            layer: "l3".to_string(),
            events: 5,
            original_tokens: 1000,
            compressed_tokens: 100,
            savings: 900,
        }];
        let p = build_payload(Some("/tmp/proj"), &layers, &data, &stats);
        // Top-level keys we promise to consumers. `agent_permissions`
        // is always present even when there are no rules on disk so
        // dashboards can rely on it without a null-check.
        for k in &[
            "project",
            "coach",
            "layers_toggled",
            "telemetry",
            "agent_permissions",
            "captured_at_epoch",
        ] {
            assert!(p.get(*k).is_some(), "missing top-level key: {}", k);
        }
        // agent_permissions block must always carry the count fields.
        let perms = &p["agent_permissions"];
        assert!(perms["deny_count"].is_u64());
        assert!(perms["allow_count"].is_u64());
        assert!(perms["rules"].is_array());
        // Project echoed verbatim.
        assert_eq!(p["project"].as_str(), Some("/tmp/proj"));
        // Telemetry rows preserved 1:1.
        let tel = p["telemetry"].as_array().unwrap();
        assert_eq!(tel.len(), 1);
        assert_eq!(tel[0]["layer"].as_str(), Some("l3"));
        assert_eq!(tel[0]["savings"].as_i64(), Some(900));
    }

    #[test]
    fn build_payload_handles_no_project() {
        let layers = LayerToggles::default();
        let data = dummy_coach();
        let p = build_payload(None, &layers, &data, &[]);
        assert!(p["project"].is_null());
    }

    #[test]
    fn ndjson_serialization_is_single_line_compact() {
        let layers = LayerToggles::default();
        let data = dummy_coach();
        let p = build_payload(Some("/x"), &layers, &data, &[]);
        let line = serde_json::to_string(&p).unwrap();
        // Compact JSON has no embedded newlines.
        assert!(!line.contains('\n'), "compact NDJSON must be single-line");
        // And must round-trip.
        let back: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(back["project"].as_str(), Some("/x"));
    }

    #[test]
    fn write_text_renders_score_and_layers() {
        let layers = LayerToggles::default();
        let data = dummy_coach();
        let mut buf = Vec::<u8>::new();
        write_text(&mut buf, None, &layers, &data, &[]).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("CRUX audit — score 80"));
        assert!(s.contains("active layers:"));
        assert!(s.contains("L7  sandbox"));
    }
}
