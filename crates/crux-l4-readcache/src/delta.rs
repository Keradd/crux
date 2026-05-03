//! Line-level diff between an old cached file body and the new on-disk
//! body. Used by `ReadCacheManager::check` when:
//!
//! 1. Layer 4 is in `block` mode
//! 2. The file mtime has changed since the last full read
//! 3. Both old and new bodies are within the per-entry size budget
//!
//! Output is unified-style without the patch header — just `+` / `-` / ` `
//! line markers — because the agent only needs to know what changed, not
//! how to apply a patch. We omit huge unchanged runs and replace them
//! with a `… [N lines unchanged]` marker.

use similar::{ChangeTag, TextDiff};

const MAX_DELTA_LINES: usize = 2000;
const UNCHANGED_RUN_THRESHOLD: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaResult {
    /// Compact `+0/-0` style summary suitable for telemetry.
    pub summary: String,
    /// Human-readable body. Pre-formatted; the hook layer prints it as-is.
    pub body: String,
    /// True when the delta cost more than serving the whole file would,
    /// or one side blew the line budget. Caller falls back to full read.
    pub fallback: bool,
}

impl DeltaResult {
    pub fn no_change() -> Self {
        Self {
            summary: "+0/-0".into(),
            body: "(no change)".into(),
            fallback: false,
        }
    }
}

/// Compute a compact delta between `old` and `new`.
pub fn compute_delta(old: &str, new: &str) -> DeltaResult {
    if old == new {
        return DeltaResult::no_change();
    }

    let old_lines = old.split_inclusive('\n').count();
    let new_lines = new.split_inclusive('\n').count();
    if old_lines > MAX_DELTA_LINES || new_lines > MAX_DELTA_LINES {
        return DeltaResult {
            summary: format!("+?/-? ({} → {} lines)", old_lines, new_lines),
            body: "(file too large for delta — request full re-read)".into(),
            fallback: true,
        };
    }

    let diff = TextDiff::from_lines(old, new);
    let mut adds = 0usize;
    let mut dels = 0usize;
    let mut body = String::new();

    // We scan the diff in order, batching consecutive `Equal` lines so
    // long unchanged runs collapse into a single placeholder.
    let changes: Vec<_> = diff.iter_all_changes().collect();
    let mut i = 0;
    while i < changes.len() {
        let c = &changes[i];
        match c.tag() {
            ChangeTag::Equal => {
                // Look ahead to the end of this Equal run.
                let mut j = i;
                while j < changes.len() && matches!(changes[j].tag(), ChangeTag::Equal) {
                    j += 1;
                }
                let run = j - i;
                if run >= UNCHANGED_RUN_THRESHOLD {
                    // First run is at the top of file: drop entirely.
                    // Mid-file or trailing: keep 2-line context on each
                    // side so the agent can locate the change.
                    let head = if i == 0 { 0 } else { 2.min(run) };
                    let tail = if j == changes.len() { 0 } else { 2.min(run) };
                    for k in 0..head {
                        body.push_str(&format!("  {}", changes[i + k].value()));
                    }
                    let omitted = run - head - tail;
                    if omitted > 0 {
                        body.push_str(&format!("  … [{} lines unchanged]\n", omitted));
                    }
                    for k in (run - tail)..run {
                        body.push_str(&format!("  {}", changes[i + k].value()));
                    }
                } else {
                    for k in 0..run {
                        body.push_str(&format!("  {}", changes[i + k].value()));
                    }
                }
                i = j;
            }
            ChangeTag::Insert => {
                adds += 1;
                body.push_str(&format!("+ {}", c.value()));
                i += 1;
            }
            ChangeTag::Delete => {
                dels += 1;
                body.push_str(&format!("- {}", c.value()));
                i += 1;
            }
        }
    }

    // Ensure the body ends with a single newline.
    if !body.ends_with('\n') {
        body.push('\n');
    }

    DeltaResult {
        summary: format!("+{}/-{}", adds, dels),
        body,
        fallback: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_returns_no_change() {
        let r = compute_delta("hello\n", "hello\n");
        assert_eq!(r.summary, "+0/-0");
        assert!(!r.fallback);
    }

    #[test]
    fn single_line_change() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nXX\nd\ne\n";
        let r = compute_delta(old, new);
        assert_eq!(r.summary, "+1/-1");
        assert!(r.body.contains("- c"));
        assert!(r.body.contains("+ XX"));
    }

    #[test]
    fn long_unchanged_run_collapses() {
        let mut old = String::new();
        let mut new = String::new();
        for i in 0..50 {
            old.push_str(&format!("line {}\n", i));
            new.push_str(&format!("line {}\n", i));
        }
        new.push_str("appended\n");
        let r = compute_delta(&old, &new);
        assert!(r.body.contains("lines unchanged"));
        assert!(r.body.contains("+ appended"));
    }

    #[test]
    fn oversized_falls_back() {
        let old: String = (0..3000).map(|i| format!("l{}\n", i)).collect();
        let new: String = (0..3001).map(|i| format!("l{}\n", i)).collect();
        let r = compute_delta(&old, &new);
        assert!(r.fallback);
        assert!(r.body.contains("too large"));
    }
}
