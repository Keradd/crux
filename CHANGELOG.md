# Changelog

All notable changes to CRUX are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **CRUX Humanizer** — new `crux-humanizer` crate + `crux humanize`
  CLI subcommand. Rewrites raw AI-flavoured prose into concise,
  human-sounding text using a deterministic, local-only rule set
  (no LLM round-trip, no network).
  - Six modes: `concise` (default), `casual`, `professional`,
    `developer`, `social`, `github-readme`. Each mode toggles a
    different combination of strike-phrase aggression, fluff-
    adjective deletion, contractions, and blank-line collapsing
    via `HumanizeOptions::for_mode`.
  - Strike list covers AI-tell phrases (`In conclusion,`, `It is
    important to note that`, `As an AI language model,`, `I hope
    this helps!`, `Let's dive in!`), wordy substitutions (`in
    order to` → `to`, `the majority of` → `most`, `due to the
    fact that` → `because`), fluff verbs (`utilize`, `leverage`,
    `facilitate`, `commence`, `delve`, `endeavor`), and
    marketing adjectives (`robust`, `comprehensive`,
    `cutting-edge`, …). Casing is preserved on word substitutions.
  - Segment-aware tokenizer in `crux-humanizer/src/tokenizer.rs`
    locks fenced code blocks, inline code spans, URLs, filesystem
    paths, IPv4 / IPv6 / hex literals, qualified identifiers
    (`foo::bar::baz`), function-call literals (`foo(args)`),
    `@scope/pkg` packages, and `SCREAMING_SNAKE_CASE` constants
    so they pass through byte-for-byte.
  - CLI surface mirrors the existing `crux execute` shape:
    `crux humanize --mode <MODE> --input <STRING>` /
    `--file <PATH>` / stdin, with `--json` for machine-readable
    output and `--stats` for a one-line stderr footer with
    chars / words saved + edit count.
  - 36 unit tests in `crux-humanizer` cover buzzword removal,
    code preservation (fenced + inline), URL preservation, IPv4 /
    hex / path / scoped-package preservation, mode-specific
    behaviour (Casual contracts, Concise does not, Professional
    keeps fluff, GithubReadme keeps blank lines), repetition
    collapse, capitalisation preservation, stats accounting, and
    a mixed-codeblock-and-strike-phrase regression case.

- **Hot reload `crux.toml`** (Phase 4 Task I). `crux-core` now ships a
  `ConfigWatcher` (in `crux_core::config_watch`) that holds a published
  `Config` behind an `Arc<RwLock<_>>` and re-parses both the global
  (`~/.crux/config.toml`) and project (`.crux/config.toml`) files when
  their on-disk mtime moves. Atomic swap under a write lock means
  readers never see a torn config. A malformed mid-session edit is
  logged and ignored so the previously-published config stays active.
  - **MCP server wire-up**: `crux mcp` builds a watcher from its
    `Runtime` at startup (via `ConfigWatcher::from_runtime`, zero extra
    disk I/O) and checks it between every JSON-RPC request. Edits to
    layer toggles land on the very next `tools/call` without needing
    to restart the long-lived MCP process. Log line
    `crux.toml changed on disk — reloaded config` confirms a swap.
  - **Pull + push consumption**: callers outside the MCP loop can tick
    manually (`ConfigWatcher::tick`) or hand the watcher to
    `spawn_polling(interval)` for a background thread (1s default
    cadence) — drop the returned `WatcherHandle` to join cleanly.
  - 14 new unit tests (10 in `config_watch.rs`, 4 in `crux-mcp`
    `server.rs` covering `reload_if_changed` + the extracted generic
    `run_loop`).

- **L7 sandbox deny-list sync with Claude Code / OpenClaw**
  (Phase 4 Task H). `crux execute` can now consult the same
  `~/.claude/settings.json` / `~/.openclaw/openclaw.json` permission
  blocks the agents themselves honour, plus their per-project
  overrides, before spawning a runtime.
  - New modules in `crux-l7-sandbox`: `permissions.rs` parses
    `Tool(pattern)` entries and maps them to runtime kinds;
    `agent_perms.rs` loads the global + project files and merges
    them; `executor.rs` grew a pre-spawn deny check driven by
    `req.permissions`.
  - New CLI flag `crux execute --check-agent-perms` opts in.
    `crux audit --json` now includes an `agent_permissions` block
    so observers can see which tool patterns are currently denied
    or allowed.
  - 29 new unit tests (16 permissions, 9 agent_perms, 4 executor).

