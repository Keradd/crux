<h1 align="center">CRUX</h1>

<p align="center">
  <a href="https://crates.io/crates/crux-cli"><img src="https://img.shields.io/crates/v/crux-cli.svg" alt="crates.io"></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License"></a>
  <a href="https://github.com/Keradd/crux/actions/workflows/ci.yml"><img src="https://github.com/Keradd/crux/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/Keradd/crux"><img src="https://img.shields.io/badge/rust-1.96%2B-orange.svg" alt="Rust"></a>
</p>

<p align="center">
  <b>Local-first Rust runtime for AI coding agents.</b><br>
  Token optimization. Context hygiene. Sandboxed execution. Zero telemetry.
</p>

---

## What is CRUX?

AI coding agents waste tokens re-reading files, echoing redundant tool output, and filling context with verbose prose. CRUX solves this with a layered pipeline that compresses every interaction — before it enters the agent's context window.

**One binary. One SQLite database. 12 layers of optimization.**

- **Token costs drop** — layered compression over tool descriptions, bash output, file reads, and conversation history.
- **Context stays lean** — per-file read cache with delta replies and structural digests prevent re-reads.
- **Code is searchable** — hybrid BM25 + dense vector + RRF search across code, memory, and conversation chunks.
- **Execution is safe** — Linux rlimits, landlock, and optional seccomp; portable fallback on other platforms.
- **Output stays clean** — comment slop guard strips AI-flavoured prose from source; humanizer rewrites raw AI output into concise text.
- **Zero telemetry** — no cloud, no analytics, no network calls unless you opt in.

---

## Quick start

```bash
# Install
cargo install crux

# Register CRUX inside every detected AI agent as an MCP server
crux setup

# Build with the comment-hygiene gate in front of cargo build
crux build --release

# Hybrid code search across the indexed project
crux search "delta cache" --kind code --limit 5

# Scan the workspace for AI-flavoured comments
crux hygiene comments --check

# Rewrite raw AI prose into concise text
crux humanize --mode concise --input "In conclusion, we leverage the robust pipeline."
# → We use the pipeline.
```

Or build from source: `git clone https://github.com/Keradd/crux.git && cd crux && cargo build --release`

---

## Why CRUX?

| Problem | CRUX solution |
|---|---|
| Agent re-reads the same files every turn | **Layer 4** — read cache with mtime tracking, delta replies, structural digests |
| Tool descriptions eat context budget | **Layer 2** — description shrinker strips redundant prose from MCP tool surfaces |
| Bash output is verbose and repetitive | **Layer 3** — TOML-defined filters (3-tier: static, dynamic, structured) |
| Code search is slow or cloud-dependent | **Layer 6** — hybrid BM25 (porter + trigram) + dense vectors + RRF, all local |
| Syntax trees are expensive to recompute | **Layer 5** — persistent tree-sitter AST graph with cross-file call resolution |
| AI agents write overly verbose comments | **Layer 12** — deterministic scanner, fixer, and stripper for AI-flavoured comments |
| Raw AI prose wastes tokens | **Humanizer** — deterministic local rewrite of verbose output into concise text |
| Unsafe code execution blows up context | **Layer 7** — sandboxed subprocess with rlimits, landlock, seccomp |

---

## Features

- **Token optimization** — layered compression across all 12 layers.
- **Context hygiene** — read cache, delta replies, structural digests.
- **Hybrid search** — BM25 porter + trigram + dense vectors + RRF.
- **AST graph** — tree-sitter for 6 languages with cross-file call resolution.
- **Sandboxed execution** — Linux rlimits + landlock + seccomp; cross-platform fallback.
- **Persistent memory** — FTS5 + decay-ranked observations with contradiction detection.
- **Comment slop guard** — detect, fix, and strip AI-flavoured source comments.
- **Output humanizer** — rewrite raw AI prose into concise, natural text.
- **MCP tooling** — stdio JSON-RPC server exposing 13 tools, plus description shrinker proxy.
- **Hygiene-aware build** — `crux build` gates on comment hygiene before `cargo build`.

---

## Commands

| Command | Description |
|---|---|
| `crux setup` | Register CRUX inside every detected AI agent |
| `crux init` | Scaffold `.crux/`, `CLAUDE.md`, and project profile |
| `crux index` / `crux reindex` | Build AST graph and chunk store |
| `crux search <query>` | Hybrid BM25 + dense + RRF search |
| `crux find <name>` / `crux impact <symbol>` | Symbol lookup and blast radius |
| `crux execute --runtime <python\|bash\|node>` | Sandboxed code execution |
| `crux remember` / `crux recall` | Decay-ranked observation store |
| `crux bash <cmd>` | Run command through the filter pipeline |
| `crux hygiene comments --check\|--fix\|--strip` | AI-comment slop guard |
| `crux humanize --mode <mode>` | Rewrite AI-flavoured prose |
| `crux build` | Hygiene check + `cargo build` |
| `crux audit` / `crux stats` | Health snapshot and telemetry |
| `crux digest` / `crux compact` | Conversation turn-event rollups |
| `crux mcp` / `crux mcp-shrink` | MCP server or description shrinker proxy |

`CRUX_HOME` overrides `~/.crux`. `CRUX_PROJECT` overrides project-root detection.

---

## Architecture

CRUX is a layered pipeline. Each layer compresses or transforms agent interactions independently:

```
Input → L1 (Output rules) → L2 (MCP shrink) → L3 (Bash filter) → L4 (Read cache)
         → L5 (AST graph) → L6 (Search) → L7 (Sandbox) → L8 (Memory)
         → L9 (Coach) → L10 (Setup) → L11 (Digest) → L12 (Hygiene)
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full design.

---

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — goals, tech stack, data model, per-layer internals, security model.
- [Install](docs/INSTALL.md) — from-source build, `--features` matrix, per-agent activation.
- [MCP tools](docs/MCP.md) — stdio server, description shrinker, full tool surface.
- [Humanizer](docs/HUMANIZER.md) — modes, examples, what it never touches.
- [Comment Hygiene](docs/HYGIENE.md) — scanner, fixer, stripper behaviour.
- [Contributing](CONTRIBUTING.md) — workspace conventions, PR checklist, comment rules.
- [Changelog](CHANGELOG.md) — release history.
- [Examples](examples/) — scaffolded projects to explore.

---

## Privacy

CRUX is local-first. No cloud backend, no analytics, no update ping. Telemetry lives in a local SQLite database (`$CRUX_HOME/db/crux.sqlite`, mode `0600`) and is only exposed via `crux audit` / `crux stats`. The MCP server defaults to stdio; the TCP daemon binds to `127.0.0.1`. No network calls happen unless you explicitly opt into a cloud embedder or remote transport.

Secrets handling: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §14.2.

---

## Development

MSRV: **Rust 1.96**. CI runs 5 jobs on Ubuntu + stable:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude crux-l7-sandbox
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Prefer `crux build` over `cargo build` — the hygiene gate runs first.
See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full PR checklist.

---

## License

Licensed under `MIT OR Apache-2.0` at your option:

- [Apache-2.0](LICENSE-APACHE) — <https://www.apache.org/licenses/LICENSE-2.0>
- [MIT](LICENSE-MIT) — <https://opensource.org/licenses/MIT>

Contributions are dual-licensed under the same terms.
