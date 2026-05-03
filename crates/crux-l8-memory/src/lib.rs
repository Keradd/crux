//! CRUX Layer 8 — persistent observation memory with decay-aware recall.
//!
//! Inputs: a writable rusqlite [`rusqlite::Connection`] whose schema has
//! migration 003 applied (CRUX `crux_core::db::open` does that automatically).
//!
//! Public surface:
//! - [`MemoryEngine`] — `remember`, `recall`, `list`, `archive`, `delete`,
//!   `decay_pass`
//! - [`NewObservation`] / [`Observation`] / [`ObservationKind`] — types
//! - [`RecallQuery`] / [`RankedObservation`] — recall input/output
//! - [`DecayParams`] / [`DecayTable`] — per-kind decay configuration

pub mod decay;
pub mod engine;
pub mod types;

pub use decay::{DecayParams, DecayTable};
pub use engine::MemoryEngine;
pub use types::{
    DecayStats, NewObservation, Observation, ObservationKind, RankedObservation, RecallQuery,
};
