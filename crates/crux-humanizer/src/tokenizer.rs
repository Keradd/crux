use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Verbatim(String),
    Prose(String),
}

impl Segment {
    pub fn as_str(&self) -> &str {
        match self {
            Segment::Verbatim(s) | Segment::Prose(s) => s.as_str(),
        }
    }
}

pub fn tokenize(input: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut prose_buf = String::new();
    let mut fence_buf = String::new();
    let mut fence_marker: Option<String> = None;

    for line in input.split_inclusive('\n') {
        let trimmed = line.trim_start();
        match &fence_marker {
            None => {
                if let Some(marker) = leading_fence(trimmed) {
                    if !prose_buf.is_empty() {
                        out.extend(split_inline(&std::mem::take(&mut prose_buf)));
                    }
                    fence_marker = Some(marker);
                    fence_buf.push_str(line);
                } else {
                    prose_buf.push_str(line);
                }
            }
            Some(marker) => {
                fence_buf.push_str(line);
                if let Some(closer) = leading_fence(trimmed) {
                    if closer.starts_with(marker.as_str()) || marker.starts_with(closer.as_str()) {
                        let body = std::mem::take(&mut fence_buf);
                        out.push(Segment::Verbatim(body));
                        fence_marker = None;
                    }
                }
            }
        }
    }

    if !fence_buf.is_empty() {
        out.push(Segment::Verbatim(std::mem::take(&mut fence_buf)));
    }
    if !prose_buf.is_empty() {
        out.extend(split_inline(&prose_buf));
    }
    out
}

pub fn join(segments: &[Segment]) -> String {
    let total: usize = segments.iter().map(|s| s.as_str().len()).sum();
    let mut buf = String::with_capacity(total);
    for s in segments {
        buf.push_str(s.as_str());
    }
    buf
}

fn leading_fence(line: &str) -> Option<String> {
    let bytes = line.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let ch = bytes[0];
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let mut count = 0;
    while count < bytes.len() && bytes[count] == ch {
        count += 1;
    }
    if count < 3 {
        return None;
    }
    Some(std::str::from_utf8(&bytes[..count]).ok()?.to_string())
}

fn split_inline(s: &str) -> Vec<Segment> {
    let re = inline_regex();
    let mut out = Vec::new();
    let mut last = 0usize;

    for m in re.find_iter(s) {
        let raw = &s[m.start()..m.end()];
        let (kept, trailing) = trim_trailing_punctuation(raw);
        let end = m.start() + kept.len();

        if m.start() > last {
            out.push(Segment::Prose(s[last..m.start()].to_string()));
        }
        out.push(Segment::Verbatim(kept.to_string()));
        if !trailing.is_empty() {
            out.push(Segment::Prose(trailing.to_string()));
        }
        last = end + trailing.len();
    }

    if last < s.len() {
        out.push(Segment::Prose(s[last..].to_string()));
    }

    coalesce_prose(out)
}

fn coalesce_prose(input: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(input.len());
    for seg in input {
        if let (Some(Segment::Prose(prev)), Segment::Prose(next)) = (out.last_mut(), &seg) {
            prev.push_str(next);
        } else {
            out.push(seg);
        }
    }
    out
}

fn trim_trailing_punctuation(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
        let c = bytes[end - 1];
        if matches!(c, b'.' | b',' | b';' | b':' | b'!' | b'?') {
            end -= 1;
        } else {
            break;
        }
    }
    (&s[..end], &s[end..])
}

