#!/usr/bin/env bash
# Project SessionStart hook — injects a standing directive so each Cortex session
# auto-launches the cortex-user-skeptic agent (in the background) when there is real
# work to challenge. Outputs SessionStart additionalContext JSON; never blocks.
#
# Gating: only nudges when the working tree has uncommitted changes OR there were
# commits in the last day — so trivial read-only sessions aren't charged for a
# multi-minute adversarial run.
set -euo pipefail
cd "$(dirname "$0")/../.." 2>/dev/null || exit 0

recent_commit=$(git log --since="24 hours ago" --oneline 2>/dev/null | head -1 || true)
dirty=$(git status --porcelain 2>/dev/null | head -1 || true)

if [ -z "$recent_commit" ] && [ -z "$dirty" ]; then
  exit 0   # nothing to challenge; stay silent
fi

read -r -d '' CONTEXT <<'EOF' || true
Cortex session: a `cortex-user-skeptic` agent is available (an isolated, adversarial
real-user persona that pressure-tests Cortex from the user's point of view). There are
recent or uncommitted changes in this repo. Early in this session, launch it in the
BACKGROUND (Agent tool, subagent_type "cortex-user-skeptic", run_in_background: true) so
its brutally-honest report is ready by the time you finish your changes. Do not block on
it; keep working and surface its findings when it completes. Skip only if this session is
a trivial one-off query with no Cortex work.
EOF

# Emit as SessionStart additionalContext (escaped via jq if available, else manual).
if command -v jq >/dev/null 2>&1; then
  jq -n --arg c "$CONTEXT" \
    '{hookSpecificOutput:{hookEventName:"SessionStart", additionalContext:$c}}'
else
  esc=$(printf '%s' "$CONTEXT" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
  printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":%s}}\n' "$esc"
fi
