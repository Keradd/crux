//! Deterministic, LLM-free renderer that turns a list of [`TurnEvent`]s
//! into a compact multi-line summary.
//!
//! The renderer groups events into a fixed set of [`Bucket`]s based on
//! tool name, then formats each bucket with its own collapsed view.
//! Output stays under the caller-supplied token budget by truncating
//! the longest bucket with `…` once the running estimate exceeds the
//! ceiling.

use std::collections::BTreeMap;

use crux_core::tokens;

use crate::types::{TurnEvent, TurnStatus};

/// Tool family for grouping. Adding a new bucket = one match arm in
/// [`bucket_for`] + one writer in [`render_bucket`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    Read,
    Edit,
    Bash,
    Search,
    Execute,
    Memory,
    Other,
}

impl Bucket {
    fn label(self) -> &'static str {
        match self {
            Bucket::Read => "Files read",
            Bucket::Edit => "Files edited",
            Bucket::Bash => "Commands",
            Bucket::Search => "Searches",
            Bucket::Execute => "Code executions",
            Bucket::Memory => "Memory ops",
            Bucket::Other => "Other tools",
        }
    }
}

/// Map a tool name to its bucket. Names match what Claude Code
/// emits natively + the CRUX MCP tool prefix.
pub fn bucket_for(tool_name: &str) -> Bucket {
    match tool_name {
        "Read" | "NotebookRead" | "mcp__crux__crux_read" | "mcp__crux__crux_get_symbol_source" => {
            Bucket::Read
        }

        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Update" => Bucket::Edit,

        "Bash" | "mcp__crux__crux_bash_filter" => Bucket::Bash,

        "Grep"
        | "Glob"
        | "mcp__crux__crux_search"
        | "mcp__crux__crux_find_symbol"
        | "mcp__crux__crux_query_graph"
        | "mcp__crux__crux_impact" => Bucket::Search,

        "mcp__crux__crux_execute" => Bucket::Execute,

        "mcp__crux__crux_remember" | "mcp__crux__crux_recall" => Bucket::Memory,

        _ => Bucket::Other,
    }
}

/// Render a list of events as a compact summary. `max_tokens` is a soft
/// upper bound; the renderer will append `… (truncated)` and stop
/// formatting buckets once it crosses it. `0` means no truncation.
pub fn render(events: &[TurnEvent], max_tokens: u32) -> String {
    if events.is_empty() {
        return "(no turn events recorded)".into();
    }

    // Bucket → ordered list of events (preserves chronological order).
    let mut buckets: BTreeMap<Bucket, Vec<&TurnEvent>> = BTreeMap::new();
    for ev in events {
        buckets
            .entry(bucket_for(&ev.tool_name))
            .or_default()
            .push(ev);
    }

    // Header line with high-level counts.
    let total = events.len();
    let mut out = String::new();
    out.push_str(&format!("Digest of {} tool call(s)\n", total));

    // Status totals so error-heavy sessions surface immediately.
    let mut errs = 0usize;
    let mut timeouts = 0usize;
    for ev in events {
        match ev.status {
            TurnStatus::Err => errs += 1,
            TurnStatus::Timeout => timeouts += 1,
            _ => {}
        }
    }
    if errs + timeouts > 0 {
        out.push_str(&format!("Errors: {errs} err, {timeouts} timeout\n"));
    }

    // Iterate buckets in fixed enum order.
    let order = [
        Bucket::Read,
        Bucket::Edit,
        Bucket::Bash,
        Bucket::Search,
        Bucket::Execute,
        Bucket::Memory,
        Bucket::Other,
    ];
    let mut truncated = false;
    for b in order {
        let Some(items) = buckets.get(&b) else {
            continue;
        };
        if items.is_empty() {
            continue;
        }
        let chunk = render_bucket(b, items);
        if max_tokens > 0
            && tokens::estimate(&out) as u32 + tokens::estimate(&chunk) as u32 > max_tokens
        {
            truncated = true;
            break;
        }
        out.push('\n');
        out.push_str(&chunk);
    }
    if truncated {
        out.push_str("\n… (digest truncated to fit budget)\n");
    }
    out
}