fn inline_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?x)
            (?:`+[^`\n]+`+)                                     # inline code (any backtick run)
            | https?://[^\s<>`]+                                # http/https URL
            | www\.[A-Za-z0-9_./?=\#&%+\-]+                     # bare www.example.com
            | (?:[A-Za-z_][A-Za-z0-9_]*::)+[A-Za-z_][A-Za-z0-9_]*  # foo::bar::baz
            | [A-Za-z_][A-Za-z0-9_]*\([^)\n]{0,200}\)           # foo(args)
            | @[A-Za-z0-9_][A-Za-z0-9_./\-]*                    # @scope/pkg / @handle
            | 0x[0-9A-Fa-f]+                                    # hex literal
            | (?:[0-9]{1,3}\.){3}[0-9]{1,3}                     # IPv4 dotted-quad
            | (?:[0-9A-Fa-f]{1,4}:){2,7}[0-9A-Fa-f]{1,4}        # IPv6 (loose)
            | \.{1,2}/[A-Za-z0-9_./~+\-]+                       # ./path or ../path
            | /[A-Za-z0-9_~+\-]+/[A-Za-z0-9_./~+\-]+            # /multi/segment/path
            | [A-Za-z]:[\\/][A-Za-z0-9_./~+\-\\]+               # C:/foo or C:\foo
            | [A-Z][A-Z0-9]+(?:_[A-Z0-9]+)+                     # SCREAMING_CONST (>=1 underscore)
            ",
        )
        .expect("inline regex compiles")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(segments: &[Segment]) -> Vec<&'static str> {
        segments
            .iter()
            .map(|s| match s {
                Segment::Verbatim(_) => "V",
                Segment::Prose(_) => "P",
            })
            .collect()
    }

    #[test]
    fn join_is_identity() {
        let inputs = [
            "",
            "plain prose with no specials",
            "Run `cargo test` then check https://example.com.",
            "```rust\nfn foo() {}\n```\nText after.",
            "Mixed ./path/to/file and crux::engine::Humanizer.",
        ];
        for input in inputs {
            let segs = tokenize(input);
            assert_eq!(join(&segs), input, "round-trip failed for: {input:?}");
        }
    }

    #[test]
    fn fenced_code_is_verbatim() {
        let input = "before\n```rust\nfn x() {}\n```\nafter";
        let segs = tokenize(input);
        assert_eq!(types(&segs), vec!["P", "V", "P"]);
        assert!(segs[1].as_str().contains("fn x() {}"));
    }

    #[test]
    fn unterminated_fence_kept_verbatim() {
        let input = "open\n```\nstill in fence\nno close";
        let segs = tokenize(input);
        let last = segs.last().expect("has segments");
        assert!(matches!(last, Segment::Verbatim(_)));
        assert!(last.as_str().contains("still in fence"));
    }

    #[test]
    fn inline_code_is_verbatim() {
        let segs = tokenize("Use `foo_bar` to call it.");
        assert_eq!(types(&segs), vec!["P", "V", "P"]);
        assert_eq!(segs[1].as_str(), "`foo_bar`");
    }

    #[test]
    fn url_is_verbatim_without_trailing_dot() {
        let segs = tokenize("See https://example.com/path.");
        assert_eq!(types(&segs), vec!["P", "V", "P"]);
        assert_eq!(segs[1].as_str(), "https://example.com/path");
        assert_eq!(segs[2].as_str(), ".");
    }

    #[test]
    fn rust_path_is_verbatim() {
        let segs = tokenize("Call crux_core::config::Config now.");
        let verbatim: Vec<_> = segs
            .iter()
            .filter_map(|s| {
                if let Segment::Verbatim(v) = s {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(
            verbatim.contains(&"crux_core::config::Config"),
            "expected qualified Rust path verbatim, got {verbatim:?}"
        );
    }

    #[test]
    fn function_call_literal_is_verbatim() {
        let segs = tokenize("Pass it to foo(bar, baz) at the end.");
        let verbatim: Vec<_> = segs
            .iter()
            .filter_map(|s| {
                if let Segment::Verbatim(v) = s {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(verbatim.contains(&"foo(bar, baz)"));
    }

    #[test]
    fn ipv4_and_hex_preserved() {
        let segs = tokenize("Server 127.0.0.1 returned 0xdeadbeef as response.");
        let verbatim: Vec<&str> = segs
            .iter()
            .filter_map(|s| {
                if let Segment::Verbatim(v) = s {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(verbatim.contains(&"127.0.0.1"));
        assert!(verbatim.contains(&"0xdeadbeef"));
    }

    #[test]
    fn screaming_const_preserved() {
        let segs = tokenize("Set MAX_DEPTH and try again.");
        let verbatim: Vec<&str> = segs
            .iter()
            .filter_map(|s| {
                if let Segment::Verbatim(v) = s {
                    Some(v.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert!(verbatim.contains(&"MAX_DEPTH"));
    }

    #[test]
    fn screaming_acronym_not_preserved() {
        let segs = tokenize("AI is everywhere these days.");
        assert_eq!(types(&segs), vec!["P"]);
    }
}
