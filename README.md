# CRUX

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-392%20passing-brightgreen.svg)](#testing)
[![CI](https://github.com/Keradd/crux/actions/workflows/ci.yml/badge.svg)](https://github.com/Keradd/crux/actions/workflows/ci.yml)

> **Compression Runtime for Universal eXecution.**
> One Rust binary. One SQLite database. Eleven layers. Local-first. Zero telemetry.

CRUX is a token-optimization runtime that sits between an AI coding agent
(Claude Code, Cursor, Cline, Continue, Aider, …) and the agent's tools
(Read, Edit, Bash, MCP servers). It reduces token usage by 60–95 % across
real workloads without degrading task quality, by attacking every layer
where tokens are wasted: prose verbosity, tool-call bloat, redundant file
reads, missing structure, lost context, conversation history bloat, and
quality decay.

**Contents** — [Why CRUX](#why-crux) · [Install](#install) ·
[Activate](#activate-inside-your-ai-agent) · [Quick start](#quick-start) ·
[MCP tools](#mcp-tools) · [Workspace](#workspace-layout) ·
[Privacy](#privacy--telemetry) · [Testing](#testing) ·
[Documentation](#documentation) · [License](#license)

---

## Why CRUX

| Pain point | CRUX layer | Mechanism |
|---|---|---|
| Verbose model output | **L1** profiles | Compressed-output skills + project-tuned style guides |
| Bloated MCP tool descriptions | **L2** shrinker | Stdio JSON-RPC proxy that rewrites `tools/list` |
| Long, noisy bash output | **L3** bash filter | TOML pipeline (8 stages) + 5 built-in filters |
| Re-reading the same file | **L4** read cache | mtime + range cache, LCS delta, `.contextignore` |
| Whole-file reads to find a symbol | **L5** AST graph | Tree-sitter graph with cross-file call resolution |
| Grep-only code search | **L6** hybrid search | BM25 (porter + trigram) + dense vectors + RRF |
| Unsafe code execution | **L7** sandbox | Subprocess + rlimits + landlock + seccomp (opt-in) |
| Lost context across sessions | **L8** memory | FTS5 + decay-ranked observations |
| Quality / loop drift | **L9** coach | Score, loop detect, CLAUDE.md drift |
| Inconsistent project setup | **L10** setup | `crux init` scaffold + profile templates |
| Long-session history bloat | **L11** digest | Deterministic turn-event rollup (per-file reads/edits, bash first-word, search query buckets) + optional L8 mirror |

All eleven layers are independent and opt-in via TOML. Flip any subset
(`[layer.l3] enabled = false`, etc.) without rebuilding — the binary ships
every layer; the config decides which ones run.

---

## Install

### One-shot installer (recommended)

```bash
# From a checkout of the repo — builds release + installs to ~/.local/bin
# + auto-registers CRUX as an MCP server in every detected agent
# + runs the first-time L5 index and L6 reindex in your CWD.
bash scripts/install.sh

# System scope (puts `crux` in /usr/local/bin; uses sudo if needed):
bash scripts/install.sh --system

# Skip bootstrap (just build + install the binary):
bash scripts/install.sh --no-bootstrap

# Install but don't touch any agent config:
bash scripts/install.sh --no-agents
```

After that, `crux --version` should work and every installed agent
(Claude Code / Desktop, Cursor, Windsurf, Cline, Zed) already has CRUX
wired up as an MCP server pointed at the current directory.

### Prebuilt binaries

Each tagged release ships statically-linked binaries on the
[GitHub Releases](https://github.com/Keradd/crux/releases) page for:

- Linux x86_64 (`gnu` + `musl`)
- Linux aarch64 (`gnu` + `musl`)
- macOS x86_64 + aarch64 (Apple Silicon)
- Windows x86_64

Each archive ships with a `.sha256` checksum next to it.

```bash
# Linux x86_64 (gnu) example — replace TAG with the latest release tag.
TAG=v0.3.0
curl -L -o crux.tar.gz \
  "https://github.com/Keradd/crux/releases/download/${TAG}/crux-${TAG}-x86_64-unknown-linux-gnu.tar.gz"
tar -xzf crux.tar.gz
sudo install crux /usr/local/bin/
crux --help
```

### From source

```bash
git clone https://github.com/Keradd/crux.git
cd crux

# Default build — offline, ~11 MB stripped, no network at runtime.
# Layer 6 ships the deterministic `HashEmbedder` for the dense ranker.
cargo build --release

# "crux-full" build — adds the ONNX-backed `FastEmbedder` (BGE-small-en-v1.5
# by default, ~30 MB model downloaded at first run from Hugging Face).
# Required if you want real semantic dense retrieval; everything else
# (BM25, RRF, fuzzy, AST graph, …) works identically with either build.
cargo build --release --features full      # alias for `--features fastembed`
```

After building with `--features full`, opt the runtime in via
`[layer.l6] embedding_provider = "fastembed"` in `~/.crux/config.toml`
and run `crux reindex --force` once to populate the new vectors.
Existing hash-indexed rows stay in the DB untouched — they're partitioned
by `(provider, model, dim)` so switching back is a single config flip
(no migration, no `--force` reindex needed).

`crux doctor` flags the mismatch when the config selects `fastembed`
but the binary was built without the feature.

### Requirements (source builds)

- Rust **1.85+**
- SQLite is bundled via `rusqlite` — no system dependency.
- Optional: `--features crux-l7-sandbox/seccomp` enables Linux seccomp BPF
  syscall filtering (requires kernel ≥ 3.5).
- Optional: `--features full` enables the L6 fastembed embedder (~30 MB
  ONNX runtime + model archive, downloaded on first use).

---

## Activate inside your AI agent

After `crux --version` works, register CRUX with your agent in one command:

```bash
crux setup                  # auto-detect every supported agent on this machine
crux setup claude-code      # specific agent
crux setup --list           # show what's supported
crux setup --dry-run        # preview without writing anything
```

| Agent | What `crux setup` writes |
|---|---|
| **Claude Code** | `mcpServers.crux` + `PreToolUse(Read)` / `PostToolUse(Edit\|Write\|MultiEdit)` hooks + `/crux` slash-command (`~/.claude/commands/crux.md`) |
| **Claude Desktop** | `mcpServers.crux` in `claude_desktop_config.json` (OS-canonical path) |
| **Cursor** | `~/.cursor/mcp.json` |
| **Windsurf** (Cascade) | `~/.codeium/windsurf/mcp_config.json` |
| **Cline** (VS Code) | `cline_mcp_settings.json` in VS Code's `globalStorage` |
| **Zed** | `context_servers.crux` in `~/.config/zed/settings.json` |
| **OpenClaw** | `mcp.servers.crux` in `~/.openclaw/openclaw.json` (honors `$OPENCLAW_CONFIG_PATH`) |
| **Hermes Agent** | `mcp_servers.crux` in `~/.hermes/config.yaml` (native YAML) |

`crux setup` is idempotent: re-running it is a no-op once the entries exist.
Use `--scope project` to write per-project configs instead of per-user, and
`--no-hooks` / `--no-skill` to opt out of the Claude Code extras.

After setup, restart your agent (or run `claude mcp list` for Claude Code)
and the thirteen CRUX MCP tools will be available — `crux_search`,
`crux_find_symbol`, `crux_impact`, `crux_remember`, `crux_recall`,
`crux_read`, `crux_execute`, `crux_digest`, `crux_compact`, and friends.

---

## Quick start

```bash
# ── One-line bootstrap (recommended) ────────────────────────────────
# Scaffold project + register CRUX in every detected agent (with
# CRUX_PROJECT=<cwd> pinned in each MCP env) + run first-time L5
# AST index and L6 hybrid-search reindex.
crux init --non-interactive --setup-agents --index

# The three concerns can also be run one at a time:
crux init --non-interactive --profile coding
crux setup                  # auto-detect every supported agent
crux index && crux reindex  # first-time AST + chunk build

# L3 — filter bash output (git/cargo/npm/jest/generic, all TOML-defined)
crux bash git status

# L5 — AST graph (Merkle-incremental; second run is a near no-op)
crux index
crux find ReadCacheManager
crux impact crates::crux-l4-readcache::src::delta::compute_delta --depth 3

# L6 — hybrid search (BM25 + dense + RRF + fuzzy fallback)
crux reindex
crux search "delta cache" --kind code --limit 5

# L7 — sandboxed code execution
crux execute --runtime python -c 'print(2+2)'
crux execute --runtime bash   -c 'sleep 5' --timeout 1     # exits 124
crux execute --runtime bash   -c 'ulimit -v' --isolate hard

# L8 — persistent memory (FTS5 + decay)
crux remember --kind decision --title "Use Pinia" --content "..."
crux recall Pinia

# L9 — quality coach
crux audit
crux coach drift

# L11 — conversation digest (compact turn-event rollups for long sessions)
crux digest                      # latest rollup + pending events
crux digest --pending            # only the un-rolled queue
crux digest --history --limit 5  # last five rollups
crux compact                     # force-roll pending into a single digest

# MCP server (13 tools, stdio JSON-RPC)
crux mcp
crux mcp-shrink npx @modelcontextprotocol/server-filesystem /some/path
```

`CRUX_HOME` overrides the default `~/.crux` data directory.
`CRUX_PROJECT` overrides project-root detection.

---

## MCP tools

Run `crux mcp` to expose CRUX over stdio JSON-RPC. The server advertises
the following tools:

| Tool | Layer | Purpose |
|---|---|---|
| `crux_remember` / `crux_recall` | L8 | Persist & search observations |
| `crux_read` | L4 | Cache-aware file reads with delta replies |
| `crux_bash_filter` | L3 | Apply L3 filter to a `(command, output)` pair |
| `crux_audit` | L9 | Health snapshot + telemetry summary |
| `crux_find_symbol` / `crux_get_symbol_source` | L5 | Symbol lookup |
| `crux_query_graph` / `crux_impact` | L5 | Callers / callees / blast radius |
| `crux_search` | L6 | Hybrid BM25 + dense + RRF (line-aware snippets + symbol enrichment) |
| `crux_execute` | L7 | Run python / bash / node snippets in the sandbox |
| `crux_digest` / `crux_compact` | L11 | Render / force-roll conversation turn digests |

---

## Privacy & telemetry

CRUX is **local-first**. There is no cloud backend, no analytics pixel, no
update ping. All telemetry is recorded in the local SQLite database at
`$CRUX_HOME/db/crux.sqlite` (default `~/.crux/db/crux.sqlite`, mode `0600`)
and exposed only through `crux audit` / `crux stats` / the `crux_audit`
MCP tool. The daemon binds to `127.0.0.1` (never `0.0.0.0`); the MCP
server defaults to stdio. No network calls happen unless you explicitly
opt in to a cloud-backed embedder or enable a remote MCP transport.

Secrets handling is detailed in `docs/ARCHITECTURE.md` §14.2 — AWS /
GitHub / Slack tokens, `Authorization: Bearer`, `*_KEY|SECRET|TOKEN=`
patterns, and `password=` are all preserved verbatim by every compression
stage.

---

## Workspace layout

```
crux/
├── Cargo.toml                 # workspace manifest
├── crates/
│   ├── crux-core/             # config, db, errors, paths, telemetry, merkle
│   ├── crux-l3-bash/          # TOML filter pipeline + 5 built-in filters
│   ├── crux-l4-readcache/     # read cache + LCS delta + .contextignore
│   ├── crux-l5-ast/           # tree-sitter AST graph (Rust/Python/TS/JS)
│   ├── crux-l6-search/        # hybrid BM25 + dense + RRF + Merkle sync
│   ├── crux-l7-sandbox/       # subprocess + rlimits + landlock + seccomp
│   ├── crux-l8-memory/        # observations + decay engine + FTS5
│   ├── crux-l9-coach/         # quality score + loop detect + drift
│   ├── crux-l10-setup/        # `crux init` + profile templates
│   ├── crux-l11-digest/       # turn-event rollup + deterministic renderer
│   ├── crux-mcp/              # MCP stdio JSON-RPC server + L2 shrinker
│   └── crux-cli/              # `crux` binary (clap-based CLI)
├── docs/
│   └── ARCHITECTURE.md        # full architecture specification
├── CHANGELOG.md
├── CONTRIBUTING.md
├── LICENSE-MIT
└── LICENSE-APACHE
```

---

## Documentation

- **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)** — full design specification:
  goals, tech stack, data model, per-layer internals, security model,
  performance targets, roadmap.
- **[`CHANGELOG.md`](CHANGELOG.md)** — release-notable changes.
- **[`CONTRIBUTING.md`](CONTRIBUTING.md)** — coding conventions, test
  policy, PR checklist.

---

## Testing

MSRV is **Rust 1.85**. The workspace builds warning-free with
`-D warnings` on stable and nightly. Every crate has in-module
`#[cfg(test)] mod tests`; nothing depends on a running agent.

```bash
cargo fmt --all -- --check                          # style
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace                              # 392 passing / 0 failed
cargo test --workspace --features crux-l7-sandbox/seccomp
                                                    # 402 passing / 0 failed (Linux)
cargo bench                                         # criterion (L4 / L5 / L6)
```

Inline TOML goldens for L3 filters live in
`crates/crux-l3-bash/filters/*.toml` under `[[tests]]` and execute via
the standard `cargo test` invocation — no separate harness needed.

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full PR checklist
(fmt + clippy + tests + docs + migration rules + commit conventions).

---

## License

Licensed under `MIT OR Apache-2.0` at your option:

- Apache License, Version 2.0 — [`LICENSE-APACHE`](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>
- MIT — [`LICENSE-MIT`](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in CRUX by you, as defined in the Apache-2.0
license, shall be dual-licensed as above, without any additional terms
or conditions.
