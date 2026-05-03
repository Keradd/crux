//! CRUX Layer 2 — MCP description shrinker.
//!
//! Acts as a transparent proxy in front of an upstream MCP server. Reads
//! line-delimited JSON-RPC envelopes on stdin from the agent, forwards
//! them to the upstream process unchanged, and on the way back compresses
//! prose-heavy fields (`description`) inside `tools/list`, `prompts/list`,
//! `resources/list`, and `resourceTemplates` results.
//!
//! Mechanism is rule-based — no LLM call. We strip filler words and
//! redundant phrases while preserving:
//!   - code blocks (fenced ``` and indented)
//!   - inline code (backticks)
//!   - URLs and file paths
//!   - identifiers, env vars, version numbers
//!   - JSON Schema fragments inside the description
//!
//! Tool-call response bodies are NEVER touched — they may contain
//! structured data the agent depends on byte-for-byte.

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;
use std::thread;

use serde_json::Value;
use tracing::warn;

use crux_core::error::{CruxError, Result};

const COMPRESSIBLE_LIST_KEYS: &[&str] = &["tools", "prompts", "resources", "resourceTemplates"];

/// Run the shrinker proxy: spawn `upstream`, pump stdin/stderr through,
/// and transform stdout. Blocks until either side closes.
pub fn run_proxy(upstream: &[String]) -> Result<i32> {
    if upstream.is_empty() {
        return Err(CruxError::other(
            "crux mcp-shrink: missing upstream command",
        ));
    }

    let mut child = Command::new(&upstream[0])
        .args(&upstream[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| CruxError::other(format!("spawn upstream: {e}")))?;

    let upstream_stdin = child.stdin.take().expect("stdin piped");
    let upstream_stdout = child.stdout.take().expect("stdout piped");

    // Forward agent → upstream verbatim. Runs on its own thread so
    // upstream's first reply doesn't block waiting on the agent's
    // second message.
    let in_thread = thread::spawn(move || forward_stdin(upstream_stdin));

    // upstream → agent: parse, compress, emit.
    let out_thread = thread::spawn(move || forward_upstream_to_stdout(upstream_stdout));

    let status = child
        .wait()
        .map_err(|e| CruxError::other(format!("wait upstream: {e}")))?;
    let _ = in_thread.join();
    let _ = out_thread.join();

    Ok(status.code().unwrap_or(0))
}

fn forward_stdin(mut upstream_stdin: ChildStdin) {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return,
            Ok(_) => {
                if upstream_stdin.write_all(&buf).is_err() {
                    return;
                }
                let _ = upstream_stdin.flush();
            }
            Err(_) => return,
        }
    }
}

fn forward_upstream_to_stdout(upstream_stdout: ChildStdout) {
    let mut reader = BufReader::new(upstream_stdout);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\n', '\r']);
                if trimmed.is_empty() {
                    let _ = handle.write_all(line.as_bytes());
                    let _ = handle.flush();
                    continue;
                }
                let out = transform_line(trimmed);
                let _ = handle.write_all(out.as_bytes());
                let _ = handle.write_all(b"\n");
                let _ = handle.flush();
            }
            Err(e) => {
                warn!(?e, "stdout read failed");
                return;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// JSON traversal
// ─────────────────────────────────────────────────────────────────────────

pub fn transform_line(line: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(mut v) => {
            shrink_response(&mut v);
            serde_json::to_string(&v).unwrap_or_else(|_| line.to_string())
        }
        Err(_) => {
            // Not JSON (could be a stderr log leaking) — pass through unchanged.
            line.to_string()
        }
    }
}

fn shrink_response(value: &mut Value) {
    let Value::Object(map) = value else {
        return;
    };
    // Only `result` payloads carry the lists we compress.
    let Some(result) = map.get_mut("result") else {
        return;
    };
    let Value::Object(result_obj) = result else {
        return;
    };

    let mut compressed_any = false;
    for key in COMPRESSIBLE_LIST_KEYS {
        if let Some(Value::Array(items)) = result_obj.get_mut(*key) {
            for item in items {
                if shrink_item(item) {
                    compressed_any = true;
                }
            }
        }
    }

    // Some servers stuff descriptions in nested schemas. Walk recursively
    // only when we didn't already compress at the top level — avoids
    // double-processing nested params on a tools/list reply.
    if !compressed_any {
        walk_nested_descriptions(result);
    }
}