- **`crux audit --watch`** (Phase 4 Task J). Streaming health-snapshot
  feed, NDJSON in `--json` mode and an ANSI-cleared text frame
  otherwise. `--interval-ms` governs cadence (default 5 s, floor 200 ms
  enforced by `clamp_interval_ms`). Helpers `build_payload`,
  `clamp_interval_ms`, and generic `watch_step<W>` factored for unit
  tests (7 new tests in `crux-cli`).

- **`crux hook openclaw-compact`** — OpenClaw / Claude Code `PreCompact`
  trigger that reads a JSON event from stdin (`session_id`, optional
  `cwd`, optional `trigger`) and forwards into the same
  `DigestEngine::compact` path that backs `crux compact`. Single
  compaction code path; OpenClaw is a trigger source only. Accepts both
  `snake_case` and `camelCase` field aliases. 13 new unit tests cover
  parsing + the compaction core.
- **`crux-cli` build features** — `cargo build --features full` (alias
  `--features fastembed`) bundles the ONNX-backed L6 embedder. Default
  build still ships the offline `HashEmbedder`. `crux doctor` now reports
  the embedder build mode and surfaces a clear error when the config
  selects `fastembed` against a binary that wasn't compiled with the
  feature. The on-disk schema partitions vectors by
  `(provider, model, dim)`, so existing hash-indexed rows stay valid
  after switching providers — switching from hash to fastembed needs
  one `crux reindex --force`; switching back is just a config flip.
- **L5 AST coverage for Lua + Bash** (Task G). New `tree-sitter-lua`
  (0.1) and `tree-sitter-bash` (0.21) parsers feed the same graph used
  by `crux find` / `crux symbol` / `crux impact`.
  - **Lua**: `function name(...)`, `local function name`,
    `function M:method`, `function M.helper`, `M.helper = function(...)`,
    `local x = ...` (Constant), `require("foo")` (`ImportsFrom` edge).
    Function bodies emit `Calls` edges via `function_call` nodes.
  - **Bash**: `function_definition` (both `name() {…}` and
    `function name {…}`), `declaration_command` (`local`, `declare`,
    `readonly`) → Constants, `alias name=value` → Constant with the
    alias body in `signature`. Function bodies emit `Calls` edges
    per `command` node. `test_*` and bats `@test` are tagged
    `is_test = true`.
  - Files routed via extension: `.lua`, `.sh`, `.bash`. Pure-dotfile
    rc scripts (`.bashrc`, `.bash_profile`) need an explicit indexer
    entry — `Path::extension()` doesn't see leading-dot names.
  - 18 new unit tests (`extract::tests::lua_*`, `extract::tests::bash_*`).

### Planned

- mdBook documentation chapters.
- `cargo install crux` publishing (crates.io).
- Homebrew tap.
- Criterion benchmarks for L1 / L2 / L3 / L7 / L8 / L9 (currently L4 / L5 / L6 only).

## [0.3.0] — 2026-05-03 — Layer 11 conversation digest + L6 search lean-shape revamp

### Added

- **Layer 11 — conversation digest** (`crux-l11-digest`). Records every
  tool call as a `turn_event` and rolls them up into compact
  `turn_digests` so long sessions stop hoarding historical noise in
  the model's context window.
  - New SQLite migration `010_turn_log.sql` (`turn_events` +
    `turn_digests`, both session-scoped, both project-tagged).
  - New CLI: `crux digest [--session=…] [--pending] [--history --limit N]`
    and `crux compact [--session=…]`.
  - New MCP tools: `crux_digest` (render latest rollup + still-pending
    events) and `crux_compact` (force-roll the pending queue, optionally
    mirror the digest into L8 as a `convention` observation).
  - `crux hook post-tool` extended to seed a `turn_event` for every
    `Edit` / `Write` / `Read` / `Bash` / MCP call it sees.
  - **Dispatch-level MCP auto-record**: every non-digest `tools/call`
    inside `crux mcp` also seeds a `turn_event`, so agents driving
    CRUX through MCP only (Cursor, Windsurf default) get conversation
    digests without needing PreToolUse/PostToolUse hooks. The digest
    tools themselves skip self-recording.
  - New `[layer.l11]` config block:
    `auto_compact_every_n` (default 50), `max_summary_tokens`
    (600), `mirror_to_l8` (true), `mirror_importance` (4),
    `render_max_events` (200).
  - Renderer is fully deterministic Rust — buckets reads/edits by file,
    bash by first-word, searches by query — no LLM round-trip.
  - 14 unit tests in `crux-l11-digest`; 5 dispatch-level tests in
    `crux-mcp`. `crux_audit` + `crux audit` now surface the
    `l11_digest` toggle.
