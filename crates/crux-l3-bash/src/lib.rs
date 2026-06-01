#![deny(unsafe_code)]

pub mod engine;
pub mod pipeline;
pub mod spec;

pub use engine::{FilterEngine, ProcessResult};
pub use pipeline::{Filter, FilterOutput, OutputKind};
pub use spec::{FilterFile, FilterSpec, FilterTest, MatchRule, ReplaceRule};

#[cfg(test)]
mod inline_tests {
    use super::*;

    const BUILTIN: &[(&str, &str)] = &[
        ("git", include_str!("../filters/git.toml")),
        ("cargo", include_str!("../filters/cargo.toml")),
        ("npm", include_str!("../filters/npm.toml")),
        ("jest", include_str!("../filters/jest.toml")),
        ("openclaw", include_str!("../filters/openclaw.toml")),
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
