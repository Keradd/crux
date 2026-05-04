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
| AI-style verbose comments in source files | **L12** hygiene | Deterministic scanner + auto-fix: drops decorative banners, `Goal:` / `Public surface:` blocks, marketing fluff, and compresses long module docs |
| AI-flavoured prose ("delve", "leverage", "in conclusion, …") | **Humanizer** | Deterministic local rewrite of raw model output into concise, human-sounding text — preserves code, URLs, paths, identifiers |

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

# L12 — comment hygiene / slop guard (scan + auto-fix)
crux hygiene comments --check     # exits 1 if any AI-style comment found
crux hygiene comments --fix       # drops banners, Goal/Public-surface blocks, compresses module docs
crux build                        # hygiene check + cargo build (clean only)

# Humanizer — rewrite raw AI prose into concise, human-sounding text
crux humanize --mode concise --input "In conclusion, we leverage the robust pipeline."
# → We use the pipeline.

crux humanize --mode developer --file output.txt
cat answer.md | crux humanize --mode github-readme --stats
crux humanize --mode casual --input "It is great. We are ready." --json

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

## Humanizer

`crux humanize` rewrites raw AI-flavoured prose into concise,
human-sounding text. The rewrite is **deterministic and local** — no
LLM round-trip, no network — so the same input always yields the
same output and CI / golden tests can pin behaviour exactly.

### What it changes

- Drops AI-tell openers and closers: `In conclusion, …`, `It is
  important to note that …`, `As an AI language model, …`,
  `I hope this helps!`, `Let's dive in!`.
- Strips sycophantic openers: `Certainly!`, `Sure!`, `Great
  question!`, `You're absolutely right!`.
- Collapses wordy phrases: `in order to` → `to`, `due to the
  fact that` → `because`, `at this point in time` → `now`,
  `the majority of` → `most`.
- Replaces fluffy verbs: `utilize` → `use`, `leverage` → `use`,
  `facilitate` → `help`, `commence` → `start`, `delve` → `dig`,
  `endeavor` → `try`.
- Removes marketing adjectives in every mode except `professional`:
  `robust`, `comprehensive`, `seamless`, `cutting-edge`,
  `state-of-the-art`, `groundbreaking`, …
- Collapses adjacent repeated words (`very very` → `very`).
- Tidies whitespace and excessive blank lines.

### What it never touches

- Fenced code blocks (```` ```lang ... ``` ````) and inline code
  spans (`` `foo` ``).
- URLs (`http://`, `https://`, `www.example.com`).
- Filesystem paths (`/foo/bar`, `./foo`, `C:\foo`).
- Hex / IPv4 / IPv6-like literals (`0xdeadbeef`, `127.0.0.1`,
  `fe80::1`).
- Identifier-shaped tokens that look like function calls or
  package names (`foo::bar::baz`, `foo(args)`, `@scope/pkg`).
- `SCREAMING_SNAKE_CASE` constants.

### Modes

| Mode | What it tunes |
|---|---|
| `concise` | Aggressive trim. Strips every buzzword + fluff adjective. Default. |
| `casual` | Concise + contractions (`it is` → `it's`, `do not` → `don't`). |
| `professional` | Strips buzzwords but keeps formal connectors and adjectives. |
| `developer` | Terse and technical. No pleasantries, no fluff, no contractions. |
| `social` | Short sentences + contractions. Good for Twitter / Mastodon. |
| `github-readme` | README-friendly: keeps blank lines and headings, strips filler. |

### Examples

```bash
# Inline rewrite
crux humanize --mode concise --input "In conclusion, we leverage \
    the robust pipeline to utilize crux::config::Config and run \
    \`cargo build\` at https://example.com. I hope this helps!"
# → We use the pipeline to use crux::config::Config and run
#   `cargo build` at https://example.com.

# Whole-file rewrite
crux humanize --mode developer --file output.md > clean.md

# Pipe stdin
cat answer.txt | crux humanize --mode social

# JSON output (text + before/after stats)
crux humanize --mode casual --input "It is great." --json
# {
#   "mode": "casual",
#   "text": "It's great.",
#   "stats": { "original_chars": 12, "rewritten_chars": 11, ... }
# }

# Stderr stats footer (does not pollute stdout for piping)
crux humanize --mode concise --file output.md --stats
```

The rule tables live in
`crates/crux-humanizer/src/rules.rs` — adding a new strike phrase
or word substitution is a one-line change with a co-located test.

