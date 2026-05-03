//! CRUX Layer 5 — tree-sitter AST graph.
//!
//! Goal: replace "read whole file to find a symbol" with structural
//! queries. Index a project once, then ask `crux find` / `crux symbol`
//! / `crux impact` instead of re-reading the source tree.
//!
//! Public surface:
//! - [`index_project`] — walk a directory and persist its AST graph.
//! - [`extract::parse`] — pure parser (per-language) returning a
//!   [`types::ParseResult`]. Useful for tests.
//! - [`graph::GraphStore`] — query API: find_symbol / callers_of /
//!   callees_of / impact_radius.
//! - [`types`] — `NodeKind`, `EdgeKind`, `Language`, `GraphNode`, ...

pub mod extract;
pub mod graph;
pub mod indexer;
pub mod resolver;
pub mod sig_cache;
pub mod tsconfig;
pub mod types;

pub use extract::parse;
pub use graph::GraphStore;
pub use indexer::{index_project, index_project_with};
pub use types::{
    ConfidenceTier, EdgeKind, GraphNode, IndexStats, Language, NodeKind, ParseResult, ParsedEdge,
    ParsedNode,
};
