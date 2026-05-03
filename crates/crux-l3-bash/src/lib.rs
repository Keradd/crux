//! CRUX Layer 3 — bash command output filter.
//!
//! Goal: replace the verbose stdout of common dev tools with the minimum
//! fact the agent needs. Mechanism is a TOML DSL describing an 8-stage
//! pipeline, ported from rtk-ai/rtk's reference implementation.
//!
//! Public surface:
//! - [`FilterEngine`] — owns compiled filters, dispatches by command line
//! - [`FilterSpec`] / [`FilterFile`] — TOML deserialization types
//! - [`Filter`] / [`FilterOutput`] — single filter and its output value
//!
//! See `crates/crux-l3-bash/filters/*.toml` for the built-in filter set.

pub mod engine;
pub mod pipeline;
pub mod spec;

pub use engine::{FilterEngine, ProcessResult};
pub use pipeline::{Filter, FilterOutput, OutputKind};
pub use spec::{FilterFile, FilterSpec, FilterTest, MatchRule, ReplaceRule};

// ─────────────────────────────────────────────────────────────────────────
// Inline TOML test runner.
//
// Each built-in filter file may include a `[[tests]]` array of golden
// inputs and expected outputs. We surface them here as one cargo test so
// `cargo test -p crux-l3-bash` exercises every fixture.
// ─────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod inline_tests {
    use super::*;

    const BUILTIN: &[(&str, &str)] = &[
        ("git", include_str!("../filters/git.toml")),
        ("cargo", include_str!("../filters/cargo.toml")),
        ("npm", include_str!("../filters/npm.toml")),
        ("jest", include_str!("../filters/jest.toml")),
        ("generic", include_str!("../filters/generic.toml")),
    ];

    #[test]
    fn every_inline_test_passes() {
        for (origin, raw) in BUILTIN {
            let parsed: FilterFile = toml::from_str(raw).expect(origin);
            if parsed.tests.is_empty() {
                continue;
            }

            let mut engine = FilterEngine::empty();
            for (name, spec) in parsed.filters.clone() {
                engine.add(name, spec).expect(origin);
            }

            for t in &parsed.tests {
                let f = parsed.filters.get(&t.filter).unwrap_or_else(|| {
                    panic!(
                        "[{origin}] test '{}' references unknown filter '{}'",
                        t.name, t.filter
                    )
                });
                let compiled = Filter::compile(t.filter.clone(), f.clone()).expect(origin);
                let got = compiled.apply(&t.input);
                assert_eq!(
                    got.text.trim_end(),
                    t.expected.trim_end(),
                    "[{origin}] inline test '{}' failed.\n--- input ---\n{}\n--- expected ---\n{}\n--- got ---\n{}",
                    t.name, t.input, t.expected, got.text
                );
            }
        }
    }
}
