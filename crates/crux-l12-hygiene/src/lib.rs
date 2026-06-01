#![deny(unsafe_code)]

pub mod fixer;
pub mod rules;
pub mod scanner;
pub mod stripper;
pub mod types;

pub use fixer::fix_comments;
pub use rules::{HygieneRule, RuleId, RULES};
pub use scanner::{scan_comments, scan_paths};
pub use stripper::strip_comments;
pub use types::{
    FixReport, HygieneOptions, HygieneReport, HygieneViolation, Severity, StripReport,
};
