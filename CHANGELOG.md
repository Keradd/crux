# Changelog

All notable changes to CRUX are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned

- mdBook documentation chapters.
- `cargo install crux` publishing (crates.io).
- Homebrew tap.
- Criterion benchmarks for L1 / L2 / L3 / L7 / L8 / L9 (currently L4 / L5 / L6 only).

## [0.2.0] — Agent integration setup

### Added

- **`crux setup [<agent>]`** — register CRUX as an MCP server (and
  hooks where supported) inside third-party AI agents in one
  command. Eight agents covered out of the box:
  - **Claude Code** — MCP entry + `PreToolUse(Read)` /
    `PostToolUse(Edit|Write|MultiEdit)` hooks routed through
    `crux hook pre-tool` / `post-tool` + `/crux` slash-command file
    at `~/.claude/commands/crux.md`.
  - **Claude Desktop** — `mcpServers.crux` in
    `claude_desktop_config.json` at the OS-canonical path
    (macOS: `~/Library/Application Support/Claude/`,
     Linux: `~/.config/Claude/`,
     Windows: `%APPDATA%\Claude\`).
  - **Cursor** — `~/.cursor/mcp.json` (or project-scoped
    `<root>/.cursor/mcp.json`).
  - **Windsurf (Cascade)** — `~/.codeium/windsurf/mcp_config.json`.
  - **Cline** (VS Code extension `saoudrizwan.claude-dev`) —
    `cline_mcp_settings.json` inside VS Code's `globalStorage`.
  - **Zed** — `context_servers.crux` in `~/.config/zed/settings.json`
    (Zed uses its own schema; CRUX writes the right shape).
  - **OpenClaw** (docs.openclaw.ai) — `mcp.servers.crux` in
    `~/.openclaw/openclaw.json` (honors `$OPENCLAW_CONFIG_PATH`
    as an override the way the Gateway does).
  - **Hermes Agent** (NousResearch) — `mcp_servers.crux` in
    `~/.hermes/config.yaml` written as native YAML so peer entries
    (filesystem, github, …) stay untouched.
- Auto-detect mode: `crux setup` (no agent argument) probes for every
  supported agent's known config directory and integrates each one
  found.
- Idempotent merge (JSON for seven agents, YAML for Hermes) —
  re-running `crux setup` is a no-op once the entries exist; updates
  only fire when the stored value differs.
- `--dry-run`, `--list`, `--scope global|project|auto`, `--crux-path`,
  `--no-hooks`, `--no-skill`, `--no-project-env`, `--env KEY=VAL`
  (repeatable), `--force`, plus `--json` machine output.
- Auto-sets `CRUX_PROJECT=<project_root>` inside each MCP entry's
  `env` block so agents that spawn MCP children from `$HOME`
  (Windsurf in particular) still target the correct project.
- **`crux init` bootstrap chain** — `--setup-agents` registers
  detected agents post-scaffold and `--index` runs a first-time
  L5 AST index + L6 hybrid-search reindex so MCP lookups return
  data immediately. Turns the five-step onboarding
  (`build → install → init → setup → index`) into a single
  `crux init --non-interactive --setup-agents --index` invocation.
  `--agents <AGENT>` (repeatable) restricts the chain to a named
  subset when auto-detect is too aggressive.
- **`scripts/install.sh`** — one-shot installer: verifies `cargo`,
  builds `--release`, installs to `~/.local/bin` (user scope, no
  sudo) or `/usr/local/bin` (`--system`, sudo when needed), and
  runs the init + setup + index bootstrap in the current directory.
  Flags: `--system`, `--no-bootstrap`, `--no-agents`, `--no-index`.
- `crux reindex --dir DIR` mirrors `crux index --dir` so the init
  bootstrap can pipe a freshly scaffolded project through both
  layers in one shot.

The `/crux` slash-command file teaches Claude Code which CRUX MCP
tool to reach for in common situations (symbol lookup, blast
radius, hybrid search, sandbox execution, persistent memory, …).

### Dependencies

- `serde_yaml 0.9` added to the workspace for native YAML merges
  (Hermes Agent config).

### Tests

- **332 pass / 0 fail** on the default feature set (+43 over the
  baseline of 289). Coverage includes JSON merge idempotency +
  claude-code hooks schema + Zed `context_servers` schema +
  OpenClaw `mcp.servers` schema + Hermes YAML merge + agent kind
  parse aliases + per-agent `integrate` E2E + dry-run + force +
  `--env CRUX_PROJECT` preservation.

## [0.1.1] — Patch release

### Fixed

- **L7 sandbox: Python runtime broken on Windows.** The default
  interpreter name was hard-coded to `python3`, but the official
  python.org Windows installer only ships `python.exe`, so calls to
  `python3` resolved to the Microsoft Store launcher stub at
  `WindowsApps\python3.exe` — a no-op binary that exits 0 with empty
  stdout. `RuntimeKind::default_interpreter()` is now Windows-aware
  (`python` on Windows, `python3` elsewhere), so `crux execute
  --runtime python` works on stock Windows installs.
- **Cross-libc rlimit type for the L7 sandbox.** The hard-isolation
  helper hard-coded `libc::__rlimit_resource_t` (glibc-only). Aliasing
  to `libc::c_int` on non-glibc Linux libcs lets the crate compile on
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, which
  the release workflow now builds.
- **Test helpers harden against silent stubs.** `require_python` /
  `require_bash` now probe by executing real code and matching the
  expected stdout, instead of `--version`, so a Microsoft Store
  launcher stub (or any stubbed shim) no longer falsely advertises a
  working interpreter.

### Added

- Multi-platform release workflow (`.github/workflows/release.yml`).
  Tag pushes (`v*`) build statically-linked binaries for Linux x86_64
  (`gnu` + `musl`), Linux aarch64 (`gnu` + `musl`), macOS x86_64 +
  aarch64, and Windows x86_64; each archive is published with a
  `.sha256` checksum to the GitHub Releases page.

## [0.1.0] — Initial release

First public release of CRUX. All ten layers ship with end-to-end coverage.

### Added

#### Layer 1 — Output compression
- Profile system (`coding`, `analysis`, `agents`) with `crux profile {list,show,apply,current}`.

#### Layer 2 — MCP description shrinker
- `crux mcp-shrink <upstream-cmd>` proxies stdio JSON-RPC and rewrites
  `tools/list` descriptions on the fly.

#### Layer 3 — Bash output filter
- TOML-DSL pipeline (8 stages: strip / dedupe / regex / summarise / cap / …).
- Built-in filters: `git`, `cargo`, `npm`, `jest`, `generic`.
- Inline `[[tests]]` goldens executed by `cargo test`.

#### Layer 4 — Read cache
- `mtime + range` cache, LCS delta replies, `.contextignore` support.
- Hooks: `crux hook pre-tool`, `crux hook post-tool`.

#### Layer 5 — AST graph (tree-sitter)
- Languages: Rust, Python, TypeScript, JavaScript.
- Per-file resolver + project-wide cross-file call resolution.
- Receiver typing (`self` / `Self` / parameter rewrite).
- `let`-binding inference, `if let` / `while let` scoping.
- Pattern bindings (`Some/Ok/Err`, tuple / struct destructure, match-arm isolation).
- User-enum tuple **and** struct variants.
- Tuple-typed locals, nested tuple destructure, or-pattern alt merge.
- Cross-file return-type inference via `ProjectFileTypes`.
- DB-persisted `FileTypes` via `ast_file_signatures` (bincode).
- TypeScript / JavaScript:
  - default-export aliasing.
  - `new_expression` CALLS edges.
  - tsconfig path-mapping (`extends`, `baseUrl`, `paths`, JSONC).
  - default- / named- / namespace-import path-mapping.
- Merkle-incremental `crux index` with `--force` rebuild.
- Resolver lifts the call graph to **2307 RESOLVED CALLS** on the CRUX
  repo itself.

#### Layer 6 — Hybrid search
- BM25 (porter + trigram FTS5) + dense vectors + Reciprocal Rank Fusion.
- Smallest-window proximity rerank for multi-token queries.
- Levenshtein-1 fuzzy fallback when every ranker missed.
- Auto-memory: `CLAUDE.md`, `MEMORY.md`, `~/.crux/memory/*` indexed.
- `HashEmbedder` default; `FastEmbedder` (ONNX) opt-in via
  `--features crux-l6-search/fastembed`.
- Merkle-incremental `crux reindex` with `--force` rebuild.

#### Layer 7 — Sandbox executor
- Polyglot runtimes: Python, Bash, Node.
- Timeout / max-output-bytes / env scrub.
- Linux rlimits + landlock filesystem isolation (`--isolate hard`).
- Optional seccomp BPF syscall filtering behind the
  `crux-l7-sandbox/seccomp` Cargo feature; per-runtime allowlists, SIGSYS
  on violation.

#### Layer 8 — Memory
- Observation CRUD with FTS5 recall + decay-weighted ranking.
- Kinds: decision, fact, todo, lesson, snippet, question, …
- `crux remember`, `crux recall`, `crux memory {kinds|list|decay|archive|delete}`.

#### Layer 9 — Coach
- Quality scoring matrix (penalty / bonus rules).
- Loop-state detection.
- `CLAUDE.md` drift detector with content-hash history.
- `crux audit`, `crux coach {snapshot,record,loop,drift}`, `crux stats`.

#### Layer 10 — Setup
- `crux init [--profile coding|analysis|agents] [--non-interactive] [--force]`
  scaffolds `CLAUDE.md`, `.crux/config.toml`, `.claudeignore`.

#### MCP server
- Stdio JSON-RPC server (`crux mcp`) exposing 11 tools:
  `crux_remember`, `crux_recall`, `crux_read`, `crux_bash_filter`,
  `crux_audit`, `crux_find_symbol`, `crux_get_symbol_source`,
  `crux_query_graph`, `crux_impact`, `crux_search`, `crux_execute`.

#### Shared infrastructure
- One SQLite database at `$CRUX_HOME/db/crux.sqlite` (WAL mode).
- Nine numbered migrations, embedded via `include_str!` and applied on
  every `Runtime::open`.
- `crux-core::merkle` partitioned by `scope` (`SCOPE_AST`, `SCOPE_CHUNKS`).
- Telemetry recorded locally; never leaves the machine.

### Tested
- 289 tests passing on default features, 299 with
  `--features crux-l7-sandbox/seccomp` (Linux). 0 failures.
- Criterion benchmarks for L4 (`compute_delta`, `check_with`),
  L5 (`parse`, `find_symbol`, `impact_radius`),
  L6 (`HashEmbedder::embed`, `index_chunks`, `hybrid_search`).

[Unreleased]: https://github.com/Keradd/crux/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Keradd/crux/releases/tag/v0.2.0
[0.1.1]: https://github.com/Keradd/crux/releases/tag/v0.1.1
[0.1.0]: https://github.com/Keradd/crux/releases/tag/v0.1.0