---

## Comment Hygiene / Slop Guard

`crux hygiene comments` is a deterministic scanner that flags
AI-style verbose comments — decorative banners, long `//!` module
docs, `Goal:` / `Public surface:` blocks, `Pattern adapted from …`
references, `Layer N` labels, and marketing fluff (`revolutionary`,
`cutting-edge`, `seamlessly`, `robust and scalable`, …). The
companion `--fix` mode rewrites the offending lines in place
without ever touching code.

Like every other CRUX layer, the rewrite is **local and
deterministic** — no LLM, no network. Same input always produces
the same output, so it is safe to run from CI hooks.

### Manual usage

```bash
crux hygiene comments --check          # scan; exit 1 on any violation
crux hygiene comments --fix            # apply auto-fix in place
crux hygiene comments --check --json   # machine-readable report
crux hygiene comments --check --path src/lib.rs --path src/main.rs
                                       # scan only the given files
```

The check tool walks the project root (`--root` overrides), reads
Rust / TOML / Markdown / YAML / JS / TS / Python files, and skips
generated artefacts (`@generated` / "do not edit" headers,
`target/`, `.git/`, `node_modules/`, `Cargo.lock`,
`package-lock.json`, …).

### Build usage

```bash
crux build                             # hygiene check + `cargo build` if clean
crux build --skip-hygiene -- --release # escape hatch + extra cargo args
```

`crux build` is a thin wrapper: it runs the same scan, aborts the
build on any violation, then hands off to `cargo build` with any
arguments after `--`. Use `--skip-hygiene` when you need a build
without running the guard (CI escape hatch).

### Agent hook usage

Agents that support hooks (currently **Claude Code**) can run the
hygiene check automatically after every Edit / Write / MultiEdit.
The hook is **opt-in** and **warn-only** — it never auto-rewrites
files.

```bash
# Register the hook alongside the regular CRUX setup.
crux setup claude-code --enable-hygiene-hook

# Remove it later.
crux setup claude-code --disable-hygiene-hook
```

This writes (or drops) a `PostToolUse` entry in
`~/.claude/settings.json` that runs:

```
crux hygiene comments --check --changed-from-stdin
```

The `--changed-from-stdin` flag makes the CLI read the Claude Code
PostToolUse JSON on stdin and scan **only the file that was just
edited**, instead of walking the whole repo. Tools other than Edit
/ Write / MultiEdit / NotebookEdit are ignored (exit 0).

Behaviour:

- Exit 2 + violation report on stderr → Claude Code surfaces the
  warning to the model so the agent sees it and can decide to
  clean up before moving on. (Manual / CI invocations still exit 1
  on violation and print to stdout, matching the existing
  `crux hygiene comments --check` and `crux build` contract.)
- Exit 0 on clean files, unsupported tools, or empty payloads — so
  a missing file path never blocks a tool call.
- **No auto-fix.** Run `crux hygiene comments --fix` manually if
  you want the deterministic rewrite.

Pass `--dry-run` to `crux setup` first if you want to preview the
`settings.json` diff without writing anything. Agents that do not
support hooks (Cursor, Windsurf, Cline, Zed, Claude Desktop,
OpenClaw, Hermes) silently ignore the two flags.

### What `--fix` does

- Drops decorative banner comments (`// ────────`, `# ====…`).
- Removes `Goal:` and `Public surface:` doc-comment blocks
  (header line + the bullet/blank lines that follow it).
- Compresses long Rust module-doc runs to a single short sentence
  (the first non-empty `//!` line in the run is kept).
- **Never** modifies code lines, fenced code blocks in markdown,
  `// SAFETY:` / `// SECURITY:` / `// WARNING:` / `// TODO:`
  comments, or any markdown source file.

### What it does *not* fix

`marketing-phrase`, `pattern-adapted-from`, and `layer-label`
violations are reported but never auto-rewritten — those need
human judgement to keep the surrounding sentence meaningful. Run
`crux humanize --mode developer` on the file if you want a
deterministic prose rewrite as well.

The rule tables live in
`crates/crux-l12-hygiene/src/rules.rs` — adding a new banner
character or marketing phrase is a one-line change with a
co-located test.

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
│   ├── crux-l12-hygiene/      # comment hygiene / slop guard (scan + fix)
│   ├── crux-humanizer/        # local rewrite of AI prose → human-sounding text
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
