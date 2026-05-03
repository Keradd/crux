# CRUX — Architecture Design

**Tagline:** *One Rust binary. One SQLite DB. Ten layers. Local-first.*

CRUX = **C**ompression **R**untime for **U**niversal e**X**ecution. Combines the best of 10 token-optimization repos into a single coherent system.

---

## 1. Goals & Non-Goals

### Goals (must)
1. **Single-binary deployment** — one `crux` executable, no Node/Python runtime
2. **Zero-network by default** — works offline; cloud features opt-in
3. **Modular layers** — turn any layer on/off via TOML
4. **Cross-layer integration** — read-cache invalidates AST graph; sandbox uses memory; etc.
5. **Measurable** — every reduction tracked in local SQLite
6. **Multi-platform** — works w/ Claude Code, Cursor, Codex, OpenClaw, Cline, Continue, Aider, etc.
7. **Multi-protocol** — MCP server, CLI, PreToolUse hook, library
8. **Reversible** — every layer has warn/block/shadow modes

### Non-Goals (won't)
1. ❌ Replace LLM APIs — CRUX sits between agent and LLM/tools
2. ❌ Full sandbox security — best-effort jailing, not production isolation
3. ❌ Cloud telemetry — never phones home
4. ❌ Custom embedding models — uses existing (fastembed-rs / cloud HTTP)
5. ❌ GUI dashboard v1 — CLI + log files initially (web later)

---

## 2. Tech Stack

### Core language: Rust
**Why Rust:**
- Single static binary (rtk validates this)
- 10× faster than Node/Python (matters for hooks on hot path)
- Strong type system for schema/config
- Excellent tree-sitter, sqlite, serde ecosystem

### Key crates

| Concern | Crate | Why |
|---------|-------|-----|
| SQLite | `rusqlite` w/ `bundled-sqlcipher` | Embedded, no system dep |
| FTS5 | `rusqlite` (built-in) | Text search |
| Vector | `sqlite-vec` (binding) | Local vector store |
| Tree-sitter | `tree-sitter` + `tree-sitter-{lang}` | Multi-lang AST |
| Embedding (local) | `fastembed-rs` | ONNX models, no Python |
| HTTP | `reqwest` (rustls) | Cloud APIs |
| TOML | `toml` + `serde` | Config |
| JSON | `serde_json` | MCP protocol |
| CLI | `clap` (derive) | Idiomatic CLI |
| Async | `tokio` | MCP server, daemon |
| File watch | `notify` | Daemon updates |
| Glob | `globset` + `ignore` | .gitignore-style |
| Regex | `regex` | Filter rules |
| Diff | `similar` (LCS) | Delta mode |
| MCP | `rmcp` (rust-mcp-sdk) OR custom | Protocol |
| Sandbox | `mlua`, `rquickjs`, subprocess | Polyglot exec |
| Logging | `tracing` + `tracing-subscriber` | Structured logs |
| Errors | `anyhow` (bin) + `thiserror` (lib) | Standard |

### Storage
**ONE SQLite database** at `~/.crux/db/crux.sqlite` with WAL mode.
Project-scoped via `project_root` column on every table.
Per-project view via `crux config`.

---

## 3. High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        AGENT (Claude/Cursor/etc.)                │
│                                                                  │
│   Tools: Read, Write, Edit, Bash, Grep, MCP-tools...             │
└────────────────────────┬────────────────────────────────────────┘
                         │ JSON-RPC (MCP) or PreToolUse Hook
                         ▼
┌─────────────────────────────────────────────────────────────────┐
│                       crux daemon (Rust)                         │
│                                                                  │
│  ┌────────────────────────────────────────────────────────┐    │
│  │ MCP Server (stdio + tcp)                               │    │
│  │ Tools: crux_search, crux_read, crux_execute,           │    │
│  │        crux_remember, crux_recall, crux_graph,         │    │
│  │        crux_compress, crux_audit, crux_session         │    │
│  └────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌────────────────────────────────────────────────────────┐    │
│  │ Hook router (PreToolUse / PostToolUse)                 │    │
│  │ - Bash → Layer 3 (TOML filters)                        │    │
│  │ - Read → Layer 4 (cache + delta)                       │    │
│  │ - Edit/Write → invalidate Layer 4-5                    │    │
│  └────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌────────────────────────────────────────────────────────┐    │
│  │ Layer engines (independent, opt-in)                    │    │
│  │ L1: Output skill installer                             │    │
│  │ L2: MCP description shrinker                           │    │
│  │ L3: Bash filter (TOML DSL + 3-tier parser)             │    │
│  │ L4: Read cache + delta + structural digest             │    │
│  │ L5: AST graph (tree-sitter)                            │    │
│  │ L6: Hybrid search (BM25 + vector + RRF)                │    │
│  │ L7: Sandbox executor (JS/Python/Bash/Lua)              │    │
│  │ L8: Memory (observations + decay + links)              │    │
│  │ L9: Coach (quality score + nudges + audit)             │    │
│  │ L10: Setup (init scaffolding + profiles)               │    │
│  └────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌────────────────────────────────────────────────────────┐    │
│  │ ONE SQLite DB (~/.crux/db/crux.sqlite)                 │    │
│  │ Tables: read_cache, ast_nodes, ast_edges, observations,│    │
│  │   sessions, embeddings, telemetry, ...                 │    │
│  └────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌────────────────────────────────────────────────────────┐    │
│  │ File watcher (notify) → Layer 5/6 incremental update   │    │
│  └────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
                         │
                         ▼
              Filesystem / Tools / LLM APIs
```

---

## 4. Workspace / Crate Structure

Single Rust workspace, multiple crates:

```
crux/
├── Cargo.toml                # workspace + shared deps (bincode, rusqlite, …)
└── crates/
    ├── crux-core/            # config, db, errors, paths, telemetry, tokens, merkle
    ├── crux-l3-bash/         # 8-stage TOML filter + 3-tier parsers + 5 builtin filters
    │   └── filters/          # git.toml, cargo.toml, npm.toml, jest.toml, generic.toml
    ├── crux-l4-readcache/    # mtime+range cache + LCS delta + contextignore
    ├── crux-l5-ast/          # tree-sitter AST graph + resolver + sig_cache
    ├── crux-l6-search/       # hybrid search: BM25 (porter+trigram) + dense + RRF
    ├── crux-l7-sandbox/      # subprocess executor + Linux rlimits + landlock + seccomp
    ├── crux-l8-memory/       # observation CRUD + decay engine + FTS5 recall
    ├── crux-l9-coach/        # quality score + loop detect + CLAUDE.md drift
    ├── crux-l10-setup/       # crux init scaffolding + profile templates
    ├── crux-mcp/             # MCP stdio JSON-RPC server (11 tools)
    └── crux-cli/             # `crux` binary
```

**Why workspace not single crate:** independent testing, optional layer compilation, clear ownership boundaries.

**Why all in one repo:** shared types, atomic refactors, one CHANGELOG.

---

## 5. Database Schema (Master)

ONE SQLite database, but logically grouped:

### 5.1 Universal columns (all tables)

```sql
-- Every table has these (or compatible)
project_root TEXT NOT NULL,
created_at_epoch INTEGER NOT NULL,
updated_at_epoch INTEGER NOT NULL
```

### 5.2 Layer 4: Read cache

```sql
CREATE TABLE read_cache (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  agent_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  project_root TEXT NOT NULL,
  file_path TEXT NOT NULL,
  mtime_epoch REAL NOT NULL,
  offset INTEGER DEFAULT 0,
  limit_lines INTEGER DEFAULT 0,
  tokens_est INTEGER NOT NULL,
  read_count INTEGER DEFAULT 1,
  digest TEXT,                    -- structural digest (lazy)
  delta_content BLOB,             -- compressed (zstd) for delta cache
  delta_size INTEGER DEFAULT 0,
  last_access_epoch REAL NOT NULL,
  UNIQUE(agent_id, session_id, project_root, file_path)
);

CREATE INDEX idx_read_cache_lookup ON read_cache(agent_id, session_id, project_root, file_path);
CREATE INDEX idx_read_cache_lru ON read_cache(last_access_epoch);
```

### 5.3 Layer 5: AST graph

```sql
CREATE TABLE ast_nodes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT NOT NULL,
  kind TEXT NOT NULL,             -- File/Class/Function/Method/Type/Test/Module/Constant
  name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  file_path TEXT NOT NULL,
  line_start INTEGER, line_end INTEGER,
  language TEXT,
  parent_qn TEXT,
  signature TEXT,                 -- params + return type, single line
  modifiers TEXT,                 -- JSON: {static, async, exported, ...}
  is_test INTEGER DEFAULT 0,
  file_hash TEXT NOT NULL,
  doc_summary TEXT,               -- first line of docstring
  extra TEXT DEFAULT '{}',
  updated_at_epoch REAL NOT NULL,
  UNIQUE(project_root, qualified_name)
);

CREATE INDEX idx_nodes_file ON ast_nodes(project_root, file_path);
CREATE INDEX idx_nodes_kind ON ast_nodes(project_root, kind);
CREATE INDEX idx_nodes_name ON ast_nodes(project_root, name);