fn render_bucket(b: Bucket, events: &[&TurnEvent]) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}:\n", b.label()));
    match b {
        Bucket::Read | Bucket::Edit => {
            // Group by target (path), keep counts + status sum.
            let counts = group_by_target(events);
            for (target, count, errs) in counts {
                let suffix = if errs > 0 {
                    format!(" [{} err]", errs)
                } else {
                    String::new()
                };
                out.push_str(&format!("- {} ×{}{}\n", target, count, suffix));
            }
        }
        Bucket::Bash => {
            // Bash collapses to first-word of command + status counts.
            let mut by_first: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            for ev in events {
                let first = ev
                    .target
                    .as_deref()
                    .map(first_word)
                    .unwrap_or("(unknown)")
                    .to_string();
                let entry = by_first.entry(first).or_insert((0, 0));
                entry.0 += 1;
                if matches!(ev.status, TurnStatus::Err | TurnStatus::Timeout) {
                    entry.1 += 1;
                }
            }
            for (cmd, (n, errs)) in by_first {
                let suffix = if errs > 0 {
                    format!(" ({} err)", errs)
                } else {
                    String::new()
                };
                out.push_str(&format!("- {} ×{}{}\n", cmd, n, suffix));
            }
        }
        Bucket::Search => {
            // Searches collapse on raw target (query / symbol name).
            let counts = group_by_target(events);
            for (q, count, _errs) in counts {
                out.push_str(&format!("- {} ×{}\n", q, count));
            }
        }
        Bucket::Execute | Bucket::Memory | Bucket::Other => {
            // Fallback: show the first 5 distinct summaries verbatim,
            // then a "+N more" line. Keeps the bucket compact even for
            // long heterogeneous tails.
            for (shown, ev) in events.iter().enumerate() {
                if shown >= 5 {
                    out.push_str(&format!("- (+{} more)\n", events.len() - shown));
                    break;
                }
                out.push_str(&format!("- {}\n", ev.summary));
            }
        }
    }
    out
}

/// Group `events` by their `target`, returning `(target, count, err_count)`
/// triples sorted by count desc then target asc (for stable output).
fn group_by_target(events: &[&TurnEvent]) -> Vec<(String, usize, usize)> {
    let mut map: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for ev in events {
        let key = ev.target.clone().unwrap_or_else(|| ev.summary.clone());
        let entry = map.entry(key).or_insert((0, 0));
        entry.0 += 1;
        if matches!(ev.status, TurnStatus::Err | TurnStatus::Timeout) {
            entry.1 += 1;
        }
    }
    let mut rows: Vec<(String, usize, usize)> =
        map.into_iter().map(|(k, (c, e))| (k, c, e)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows
}

fn first_word(s: &str) -> &str {
    s.split_whitespace().next().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(tool: &str, target: &str, summary: &str) -> TurnEvent {
        TurnEvent {
            session_id: "s1".into(),
            project_root: "/p".into(),
            agent_id: None,
            tool_name: tool.into(),
            target: Some(target.into()),
            status: TurnStatus::Ok,
            original_tokens: 0,
            compressed_tokens: 0,
            summary: summary.into(),
        }
    }

    #[test]
    fn bucket_routes_correctly() {
        assert_eq!(bucket_for("Read"), Bucket::Read);
        assert_eq!(bucket_for("Edit"), Bucket::Edit);
        assert_eq!(bucket_for("Bash"), Bucket::Bash);
        assert_eq!(bucket_for("Grep"), Bucket::Search);
        assert_eq!(bucket_for("mcp__crux__crux_search"), Bucket::Search);
        assert_eq!(bucket_for("mcp__crux__crux_execute"), Bucket::Execute);
        assert_eq!(bucket_for("Unknown_Tool"), Bucket::Other);
    }

    #[test]
    fn render_groups_reads_by_path() {
        let events = vec![
            ev("Read", "src/a.rs", "read a.rs"),
            ev("Read", "src/a.rs", "read a.rs"),
            ev("Read", "src/b.rs", "read b.rs"),
        ];
        let out = render(&events, 0);
        assert!(out.contains("Files read:"));
        assert!(out.contains("src/a.rs ×2"));
        assert!(out.contains("src/b.rs ×1"));
    }

    #[test]
    fn render_collapses_bash_by_first_word() {
        let events = vec![
            ev("Bash", "cargo test --features x", "cargo test"),
            ev("Bash", "cargo test", "cargo test"),
            ev("Bash", "git status", "git status"),
        ];
        let out = render(&events, 0);
        assert!(out.contains("cargo ×2"));
        assert!(out.contains("git ×1"));
    }

    #[test]
    fn render_surfaces_errors() {
        let mut e = ev("Bash", "cargo test", "cargo test");
        e.status = TurnStatus::Err;
        let out = render(&[e], 0);
        assert!(out.contains("Errors:"));
        assert!(out.contains("1 err"));
    }

    #[test]
    fn render_truncates_to_budget() {
        // Make a noisy events vec; render with a very small budget and
        // verify the truncation marker appears.
        let mut events = Vec::new();
        for i in 0..50 {
            events.push(ev(
                "Read",
                &format!("src/file_{i:03}.rs"),
                &format!("read file_{i:03}.rs"),
            ));
        }
        let out = render(&events, 60);
        assert!(out.contains("… (digest truncated to fit budget)"));
    }

    #[test]
    fn empty_events_renders_placeholder() {
        let out = render(&[], 0);
        assert_eq!(out, "(no turn events recorded)");
    }
}
