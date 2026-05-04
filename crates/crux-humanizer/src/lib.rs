pub mod engine;
pub mod rules;
pub mod tokenizer;
pub mod types;

pub use engine::Humanizer;
pub use types::{HumanizeOptions, HumanizeResult, Mode, Stats};

pub fn humanize(input: &str, mode: Mode) -> HumanizeResult {
    Humanizer::new(mode).rewrite(input)
}
