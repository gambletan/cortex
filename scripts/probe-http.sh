#!/bin/bash
# HTTP service correctness probe: boot cortex-http on an ephemeral port against a throwaway
# DB, verify /health and /v1/stats respond correctly, then tear down. This validates the
# server layer actually boots and serves — beyond the library-level test suite.
# Exit 0 = HTTP layer correct, non-zero = problem. Read-only (throwaway DB, killed on exit).

set -uo pipefail
# Derive the repo root from this script's own location (scripts/ is under the root), so the
# probe always validates the checkout it ships with — never a hard-coded other path.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
export PATH="$HOME/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
cd "$REPO_DIR" || { echo "❌ http: repo dir missing"; exit 2; }

DB="$(mktemp -u /tmp/cortex-probe-XXXXXX.db)" || true
SRVLOG="$(mktemp /tmp/cortex-probe-srv-XXXXXX.log)" || true
# Guard: if mktemp ever yields an empty path, a globbed rm could expand to `rm -f *` in the
# repo cwd. Refuse to run rather than risk deleting tracked files.
if [ -z "$DB" ] || [ -z "$SRVLOG" ]; then
  echo "❌ http: failed to allocate temp paths — aborting (refusing unsafe cleanup)"; exit 2
fi
PID=""
cleanup() {
  [ -n "${PID:-}" ] && kill "$PID" 2>/dev/null
  # Explicit paths only (incl. SQLite -wal/-shm sidecars) — never a glob on an unchecked var.
  [ -n "${DB:-}" ] && rm -f "$DB" "$DB-wal" "$DB-shm"
  [ -n "${SRVLOG:-}" ] && rm -f "$SRVLOG"
}
trap cleanup EXIT

# Build once and resolve the EXACT artifact path from Cargo's own JSON output — robust to
# CARGO_TARGET_DIR, CARGO_BUILD_TARGET, and target-triple subdirs (no path guessing).
BIN="$(cargo build -p cortex-http --message-format=json 2>>"$SRVLOG" \
  | python3 -c 'import sys, json
exe = ""
for line in sys.stdin:
    try:
        m = json.loads(line)
    except Exception:
        continue
    if (m.get("reason") == "compiler-artifact" and m.get("executable")
            and m.get("target", {}).get("name") == "cortex-http"):
        exe = m["executable"]
print(exe)')"
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  echo "❌ http: build failed or binary unresolved —"; tail -3 "$SRVLOG" 2>/dev/null | sed 's/^/    /'; exit 1
fi

# Choose a probe port: an explicit override, else an OS-assigned ephemeral port, else a scan.
pick_port() {
  if [ -n "${CORTEX_PROBE_PORT:-}" ]; then echo "$CORTEX_PROBE_PORT"; return; fi
  local p
  p="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()' 2>/dev/null || true)"
  if [ -z "$p" ]; then
    local cand
    for cand in $(seq 3399 3450); do
      lsof -iTCP:"$cand" -sTCP:LISTEN -P >/dev/null 2>&1 || { p="$cand"; break; }
    done
  fi
  echo "$p"
}

# Boot with retry. The OS-assigned port is released before the server binds it (a TOCTOU
# gap), so another process could steal it and our server would die on bind failure. We
# detect that death and retry on a fresh port. We do NOT retry an alive-but-unresponsive
# server — that is a genuine failure worth reporting, not a race.
READY=0; PORT=""
for _attempt in 1 2 3; do
  PORT="$(pick_port)"
  [ -n "$PORT" ] || { echo "❌ http: could not allocate a free probe port"; break; }
  : > "$SRVLOG"
  "$BIN" --port "$PORT" --host 127.0.0.1 --db "$DB" >"$SRVLOG" 2>&1 &
  PID=$!
  disown "$PID" 2>/dev/null || true  # silence the shell's "Terminated" notice on cleanup
  DIED=0
  for _ in $(seq 1 50); do
    if ! kill -0 "$PID" 2>/dev/null; then DIED=1; break; fi
    if curl -fsS --connect-timeout 1 --max-time 2 "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then READY=1; break; fi
    sleep 0.2
  done
  [ "$READY" = 1 ] && break
  kill "$PID" 2>/dev/null; PID=""
  [ "$DIED" = 1 ] || break   # alive but never answered = real failure; don't burn retries
done
if [ "$READY" != 1 ]; then
  echo "❌ http: server never became ready —"; tail -3 "$SRVLOG" 2>/dev/null | sed 's/^/    /'; exit 1
fi

FAIL=0
# /health correctness — must be the cortex engine reporting ok.
H=$(curl -fsS --connect-timeout 2 --max-time 5 "http://127.0.0.1:$PORT/health" 2>/dev/null)
if echo "$H" | grep -q '"status":"ok"' && echo "$H" | grep -q '"engine":"cortex"'; then
  echo "✅ http     : /health ok ($(echo "$H" | grep -oE '"version":"[^"]*"'))"
else
  echo "❌ http     : /health bad response: $H"; FAIL=1
fi
# /v1/stats correctness — exercises the engine read path; -f fails on any non-2xx.
if curl -fsS --connect-timeout 2 --max-time 5 "http://127.0.0.1:$PORT/v1/stats" >/dev/null 2>&1; then
  echo "✅ http     : /v1/stats 200"
else
  echo "❌ http     : /v1/stats did not return 2xx"; FAIL=1
fi

exit $FAIL
