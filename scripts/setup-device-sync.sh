#!/usr/bin/env bash
# Cortex — one-shot encrypted iCloud sync setup for a new device (macOS).
#
# What it does:
#   1. Installs cortex-mcp-server (builds from this repo if not already installed).
#   2. Waits for the iCloud `cortex-sync` folder to come down from iCloud Drive.
#   3. Joins the encrypted sync with your passphrase — full restore happens on join,
#      the passphrase goes into this device's login Keychain, and sync auto-resumes
#      on every restart from then on.
#
# Usage:
#   ./scripts/setup-device-sync.sh
#   CORTEX_SYNC_PASSPHRASE=... ./scripts/setup-device-sync.sh   # non-interactive
#
# Privacy notes: only memories you explicitly mark `shared` ever leave a device;
# everything is AES-256-GCM encrypted client-side before it touches iCloud.

set -euo pipefail

BOLD=$'\033[1m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
say()  { printf '%s\n' "${BOLD}${1}${RESET}"; }
ok()   { printf '%s\n' "${GREEN}✓ ${1}${RESET}"; }
warn() { printf '%s\n' "${YELLOW}! ${1}${RESET}"; }
die()  { printf '%s\n' "${YELLOW}✗ ${1}${RESET}" >&2; exit 1; }

[ "$(uname)" = "Darwin" ] || die "This script targets macOS (iCloud Drive). For Linux, use a synced folder + 'sync enable' manually."

DB_PATH="${CORTEX_DB_PATH:-$HOME/.cortex/memory.db}"
BIN="$HOME/.local/bin/cortex-mcp-server"
ICLOUD_DIR="$HOME/Library/Mobile Documents/com~apple~CloudDocs"
SYNC_DIR="$ICLOUD_DIR/cortex-sync"

# ── 1. Binary ────────────────────────────────────────────────────────────────
say "[1/3] cortex-mcp-server binary"
if command -v cortex-mcp-server >/dev/null 2>&1; then
    BIN="$(command -v cortex-mcp-server)"
    ok "found: $BIN ($($BIN --version))"
elif [ -x "$BIN" ]; then
    ok "found: $BIN ($($BIN --version))"
else
    REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
    [ -f "$REPO_DIR/Cargo.toml" ] || die "binary not installed and no repo found — clone github.com/gambletan/cortex first"
    command -v cargo >/dev/null 2>&1 || die "cargo not found — install Rust (https://rustup.rs) and re-run"
    say "    building from $REPO_DIR (one-time, ~2 min)..."
    (cd "$REPO_DIR" && cargo build --release -p cortex-mcp-server)
    mkdir -p "$HOME/.local/bin"
    rm -f "$BIN"  # fresh inode: avoids macOS code-sign cache SIGKILL on overwrite
    cp "$REPO_DIR/target/release/cortex-mcp-server" "$BIN"
    ok "installed: $BIN ($($BIN --version))"
fi

# ── 2. iCloud sync folder ────────────────────────────────────────────────────
say "[2/3] waiting for iCloud Drive to deliver cortex-sync"
[ -d "$ICLOUD_DIR" ] || die "iCloud Drive folder not found — sign into iCloud and enable iCloud Drive first"

WAIT=120
while [ ! -f "$SYNC_DIR/manifest.json" ] && [ "$WAIT" -gt 0 ]; do
    # Nudge iCloud to download if it is still a placeholder
    brctl download "$SYNC_DIR" >/dev/null 2>&1 || true
    sleep 3; WAIT=$((WAIT - 3))
    printf '.'
done
printf '\n'
[ -f "$SYNC_DIR/manifest.json" ] || die "cortex-sync/manifest.json never appeared. Is sync enabled on your other device, and has iCloud finished syncing?"
ok "sync folder present: $SYNC_DIR"

# ── 3. Join (full restore) ───────────────────────────────────────────────────
say "[3/3] joining encrypted sync"
PASS="${CORTEX_SYNC_PASSPHRASE:-}"
if [ -z "$PASS" ]; then
    printf 'Sync passphrase (input hidden): '
    read -rs PASS; printf '\n'
fi
[ -n "$PASS" ] || die "empty passphrase"

mkdir -p "$(dirname "$DB_PATH")"
"$BIN" "$DB_PATH" sync enable --provider icloud --passphrase "$PASS"

# Deny-by-default capability policy (read+write+sync) if none exists yet
CAPS="$(dirname "$DB_PATH")/capabilities.json"
if [ ! -f "$CAPS" ]; then
    printf '{\n  "version": 1,\n  "grants": ["read", "write", "sync"]\n}\n' > "$CAPS"
    ok "capability policy created: $CAPS"
fi

echo
"$BIN" "$DB_PATH" stats
echo
ok "Done. Shared memories now flow both ways automatically (~30s)."
ok "Passphrase is in this device's login Keychain — sync auto-resumes on restart."
echo
say "To use with Claude Code, register the MCP server:"
printf '  claude mcp add cortex-memory -- %s %s\n' "$BIN" "$DB_PATH"