CREATE TABLE ast_edges (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT NOT NULL,
  kind TEXT NOT NULL,             -- CALLS/IMPORTS_FROM/INHERITS/IMPLEMENTS/CONTAINS/TESTED_BY/DEPENDS_ON/REFERENCES
  source_qn TEXT NOT NULL,
  target_qn TEXT NOT NULL,
  file_path TEXT NOT NULL,
  line INTEGER DEFAULT 0,
  confidence REAL DEFAULT 1.0,
  confidence_tier TEXT DEFAULT 'EXTRACTED',  -- EXTRACTED/RESOLVED/INFERRED
  extra TEXT DEFAULT '{}',
  updated_at_epoch REAL NOT NULL
);

CREATE INDEX idx_edges_source ON ast_edges(project_root, source_qn);
CREATE INDEX idx_edges_target ON ast_edges(project_root, target_qn);
CREATE INDEX idx_edges_target_kind ON ast_edges(project_root, target_qn, kind);
CREATE INDEX idx_edges_source_kind ON ast_edges(project_root, source_qn, kind);
```

### 5.4 Layer 6: Search

```sql
CREATE TABLE chunks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT NOT NULL,
  source_id INTEGER,              -- FK to ast_nodes.id (if code chunk)
  file_path TEXT NOT NULL,
  language TEXT,
  content_type TEXT,              -- code | prose | symbol
  title TEXT,
  content TEXT NOT NULL,
  line_start INTEGER, line_end INTEGER,
  tokens_est INTEGER,
  content_hash TEXT NOT NULL,
  created_at_epoch INTEGER NOT NULL
);

CREATE INDEX idx_chunks_file ON chunks(project_root, file_path);
CREATE INDEX idx_chunks_source ON chunks(source_id);

-- Two parallel FTS5 tokenizers for RRF
CREATE VIRTUAL TABLE chunks_fts_porter USING fts5(
  content, title, content='chunks', content_rowid='id',
  tokenize='porter unicode61'
);
CREATE VIRTUAL TABLE chunks_fts_trigram USING fts5(
  content, title, content='chunks', content_rowid='id',
  tokenize='trigram'
);

-- Vector embeddings (sqlite-vec)
CREATE VIRTUAL TABLE chunks_vec USING vec0(
  embedding float[384]            -- fastembed-rs default dim
);

-- FTS sync triggers (insert/update/delete)
-- Standard pattern, omitted here for brevity
```

### 5.5 Layer 8: Memory (port from token-savior, simplified)

```sql
CREATE TABLE sessions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT NOT NULL,
  agent_id TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','completed','failed')),
  summary TEXT,
  symbols_changed TEXT,           -- JSON array
  files_changed TEXT,             -- JSON array
  events_count INTEGER DEFAULT 0,
  created_at_epoch INTEGER NOT NULL,
  completed_at_epoch INTEGER
);

CREATE TABLE observations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
  project_root TEXT NOT NULL,
  agent_id TEXT,                  -- inter-agent memory bus
  type TEXT NOT NULL,             -- user/feedback/project/reference/guardrail/error_pattern/decision/convention
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  why TEXT,
  how_to_apply TEXT,
  symbol TEXT,                    -- linked AST node qualified_name
  file_path TEXT,
  tags TEXT,                      -- JSON array
  importance INTEGER DEFAULT 5 CHECK (importance BETWEEN 1 AND 10),
  relevance_score REAL DEFAULT 1.0,
  access_count INTEGER DEFAULT 0,
  content_hash TEXT NOT NULL,
  archived INTEGER DEFAULT 0,
  private INTEGER DEFAULT 0,
  narrative TEXT,
  facts TEXT,                     -- JSON array
  concepts TEXT,                  -- JSON array
  last_accessed_epoch INTEGER,
  created_at_epoch INTEGER NOT NULL,
  updated_at_epoch INTEGER NOT NULL
);

CREATE INDEX idx_obs_project ON observations(project_root);
CREATE INDEX idx_obs_type ON observations(type);
CREATE INDEX idx_obs_symbol ON observations(symbol);
CREATE INDEX idx_obs_hash ON observations(content_hash, project_root);
CREATE INDEX idx_obs_archived ON observations(archived);
CREATE INDEX idx_obs_agent ON observations(agent_id) WHERE agent_id IS NOT NULL;

CREATE VIRTUAL TABLE observations_fts USING fts5(
  title, content, why, how_to_apply, tags, narrative, facts, concepts,
  content='observations', content_rowid='id'
);

CREATE TABLE observation_links (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_id INTEGER REFERENCES observations(id) ON DELETE CASCADE,
  target_id INTEGER REFERENCES observations(id) ON DELETE CASCADE,
  link_type TEXT CHECK (link_type IN ('related','contradicts','supersedes','consolidation')),
  auto_detected INTEGER DEFAULT 0,
  created_at_epoch INTEGER NOT NULL,
  UNIQUE(source_id, target_id, link_type)
);

CREATE TABLE decay_config (
  type TEXT PRIMARY KEY,
  decay_rate REAL DEFAULT 1.0,
  min_score REAL DEFAULT 0.1,
  boost_on_access REAL DEFAULT 0.1
);

INSERT OR IGNORE INTO decay_config VALUES ('guardrail', 1.0, 1.0, 0.0);
INSERT OR IGNORE INTO decay_config VALUES ('user', 1.0, 0.8, 0.0);
INSERT OR IGNORE INTO decay_config VALUES ('convention', 1.0, 0.8, 0.0);
INSERT OR IGNORE INTO decay_config VALUES ('feedback', 0.999, 0.5, 0.1);
INSERT OR IGNORE INTO decay_config VALUES ('decision', 0.998, 0.3, 0.1);
INSERT OR IGNORE INTO decay_config VALUES ('error_pattern', 0.997, 0.2, 0.15);
INSERT OR IGNORE INTO decay_config VALUES ('reference', 0.995, 0.2, 0.1);
INSERT OR IGNORE INTO decay_config VALUES ('project', 0.99, 0.1, 0.2);

CREATE TABLE reasoning_chains (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT NOT NULL,
  goal TEXT NOT NULL, goal_hash TEXT NOT NULL,
  steps TEXT NOT NULL,            -- JSON array of {tool, args, observation}
  conclusion TEXT NOT NULL,
  confidence REAL DEFAULT 0.8,
  evidence_hash TEXT,
  access_count INTEGER DEFAULT 0,
  created_at_epoch INTEGER NOT NULL,
  expires_at_epoch INTEGER
);

CREATE TABLE tool_captures (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER REFERENCES sessions(id),
  project_root TEXT,
  tool_name TEXT NOT NULL,
  args_hash TEXT, args_summary TEXT,
  output_full TEXT NOT NULL,      -- DB only
  output_preview TEXT,             -- exposed to agent
  output_bytes INTEGER NOT NULL,
  output_lines INTEGER DEFAULT 0,
  created_at_epoch INTEGER NOT NULL,
  meta_json TEXT
);

CREATE VIRTUAL TABLE tool_captures_fts USING fts5(
  output_full, args_summary, tool_name,
  content='tool_captures', content_rowid='id'
);

CREATE TABLE session_summaries (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER REFERENCES sessions(id) ON DELETE CASCADE,
  project_root TEXT NOT NULL,
  request TEXT, investigated TEXT, learned TEXT,
  completed TEXT, next_steps TEXT, notes TEXT,
  created_at_epoch INTEGER NOT NULL
);

-- Cross-session chunk dedup (DCP)
CREATE TABLE chunk_registry (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  fingerprint TEXT UNIQUE NOT NULL,
  content_preview TEXT NOT NULL,
  seen_count INTEGER DEFAULT 1,
  last_seen_epoch INTEGER NOT NULL
);
```

### 5.6 Layer 9: Telemetry & Coach

```sql
CREATE TABLE telemetry (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT,
  layer TEXT NOT NULL,            -- l1..l10
  feature TEXT NOT NULL,          -- e.g. "delta_read", "bash_filter:git-status"
  agent_id TEXT, session_id TEXT,
  command_pattern TEXT,
  original_tokens INTEGER NOT NULL,
  compressed_tokens INTEGER NOT NULL,
  savings INTEGER NOT NULL,
  exec_time_ms INTEGER,
  quality_preserved INTEGER DEFAULT 1,
  detail TEXT,
  created_at_epoch INTEGER NOT NULL
);

CREATE INDEX idx_tel_layer ON telemetry(layer, created_at_epoch DESC);
CREATE INDEX idx_tel_project ON telemetry(project_root, created_at_epoch DESC);

CREATE TABLE quality_scores (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_root TEXT NOT NULL,
  session_id TEXT,
  score INTEGER NOT NULL,
  grade TEXT NOT NULL,
  patterns_good TEXT,             -- JSON
  patterns_bad TEXT,              -- JSON
  snapshot TEXT,                  -- JSON
  created_at_epoch INTEGER NOT NULL
);

