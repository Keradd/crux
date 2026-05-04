# CRUX

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-621%20passing-brightgreen.svg)](#development)
[![CI](https://github.com/Keradd/crux/actions/workflows/ci.yml/badge.svg)](https://github.com/Keradd/crux/actions/workflows/ci.yml)

CRUX is a local-first Rust runtime for AI coding agents. It reduces
token waste, improves context recall, cleans AI-generated comments,
exposes MCP tools, and can humanize final AI output without sending
code to a cloud service.

One Rust binary. One SQLite database. Layered, local-first, zero
telemetry.

---

## Features

- **Token optimization** — layered compression over prose, tool
  descriptions, bash output, file reads, and conversation history.
- **Context hygiene** — per-file read cache, delta replies, and
  structural digests that keep the model from re-reading the same
  code.
- **Hybrid search** — BM25 (porter + trigram) + dense vectors
  (hash or fastembed) + RRF across code, memory, and chunks.
- **AST graph** — tree-sitter (Rust / Python / TS / JS / Lua /
  Bash) with cross-file call resolution and blast-radius queries.
- **Sandboxed execution** — Linux rlimits + landlock + optional
  seccomp; cross-platform fallback via subprocess.
- **Persistent memory** — FTS5 + decay-ranked observations with
  contradiction detection.
- **MCP tooling** — stdio JSON-RPC server exposing 13 tools, plus
  a description shrinker proxy for upstream MCP servers.
- **Comment hygiene / Slop Guard** — deterministic scanner,
  auto-fixer, and stripper for AI-flavoured source comments.
- **Output humanizer** — deterministic local rewrite of raw AI
  prose into concise, human-sounding text.
- **Hygiene-aware build** — `crux build` runs the hygiene gate
  before handing off to `cargo build`.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the
per-layer design.

---

## Install

Recommended. Builds release, installs to `~/.local/bin`, registers
CRUX inside every detected agent, runs first-time index + reindex:

```bash
bash scripts/install.sh
```

Prebuilt binaries are published on every tagged
[GitHub release](https://github.com/Keradd/crux/releases) (Linux
`gnu`/`musl`, macOS, Windows; all x86_64 + aarch64). See
[`docs/INSTALL.md`](docs/INSTALL.md) for prebuilt downloads, the
`--features full` / `fastembed` opt-in, and the per-agent setup
matrix.

---

## Quick start

```bash
# Register CRUX inside every detected AI agent as an MCP server.
crux setup

# Build with the comment-hygiene gate in front of cargo build.
crux build --release

# Hybrid code search across the indexed project.
crux search "delta cache" --kind code --limit 5

# Scan the workspace for AI-flavoured comments.
crux hygiene comments --check

# Rewrite raw AI prose into concise text.
crux humanize --mode concise --input "In conclusion, we leverage the robust pipeline."
# → We use the pipeline.
```

---

## Core commands

| Command | Purpose |
|---|---|
| `crux setup` | Register CRUX inside every detected AI agent. |
| `crux init` | Scaffold `.crux/`, `CLAUDE.md`, and project profile. |
| `crux index` / `crux reindex` | Build the L5 AST graph and L6 chunk store. |
| `crux search <query>` | Hybrid BM25 + dense + RRF search. |
| `crux find <name>` / `crux impact <symbol>` | Symbol lookup + blast radius. |
| `crux execute --runtime <python\|bash\|node>` | Sandboxed code execution. |
| `crux remember` / `crux recall` | Decay-ranked observation store. |
| `crux bash <cmd>` | Run a shell command through the L3 filter pipeline. |
| `crux hygiene comments --check\|--fix\|--strip` | AI-comment slop guard. |
| `crux humanize --mode <mode>` | Rewrite AI-flavoured prose. |
| `crux build` | Hygiene check + `cargo build`. |
| `crux audit` / `crux stats` | Health snapshot + per-layer telemetry. |
| `crux digest` / `crux compact` | Conversation turn-event rollups. |
| `crux mcp` / `crux mcp-shrink` | Run the MCP server or proxy an upstream one. |

`CRUX_HOME` overrides the default `~/.crux` data directory.
`CRUX_PROJECT` overrides project-root detection.

---

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — goals, tech stack, data
  model, per-layer internals, security model, roadmap.
- [Install](docs/INSTALL.md) — one-shot installer, prebuilt
  binaries, `--features` matrix, per-agent activation.
- [MCP tools](docs/MCP.md) — stdio server, description shrinker,
  full tool surface.
- [Humanizer](docs/HUMANIZER.md) — what it rewrites, what it
  never touches, modes, examples.
- [Comment Hygiene](docs/HYGIENE.md) — scanner / fixer / stripper
  behaviour, build + hook integration.
- [Contributing](CONTRIBUTING.md) — workspace conventions,
  commit conventions, PR checklist, comment-hygiene rules.
- [Changelog](CHANGELOG.md) — release-notable changes.

---

## Privacy

CRUX is local-first. No cloud backend, no analytics, no update
ping. Telemetry lives in `$CRUX_HOME/db/crux.sqlite`
(mode `0600`, defaults to `~/.crux/db/crux.sqlite`) and is
exposed only via `crux audit` / `crux stats` / the `crux_audit`
MCP tool. The daemon binds to `127.0.0.1`; the MCP server defaults
to stdio. No network calls happen unless you explicitly opt into a
cloud-backed embedder or a remote MCP transport.

Secrets handling is detailed in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §14.2.

---

## Development

MSRV is **Rust 1.85**. CI runs four jobs on Ubuntu + stable:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                              # 621 / 0
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Prefer `crux build` over raw `cargo build` — the hygiene gate runs
first. See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the full PR
checklist.

---

## License

Licensed under `MIT OR Apache-2.0` at your option:

- Apache-2.0 — [`LICENSE-APACHE`](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>
- MIT — [`LICENSE-MIT`](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>

Unless explicitly stated otherwise, contributions are dual-licensed
under the same terms.
