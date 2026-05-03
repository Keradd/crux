//! 8-stage filter pipeline.
//!
//! Stage order is rigid and matches rtk's reference implementation:
//! 1. Strip ANSI escape sequences
//! 2. Apply `replace` regex substitutions (per-line or multiline)
//! 3. Short-circuit on `match_output` patterns
//! 4. Drop lines matching `strip_lines_matching`
//! 5. Truncate per-line beyond `truncate_lines_at`
//! 6. Keep `head_lines` then optionally `tail_lines`
//! 7. Cap total lines at `max_lines`
//! 8. If output is empty, return `on_empty` fallback
//!
//! Compiled regexes are cached on the `Filter` so `apply()` is cheap to
//! call repeatedly.

use std::sync::OnceLock;

use regex::Regex;

use crate::spec::{FilterSpec, MatchRule, ReplaceRule};

/// Compiled, ready-to-run filter.
#[derive(Debug)]
pub struct Filter {
    pub name: String,
    pub spec: FilterSpec,
    match_command: Option<Regex>,
    replace: Vec<CompiledReplace>,
    match_output: Vec<CompiledMatch>,
    strip_lines: Vec<Regex>,
}

#[derive(Debug)]
struct CompiledReplace {
    re: Regex,
    replacement: String,
    multiline: bool,
}

#[derive(Debug)]
struct CompiledMatch {
    re: Regex,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterOutput {
    pub text: String,
    /// Reason the output is what it is. Used for telemetry detail.
    pub kind: OutputKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputKind {
    /// `match_output` rule fired — output is the rule's message.
    Matched(String),
    /// Empty after all stages — `on_empty` fallback used.
    OnEmpty,
    /// Normal pipeline run produced this text.
    Filtered,
    /// No filter matched; raw output passed through.
    Passthrough,
}

impl Filter {
    pub fn compile(name: String, spec: FilterSpec) -> Result<Self, regex::Error> {
        let match_command = if spec.match_command.is_empty() {
            None
        } else {
            Some(Regex::new(&spec.match_command)?)
        };

        let replace = spec
            .replace
            .iter()
            .map(compile_replace)
            .collect::<Result<Vec<_>, _>>()?;

        let match_output = spec
            .match_output
            .iter()
            .map(compile_match)
            .collect::<Result<Vec<_>, _>>()?;

        let strip_lines = spec
            .strip_lines_matching
            .iter()
            .map(|p| Regex::new(p))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            name,
            spec,
            match_command,
            replace,
            match_output,
            strip_lines,
        })
    }

    /// True if this filter wants to handle `command_line`.
    pub fn matches(&self, command_line: &str) -> bool {
        match &self.match_command {
            Some(re) => re.is_match(command_line),
            None => false,
        }
    }