-- Loop detection state (per session)
CREATE TABLE loop_detection_state (
  session_id TEXT PRIMARY KEY,
  last_user_msgs TEXT,            -- JSON array (last 4)
  last_tool_results TEXT,         -- JSON array (last 5)
  notes_emitted INTEGER DEFAULT 0,
  updated_at_epoch INTEGER NOT NULL
);
```

### 5.7 Universal: feature flags + state

```sql
CREATE TABLE feature_flags (
  id TEXT PRIMARY KEY,
  enabled INTEGER NOT NULL DEFAULT 0,
  risk TEXT,                      -- low/medium/high
  status TEXT,                    -- shipped/beta/deferred
  config_json TEXT,               -- per-feature config
  updated_at_epoch INTEGER NOT NULL
);

CREATE TABLE state (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at_epoch INTEGER NOT NULL
);
```

---

## 6. TOML Configuration

### 6.1 Global config: `~/.crux/config.toml`

```toml
[general]
db_path = "~/.crux/db/crux.sqlite"
log_path = "~/.crux/logs/crux.log"
log_level = "info"  # error/warn/info/debug/trace

[layers]
# All default ON unless risk=high
l1_output = true
l2_mcp_shrink = true
l3_bash_filter = true
l4_read_cache = true
l5_ast_graph = true
l6_hybrid_search = true
l7_sandbox = false  # opt-in (security)
l8_memory = true
l9_coach = true
l10_setup = true

[modes]
# warn / block / shadow per layer
l4_read_cache = "warn"
l3_bash_filter = "warn"

[layer.l1]
profile = "coding"  # coding/analysis/agents/general
intensity = "full"  # lite/full/ultra/wenyan-lite/wenyan-full/wenyan-ultra
auto_clarity = true

[layer.l4]
delta_max_bytes = 51200       # 50KB
delta_max_lines = 2000
cache_max_entries = 500
contextignore_max_patterns = 200

[layer.l5]
languages = ["rust", "python", "javascript", "typescript", "go", "java"]
bfs_engine = "sql"            # sql / in-memory
max_impact_nodes = 500
max_impact_depth = 2
daemon_enabled = true

[layer.l6]
embedding_provider = "fastembed"  # hash / fastembed
embedding_model = "BGE-small-en-v1.5"
embedding_dim = 384
vector_store = "sqlite-vec"   # sqlite-vec / milvus
similarity_threshold = 0.7
top_k = 10
rrf_k = 60

[layer.l7]
allowed_runtimes = ["lua", "javascript", "python", "bash"]
default_runtime = "lua"
timeout_secs = 30
memory_limit_mb = 256
network_allowed = false       # default deny

[layer.l8]
auto_extract = true
decay_check_interval_hours = 24
contradiction_check = true

[layer.l9]
score_target = 80
nudge_threshold_drop = 15
nudge_cooldown_minutes = 5
nudge_max_per_session = 3

[telemetry]
enabled = true
retention_days = 90

[mcp]
listen_addr = "stdio"  # stdio / "127.0.0.1:9787"
```

### 6.2 Project config: `<project>/.crux/config.toml`

Overrides global:

```toml
[layers]
l7_sandbox = true   # enable for this project

[layer.l5]
languages = ["rust"]  # only Rust for this project

[ignore]
patterns = [
  "node_modules/**", "target/**", "dist/**",
  "*.min.js", "*.bundle.js"
]
```

### 6.3 Filter config (Layer 3): `<project>/.crux/filters.toml`

Project filters override built-in. Requires explicit trust:

```toml
[filters.my-tool]
description = "Custom tool filter"
match_command = "^my-tool\\b"
strip_ansi = true
replace = [{pattern = "noise", replacement = ""}]
strip_lines_matching = ["^Using "]
truncate_lines_at = 120
head_lines = 20
max_lines = 40
on_empty = "my-tool: ok"
```

### 6.4 Profile config (Layer 1+10): `<project>/.crux/profile.toml`

```toml
[profile]
name = "coding"
version = "v1"
domain = "rust+typescript"

[profile.rules]
output = """
- Return code first. Explanation after, only if non-obvious.
- No inline prose. No boilerplate unless requested.
"""

code = """
- Simplest working solution. No over-engineering.
- No abstractions for single-use operations.
- Read the file before modifying it.
"""

review = """
- State the bug. Show the fix. Stop.
- No suggestions beyond review scope.
"""

debugging = """
- Never speculate without reading code first.
- One pass: state finding, location, fix.
"""
```

---

## 7. Per-Layer Implementation

### 7.1 Layer 1 — Output Compression

**Purpose:** instruct LLM to compress prose output via skill/profile.

**Mechanism:**
- Profile selection (`crux profile <name>`) writes `CLAUDE.md` w/ rules
- Rules drawn from drona+caveman best practices
- Multi-intensity selectable
- Auto-clarity for security/destructive

**Module: `crux-l1-output`**
```rust
pub fn install_skill(profile: &str, intensity: &str) -> Result<()> {
    let template = load_profile_template(profile)?;
    let rules = render_rules(&template, intensity)?;
    write_claude_md(&rules)?;
    Ok(())
}

pub fn export_skill(target: SkillTarget, out_dir: &Path) -> Result<()> {
    // SkillTarget: ClaudeCode, Cursor, Codex, OpenClaw, Cline, Continue, Aider, ...
    // Write platform-specific format
}
```

**No runtime cost** — pure setup-time. CRUX runs `compose_claude_md()` once.

### 7.2 Layer 2 — MCP Description Shrinker

**Purpose:** compress `description` fields of upstream MCP server's `tools/list`, `prompts/list`, `resources/list`.

**Module: `crux-l2-mcp-shrink`**
```rust
pub struct McpShrinkProxy {
    upstream_cmd: Vec<String>,
    fields: Vec<String>,    // default ["description"]
    debug: bool,
}

impl McpShrinkProxy {
    pub async fn run(&self) -> Result<()> {
        let mut child = Command::new(&self.upstream_cmd[0])
            .args(&self.upstream_cmd[1..])
            .stdin(Stdio::piped()).stdout(Stdio::piped())
            .spawn()?;
        let upstream_stdout = child.stdout.take().unwrap();
        let upstream_stdin = child.stdin.take().unwrap();
        // line-buffered JSON-RPC: stdin → upstream, upstream stdout → transform → stdout
        // transform: walk msg.result.tools[], compress each .description
        // ...
    }
}
```

**Compression algorithm:** rule-based deletion (not LLM call):
1. Strip filler words (`the`, `a`, `that`, `which`, `please`, `note that`, etc.)
2. Replace verbose phrases (`in order to` → `to`, `make use of` → `use`)
3. Drop redundant qualifiers (`very`, `really`, `actually`, `basically`)
4. Preserve: code, paths, URLs, identifiers, technical terms

**Integration:** users wrap upstream MCP servers:
```json
"mcpServers": {
  "fs-shrunk": {
    "command": "crux", "args": ["mcp-shrink", "npx", "@modelcontextprotocol/server-filesystem", "/path"]
  }
}
```

### 7.3 Layer 3 — Bash Filter Engine

**Purpose:** compress bash command output. Port rtk's TOML DSL + 3-tier parsers to Rust.

**Module: `crux-l3-bash`**

```rust
// 8-stage pipeline (same as rtk)
pub struct FilterPipeline {
    pub strip_ansi: bool,
    pub replace: Vec<ReplaceRule>,
    pub match_output: Vec<MatchRule>,
    pub strip_lines_matching: Vec<String>,  // regex
    pub truncate_lines_at: Option<usize>,
    pub head_lines: Option<usize>,
    pub tail_lines: Option<usize>,
    pub max_lines: Option<usize>,
    pub on_empty: Option<String>,
}

impl FilterPipeline {
    pub fn apply(&self, input: &str) -> FilteredOutput {
        let mut s = input.to_string();
        if self.strip_ansi { s = strip_ansi(&s); }
        s = self.apply_replace(&s);
        if let Some(out) = self.apply_match_output(&s) { return out.into(); }
        s = self.apply_strip_lines(&s);
        s = self.apply_truncate_lines(&s);
        s = self.apply_head_tail(&s);
        s = self.apply_max_lines(&s);
        if s.trim().is_empty() {
            if let Some(msg) = &self.on_empty { return msg.clone().into(); }
        }
        s.into()
    }
}

// 3-tier parser trait
pub trait Parser {
    fn try_full(&self, input: &str) -> Result<ParsedOutput>;
    fn try_degraded(&self, input: &str) -> Result<ParsedOutput>;
    fn passthrough(&self, input: &str) -> ParsedOutput;
}