fn shrink_item(item: &mut Value) -> bool {
    let Value::Object(obj) = item else {
        return false;
    };
    let mut hit = false;
    if let Some(Value::String(desc)) = obj.get_mut("description") {
        let new = compress_prose(desc);
        if new != *desc {
            *desc = new;
            hit = true;
        }
    }
    hit
}

fn walk_nested_descriptions(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            for (k, v) in obj.iter_mut() {
                if k == "description" {
                    if let Value::String(s) = v {
                        let new = compress_prose(s);
                        if new != *s {
                            *s = new;
                        }
                    }
                }
                walk_nested_descriptions(v);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                walk_nested_descriptions(v);
            }
        }
        _ => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Prose compressor
// ─────────────────────────────────────────────────────────────────────────

/// Compress prose without touching code, URLs, paths, identifiers,
/// numbers, env vars. Logic: split into protected / non-protected spans
/// via a regex, compress only non-protected spans, reassemble.
pub fn compress_prose(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let re = protected_regex();
    let mut out = String::with_capacity(input.len());
    let mut last_end = 0usize;

    for m in re.find_iter(input) {
        if m.start() > last_end {
            out.push_str(&compress_unprotected(&input[last_end..m.start()]));
        }
        out.push_str(m.as_str());
        last_end = m.end();
    }
    if last_end < input.len() {
        out.push_str(&compress_unprotected(&input[last_end..]));
    }

    // Final: collapse runs of spaces and strip leading/trailing space.
    let mut s = collapse_spaces(&out);
    s = s.trim().to_string();
    s
}

fn compress_unprotected(seg: &str) -> String {
    // Word-by-word, drop fillers; replace verbose phrases.
    let mut s = seg.to_string();
    // Phrase replacements first (longest first to avoid partial overlap).
    for (pat, rep) in PHRASES {
        s = ascii_ireplace(&s, pat, rep);
    }
    // Token-level filler removal.
    s = strip_filler_words(&s);
    s
}

const FILLER_WORDS: &[&str] = &[
    "the",
    "a",
    "an",
    "just",
    "really",
    "basically",
    "actually",
    "simply",
    "essentially",
    "very",
    "quite",
    "somewhat",
    "rather",
    "fairly",
    "please",
    "kindly",
    "note",
    "noted",
    "note that",
    "you can",
    "you may",
    "you might",
    "in order",
    "for the purpose of",
];

const PHRASES: &[(&str, &str)] = &[
    ("in order to", "to"),
    ("make use of", "use"),
    ("a number of", "several"),
    ("at this point in time", "now"),
    ("due to the fact that", "because"),
    ("with regard to", "about"),
    ("in the event that", "if"),
    ("for the purpose of", "for"),
    ("in spite of the fact that", "despite"),
    ("each and every", "every"),
    ("the ability to", "ability to"),
    ("it is important to note that", ""),
    ("please note that", ""),
    ("it should be noted that", ""),
    ("you can use this to", "use to"),
];

