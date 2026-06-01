#![deny(unsafe_code)]

pub mod drift;
pub mod engine;
pub mod loop_detect;
pub mod openclaw;
pub mod types;

pub use drift::DriftTracker;
pub use engine::{CoachEngine, TOTAL_LAYERS};
pub use loop_detect::LoopDetector;
pub use openclaw::{
    audit as audit_openclaw, category_label as openclaw_category_label, AuditReport,
    Component as OpenClawComponent, ContextCategory, Recommendation as OpenClawRecommendation,
};
pub use types::{
    score_to_grade, CoachData, DriftCheckResult, LoopCheckResult, Pattern, Severity, Snapshot,
};