pub struct GitStatusParser;
impl Parser for GitStatusParser {
    fn try_full(&self, input: &str) -> Result<ParsedOutput> {
        // Try `git status --porcelain=v2 -z` parsing
    }
    fn try_degraded(&self, input: &str) -> Result<ParsedOutput> {
        // Fallback regex parsing
    }
    fn passthrough(&self, input: &str) -> ParsedOutput {
        truncate_with_marker(input, "[CRUX:PASSTHROUGH]")
    }
}
```

**Filter loading priority:**
1. `<project>/.crux/filters.toml` (requires `crux trust`)
2. `~/.config/crux/filters.toml`
3. Built-in (compiled via `build.rs` into binary)

**60+ built-in filters:** ported from rtk verbatim.

### 7.4 Layer 4 — Read Cache + Delta + Structural Digest

**Purpose:** dedupe re-reads of same file; serve diff if changed.

**Module: `crux-l4-readcache`**

```rust
pub struct ReadCacheManager { db: Database, config: ReadCacheConfig }

pub enum CacheDecision {
    Allow,                                  // first read or different range
    Redundant { digest: String },           // same content, return digest
    Delta { summary: String, body: String }, // file changed, serve diff
    Block { reason: String },               // .contextignore
}

impl ReadCacheManager {
    pub fn check_read(&self, agent: &str, session: &str,
                      path: &Path, offset: usize, limit: usize) -> CacheDecision {
        // 1. .contextignore check → Block
        // 2. Lookup cache by (agent, session, path)
        // 3. If miss → cache mtime + size + tokens_est, return Allow
        // 4. If hit + same mtime + same range → Redundant
        // 5. If hit + different mtime + same range + delta_eligible → Delta
        // 6. Else → Allow + update cache
    }
    
    pub fn invalidate(&self, agent: &str, session: &str, path: &Path) {
        // Called on Edit/Write
    }
}

// Delta engine (LCS line diff, ported from alex)
pub fn compute_delta(old: &str, new: &str) -> DeltaResult {
    if old == new { return DeltaResult::no_change(); }
    if old.len() > MAX_LCS_BYTES || new.len() > MAX_LCS_BYTES {
        return DeltaResult::fallback(new);
    }
    // similar::TextDiff → emit '+'/'-' lines
}

// Structural digest
pub fn structural_digest(path: &Path, content: &str) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("py") => digest_python(content),
        Some("js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs") => digest_javascript(content),
        Some("rs") => digest_rust(content),  // tree-sitter (fast)
        _ => digest_fallback(content),
    }
}
```

**Modes** (per config):
- `warn`: log, allow read
- `block`: return digest instead of file content
- `shadow`: disabled but log telemetry

**Cache backend:** SQLite (not in-memory) for cross-process consistency. zstd-compress delta cache to save RAM.

### 7.5 Layer 5 — AST Graph

**Purpose:** structural code knowledge replaces "read whole file".

**Module: `crux-l5-ast`**

```rust
pub struct AstGraph { db: Database, config: AstConfig }

impl AstGraph {
    pub fn index_file(&mut self, path: &Path) -> Result<IndexStats> {
        let content = fs::read_to_string(path)?;
        let lang = detect_language(path)?;
        let tree = tree_sitter_parse(lang, &content)?;
        let nodes = extract_nodes(&tree, &content, path);
        let edges = extract_edges(&tree, &content, path);
        self.db.upsert_nodes(&nodes)?;
        self.db.upsert_edges(&edges)?;
        Ok(IndexStats { nodes: nodes.len(), edges: edges.len() })
    }
    
    pub fn find_symbol(&self, name: &str, project: &str) -> Vec<NodeRef> {
        // Search by name + qualified_name fuzzy
    }
    
    pub fn get_symbol_source(&self, qn: &str) -> Result<String> {
        let node = self.db.find_node(qn)?;
        let content = fs::read_to_string(&node.file_path)?;
        let lines: Vec<_> = content.lines().collect();
        Ok(lines[node.line_start-1..=node.line_end-1].join("\n"))
    }
    
    pub fn impact_radius(&self, qn: &str, depth: usize) -> Vec<NodeRef> {
        // BFS using SQL recursive CTE OR networkx-style in-memory
        // Returns nodes affected by changes to qn
    }
    
    pub fn callers_of(&self, qn: &str) -> Vec<NodeRef> {
        // SELECT * FROM ast_edges WHERE target_qn = ? AND kind = 'CALLS'
    }
    
    pub fn callees_of(&self, qn: &str) -> Vec<NodeRef> {
        // SELECT * FROM ast_edges WHERE source_qn = ? AND kind = 'CALLS'
    }
    
    pub fn tests_for(&self, qn: &str) -> Vec<NodeRef> {
        // SELECT n.* FROM ast_edges e JOIN ast_nodes n ON e.source_qn = n.qualified_name
        // WHERE e.target_qn = ? AND e.kind = 'TESTED_BY'
    }
}

// Confidence tiers
pub enum ConfidenceTier {
    Extracted,  // directly from AST
    Resolved,   // via name resolution / type inference
    Inferred,   // heuristic
}
```

**Languages (v1):** Rust, Python, JavaScript, TypeScript, Go, Java. Add via tree-sitter crates.

**Incremental indexing (Merkle):** `crux index` short-circuits via `MerkleSync` (`SCOPE_AST`); `--force` wipes graph + snapshot. No-op index ~180 ms on 80-file repo.

**L5 depth stages (shipped):**

| Stage | What it adds |
|---|---|
| L5.5 | Per-file resolver + project-wide `resolve_cross_file_calls` (leaf → FQN promotion). |
| L5.6 | `RustScope`: `self.x()` / `Self::x()` / `param.x()` rewrite + `Head::method` promotion. |
| L5.7 | `let` binding inference: `Foo::new(..)` / `Foo { .. }` / type-annotation; block-scope isolation. |
| L5.8 | `FileTypes` pre-pass (`fn_returns`, `method_returns`); `if let` / `while let` scoping. |
| L5.9 | Pattern bindings: `Some/Ok/Err` enum unwrap, tuple / struct destructure (literal RHS), match-arm isolation. |
| L5.10 | User-enum tuple variants (`enum_variants`) + tuple-typed fn / method returns. |
| L5.12 | `ProjectFileTypes` cross-file aggregate with per-map ambiguity sets. |
| L5.12.5 | DB-persisted `FileTypes` via `ast_file_signatures` + bincode; recovers no-op index speed. |
| L5.13a | User-enum struct variants (`enum_struct_variants`) — `if let P::Ping { header } = make()` resolves. |
| L5.13b | Tuple-typed locals via `RustScope::locals_tuple` — `let x = pair(); let (a, b) = x;` resolves. |
| L5.13c | Or-pattern alt merge — `Ok(x) \| Err(x)` binds `x` only if present in ALL alternatives (intersection). |
| L5.13d | Nested tuple destructure — `let ((a, b), c) = pair_pair()` resolves via recursive `destructure_tuple_elements`. |

**Signature cache:** `ast_file_signatures` table stores bincode-serialized `FileTypes` per file, keyed by content hash. No-op index reads cached signatures instead of re-parsing.

**Daemon:** `crux daemon` watches files via `notify`, debounces 200ms, re-indexes changed files. Updates within <2s.

### 7.6 Layer 6 — Hybrid Search

**Purpose:** semantic + keyword search across code + memory + chunks.

**Module: `crux-l6-search`**

```rust
pub struct SearchEngine {
    db: Database,
    embedder: Box<dyn Embedder>,    // hash / fastembed
    config: SearchConfig,
}

pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn dim(&self) -> usize;
}

impl SearchEngine {
    pub fn hybrid_search(&self, query: &str, limit: usize) -> Result<Vec<HybridResult>> {
        // 1. Parallel:
        //    a) BM25 porter
        //    b) BM25 trigram
        //    c) Dense vector (embed query, sqlite-vec search)
        // 2. RRF merge: score(d) = sum(1 / (k + rank(d))) where k=60
        // 3. Proximity rerank for multi-term
        // 4. Smart snippet extraction
        // 5. Top-K
    }
    
    pub fn fuzzy_correct(&self, query: &str) -> String {
        // Levenshtein distance vs known terms
    }
    
    pub fn search_unified(&self, query: &str, sources: &[Source]) -> Result<Vec<UnifiedResult>> {
        // Source: CodeChunks, Observations, ToolCaptures, AstNodes, AutoMemory
    }
}

// Default embedder: hash (fast, deterministic, no external deps)
pub struct HashEmbedder { dim: usize }
impl Embedder for HashEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        // SHA-256 based, normalized to unit vector
    }
    fn dim(&self) -> usize { 384 }
}

// Optional: FastEmbedder (ONNX, local, behind feature flag)
#[cfg(feature = "fastembed")]
pub struct FastEmbedder { model: TextEmbedding }
#[cfg(feature = "fastembed")]
impl Embedder for FastEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.model.embed(vec![text], None)?[0].clone())
    }
    fn dim(&self) -> usize { 384 }  // BGE-small-en-v1.5
}

