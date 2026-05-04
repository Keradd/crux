# CRUX Install

CRUX ships as a single Rust binary. Three supported install paths.

## One-shot installer (recommended)

From a checkout of the repo. Builds release, installs to
`~/.local/bin`, auto-registers CRUX as an MCP server in every
detected agent, and runs the first-time L5 index and L6 reindex
in your CWD.

```bash
bash scripts/install.sh

# System scope — `/usr/local/bin`, sudo if needed:
bash scripts/install.sh --system

# Skip bootstrap (just build + install the binary):
bash scripts/install.sh --no-bootstrap

# Install but do not touch any agent config:
bash scripts/install.sh --no-agents
```

After `crux --version` works, every installed agent (Claude Code /
Desktop, Cursor, Windsurf, Cline, Zed) is already wired up as an
MCP server pointed at the current directory.

## Prebuilt binaries

Not shipped for v0.4.0 — releases are **source-only** while CRUX is
still pre-1.0. The
[GitHub Releases](https://github.com/Keradd/crux/releases) page
carries the signed tag and the CHANGELOG excerpt only; no tarballs
or `.sha256` checksums are uploaded.

Use the one-shot installer above or the [from-source](#from-source)
path below. A cross-platform binary matrix can be reinstated later
by restoring the `upload-assets` job in
`.github/workflows/release.yml`.

## From source

```bash
git clone https://github.com/Keradd/crux.git
cd crux

# Default build — offline, ~11 MB stripped, no network at runtime.
# Layer 6 ships the deterministic `HashEmbedder` for the dense ranker.
cargo build --release

# "crux-full" build — adds the ONNX-backed `FastEmbedder` (BGE-small-en-v1.5
# by default, ~30 MB model downloaded at first run from Hugging Face).
cargo build --release --features full   # alias for `--features fastembed`
```

After `--features full`, opt in via
`[layer.l6] embedding_provider = "fastembed"` in `~/.crux/config.toml`
and run `crux reindex --force` once to populate the new vectors.
Existing hash-indexed rows stay valid — they are partitioned by
`(provider, model, dim)` so switching back is a single config flip.
`crux doctor` flags the mismatch when the config selects `fastembed`
but the binary was built without the feature.

## Requirements

- Rust **1.85+**.
- SQLite is bundled via `rusqlite` — no system dependency.
- Optional: `--features crux-l7-sandbox/seccomp` enables Linux
  seccomp BPF syscall filtering (requires kernel ≥ 3.5).
- Optional: `--features full` enables the L6 fastembed embedder
  (~30 MB ONNX runtime + model archive, downloaded on first use).

## Activating inside your AI agent

```bash
crux setup                  # auto-detect every supported agent
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

`crux setup` is idempotent — re-running it is a no-op once the
entries exist. Use `--scope project` to write per-project configs,
`--no-hooks` / `--no-skill` to opt out of the Claude Code extras.
