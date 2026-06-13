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

# ── 4. Claude Code integration (optional; skip with CORTEX_NO_CLAUDE_SETUP=1) ──
if [ -z "${CORTEX_NO_CLAUDE_SETUP:-}" ] && { [ -d "$HOME/.claude" ] || command -v claude >/dev/null 2>&1; }; then
    say "[4/4] Claude Code integration (auto-recall hook + memory protocol)"
    mkdir -p "$HOME/.claude/hooks"

    cat > "$HOME/.claude/hooks/cortex-recall.sh" << 'HOOK'
#!/usr/bin/env bash
# Cortex auto-recall: inject a long-term memory digest at session start.
BIN="$HOME/.local/bin/cortex-mcp-server"
DB="$HOME/.cortex/memory.db"
[ -x "$BIN" ] && [ -f "$DB" ] || exit 0
DIGEST=$("$BIN" "$DB" search "user preferences rules projects identity decisions" --limit 8 2>/dev/null)
case "$DIGEST" in *"No results"*|"") exit 0;; esac
echo "## Cortex long-term memory (auto-recalled at session start)"
echo "$DIGEST"
echo
echo "(Recall more with the cortex-memory MCP memory_search tool. Before the session ends, ingest durable new facts via memory_ingest; default privacy=private, use privacy=shared only for cross-device-worthy memories.)"
HOOK
    chmod +x "$HOME/.claude/hooks/cortex-recall.sh"

    # Merge the SessionStart hook into settings.json (idempotent, never clobbers)
    python3 - << 'PY'
import json, os
p = os.path.expanduser("~/.claude/settings.json")
d = {}
if os.path.exists(p):
    with open(p) as f:
        d = json.load(f)
hooks = d.setdefault("hooks", {}).setdefault("SessionStart", [])
cmd = "bash ~/.claude/hooks/cortex-recall.sh"
present = any(h.get("command") == cmd for g in hooks for h in g.get("hooks", []))
if not present:
    hooks.append({"hooks": [{"type": "command", "command": cmd, "timeout": 10,
                              "statusMessage": "Recalling Cortex long-term memory"}]})
    with open(p, "w") as f:
        json.dump(d, f, indent=2)
    print("    hook merged into ~/.claude/settings.json")
else:
    print("    hook already present — skipped")
PY

    # Memory capture protocol in global CLAUDE.md (idempotent)
    CMD_MD="$HOME/.claude/CLAUDE.md"
    if ! grep -q "Long-term Memory Protocol (Cortex)" "$CMD_MD" 2>/dev/null; then
        cat >> "$CMD_MD" << 'PROTO'

# Long-term Memory Protocol (Cortex)

A SessionStart hook auto-injects a Cortex memory digest. Close the loop on the capture side:

- **Recall**: when a task references past work, people, or preferences not in the injected digest, call `memory_search` (cortex-memory MCP) before asking the user.
- **Capture**: before a substantive session ends, distill durable new knowledge into Cortex via `memory_ingest` (channel `claude-code`): identity/preferences/working style, decisions + rationale, project state changes, behavioral corrections. Never store secrets or repo-derivable facts.
- **Privacy**: default `private`. Use `privacy: "shared"` only for cross-device-worthy memories.
PROTO
        ok "memory protocol appended to ~/.claude/CLAUDE.md"
    fi

    # Register the MCP server (user scope) if the claude CLI is available
    if command -v claude >/dev/null 2>&1; then
        if ! claude mcp list 2>/dev/null | grep -q "cortex-memory"; then
            claude mcp add cortex-memory --scope user -- "$BIN" "$DB_PATH" \
                && ok "MCP server registered (cortex-memory)" \
                || warn "MCP registration failed — run manually: claude mcp add cortex-memory -- $BIN $DB_PATH"
        else
            ok "MCP server already registered"
        fi
    else
        warn "claude CLI not found — register later: claude mcp add cortex-memory -- $BIN $DB_PATH"
    fi
    ok "Claude Code will auto-recall memory from the next session (type /hooks once or restart to activate now)"
else
    say "To use with Claude Code, register the MCP server:"
    printf '  claude mcp add cortex-memory -- %s %s\n' "$BIN" "$DB_PATH"
fi