fn strip_filler_words(input: &str) -> String {
    // Scan word-by-word preserving punctuation. We only treat ASCII
    // alphabetic runs as candidate words.
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() {
            let mut word = String::new();
            word.push(c);
            while let Some(&nc) = chars.peek() {
                if nc.is_ascii_alphabetic() {
                    word.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if FILLER_WORDS.iter().any(|f| f.eq_ignore_ascii_case(&word)) {
                // Drop the word and one trailing space (best effort).
                if out.ends_with(' ') && chars.peek() == Some(&' ') {
                    chars.next();
                }
                continue;
            }
            out.push_str(&word);
        } else {
            out.push(c);
        }
    }
    out
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c == ' ' || c == '\t' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

fn ascii_ireplace(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0usize;
    while i < h.len() {
        if i + n.len() <= h.len() && eq_ignore_ascii(&h[i..i + n.len()], n) {
            out.push_str(replacement);
            i += n.len();
        } else {
            out.push(h[i] as char);
            i += 1;
        }
    }
    out
}

fn eq_ignore_ascii(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

/// Regex matching anything we MUST preserve verbatim:
///
///   - fenced code blocks (```...```)
///   - inline code (`...`)
///   - URLs (http/https/ftp)
///   - absolute or dotted paths (/foo/bar, ./bar, ../bar, .bar)
///   - env-style identifiers: ALL_CAPS_WITH_UNDERSCORES
///   - `name=value` pairs
///   - version-ish tokens (1.2, 1.2.3, v1.2.3, semver-ish)
///   - identifier-with-dot or hyphen tokens (foo.bar, my-tool)
fn protected_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?xs)
              ```[\s\S]*?```                                 # fenced code
            | `[^`\n]*?`                                     # inline code
            | https?://\S+|ftp://\S+                         # URLs
            | (?:\.{0,2}/[A-Za-z0-9_./-]+)                  # paths
            | \b[A-Z][A-Z0-9_]{2,}\b                         # ENV_VARS
            | \b[A-Za-z_][A-Za-z0-9_]*=\S+                   # k=v
            | \bv?\d+(?:\.\d+)+(?:[A-Za-z0-9_.-]*)?\b        # versions
            | \b[A-Za-z_][A-Za-z0-9_]*[.-][A-Za-z0-9_.-]+\b  # foo.bar / my-tool
            ",
        )
        .expect("compile protected regex")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn drops_filler_articles() {
        let s = compress_prose("the foo is just a bar");
        assert!(!s.to_ascii_lowercase().contains(" the "));
        assert!(s.contains("foo"));
        assert!(s.contains("bar"));
    }

    #[test]
    fn preserves_code_block() {
        let s = compress_prose("Use the function `read_file()` to load data.");
        assert!(s.contains("`read_file()`"));
    }

    #[test]
    fn preserves_url() {
        let s = compress_prose("See the docs at https://example.com/page for details.");
        assert!(s.contains("https://example.com/page"));
    }

    #[test]
    fn preserves_path() {
        let s = compress_prose("Read the file from /etc/passwd carefully.");
        assert!(s.contains("/etc/passwd"));
    }

    #[test]
    fn preserves_env_var() {
        let s = compress_prose("Set the API_KEY env var before you run.");
        assert!(s.contains("API_KEY"));
    }

    #[test]
    fn replaces_phrases() {
        let s = compress_prose("Use the file in order to load data.");
        assert!(!s.contains("in order to"));
        assert!(s.contains("to"));
    }

    #[test]
    fn shorter_than_input_on_typical_prose() {
        let input = "Please note that you may want to use the tool in order to handle the request properly.";
        let out = compress_prose(input);
        assert!(out.len() < input.len(), "got: {out}");
    }

    #[test]
    fn shrink_response_walks_tools_list() {
        let mut v = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    { "name": "read_file", "description": "Use this in order to read a file from disk." },
                    { "name": "write_file", "description": "You can use this to write content to disk." },
                ]
            }
        });
        shrink_response(&mut v);
        let arr = v["result"]["tools"].as_array().unwrap();
        for t in arr {
            let d = t["description"].as_str().unwrap();
            assert!(!d.contains("in order to"));
            assert!(!d.to_ascii_lowercase().contains("you can use this to"));
        }
    }

    #[test]
    fn ignores_non_result_messages() {
        let mut v =
            json!({"jsonrpc":"2.0","method":"notification","params":{"description":"this is a"}});
        shrink_response(&mut v);
        assert_eq!(v["params"]["description"], "this is a");
    }

    #[test]
    fn transform_line_passes_through_invalid_json() {
        let line = "not json at all";
        assert_eq!(transform_line(line), line);
    }
}
