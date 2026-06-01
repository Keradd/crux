pub mod audit;
pub mod bash;
pub mod common;
pub mod digest;
pub mod execute;
pub mod graph;
pub mod memory;
pub mod read;
pub mod search;
pub mod symbols;

use std::sync::LazyLock;

use serde_json::{json, Value};
use tracing::info;

use crate::dispatch::AppContext;
use crate::protocol::ToolDefinition;

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn call(&self, ctx: &AppContext, args: &Value) -> Result<String, String>;
}

pub static REGISTRY: LazyLock<Vec<Box<dyn Tool>>> = LazyLock::new(|| {
    vec![
        Box::new(memory::Remember),
        Box::new(memory::Recall),
        Box::new(read::Read),
        Box::new(bash::BashFilter),
        Box::new(audit::Audit),
        Box::new(symbols::FindSymbol),
        Box::new(symbols::GetSymbolSource),
        Box::new(graph::QueryGraph),
        Box::new(graph::Impact),
        Box::new(search::Search),
        Box::new(execute::Execute),
        Box::new(digest::Digest),
        Box::new(digest::Compact),
    ]
});

pub fn all_tools() -> Vec<ToolDefinition> {
    info!("all_tools called in tools/mod.rs");
    vec![
        tool("crux_remember",
            "Persist an observation (user/feedback/project/reference/guardrail/error_pattern/decision/convention) for future sessions. Returns the observation id.",
            json!({
                "type": "object",
                "required": ["kind", "title", "content"],
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["user","feedback","project","reference","guardrail","error_pattern","decision","convention"],
                        "description": "Observation kind"
                    },
                    "title": { "type": "string", "description": "One-line summary" },
                    "content": { "type": "string", "description": "Full observation body" },
                    "why": { "type": "string", "description": "Optional rationale" },
                    "how_to_apply": { "type": "string", "description": "Optional how-to-apply note" },
                    "symbol": { "type": "string", "description": "Optional related symbol" },
                    "file_path": { "type": "string", "description": "Optional related file" },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "importance": { "type": "integer", "minimum": 1, "maximum": 10, "description": "1..10" },
                    "private": { "type": "boolean" }
                }
            }),
        ),
        tool("crux_recall",
            "Recall observations matching a query. Decay-ranked, optionally filtered by kind/symbol.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Free-text query (FTS5)." },
                    "kinds": { "type": "array", "items": { "type": "string" }, "description": "Optional kind filter." },
                    "symbol": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "include_archived": { "type": "boolean", "default": false }
                }
            }),
        ),
        tool("crux_read",
            "Cache-aware file read with optional range. Prefer a narrow slice — pass offset+limit or symbol.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Required unless symbol is set." },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit":  { "type": "integer", "minimum": 0, "default": 0 },
                    "symbol": { "type": "string", "description": "Optional qualified_name from AST graph." },
                    "agent_id": { "type": "string", "default": "default" },
                    "session_id": { "type": "string", "default": "default" }
                }
            }),
        ),
        tool("crux_bash_filter",
            "Filter pre-captured bash output through L3 token-optimization filters.",
            json!({
                "type": "object",
                "required": ["command", "output"],
                "properties": {
                    "command": { "type": "string" },
                    "output": { "type": "string" }
                }
            }),
        ),
        tool("crux_audit",
            "Get CRUX health snapshot: layer toggles + telemetry totals.",
            json!({ "type": "object", "properties": {} }),
        ),
        tool("crux_find_symbol",
            "Find symbols by name in the AST graph. Substring match by default.",
            json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string" },
                    "kind": { "type": "string", "enum": ["File","Module","Class","Function","Method","Type","Test","Constant"] },
                    "exact": { "type": "boolean", "default": false },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 }
                }
            }),
        ),
        tool("crux_get_symbol_source",
            "Return source body of a symbol by qualified name. Cheaper than reading the whole file.",
            json!({
                "type": "object",
                "required": ["qualified_name"],
                "properties": {
                    "qualified_name": { "type": "string" },
                    "include_metadata": { "type": "boolean", "default": true }
                }
            }),
        ),
        tool("crux_query_graph",
            "Query AST graph: callers or callees of a symbol.",
            json!({
                "type": "object",
                "required": ["qualified_name", "direction"],
                "properties": {
                    "qualified_name": { "type": "string" },
                    "direction": { "type": "string", "enum": ["callers", "callees"] }
                }
            }),
        ),
        tool("crux_impact",
            "Conservative blast-radius BFS over call graph. Shows affected callers before a refactor.",
            json!({
                "type": "object",
                "required": ["qualified_name"],
                "properties": {
                    "qualified_name": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 10, "default": 2 },
                    "max":   { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
                }
            }),
        ),
        tool("crux_search",
            "Hybrid search (BM25 + dense vector) across indexed chunks. Run crux reindex first.",
            json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "kinds": { "type": "array", "items": { "type": "string", "enum": ["code","prose","symbol","memory"] } },
                    "view": { "type": "string", "enum": ["compact","default","full"], "default": "default" },
                    "view_lines": { "type": "integer", "minimum": 0, "maximum": 20, "default": 3 },
                    "debug": { "type": "boolean", "default": false }
                }
            }),
        ),
        tool("crux_execute",
            "Run a short snippet in a sandboxed subprocess (python/bash/node).",
            json!({
                "type": "object",
                "required": ["runtime", "code"],
                "properties": {
                    "runtime": { "type": "string", "enum": ["python", "bash", "node"] },
                    "code": { "type": "string" },
                    "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 60, "default": 10 },
                    "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 65536 },
                    "inherit_env": { "type": "boolean", "default": false }
                }
            }),
        ),
        tool("crux_digest",
            "Render compact summary of past tool calls for a session (L11).",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "default": "default" },
                    "pending_only": { "type": "boolean", "default": false }
                }
            }),
        ),
        tool("crux_compact",
            "Force-roll up pending turn events into a single digest row (L11).",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "default": "default" }
                }
            }),
        ),
    ]
}

fn tool(name: &str, desc: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: desc.to_string(),
        input_schema,
    }
}