- L9 Coach now treats CRUX as 11 layers when computing `unused_layers`
  and the "Few layers active" pattern threshold.
- **L6 `crux_search` lean-shape revamp.** The MCP search tool now
  returns a flat per-hit object (`id` / `kind` / `file` / `lines` /
  `title` / `snippet` / `score`) instead of the previous
  `{chunk: {...}, ranks: {...}}` envelope. Three behavior changes
  worth flagging:
  - **Line-aware default snippet** for code/symbol chunks. Replaces the
    legacy ~80-char text window with the matched line plus
    `view_lines` (default 3) lines on either side, with the matched
    line prefixed by `> `. Saves the agent's follow-up `crux_read` /
    `crux_get_symbol_source` call when the snippet is enough.
  - **`symbol` enrichment**: when a chunk has a `source_id` linked to
    `ast_nodes` (any L5-derived chunk), the dispatcher joins to fetch
    the qualified_name and surfaces it as `symbol`. Lets agents chain
    directly into `crux_get_symbol_source` without parsing the file
    path.
  - **Metadata pruning**: `ranks`, `tokens_est`, `source_id`,
    `language`, and the unrounded raw score moved behind `debug=true`.
    Default payload is ~30% leaner per result.
  - New args: `view: "compact" | "default" | "full"` (default
    `default`); `view_lines: 0..=20` (default 3); `debug: bool`
    (default false). `view=compact` keeps the legacy 80-char shape;
    `view=full` returns the entire chunk content so agents can skip
    follow-up reads when they want.

### Docs

- `README.md`: eleven-layer phrasing, L11 row in the "Why CRUX"
  table, `crux-l11-digest` in the workspace tree, thirteen MCP tools
  (adding `crux_digest` / `crux_compact`), new L11 quick-start block,
  refreshed test badge + Testing section counts, compact in-page TOC,
  dedicated **Privacy & telemetry** section, MSRV + fmt/clippy/test
  commands in Testing, and SPDX-style license footer.
- `docs/ARCHITECTURE.md`: eleven-layer tagline + high-level
  diagram, `crux-l11-digest` added to the workspace crate list,
  MCP server advertised as 13 tools, new Phase 11 roadmap entry,
  status line flipped to "All 11 layers shipped", new L11 row
  in the §14.1 security table, navigable top-level **Contents** list,
  and §16 split into *Resolved design decisions* (table) +
  *Still open* questions so answered items stop masquerading as open.
- `CONTRIBUTING.md`: Contents index, MSRV note, **Development
  workflow** section, **Commit message conventions** section
  (Conventional Commits, matches actual git history), expanded PR
  checklist (adds `cargo clippy`, `--help` docs, migration schema,
  commit-convention tick), new **Adding a new layer** checklist,
  new **Security** section with private-advisory flow + high-value
  audit surface, and telemetry layer range corrected to `l1..l11`.
- `CHANGELOG.md`: ISO release dates (`2026-05-03`) added to `[0.1.0]`,
  `[0.1.1]`, and `[0.2.0]` headers per Keep-a-Changelog convention.

### Tests

- **392 pass / 0 fail** on the default feature set (+60 over the
  v0.2.0 baseline of 332). **402 pass / 0 fail** with
  `--features crux-l7-sandbox/seccomp` on Linux. New coverage:
  14 unit tests in `crux-l11-digest` (turn-event record + rollup
  + renderer buckets), 5 dispatch-level tests in `crux-mcp`
  (auto-record + digest tool self-skip), and 6 new `crux_search`
  dispatch tests pinning the lean-shape + line-aware snippet +
  symbol enrichment + `view` / `debug` flag behavior.

## [0.2.0] — 2026-05-03 — Agent integration setup

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

## [0.1.1] — 2026-05-03 — Patch release

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

## [0.1.0] — 2026-05-03 — Initial release

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

[Unreleased]: https://github.com/Keradd/crux/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Keradd/crux/releases/tag/v0.3.0
[0.2.0]: https://github.com/Keradd/crux/releases/tag/v0.2.0
[0.1.1]: https://github.com/Keradd/crux/releases/tag/v0.1.1
[0.1.0]: https://github.com/Keradd/crux/releases/tag/v0.1.0
