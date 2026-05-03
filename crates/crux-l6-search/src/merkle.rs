//! Backwards-compatible re-exports.
//!
//! [`MerkleSync`] was promoted to `crux-core` so Layer 5 can share the
//! same snapshot plumbing. Existing callers that import from
//! `crux_l6_search::merkle` keep working via this thin shim.

pub use crux_core::merkle::{FileChangeSet, FileSnapshot, MerkleSync, SCOPE_AST, SCOPE_CHUNKS};
