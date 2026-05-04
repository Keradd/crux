use similar::{ChangeTag, TextDiff};

const MAX_DELTA_LINES: usize = 2000;
const UNCHANGED_RUN_THRESHOLD: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaResult {
    pub summary: String,
    pub body: String,
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

    let changes: Vec<_> = diff.iter_all_changes().collect();
    let mut i = 0;
    while i < changes.len() {
        let c = &changes[i];
        match c.tag() {
            ChangeTag::Equal => {
                let mut j = i;
                while j < changes.len() && matches!(changes[j].tag(), ChangeTag::Equal) {
                    j += 1;
                }
                let run = j - i;
                if run >= UNCHANGED_RUN_THRESHOLD {
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
