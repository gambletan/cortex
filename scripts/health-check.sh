#!/bin/bash
# Cortex service correctness / health monitor.
# Reports PASS/FAIL across the signals that define "the service is correct":
#   1. main has not diverged from origin/main (the fork incident on 2026-06-11)
#   2. full cortex-core suite (lib + integration) is green
#   3. build is warning-clean
#   4. lib clippy is clean
#   5. the self-evolution loop (launchd) is healthy and its last run was sane
# Exit 0 = HEALTHY, non-zero = NEEDS-ATTENTION. Safe to run on a cron/loop.

set -uo pipefail  # intentionally NOT -e: run every check, then report.

REPO_DIR="/Users/alvinclaw/work/cortex"
export PATH="/Users/alvinclaw/.local/bin:/Users/alvinclaw/.cargo/bin:$PATH"
cd "$REPO_DIR" || { echo "❌ repo dir missing: $REPO_DIR"; exit 2; }

FAIL=0
echo "════════ Cortex health @ $(date) ════════"

# 1. No divergence from origin/main.
git fetch origin main -q 2>/dev/null
LOCAL=$(git rev-parse main 2>/dev/null)
REMOTE=$(git rev-parse origin/main 2>/dev/null)
if [ "$LOCAL" = "$REMOTE" ]; then
  echo "✅ git      : main == origin/main (${LOCAL:0:9})"
else
  echo "⚠️  git      : DIVERGED — local ${LOCAL:0:9} vs origin ${REMOTE:0:9}"
  FAIL=1
fi
DIRTY=$(git status --porcelain --untracked-files=no)
[ -n "$DIRTY" ] && { echo "⚠️  git      : working tree has uncommitted tracked changes"; FAIL=1; }

# 2. Full test suite (lib + integration).
TLOG=$(mktemp)
if cargo test -p cortex-core >"$TLOG" 2>&1; then
  PASS=$(grep -E "test result: ok" "$TLOG" | awk '{s+=$4} END{print s}')
  echo "✅ tests    : ${PASS:-?} passed, 0 failed (lib + integration)"
else
  echo "❌ tests    : FAILURES —"
  grep -E "FAILED|panicked|test result: FAILED|error\[" "$TLOG" | head -8 | sed 's/^/             /'
  FAIL=1
fi
rm -f "$TLOG"

# 3. Build warnings.
W=$(cargo build -p cortex-core 2>&1 | grep -c "^warning")
if [ "$W" -eq 0 ]; then echo "✅ build    : warning-clean"; else echo "⚠️  build    : $W warning(s)"; FAIL=1; fi

# 4. Lib clippy.
C=$(cargo clippy -p cortex-core --lib 2>&1 | grep -c "^warning")
if [ "$C" -eq 0 ]; then echo "✅ clippy   : lib clean"; else echo "⚠️  clippy   : $C lib lint(s)"; FAIL=1; fi

# 5. HTTP service correctness — boot cortex-http and smoke-test /health + /v1/stats.
if [ -x "$REPO_DIR/scripts/probe-http.sh" ]; then
  if HTTP_OUT=$(bash "$REPO_DIR/scripts/probe-http.sh" 2>&1); then
    echo "$HTTP_OUT" | grep -E "^✅ http"
  else
    echo "$HTTP_OUT" | grep -E "http" | head -3 | sed 's/^/   /'
    FAIL=1
  fi
fi

# 6. Self-evolution loop health (informational — does not fail the check).
STATUS=$(launchctl list 2>/dev/null | awk '/com.cortex.auto-iterate/{print $2}')
echo "ℹ️  loop     : launchd registered, last-exit=${STATUS:-<not loaded>}"
LATEST=$(ls -1t "$REPO_DIR"/.logs/iteration-*.log 2>/dev/null | head -1)
if [ -n "$LATEST" ]; then
  RESULT=$(grep -E "complete|No changes|RED|❌|Review branch:" "$LATEST" | tail -1)
  echo "ℹ️  loop     : latest run $(basename "$LATEST") → ${RESULT:-<in progress / no terminal marker>}"
fi

echo "════════ OVERALL: $([ $FAIL -eq 0 ] && echo '✅ HEALTHY' || echo '⚠️  NEEDS-ATTENTION') ════════"
exit $FAIL
