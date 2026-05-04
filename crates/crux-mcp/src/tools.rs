use serde_json::{json, Value};

use crate::protocol::ToolDefinition;

pub fn all_tools() -> Vec<ToolDefinition> {
    vec![
        tool(
            "crux_remember",
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
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "importance": {
                        "type": "integer",
                        "minimum": 1, "maximum": 10,
                        "description": "1..10. Higher persists longer through decay."
                    },
                    "private": { "type": "boolean" }
                }
            }),
        ),
        tool(
            "crux_recall",
            "Recall observations matching a query. Decay-ranked, optionally filtered by kind/symbol. Empty query returns the highest-relevance items for the project.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Free-text query (FTS5)." },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional kind filter."
                    },
                    "symbol": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "include_archived": { "type": "boolean", "default": false }
                }
            }),
        ),
        tool(
            "crux_read",
            "Cache-aware file read with optional range. First read: returns the requested content. Subsequent identical reads (same range): returns a structural digest. Reads on a changed file return a line-level diff. Prefer a narrow slice over the whole file — pass `offset` + `limit` for a line range, or `symbol=<qualified_name>` to slice exactly one symbol via the L5 graph. Response also includes any L8 observations attached to the file.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute or project-relative path. Required unless `symbol` is set." },
                    "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "1-based starting line (0 and 1 both mean top of file). Ignored when `symbol` is set." },
                    "limit":  { "type": "integer", "minimum": 0, "default": 0, "description": "Number of lines to return (0 = to end of file). Ignored when `symbol` is set." },
                    "symbol": { "type": "string", "description": "Optional qualified_name — resolves file + line range from the L5 AST graph, overriding file_path/offset/limit." },
                    "agent_id": { "type": "string", "default": "default" },
                    "session_id": { "type": "string", "default": "default" }
                }
            }),
        ),
        tool(
            "crux_bash_filter",
            "Run a bash command's pre-captured output through CRUX Layer 3 filters. Use this when you already have command output and want a token-efficient summary.",
            json!({
                "type": "object",
                "required": ["command", "output"],
                "properties": {
                    "command": { "type": "string", "description": "Original command line (used for filter matching)." },
                    "output": { "type": "string", "description": "Raw stdout/stderr to filter." }
                }
            }),
        ),
        tool(
            "crux_audit",
            "Get current CRUX health snapshot: layer toggles + telemetry totals.",
            json!({
                "type": "object",
                "properties": {}
            }),
        ),
        tool(
            "crux_find_symbol",
            "Find symbols (functions, methods, classes, structs, types, modules, constants) by name in the AST graph. Substring match by default. Run `crux index` first if the graph is empty.",
            json!({
                "type": "object",
                "required": ["name"],
                "properties": {
                    "name": { "type": "string", "description": "Symbol name to search for." },
                    "kind": {
                        "type": "string",
                        "enum": ["File","Module","Class","Function","Method","Type","Test","Constant"],
                        "description": "Optional kind filter."
                    },
                    "exact": { "type": "boolean", "default": false, "description": "If true, match name exactly; otherwise substring (LIKE)." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 30 }
                }
            }),
        ),
        tool(
            "crux_get_symbol_source",
            "Return the source body of a symbol by qualified name. Reads the underlying file and slices to the symbol's line range. Cheaper than reading the whole file.",
            json!({
                "type": "object",
                "required": ["qualified_name"],
                "properties": {
                    "qualified_name": { "type": "string", "description": "Fully-qualified symbol name (as printed by crux_find_symbol)." },
                    "include_metadata": { "type": "boolean", "default": true, "description": "Prefix the source with kind/file/lines header." }
                }
            }),
        ),
        tool(
            "crux_query_graph",
            "Query AST graph relationships for a symbol. Direction `callers` = who calls this symbol; `callees` = which symbols this one calls. Returns the related node list.",
            json!({
                "type": "object",
                "required": ["qualified_name", "direction"],
                "properties": {
                    "qualified_name": { "type": "string" },
                    "direction": {
                        "type": "string",
                        "enum": ["callers", "callees"]
                    }
                }
            }),
        ),
        tool(
            "crux_impact",
            "Conservative blast-radius BFS over the call graph. Returns up to `max` ancestor callers within `depth` hops. Useful before a refactor: shows who would be affected by a change to the symbol.",
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
        tool(
            "crux_search",
            "Hybrid search across the indexed chunk store: BM25 (porter + trigram FTS5) + dense vector ranker fused via RRF. Run `crux reindex` once to populate the chunk store. Default `view=default` returns a line-aware multi-line snippet for code chunks (matched line ± `view_lines` lines, marked with `>`); `view=compact` keeps the legacy ~80-char text window; `view=full` returns the entire chunk content (skip the follow-up read at the cost of a fatter response). Code/symbol hits also include the linked `symbol` (qualified_name from the AST graph) so you can chain into `crux_get_symbol_source` without parsing the file path.",
            json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "Free-text query." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 },
                    "kinds": {
                        "type": "array",
                        "items": {
                            "type": "string",
                            "enum": ["code", "prose", "symbol", "memory"]
                        },
                        "description": "Optional content-type filter."
                    },
                    "view": {
                        "type": "string",
                        "enum": ["compact", "default", "full"],
                        "default": "default",
                        "description": "Snippet shape per hit. `compact` = legacy ~80-char window. `default` = line-aware multi-line for code, char-window for prose. `full` = entire chunk content."
                    },
                    "view_lines": {
                        "type": "integer",
                        "minimum": 0, "maximum": 20, "default": 3,
                        "description": "Context lines on each side of the matched line when `view=default` (only applies to code/symbol chunks)."
                    },
                    "debug": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, attach a `debug` block with per-ranker rank, raw score, source_id, language, and tokens_est. Off by default to keep payloads lean."
                    }
                }
            }),
        ),
        tool(
            "crux_execute",
            "Run a short snippet in a sandboxed subprocess (Python / Bash / Node). Returns stdout, stderr, exit code, and timeout/truncation flags. Use this to compute values deterministically instead of asking the model to reason it out.",
            json!({
                "type": "object",
                "required": ["runtime", "code"],
                "properties": {
                    "runtime": {
                        "type": "string",
                        "enum": ["python", "bash", "node"]
                    },
                    "code":               { "type": "string", "description": "Source to run." },
                    "timeout_seconds":    { "type": "integer", "minimum": 1, "maximum": 60, "default": 10 },
                    "max_output_bytes":   { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 65536 },
                    "inherit_env":        { "type": "boolean", "default": false }
                }
            }),
        ),
        tool(
            "crux_digest",
            "Render a compact, deterministic summary of past tool calls for a session (Layer 11). Replaces re-feeding raw history into context. Returns the latest rolled-up digest (if any) followed by still-pending events. Pass `pending_only=true` to skip the rollup view. Default session id is `default`.",
            json!({
                "type": "object",
                "properties": {
                    "session_id":   { "type": "string", "default": "default", "description": "Session id whose digest you want." },
                    "pending_only": { "type": "boolean", "default": false, "description": "When true, render only the pending (unrolled) events for the session." }
                }
            }),
        ),
        tool(
            "crux_compact",
            "Force-roll up all pending turn events for a session into a single digest row (Layer 11). Useful at session boundaries or when you want the current activity mirrored into long-term L8 memory. Returns the new digest id, event count, and rendered summary.",
            json!({
                "type": "object",
                "properties": {
                    "session_id": { "type": "string", "default": "default", "description": "Session id to compact." }
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
