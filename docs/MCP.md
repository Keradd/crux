# CRUX MCP Server and Tools

`crux mcp` exposes CRUX over stdio JSON-RPC so every AI agent can
call into it. `crux mcp-shrink` boots CRUX as a proxy in front of
any upstream MCP server so `tools/list` / `prompts/list` /
`resources/list` payloads are compressed before the model sees them.

## Start the server

```bash
crux mcp                                                         # stdio JSON-RPC
crux mcp-shrink npx @modelcontextprotocol/server-filesystem /path # L2 shrinker
```

Agents registered through `crux setup` point at `crux mcp`
automatically — see [`INSTALL.md`](INSTALL.md) for the per-agent
matrix.

## Exposed tools

| Tool | Layer | Purpose |
|---|---|---|
| `crux_remember` / `crux_recall` | L8 | Persist & search observations |
| `crux_read` | L4 | Cache-aware file reads with delta replies |
| `crux_bash_filter` | L3 | Apply the L3 filter to a `(command, output)` pair |
| `crux_audit` | L9 | Health snapshot + telemetry summary |
| `crux_find_symbol` / `crux_get_symbol_source` | L5 | Symbol lookup |
| `crux_query_graph` / `crux_impact` | L5 | Callers / callees / blast radius |
| `crux_search` | L6 | Hybrid BM25 + dense + RRF (line-aware snippets + symbol enrichment) |
| `crux_execute` | L7 | Run python / bash / node snippets in the sandbox |
| `crux_digest` / `crux_compact` | L11 | Render / force-roll conversation turn digests |

See [`ARCHITECTURE.md`](ARCHITECTURE.md) §8 for the full JSON
schema of every tool.

## Description shrinker (L2)

`crux mcp-shrink <upstream> [args…]` proxies an upstream MCP
server and rewrites the descriptive fields on the way through.
No persistence, no policy — just deterministic compression of the
`description` fields CRUX sees the agent fetching from
`tools/list`, `prompts/list`, and `resources/list`.

Combine with `crux mcp` (CRUX-as-server) and any upstream server
(CRUX-as-proxy) to keep the model's context window lean on both
sides of the JSON-RPC boundary.