// No cloud embedder: retrieval stays local-first.
```

**Shipped features:**
- **Merkle-incremental indexing:** `crux reindex` no-op ~50 ms on 80-file repo. `--force` wipes chunk store + snapshot.
- **Auto-memory scanner:** Reads `CLAUDE.md`, `MEMORY.md`, `.crux/memory/*` on reindex, indexes as `ContentType::Memory`.
- **Proximity rerank:** Smallest-window boost for multi-token queries — tighter span = higher score.
- **Fuzzy correction:** Levenshtein-1 fallback when every ranker missed (e.g., `"merkl snc"` → `"merkle sync"`).
- **FTS5 dual tokenizers:** `chunks_fts_porter` (stemming) + `chunks_fts_trigram` (substring) for RRF.
- **Content-type filtering:** `--kind code|prose|memory` to restrict search scope.

**Merkle sync:**
```rust
pub struct MerkleSync { snapshot_dir: PathBuf }
impl MerkleSync {
    pub fn snapshot_path(&self, project: &Path) -> PathBuf {
        // ~/.crux/merkle/<sha256(absolute_path)>.json
    }
    
    pub fn sync(&self, project: &Path) -> SyncResult {
        // 1. Walk project, hash each file
        // 2. Build Merkle tree
        // 3. Compare root w/ snapshot
        // 4. If diff: classify Added/Modified/Removed
        // 5. Trigger Layer 5 (re-index AST) and Layer 6 (re-embed chunks) for changed
    }
}
```

**Embedder default:** `HashEmbedder` remains the default (fast, deterministic, zero deps). `FastEmbedder` (ONNX via fastembed-rs) opt-in via `cargo build --features crux-l6-search/fastembed` + `[layer.l6] embedding_provider = "fastembed"`. Cloud HTTP backends off the table.

### 7.7 Layer 7 — Sandbox Executor

**Purpose:** "think in code" — LLM writes script, executes, returns ONLY result.

**Module: `crux-l7-sandbox`**

**Actual implementation (shipped):**

```rust
/// Which interpreter to spawn for a given snippet.
pub enum RuntimeKind { Python, Bash, Node }

/// How aggressively to isolate the child process.
pub enum IsolationLevel {
    Portable,  // (default) time + output volume + env scrubbing + cwd anchoring
    Hard,      // rlimits + optional landlock + optional seccomp (Linux only)
}

/// Caller-supplied execution request.
pub struct ExecRequest {
    pub runtime: RuntimeKind,
    pub code: String,
    pub project_root: Option<PathBuf>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub env: HashMap<String, String>,
    pub inherit_env: bool,
    pub isolation: IsolationLevel,
}

/// Caps applied under IsolationLevel::Hard.
pub struct HardLimits {
    pub address_space_bytes: u64,  // RLIMIT_AS, default 512 MiB
    pub cpu_seconds: u64,          // RLIMIT_CPU, derived from timeout
    pub open_files: u64,           // RLIMIT_NOFILE, default 64
    pub processes: u64,            // RLIMIT_NPROC, default 32
    pub file_size_bytes: u64,      // RLIMIT_FSIZE, default 64 MiB
}

/// Result of a sandboxed run.
pub struct ExecResult {
    pub runtime: RuntimeKind,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub elapsed_ms: u128,
    pub isolation_applied: Vec<String>,  // ["rlimits", "landlock", "seccomp"]
}

/// One-shot subprocess runner.
pub struct Executor;
impl Executor {
    pub fn execute(&self, req: &ExecRequest) -> Result<ExecResult> { ... }
}
```

**Isolation levels:**

| Primitive | Portable | Hard |
|-----------|----------|------|
| Timeout (wall-clock kill) | ✓ | ✓ |
| Output volume cap | ✓ | ✓ |
| Environment scrubbing | ✓ | ✓ |
| Working dir anchoring | ✓ | ✓ |
| `setrlimit` (AS/CPU/NOFILE/NPROC/FSIZE) | — | ✓ |
| landlock filesystem confinement | — | ✓ (optional feature) |
| seccomp syscall filtering | — | ✓ (optional feature) |

**Cargo features:**
```toml
[features]
landlock = ["dep:landlock"]  # filesystem confinement, kernel >= 5.13
seccomp = []                 # syscall filtering, uses libc BPF
```

**Seccomp implementation (shipped):**
- Per-runtime syscall allowlists (Python / Bash / Node)
- BPF filter installed via `prctl(PR_SET_NO_NEW_PRIVS)` + `seccomp(SECCOMP_SET_MODE_FILTER)`
- Blocked syscalls: `ptrace`, `mount`, `kexec_load`, `reboot`, `init_module`, etc.
- Behavior: `SIGSYS` trap on violation (softer, debuggable)
- Degrades gracefully: if kernel doesn't support seccomp, logs warning and continues

**CLI surface:**
```bash
crux execute --runtime python -c 'print(2+2)'
crux execute --runtime bash -c 'sleep 5' --timeout 1
crux execute --runtime bash -c 'ulimit -v' --isolate hard
crux execute --runtime python -c 'import os; os.getcwd()' --isolate hard --features seccomp
```

**Tool capture integration:**
- Output >`output_full_threshold` → store full in DB, return preview
- Preview: first N lines + last N lines + `[...truncated, see capture #N]`
- `crux capture get <id>` to retrieve full

**Token capture pattern (from token-savior):**
```rust
pub fn capture_tool_output(tool: &str, args: Value, output: &str) -> i64 {
    let bytes = output.len();
    let preview = if bytes > THRESHOLD { make_preview(output) } else { output.to_string() };
    let id = db.insert_capture(tool, &args, output, &preview, bytes)?;
    id
}
```

### 7.8 Layer 8 — Memory Engine

**Purpose:** persistent observations, decay-ranked, contradiction-aware.

**Module: `crux-l8-memory`**

```rust
pub struct MemoryEngine { db: Database, config: MemoryConfig }

pub enum ObservationType {
    User, Feedback, Project, Reference,
    Guardrail, ErrorPattern, Decision, Convention,
}

pub struct Observation {
    pub session_id: Option<i64>,
    pub project_root: String,
    pub agent_id: Option<String>,
    pub kind: ObservationType,
    pub title: String,
    pub content: String,
    pub why: Option<String>,
    pub how_to_apply: Option<String>,
    pub symbol: Option<String>,
    pub file_path: Option<String>,
    pub tags: Vec<String>,
    pub importance: u8,  // 1-10
    pub narrative: Option<String>,
    pub facts: Vec<String>,
    pub concepts: Vec<String>,
}

impl MemoryEngine {
    pub fn remember(&self, obs: Observation) -> Result<i64> {
        // 1. Compute content_hash
        // 2. Dedup check (same hash + project)
        // 3. Insert
        // 4. Auto-detect contradictions w/ existing obs of same type
        // 5. Auto-link related observations
        // 6. Compute initial relevance_score
    }
    
    pub fn recall(&self, query: RecallQuery) -> Result<Vec<RankedObservation>> {
        // 1. FTS5 search across observations_fts
        // 2. Apply decay (per-type rate × age)
        // 3. Boost on access_count
        // 4. Rank by score
        // 5. Return top-K
    }
    
    pub fn decay_pass(&self) -> Result<DecayStats> {
        // Run periodically (config.decay_check_interval_hours)
        // For each obs: new_score = relevance_score * decay_rate^days_since_access
        // Floor at min_score (per type)
        // Archive obs below archival_threshold
    }
    
    pub fn detect_contradictions(&self, new_obs: &Observation) -> Vec<Contradiction> {
        // Compare new vs existing obs of same type+symbol
        // Use simple textual + semantic similarity
    }
    
    pub fn distill(&self, project: &str) -> Result<DistillationStats> {
        // For old obs (>30 days): consolidate similar into single obs
        // Update via 'consolidation' link
    }
    
    pub fn auto_extract(&self, transcript: &str) -> Vec<ProposedObservation> {
        // Pattern matching: "remember:", "always:", "never:", "use:", "decision:", ...
        // Return proposals (don't auto-save)
    }
}

pub struct ReasoningChain {
    pub goal: String,
    pub steps: Vec<ReasoningStep>,  // {tool, args, observation}
    pub conclusion: String,
    pub confidence: f64,
}

impl MemoryEngine {
    pub fn save_chain(&self, chain: ReasoningChain) -> Result<i64> {
        // Compute goal_hash, evidence_hash
        // Insert into reasoning_chains
    }
    
    pub fn replay_chain(&self, goal: &str) -> Option<ReasoningChain> {
        // Search by goal_hash → if found, return for reuse
    }
}
```

**Memory search budget:** when `recall` returns >budget, summarize + bullet list.

### 7.9 Layer 9 — Coach / Quality / Audit

**Purpose:** real-time scoring + recommendations.

**Module: `crux-l9-coach`**

```rust
pub struct CoachEngine { db: Database, config: CoachConfig }

pub struct CoachData {
    pub health_score: i32,           // 0-100
    pub grade: char,                 // A-F
    pub patterns_good: Vec<Pattern>,
    pub patterns_bad: Vec<Pattern>,
    pub costly_prompts: Vec<CostlyPrompt>,
    pub agent_costs: Vec<AgentCost>,
    pub snapshot: Snapshot,
}

impl CoachEngine {
    pub fn compute_score(&self, project: &str, sessions: &[SessionStats]) -> CoachData {
        let mut score = 75;
        let mut patterns_good = vec![];
        let mut patterns_bad = vec![];
        
        // SOUL.md / CLAUDE.md analysis
        let claude_md = compute_claude_md_tokens(project);
        let ctx_window = dominant_context_window(sessions);
        let pct = (claude_md as f64 / ctx_window as f64) * 100.0;
        
        if pct < 2.0 {
            score += 5;
            patterns_good.push(Pattern::lean_claude_md(claude_md, pct));
        } else if pct > 3.0 {
            score -= 5;
            patterns_bad.push(Pattern::oversized_claude_md(claude_md, pct));
        }
        
        // Skill usage
        // MCP server count
        // Model routing (% Sonnet/Opus vs Haiku)
        // Cache hit rate (Layer 4)
        // Sandbox usage (Layer 7)
        // ... port full alex penalty/bonus matrix
        
        score = score.clamp(0, 100);
        let grade = score_to_grade(score);
        CoachData { health_score: score, grade, /* ... */ }
    }
    
    pub fn check_loop(&self, session: &str, user_msg: &str, tool_result: &str) -> Option<LoopWarning> {
        // Compare with last 4 user msgs and last 5 tool results
        // Cosine similarity ≥ 0.7 = loop
        // Cap at 2 warnings/session
    }
    
    pub fn check_quality_drop(&self, project: &str) -> Option<QualityNudge> {
        // Compare current score with last
        // If drop ≥ 15 OR cross below 60: emit nudge
        // Cooldown 5 min, max 3/session
    }
}
```

**Memory drift detection** (port from alex):
- Compare CLAUDE.md hash over time
- Detect rule additions w/o commits
- Detect rule contradictions

### 7.10 Layer 10 — Setup / Scaffolding

**Purpose:** initialize project for CRUX.

**Module: `crux-l10-setup`**

```rust
pub struct SetupEngine { config: SetupConfig }

