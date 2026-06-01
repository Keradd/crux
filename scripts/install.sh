#!/usr/bin/env bash
# CRUX install script — builds from source.
#
# Usage:
#   bash scripts/install.sh [--system] [--no-bootstrap] [--no-agents] [--no-index]
#
# Options:
#   --system        Install to /usr/local/bin (sudo if needed)
#   --no-bootstrap  Build + install only, skip init/setup/index
#   --no-agents     During bootstrap, skip agent registration
#   --no-index      During bootstrap, skip initial index
#
# Exit codes:
#   0 — success
#   1 — cargo missing
#   2 — build failed
#   3 — install path not writable
#   4 — bootstrap failed

set -euo pipefail

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

info()  { printf '\033[1;34m>>>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m!!!\033[0m %s\n' "$*" >&2; }
fail()  { printf '\033[1;31mxxx\033[0m %s\n' "$*" >&2; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
info "repo root: $REPO_ROOT"

if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is not installed. Install Rust first:"
    echo "    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
info "cargo: $(cargo --version)"

info "building crux (release)…"
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
    info "installing to $DEST_DIR (user scope)…"
    install -m 0755 "$BIN_SRC" "$DEST_DIR/crux"
    case ":$PATH:" in
        *":$DEST_DIR:"*) : ;;
        *)
            warn "$DEST_DIR is not on your PATH."
            warn "add this to your shell rc (.bashrc / .zshrc):"
            echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
            ;;
    esac
fi

info "installed: $DEST_DIR/crux"
"$DEST_DIR/crux" --version

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

CRUX ready.

Next steps:
  - restart your agent (if not already hot-reloading MCP)
  - try: crux audit
  - try: crux find <symbol>
  - try: crux search "<query>"

Docs: $REPO_ROOT/README.md
EOF
