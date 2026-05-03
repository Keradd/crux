#!/usr/bin/env bash
# CRUX one-shot installer.
#
# Usage:
#   curl -sSf https://.../install.sh | bash           # user install (~/.local/bin)
#   curl -sSf https://.../install.sh | bash -s -- --system  # system install (/usr/local/bin, needs sudo)
#   bash scripts/install.sh [--system] [--no-bootstrap] [--no-agents] [--no-index]
#
# What it does:
#   1. Verify `cargo` is installed (prints rustup one-liner if not).
#   2. `cargo build --release` in the repo root.
#   3. Install the binary to:
#        - $HOME/.local/bin/crux     (default — no sudo required)
#        - /usr/local/bin/crux       (with --system — uses sudo if needed)
#   4. Unless --no-bootstrap, run `crux init --non-interactive --setup-agents --index`
#      in the user's current working directory so the fresh install is
#      usable in one command.
#
# Exit codes:
#   0 — success
#   1 — cargo missing
#   2 — build failed
#   3 — install path not writable
#   4 — bootstrap failed

set -euo pipefail

# ─── args ─────────────────────────────────────────────────────────────
SYSTEM_INSTALL=0
RUN_BOOTSTRAP=1
BOOTSTRAP_AGENTS=1
BOOTSTRAP_INDEX=1
for arg in "$@"; do
    case "$arg" in
        --system)        SYSTEM_INSTALL=1 ;;
        --no-bootstrap)  RUN_BOOTSTRAP=0 ;;
        --no-agents)     BOOTSTRAP_AGENTS=0 ;;
        --no-index)      BOOTSTRAP_INDEX=0 ;;
        -h|--help)
            sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown arg: $arg" >&2
            exit 64
            ;;
    esac
done

# ─── helpers ──────────────────────────────────────────────────────────
info()  { printf '\033[1;34m>>>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }
fail()  { printf '\033[1;31mxxx\033[0m %s\n' "$*" >&2; }

# Resolve the repo root. When this script is piped via curl the caller
# must have the repo on disk (or we exit with a hint to clone it). When
# run from the repo itself we locate via the script's own path.
resolve_repo_root() {
    local here
    here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
    # scripts/install.sh → repo root is one level up.
    if [[ -f "$here/../Cargo.toml" ]]; then
        cd "$here/.." && pwd -P
        return
    fi
    # Running via stdin (curl | bash) — no reliable way to find source.
    # Fall back to $CRUX_SRC if set.
    if [[ -n "${CRUX_SRC:-}" && -f "$CRUX_SRC/Cargo.toml" ]]; then
        echo "$CRUX_SRC"
        return
    fi
    fail "cannot locate CRUX source."
    fail "either run this script from inside the repo, or set CRUX_SRC=/path/to/crux before piping it into bash."
    exit 1
}

REPO_ROOT="$(resolve_repo_root)"
info "repo root: $REPO_ROOT"

# ─── check cargo ──────────────────────────────────────────────────────
if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not installed. Install Rust first:"
    echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
info "cargo: $(cargo --version)"

# ─── build ────────────────────────────────────────────────────────────
info "building crux (release) — this may take ~45s on a cold cache…"
if ! (cd "$REPO_ROOT" && cargo build --release --quiet); then
    fail "cargo build --release failed."
    exit 2
fi
BIN_SRC="$REPO_ROOT/target/release/crux"
if [[ ! -x "$BIN_SRC" ]]; then
    fail "expected $BIN_SRC to exist after build."
    exit 2
fi
info "built: $BIN_SRC ($(du -h "$BIN_SRC" | cut -f1))"

# ─── install ──────────────────────────────────────────────────────────
if [[ $SYSTEM_INSTALL -eq 1 ]]; then
    DEST_DIR="/usr/local/bin"
    info "installing to $DEST_DIR (system scope — sudo may prompt)…"
    if [[ -w "$DEST_DIR" ]]; then
        install -m 0755 "$BIN_SRC" "$DEST_DIR/crux"
    elif command -v sudo >/dev/null 2>&1; then
        sudo install -m 0755 "$BIN_SRC" "$DEST_DIR/crux"
    else
        fail "$DEST_DIR is not writable and sudo is not installed."
        exit 3
    fi
else
    DEST_DIR="$HOME/.local/bin"
    mkdir -p "$DEST_DIR"
    info "installing to $DEST_DIR (user scope — no sudo)…"
    install -m 0755 "$BIN_SRC" "$DEST_DIR/crux"

    # PATH hint if ~/.local/bin is not already on PATH.
    case ":$PATH:" in
        *":$DEST_DIR:"*)
            : # already on PATH
            ;;
        *)
            warn "$DEST_DIR is not on your PATH."
            warn "add this to your shell rc (.bashrc / .zshrc):"
            echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
            ;;
    esac
fi

info "installed: $DEST_DIR/crux"
"$DEST_DIR/crux" --version

# ─── bootstrap ────────────────────────────────────────────────────────
if [[ $RUN_BOOTSTRAP -eq 0 ]]; then
    info "bootstrap skipped (--no-bootstrap)."
    info "run \`crux init --non-interactive --setup-agents --index\` in your project to finish setup."
    exit 0
fi

CWD="$(pwd -P)"
info "bootstrapping CRUX in: $CWD"

BOOTSTRAP_FLAGS=("init" "--non-interactive" "--profile" "coding" "--dir" "$CWD")
[[ $BOOTSTRAP_AGENTS -eq 1 ]] && BOOTSTRAP_FLAGS+=("--setup-agents")
[[ $BOOTSTRAP_INDEX   -eq 1 ]] && BOOTSTRAP_FLAGS+=("--index")

if ! "$DEST_DIR/crux" "${BOOTSTRAP_FLAGS[@]}"; then
    fail "bootstrap failed — you can re-run manually:"
    echo "    crux ${BOOTSTRAP_FLAGS[*]}"
    exit 4
fi

cat <<EOF

╔═════════════════════════════════════════════════════════════╗
║  CRUX ready.                                                ║
║                                                             ║
║  Next steps:                                                ║
║    - restart your agent (if not already hot-reloading MCP)  ║
║    - try: crux audit                                        ║
║    - try: crux find <symbol>                                ║
║    - try: crux search "<query>"                             ║
║                                                             ║
║  Docs: $REPO_ROOT/README.md
╚═════════════════════════════════════════════════════════════╝
EOF
