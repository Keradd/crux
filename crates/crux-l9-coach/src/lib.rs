//! CRUX Layer 9 — coach / quality scoring / drift + loop detection.
//!
//! Three independent engines share the same SQLite tables from migration
//! 004:
//!
//! - [`CoachEngine`] — compute a 0..100 health score from config +
//!   telemetry + memory state, persist snapshots to `quality_scores`.
//! - [`LoopDetector`] — token-Jaccard similarity on the last N user
//!   messages and tool results per session. Backed by `loop_state`.
//! - [`DriftTracker`] — hash-and-diff `CLAUDE.md` into `claude_md_history`
//!   so we can flag silent rule edits between sessions.
//!
//! Engines are synchronous and take an `&rusqlite::Connection` so
//! callers can share the main CRUX DB handle without a separate pool.

pub mod drift;
pub mod engine;
pub mod loop_detect;
pub mod types;

pub use drift::DriftTracker;
pub use engine::CoachEngine;
pub use loop_detect::LoopDetector;
pub use types::{
    score_to_grade, CoachData, DriftCheckResult, LoopCheckResult, Pattern, Severity, Snapshot,
};
