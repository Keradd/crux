use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Args as ClapArgs;

use crux_core::{telemetry, Runtime};
use crux_l9_coach::CoachEngine;

use super::resolve_project_root;
use crate::Cli;

const DEFAULT_WATCH_INTERVAL_MS: u64 = 5000;
const MIN_WATCH_INTERVAL_MS: u64 = 200;

#[derive(Debug, Default, ClapArgs)]
pub struct Args {
    #[arg(long)]
    pub watch: bool,

    #[arg(long = "interval-ms", value_name = "MS")]
    pub interval_ms: Option<u64>,
}

pub fn run(cli: &Cli, args: &Args) -> Result<()> {
    if args.watch {
        return run_watch(cli, args);
    }
    run_once(cli)
}

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
        let payload = build_payload(pr_str.as_deref(), &runtime.config.layers, &data, &stats);
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
    print_layer_with_note(
        &runtime.config.layers.l12_hygiene,
        "L12 hygiene",
        Some("opt-in"),
    );
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

fn print_layer_with_note(active: &bool, label: &str, note: Option<&str>) {
    let marker = if *active { "ON " } else { "off" };
    match note {
        Some(n) if !*active => println!("  [{}] {} ({})", marker, label, n),
        _ => println!("  [{}] {}", marker, label),
    }
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
        "l12_hygiene": t.l12_hygiene,
    })
}

pub(crate) fn layers_info(t: &crux_core::config::LayerToggles) -> serde_json::Value {
    serde_json::json!({
        "l1_output":       layer_info_entry(t.l1_output, None),
        "l2_mcp_shrink":   layer_info_entry(t.l2_mcp_shrink, None),
        "l3_bash_filter":  layer_info_entry(t.l3_bash_filter, None),
        "l4_read_cache":   layer_info_entry(t.l4_read_cache, None),
        "l5_ast_graph":    layer_info_entry(t.l5_ast_graph, None),
        "l6_hybrid_search": layer_info_entry(t.l6_hybrid_search, None),
        "l7_sandbox":      layer_info_entry(t.l7_sandbox, None),
        "l8_memory":       layer_info_entry(t.l8_memory, None),
        "l9_coach":        layer_info_entry(t.l9_coach, None),
        "l10_setup":       layer_info_entry(t.l10_setup, None),
        "l11_digest":      layer_info_entry(t.l11_digest, None),
        "l12_hygiene":     layer_info_entry(
            t.l12_hygiene,
            if t.l12_hygiene { None } else { Some("opt-in hygiene layer") },
        ),
    })
}

fn layer_info_entry(enabled: bool, reason: Option<&str>) -> serde_json::Value {
    match reason {
        Some(r) => serde_json::json!({
            "available": true,
            "enabled": enabled,
            "reason": r,
        }),
        None => serde_json::json!({
            "available": true,
            "enabled": enabled,
        }),
    }
}

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
        "layers_info": layers_info(layers),
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

fn run_watch(cli: &Cli, args: &Args) -> Result<()> {
    let interval = Duration::from_millis(clamp_interval_ms(args.interval_ms));
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();

    loop {
        let tick_start = Instant::now();
        watch_step(cli, &mut handle, interval)?;
        let elapsed = tick_start.elapsed();
        if elapsed < interval {
            std::thread::sleep(interval - elapsed);
        }
    }
}

#[allow(dead_code)]
pub(crate) fn watch_step<W: Write>(cli: &Cli, writer: &mut W, interval: Duration) -> Result<()> {
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
        let line = serde_json::to_string(&payload)?;
        writeln!(writer, "{}", line)?;
        writer.flush()?;
    } else {
        write!(writer, "\x1b[2J\x1b[H")?;
        write_text(
            writer,
            project_opt.as_deref(),
            &runtime.config.layers,
            &data,
            &stats,
        )?;
        writeln!(
            writer,
            "(audit refresh every {}ms — Ctrl-C to exit)",
            interval.as_millis()
        )?;
        writer.flush()?;
    }
    Ok(())
}

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
    write_layer_with_note(writer, layers.l12_hygiene, "L12 hygiene", Some("opt-in"))?;
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
    writeln!(
        writer,
        "  observations : {}",
        data.snapshot.memory_observations
    )?;
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

fn write_layer_with_note<W: Write>(
    writer: &mut W,
    active: bool,
    label: &str,
    note: Option<&str>,
) -> Result<()> {
    let marker = if active { "ON " } else { "off" };
    match note {
        Some(n) if !active => writeln!(writer, "  [{}] {} ({})", marker, label, n)?,
        _ => writeln!(writer, "  [{}] {}", marker, label)?,
    }
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
                unused_layers: 1,
            },
        }
    }

    #[test]
    fn clamp_interval_uses_default_when_unset() {
        assert_eq!(clamp_interval_ms(None), DEFAULT_WATCH_INTERVAL_MS);
    }

    #[test]
    fn clamp_interval_floors_too_aggressive_polling() {
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
        let perms = &p["agent_permissions"];
        assert!(perms["deny_count"].is_u64());
        assert!(perms["allow_count"].is_u64());
        assert!(perms["rules"].is_array());
        assert_eq!(p["project"].as_str(), Some("/tmp/proj"));
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
        assert!(!line.contains('\n'), "compact NDJSON must be single-line");
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
        assert!(s.contains("L12 hygiene"));
        assert!(
            s.contains("L12 hygiene (opt-in)"),
            "default-off L12 must render with opt-in annotation, got:\n{s}"
        );
    }

    #[test]
    fn layers_info_reports_l12_as_available_but_opt_in_when_off() {
        let layers = LayerToggles::default();
        assert!(!layers.l12_hygiene);
        let info = layers_info(&layers);
        let l12 = &info["l12_hygiene"];
        assert_eq!(l12["available"].as_bool(), Some(true));
        assert_eq!(l12["enabled"].as_bool(), Some(false));
        assert_eq!(l12["reason"].as_str(), Some("opt-in hygiene layer"));

        let l11 = &info["l11_digest"];
        assert_eq!(l11["available"].as_bool(), Some(true));
        assert!(l11.get("reason").is_none());
    }

    #[test]
    fn layers_info_omits_reason_when_l12_enabled() {
        let layers = LayerToggles {
            l12_hygiene: true,
            ..LayerToggles::default()
        };
        let info = layers_info(&layers);
        let l12 = &info["l12_hygiene"];
        assert_eq!(l12["enabled"].as_bool(), Some(true));
        assert!(l12.get("reason").is_none());
    }

    #[test]
    fn build_payload_exposes_layers_info() {
        let layers = LayerToggles::default();
        let data = dummy_coach();
        let p = build_payload(None, &layers, &data, &[]);
        let info = p
            .get("layers_info")
            .expect("payload must carry layers_info");
        assert!(info.get("l12_hygiene").is_some());
    }
}
