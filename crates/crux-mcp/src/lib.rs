//! CRUX MCP server.
//!
//! Exposes the layer APIs (memory, read cache, bash filter, audit) as
//! Model Context Protocol tools over stdio JSON-RPC. Designed to be
//! launched as `crux mcp` from any MCP-compatible client (Claude Code,
//! Cursor, Cline, Continue, Aider, ...).
//!
//! Public surface:
//! - [`serve_stdio`] — run the server (blocking) on stdin/stdout
//! - [`tools::all_tools`] — every tool definition (for `tools/list`)
//! - [`dispatch::call`] — pure dispatcher, useful for tests
//! - [`protocol`] — JSON-RPC + MCP wire types

pub mod dispatch;
pub mod protocol;
pub mod server;
pub mod shrink;
pub mod tools;

pub use server::serve_stdio;
pub use shrink::run_proxy as run_shrink_proxy;