pub fn init_project(opts: InitOptions) -> Result<()> {
    // 1. Detect project type (Cargo.toml, package.json, requirements.txt, ...)
    // 2. Prompt for: project_type, tech_stack, main_features, frameworks
    // 3. Create directory structure
    //    ├── CLAUDE.md (~450 tok)
    //    ├── .crux/
    //    │   ├── config.toml
    //    │   ├── COMMON_MISTAKES.md
    //    │   ├── QUICK_START.md
    //    │   ├── ARCHITECTURE_MAP.md
    //    │   └── completions/
    //    └── .claudeignore (or project ignore equivalent)
    // 4. Apply framework template (express/nextjs/django/...)
    // 5. Install profile (Layer 1)
    // 6. Index codebase (Layer 5+6)
    // 7. Print setup summary
}
```

**Framework templates (port + enhance from nadim):**
- `express.md`, `nextjs.md`, `vue.md`, `nuxtjs.md`, `angular.md`
- `django.md`, `rails.md`, `nestjs.md`, `laravel.md`
- **NEW:** `rust-cli.md`, `rust-actix.md`, `go-gin.md`, `python-fastapi.md`

---

## 8. MCP Server Tools

The `crux mcp` command runs the MCP server. Exposes these tools to agents:

### 8.1 Search & Read Tools (Layer 4-6)

```typescript
// crux_search — hybrid search
{
  name: "crux_search",
  desc: "Search code, memory, captures, auto-memory. Hybrid BM25+vector.",
  input: {
    query: string;
    limit?: number;          // default 10
    sources?: ("code"|"observations"|"captures"|"auto_memory")[];
    languages?: string[];
    sort?: "relevance"|"timeline";  // default "relevance"
  },
  output: {
    results: Array<{
      title: string; content: string; source: string;
      origin: string; score: number;
      file_path?: string; line_start?: number; line_end?: number;
      match_layer?: string;
    }>;
    total: number;
  }
}

// crux_read — cache-aware read
{
  name: "crux_read",
  desc: "Read file with cache + delta + structural digest.",
  input: { file_path: string; offset?: number; limit?: number; force?: boolean }
  output: {
    content?: string;        // if Allow
    digest?: string;         // if Redundant
    delta?: { summary, body };  // if Delta
    blocked?: string;        // if Block
  }
}
```

### 8.2 Code Graph Tools (Layer 5)

```typescript
// crux_find_symbol — by name
{
  name: "crux_find_symbol",
  desc: "Find symbols (class/function/etc.) by name or qualified name. Returns signatures, no full file.",
  input: { name: string; kind?: string; language?: string }
  output: {
    matches: Array<{ qualified_name, kind, file_path, line_start, line_end, signature, doc_summary }>
  }
}

// crux_get_symbol_source — get just the function
{
  name: "crux_get_symbol_source",
  desc: "Return source of a symbol (just the function/class lines, not whole file).",
  input: { qualified_name: string }
  output: { source: string; file_path: string; line_start, line_end }
}

// crux_query_graph — relationships
{
  name: "crux_query_graph",
  desc: "Query callers/callees/imports/tests/dependencies of a symbol.",
  input: {
    qualified_name: string;
    relationship: "callers_of"|"callees_of"|"imports_of"|"imported_by"|"tests_for"|"tested_by"|"inherits"|"impact_radius";
    depth?: number;          // for impact_radius
    limit?: number;
  }
  output: { results: Array<NodeRef>; truncated: boolean }
}

// crux_impact — blast radius
{
  name: "crux_impact",
  desc: "Get all symbols affected by changing this one (impact radius).",
  input: { qualified_name: string; depth?: number }
  output: { affected: Array<NodeRef>; flows: Array<FlowPath> }
}
```

### 8.3 Sandbox Tools (Layer 7)

```typescript
// crux_execute — think in code
{
  name: "crux_execute",
  desc: "Execute code in sandbox, return ONLY result. Avoids loading raw data into context.",
  input: {
    runtime: "lua"|"javascript"|"python"|"bash";
    code: string;
    timeout_ms?: number;
    background?: boolean;
  }
  output: { stdout, stderr, exit_code, timed_out, capture_id?: number }
}

// crux_capture_get — retrieve full output
{
  name: "crux_capture_get",
  desc: "Retrieve full output of a previous capture by ID.",
  input: { capture_id: number; range?: {offset, limit} }
  output: { output: string; preview_was: string }
}

// crux_capture_search — search across captures
{
  name: "crux_capture_search",
  desc: "FTS5 search across past tool captures.",
  input: { query: string; limit?: number }
  output: { results: Array<CaptureRef> }
}
```

### 8.4 Memory Tools (Layer 8)

```typescript
// crux_remember — save observation
{
  name: "crux_remember",
  desc: "Persist an observation (user/feedback/project/decision/etc.) for future sessions.",
  input: {
    type: "user"|"feedback"|"project"|"reference"|"guardrail"|"error_pattern"|"decision"|"convention";
    title: string; content: string;
    why?: string; how_to_apply?: string;
    symbol?: string; file_path?: string;
    tags?: string[]; importance?: number;
  }
  output: { id: number; contradictions: Array<Contradiction> }
}

// crux_recall — load observations
{
  name: "crux_recall",
  desc: "Recall observations matching query, decay-ranked, filtered by type/symbol.",
  input: {
    query: string;
    types?: string[];
    symbol?: string;
    limit?: number;
  }
  output: { observations: Array<RankedObservation> }
}

// crux_session_summary — session rollup
{
  name: "crux_session_summary",
  desc: "Generate or retrieve structured session summary (request/investigated/learned/completed/next_steps).",
  input: { session_id?: number }
  output: { summary: SessionSummary }
}

// crux_save_chain — save reasoning chain
{
  name: "crux_save_chain",
  desc: "Save a multi-step reasoning chain for replay on similar future goals.",
  input: { goal: string; steps: Array<Step>; conclusion: string; confidence?: number }
  output: { id: number }
}

// crux_replay_chain — find chain for goal
{
  name: "crux_replay_chain",
  desc: "Find a past reasoning chain matching the current goal (replay shortcut).",
  input: { goal: string }
  output: { chain?: ReasoningChain }
}
```

### 8.5 Coach & Audit Tools (Layer 9)

```typescript
// crux_audit — health check
{
  name: "crux_audit",
  desc: "Get current quality score, grade, recommendations.",
  input: {}
  output: { coach: CoachData; recent_telemetry: TelemetrySnapshot }
}

// crux_loop_check — loop detection
{
  name: "crux_loop_check",
  desc: "Check if current trajectory looks like a stuck loop (compares last user msgs and tool results).",
  input: { session_id: string; user_msg: string; tool_result: string }
  output: { is_loop: boolean; warning?: string }
}
```

---

## 9. CLI Interface

### 9.1 Top-level commands

```bash
crux init [--profile PROFILE] [--framework FRAMEWORK] [--non-interactive]
    # Set up project: writes CLAUDE.md, .crux/config.toml, indexes codebase

crux index [--force]
    # Index codebase: AST graph (Layer 5), embeddings (Layer 6)

crux profile <name>           # apply profile (coding/analysis/agents/...)
crux profile list             # show available profiles
crux profile diff             # diff current CLAUDE.md vs profile

crux config get/set <key>     # config CRUD
crux config validate          # validate config