    /// Run the 8-stage pipeline on raw stdout/stderr text.
    pub fn apply(&self, input: &str) -> FilterOutput {
        // Stage 1
        let mut text = if self.spec.strip_ansi {
            strip_ansi(input)
        } else {
            input.to_string()
        };

        // Stage 2
        for r in &self.replace {
            text = if r.multiline {
                r.re.replace_all(&text, r.replacement.as_str()).into_owned()
            } else {
                let lines: Vec<String> = text
                    .lines()
                    .map(|line| r.re.replace_all(line, r.replacement.as_str()).into_owned())
                    .collect();
                lines.join("\n") + if text.ends_with('\n') { "\n" } else { "" }
            };
        }

        // Stage 3 — short-circuit
        for m in &self.match_output {
            if m.re.is_match(&text) {
                return FilterOutput {
                    text: m.message.clone(),
                    kind: OutputKind::Matched(m.message.clone()),
                };
            }
        }

        // Stage 4
        if !self.strip_lines.is_empty() {
            text = text
                .lines()
                .filter(|line| !self.strip_lines.iter().any(|re| re.is_match(line)))
                .collect::<Vec<_>>()
                .join("\n");
        }

        // Stage 5
        if let Some(width) = self.spec.truncate_lines_at {
            text = truncate_each_line(&text, width);
        }

        // Stage 6
        if self.spec.head_lines.is_some() || self.spec.tail_lines.is_some() {
            text = head_tail(
                &text,
                self.spec.head_lines.unwrap_or(0),
                self.spec.tail_lines.unwrap_or(0),
            );
        }

        // Stage 7
        if let Some(cap) = self.spec.max_lines {
            text = max_lines(&text, cap);
        }

        // Final cleanup: drop leading/trailing blank lines so head/tail
        // splices don't leave the agent staring at an empty first line.
        // Internal blanks survive — they're often semantically meaningful
        // (e.g., separating test summary from failure list).
        text = trim_outer_blanks(&text);

        // Stage 8
        if text.trim().is_empty() {
            if let Some(fb) = &self.spec.on_empty {
                return FilterOutput {
                    text: fb.clone(),
                    kind: OutputKind::OnEmpty,
                };
            }
        }

        FilterOutput {
            text,
            kind: OutputKind::Filtered,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────

fn compile_replace(r: &ReplaceRule) -> Result<CompiledReplace, regex::Error> {
    // Per-line replace runs against each individual line so `^`/`$` already
    // anchor to the line boundary. Multiline replace runs against the whole
    // text — turn on `(?m)` so `^`/`$` still mean line edges, which is what
    // every real-world filter author actually wants.
    let pattern = if r.multiline && !r.pattern.starts_with("(?") {
        format!("(?m){}", r.pattern)
    } else {
        r.pattern.clone()
    };
    Ok(CompiledReplace {
        re: Regex::new(&pattern)?,
        replacement: r.replacement.clone(),
        multiline: r.multiline,
    })
}

fn compile_match(m: &MatchRule) -> Result<CompiledMatch, regex::Error> {
    // `match_output` rules are short-circuit fingerprints. Real-world
    // patterns use line anchors (e.g. `^all good$`) and the input always
    // contains the trailing newline from the captured stdout. Enable
    // multiline by default so anchors land per-line, not per-string.
    let pat = if m.pattern.starts_with("(?") {
        m.pattern.clone()
    } else {
        format!("(?m){}", m.pattern)
    };
    Ok(CompiledMatch {
        re: Regex::new(&pat)?,
        message: m.message.clone(),
    })
}

/// Strip ANSI CSI/OSC escape sequences. Cached compiled regex.
pub fn strip_ansi(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Covers CSI (ESC [ ...), OSC (ESC ] ...), and bare ESC sequences.
        Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[@-Z\\-_]").unwrap()
    });
    re.replace_all(s, "").into_owned()
}

fn truncate_each_line(text: &str, max_chars: usize) -> String {
    text.lines()
        .map(|line| {
            if line.chars().count() > max_chars {
                let prefix: String = line.chars().take(max_chars.saturating_sub(1)).collect();
                format!("{}…", prefix)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn head_tail(text: &str, head: usize, tail: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if head == 0 && tail == 0 {
        return text.to_string();
    }
    if head + tail >= total {
        return text.to_string();
    }
    let mut out = Vec::with_capacity(head + tail + 1);
    out.extend(lines.iter().take(head).copied());
    out.push("…");
    out.extend(lines.iter().skip(total.saturating_sub(tail)).copied());
    out.join("\n")
}

fn max_lines(text: &str, cap: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= cap {
        return text.to_string();
    }
    let kept = &lines[..cap];
    let dropped = lines.len() - cap;
    format!("{}\n… [+{} lines truncated]", kept.join("\n"), dropped)
}

/// Drop fully-empty lines at the very start and end of `text`. Lines
/// containing only whitespace count as blank. Internal blanks are
/// preserved.
fn trim_outer_blanks(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut start = 0;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_min(re: &str) -> FilterSpec {
        FilterSpec {
            match_command: re.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn strip_ansi_removes_csi() {
        let s = "\x1b[31merror\x1b[0m: bad";
        assert_eq!(strip_ansi(s), "error: bad");
    }

    #[test]
    fn pipeline_short_circuits_on_match_output() {
        let mut s = spec_min("^demo");
        s.match_output.push(MatchRule {
            pattern: "all good".into(),
            message: "ok".into(),
        });
        let f = Filter::compile("demo".into(), s).unwrap();
        let out = f.apply("everything is all good here\n");
        assert_eq!(out.text, "ok");
        assert!(matches!(out.kind, OutputKind::Matched(_)));
    }

    #[test]
    fn replace_per_line() {
        let mut s = spec_min("^demo");
        s.replace.push(ReplaceRule {
            pattern: "noise".into(),
            replacement: "".into(),
            multiline: false,
        });
        let f = Filter::compile("demo".into(), s).unwrap();
        let out = f.apply("noise here\nclean line\n");
        assert!(out.text.contains("clean line"));
        assert!(!out.text.contains("noise"));
    }

    #[test]
    fn strip_lines_drops_matching() {
        let mut s = spec_min("^demo");
        s.strip_lines_matching.push("^Using ".into());
        let f = Filter::compile("demo".into(), s).unwrap();
        let out = f.apply("Using cache\nReal line\nUsing rng\n");
        assert_eq!(out.text, "Real line");
    }

    #[test]
    fn truncate_each_line_cuts_long() {
        assert_eq!(truncate_each_line("hello world", 5), "hell…");
    }

    #[test]
    fn head_tail_inserts_ellipsis() {
        let s = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10";
        assert_eq!(head_tail(s, 2, 2), "1\n2\n…\n9\n10");
    }

    #[test]
    fn max_lines_appends_marker() {
        let s = "a\nb\nc\nd\ne";
        let out = max_lines(s, 2);
        assert!(out.starts_with("a\nb"));
        assert!(out.contains("[+3 lines truncated]"));
    }

    #[test]
    fn on_empty_fallback() {
        let mut s = spec_min("^demo");
        s.on_empty = Some("nothing to report".into());
        s.strip_lines_matching.push(".+".into()); // drop everything
        let f = Filter::compile("demo".into(), s).unwrap();
        let out = f.apply("line 1\nline 2\n");
        assert_eq!(out.text, "nothing to report");
        assert!(matches!(out.kind, OutputKind::OnEmpty));
    }

    #[test]
    fn matches_command() {
        let s = spec_min(r"^git\s+status\b");
        let f = Filter::compile("g".into(), s).unwrap();
        assert!(f.matches("git status"));
        assert!(f.matches("git status -s"));
        assert!(!f.matches("git log"));
    }
}