crux daemon [--background]    # start file watcher daemon
crux daemon stop
crux daemon status

crux mcp                      # run MCP server (stdio)
crux mcp --tcp 127.0.0.1:9787

crux mcp-shrink <upstream-cmd> [args...]
    # Run as MCP middleware proxy

crux audit [--json]           # show health report
crux stats [--layer L]        # show telemetry stats

crux search <query> [--source S] [--limit N]
crux find <symbol> [--kind K]
crux symbol <qualified-name>  # get source
crux impact <qualified-name>  # blast radius

crux remember <type> <title>  # interactive prompt for observation
crux recall <query>           # search memory
crux session list/show/summary

crux execute <runtime> <code> # one-shot sandbox
crux capture list/get/search

crux trust [<path>]           # trust .crux/filters.toml in project
crux untrust [<path>]

crux export <target>          # export skill for ClaudeCode/Cursor/Codex/...

crux migrate [up|down]        # DB migrations
crux doctor                   # diagnose problems
crux purge                    # clear caches (with confirmation)
```

### 9.2 Hook commands (called by agents)

```bash
crux hook pre-tool             # PreToolUse handler (reads JSON from stdin)
crux hook post-tool            # PostToolUse
crux hook pre-prompt           # PrePrompt (Layer 1)
crux hook post-compact         # PostCompact (Layer 8 — preserve decisions)
```

---

## 10. Hook Integration Patterns

### 10.1 Claude Code

`~/.claude/settings.json`:
```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Bash|Read|Edit|Write",
      "command": "crux hook pre-tool"
    }],
    "PostToolUse": [{
      "matcher": "Edit|Write|MultiEdit",
      "command": "crux hook post-tool"
    }]
  },
  "mcpServers": {
    "crux": { "command": "crux", "args": ["mcp"] }
  }
}
```

### 10.2 Cursor / Continue / Cline

MCP server config:
```json
{
  "mcp": {
    "servers": {
      "crux": { "command": "crux", "args": ["mcp"] }
    }
  }
}
```

### 10.3 OpenClaw

Plugin format:
```typescript
// ~/.openclaw/plugins/crux/index.ts
export default {
  name: "crux",
  version: "1.0.0",
  events: {
    "agent:tool:before": (event) => spawn("crux", ["hook", "pre-tool"], { stdin: JSON.stringify(event) }),
    "agent:tool:after": (event) => spawn("crux", ["hook", "post-tool"], { stdin: JSON.stringify(event) }),
  }
};
```

### 10.4 Generic CLI shim

For tools without native hook support (e.g., `aider`):
```bash
# Replace `git status` with `crux bash git status`
alias git='crux bash git'
alias docker='crux bash docker'
# Or install shim binaries in PATH that forward to crux
```

---

## 11. Roadmap (Phased)

### Phase 0: Foundation (1-2 weeks) — ✓ SHIPPED
- [x] Workspace + `crux-core` (config, db, errors, paths, telemetry, tokens, merkle)
- [x] SQLite migrations + WAL setup (9 migrations)
- [x] CLI skeleton (`clap` derive)
- [x] TOML config loading
- [x] Logging (`tracing`)
- [x] Test harness (golden + property)

### Phase 1: Layers 4 + 10 (2-3 weeks) — ✓ SHIPPED
- [x] **L4 read cache** — mtime + range + LCS delta + structural digest + contextignore
- [x] **L10 setup** — `crux init` w/ framework detection + profile templates
- [x] CLAUDE.md scaffold writing
- [x] `.crux/config.toml` generation
- [x] Project detection (Cargo.toml/package.json/etc.)

### Phase 2: Layers 1 + 3 (2 weeks) — ✓ SHIPPED
- [x] **L1 output** — profile system, intensity, multi-platform export
- [x] **L3 bash** — TOML DSL parser, 8-stage pipeline, 5 builtin filters
- [x] 5 TOML filter files (git, cargo, npm, jest, generic)
- [x] 3-tier parsers for top tools (git status, cargo build, npm install, etc.)
- [x] Hook integration (`crux hook pre-tool/post-tool`)

### Phase 3: Layer 4 enhancements (2 weeks) — ✓ SHIPPED
- [x] Delta mode (LCS via `similar` crate)
- [x] Structural digest (Python AST, JS regex, Rust tree-sitter, fallback)
- [x] `.contextignore` w/ pre-compiled regex cache
- [x] Modes (warn/block/shadow)
- [x] Atomic state writes (tmp + rename, 0o600)

### Phase 4: Layer 5 (3 weeks) — ✓ SHIPPED
- [x] Tree-sitter integration (Rust + Python + TS/JS)
- [x] AST node + edge extraction
- [x] SQL recursive CTE for impact radius
- [x] `crux find`, `crux symbol`, `crux impact` CLI
- [x] Incremental index via MerkleSync (`SCOPE_AST`)
- [x] Signature cache (`ast_file_signatures` + bincode)
- [x] L5.5–L5.13d: cross-file inference, receiver typing, pattern bindings
- [ ] Add Go, Java, C/C++ (deferred)

### Phase 5: Layer 6 (3 weeks) — ✓ SHIPPED
- [x] BM25 FTS5 (porter + trigram)
- [x] RRF merge
- [x] HashEmbedder (default, fast, deterministic)
- [x] Hybrid search w/ proximity rerank + fuzzy correction
- [x] Auto-memory scanner (CLAUDE.md/MEMORY.md/.crux/memory/*)
- [x] Merkle-incremental reindex (no-op ~50 ms)
- [x] Content-type filtering (`--kind code|prose|memory`)
- [x] Optional: `FastEmbedder` (ONNX, local) — no cloud backends

### Phase 6: Layer 8 (3 weeks) — ✓ SHIPPED
- [x] Memory schema migrations (9 tables, FTS5, decay config)
- [x] Observation CRUD + decay engine
- [x] FTS5 search across observations
- [x] Contradiction detection (textual)
- [x] Auto-extract from transcripts
- [x] Session summaries
- [x] Reasoning chains
- [ ] Distillation engine (deferred)

### Phase 7: Layer 7 (2 weeks) — ✓ SHIPPED
- [x] Subprocess runtime (Python, Bash, Node)
- [x] IsolationLevel::Portable (default) — timeout, output cap, env scrub, cwd
- [x] IsolationLevel::Hard — rlimits (AS/CPU/NOFILE/NPROC/FSIZE)
- [x] Linux landlock filesystem confinement (optional feature)
- [x] Linux seccomp syscall filtering (optional feature, per-runtime allowlists)
- [x] CLI: `crux execute --runtime python|bash|node [--isolate portable|hard]`
- [ ] Lua runtime (`mlua`) — deferred
- [ ] JavaScript runtime (`rquickjs`) — deferred
- [ ] Network blocking — deferred
- [ ] Tool capture integration — deferred

### Phase 8: Layers 2 + 9 (2 weeks) — ✓ SHIPPED
- [x] **L2 MCP shrink** — stdio proxy, description compression
- [x] **L9 coach** — quality score, grade, patterns
- [x] Loop detection (cosine similarity)
- [x] Quality nudge (drop trigger)
- [x] Memory drift detection (CLAUDE.md hash diff)
- [x] `crux audit` + `crux coach`

### Phase 9: MCP server (2 weeks) — ✓ SHIPPED
- [x] `crux mcp` w/ custom JSON-RPC (stdio)
- [x] 11 tools: search, read, bash_filter, execute, remember, recall, audit, find_symbol, get_symbol_source, query_graph, impact
- [x] Cross-platform configs (Claude Code, Cursor, Cline, etc.)
- [ ] TCP transport — deferred
- [ ] Adapter generators (`crux export <target>`) — deferred

### Phase 10: Polish (2-3 weeks) — IN PROGRESS
- [ ] Documentation (mdBook)
- [ ] Benchmark suite (criterion)
- [ ] Multi-platform binaries (Linux/macOS/Windows, x86_64+aarch64)
- [ ] Homebrew tap, AUR, .deb, scoop
- [ ] CHANGELOG, SemVer
- [ ] Web dashboard (optional, separate crate)

**Status:** All 10 layers shipped. Phase 10 polish remaining.

**Test count:** 244 pass / 0 failed across 11 implementation crates.

---

## 12. Testing Strategy

### 12.1 Test categories

| Category | Coverage | Tool |
|----------|----------|------|
| Unit | Per-function logic | `#[cfg(test)]` mod |
| Property | Invariants (e.g., compress→decompress = identity for ASCII) | `proptest` |
| Golden | Filter outputs match recorded fixtures | Custom harness |
| Integration | Multi-layer flow | `crates/crux-test` |
| End-to-end | Full agent simulation | Shell scripts |
| Benchmarks | Token reduction targets | `criterion` |

### 12.2 Golden test pattern (per filter)

```toml
# crux-l3-bash/filters/git-status.toml
[filters.git-status]
match_command = "^git\\s+status\\b"
# ...

[[tests]]
name = "clean_repo"
input = "On branch main\nnothing to commit, working tree clean"
expected = "branch: main\nstatus: clean"

[[tests]]
name = "with_changes"
input = """
On branch main
Changes not staged for commit:
  modified:   src/lib.rs
"""
expected = """
branch: main
modified: src/lib.rs
"""
```

`build.rs` runs all `[[tests]]` at compile time, fails build on mismatch.

### 12.3 Layer-specific test focus

- **L3 (bash):** Golden tests w/ real CLI outputs (record once, replay forever)
- **L4 (read cache):** Property tests (cache invariant: same input → same digest)
- **L5 (AST):** Integration tests w/ small projects in 6 langs
- **L6 (search):** MRR (Mean Reciprocal Rank) on labeled queries
- **L7 (sandbox):** Security tests (try escape attempts)
- **L8 (memory):** Decay rate verification, contradiction detection accuracy
- **L9 (coach):** Score reproducibility on fixture sessions

### 12.4 Benchmark targets

| Metric | Target |
|--------|--------|
| `crux hook pre-tool` latency | < 10ms p99 |
| `crux mcp` cold start | < 100ms |
| `crux index` 1000-file repo | < 30s |
| `crux search` query | < 50ms p99 |
| Memory: idle daemon | < 50 MB RSS |
| Memory: indexed 10K-file repo | < 500 MB RSS |
| Binary size | < 30 MB stripped |

---

## 13. Performance Targets

### 13.1 Token reduction (vs baseline agent w/o CRUX)

| Scenario | Baseline | CRUX | Reduction |
|----------|----------|------|-----------|
| Read same file twice | 2× full | full + digest | ~98% on 2nd |
| Read changed file | full | delta | ~80-95% |
| Code review | full files × N | impact radius + signatures | 50-90% |
| Bash output (`git status`, `npm test`, etc.) | raw | filtered | 70-95% |
| MCP server desc fields | as-is | compressed | 40-60% |
| Session resume | full transcript | summary + obs | 80-95% |
| Symbol lookup | grep + read full | `crux_find_symbol` | 90-99% |
| Multi-file analysis | N reads | `crux_execute` script | 95-98% |

### 13.2 Quality preservation

CRUX must NEVER:
- Drop code blocks, URLs, paths, credentials
- Modify diff patches, commit messages, PR text
- Compress security-related warnings
- Truncate without explicit `[truncated]` marker
- Hide errors (always preserve `stderr`)

CRUX MAY:
- Drop verbose progress bars
- Compress repeated identical lines
- Summarize prose tool descriptions
- Replace numeric counts with summaries (`12 errors` instead of listing all)

---

## 14. Security Model

### 14.1 Layer-by-layer concerns

| Layer | Risk | Mitigation |
|-------|------|------------|
| L1 | LLM ignores rules | Reflective check (Coach detects) |
| L2 | Compress safety-critical text | Whitelist preserved patterns; opt-in |
| L3 | Lossy filter hides error | `tee` raw output; on_empty detection; fallback to passthrough |
| L4 | Read cache stale | mtime check + range key; invalidate on write |
| L5 | AST mis-parse | Confidence tier reflects this; fallback to text search |
| L6 | Embedding leakage to cloud | Default local; cloud opt-in w/ explicit config |
| L7 | Sandbox escape | landlock + seccomp + rlimits; no network default; timeout |
| L8 | Memory contradiction unsurfaced | Auto-detect contradictions; surface via `crux_recall` |
| L9 | Score gaming | Penalty for ignoring nudges (cooldown shrinks) |
| L10 | Init overrides existing config | Refuse if files exist; require `--force` |

### 14.2 Credentials & secrets

CRUX preserves verbatim (never compresses):
- AWS keys (`AKIA*`, `ASIA*`)
- GitHub tokens (`ghp_*`, `gho_*`, `ghu_*`, `ghs_*`, `ghr_*`)
- Slack tokens (`xoxb-*`, `xoxp-*`)
- Generic patterns: `[A-Z_]+_(KEY|SECRET|TOKEN)=`
- `Authorization: Bearer ...`
- `password=...`, `pwd=...`

Match in `regex` crate, applied before any compression step.

### 14.3 Network & filesystem

- **Daemon:** binds to `127.0.0.1` only (never 0.0.0.0)
- **DB:** mode 0o600 owner-only
- **Cache files:** mode 0o600
- **Sandbox:** no network by default, no fs writes outside project_root
- **MCP:** stdio default; TCP requires explicit config

### 14.4 Trust model

- Built-in filters: trusted (compiled into binary)
- User filters: trusted (`~/.config/crux`)
- Project filters: untrusted by default — require `crux trust` (writes hash to user trust DB)
- Re-trust on hash change

---

## 15. Differentiators Recap

CRUX vs the field:

1. **Single Rust binary** — like rtk, but covers all 10 layers
2. **Local-first by default** — like alex, but adds Layer 5-6 graph + search
3. **Sandbox + memory + coach combined** — no other repo combines all three
4. **TOML for everything declarative** — rtk's pattern, applied universally
5. **One SQLite DB** — simpler than multi-store designs (claude-context Milvus, ooples 3-layer)
6. **MCP + hook + library** — three integration paths, one codebase
7. **Multi-platform skill export** — caveman's pattern, automated
8. **Cross-layer telemetry** — only CRUX measures Layer 4 hit rate × Layer 9 score correlation
9. **Reasoning chain replay** — token-savior's pattern, broader use
10. **Decay-aware memory** — token-savior schema in Rust performance

---

## 16. Open Questions

1. **MCP framework choice:** `rmcp` (less mature) vs hand-rolled JSON-RPC vs Python `fastmcp` shim?
   - **Answered:** Custom JSON-RPC (shipped in `crux-mcp`).
2. **Embedding model size tradeoff:** BGE-small (384d, 100MB) vs BGE-base (768d, 400MB)?
   - **Answered:** `HashEmbedder` (384d) as default, `FastEmbedder` (BGE-small-en-v1.5) opt-in via feature flag.
3. **Tree-sitter language coverage:** start w/ 6 (Rust/Python/TS/JS/Go/Java) or expand to 12 (add Ruby/PHP/C/C++/Swift/Kotlin)?
   - **Answered:** Started with Rust/Python/TS/JS. Go/Java deferred.
4. **Sandbox isolation level:** subprocess-only (portable) vs landlock+seccomp (Linux-best)?
   - **Answered:** Both shipped. `IsolationLevel::Portable` (default) + `IsolationLevel::Hard` (rlimits + landlock + seccomp).
5. **Coach scoring:** keep alex's matrix verbatim or recalibrate based on CRUX-specific metrics?
   - **Answered:** Adapted from alex, CRUX-specific adjustments.
6. **Memory schema simplification:** keep token-savior's full 355 lines or trim to ~200 essential?
   - **Answered:** Simplified to ~100 lines (9 tables, FTS5, decay config).
7. **Distribution:** static musl binary (largest compatibility) vs dynamic glibc (smaller)?
   - **Open:** TBD in Phase 10.
8. **License:** MIT (permissive) vs Apache-2.0 (patent grant)?
   - **Open:** TBD.

---

## 17. Migration & Adoption

### From caveman
- `crux profile coding` writes CLAUDE.md compatible w/ caveman skill
- `crux mcp-shrink <cmd>` replaces `caveman-shrink <cmd>`

### From rtk
- `crux bash <cmd>` replaces `rtk <cmd>`
- TOML filter format is forward-compatible
- Run `crux migrate-from rtk` to import `~/.local/share/rtk/savings.db`

### From context-mode
- `crux mcp` exposes equivalent tools (`crux_search`, `crux_execute`, etc.)
- Session DB import: `crux migrate-from context-mode <db-path>`

### From alex token-optimizer
- `crux migrate-from openclaw` ports v5 features + telemetry SQLite
- Profile compatibility: `crux profile openclaw-coding`

### From token-savior
- `crux migrate-from token-savior <db-path>` ports observations
- All 8 obs types preserved + decay rates

### From claude-context
- `crux migrate-from claude-context <merkle-path>` ports merkle snapshots

### From nadim
- Existing `.claude/` structure detected by `crux init`
- Optional rename: `.claude/` → `.crux/` (keeps both during transition)

### From drona
- `crux profile drona-v8` ports rules verbatim

---

## 18. Summary

CRUX is the **synthesis** of 10 token-optimization repos into a single coherent Rust system. Every pattern proven elsewhere is preserved; every gap identified is filled by cross-layer integration. The result is:

- **More layers** than any single repo (10 vs 1-3 typical)
- **Faster** than Python/Node implementations (Rust + SQLite)
- **Local-first** (no API keys required, but pluggable for cloud)
- **Cross-platform** (Linux/macOS/Windows × x86_64/aarch64)
- **Integrated** (read cache talks to AST graph; sandbox writes to memory; coach scores all)
- **Measurable** (telemetry per-layer; correlations surfaced)
- **Reversible** (warn/block/shadow modes per layer)

**Token reduction target:** 60-90% on typical sessions, 95%+ on heavy-read workflows.

Next step: implement Phase 0 (foundation), then iterate per roadmap.
