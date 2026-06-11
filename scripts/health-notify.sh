#!/bin/bash
# Durable correctness monitor: run the health check, log it, and fire a macOS
# notification ONLY when the service is NEEDS-ATTENTION. Driven by launchd
# (com.cortex.health-monitor). Read-only — never edits code or touches branches.

REPO_DIR="/Users/alvinclaw/work/cortex"
export PATH="/Users/alvinclaw/.cargo/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
LOG="$REPO_DIR/.logs/health-$(date +%Y-%m-%d).log"
mkdir -p "$REPO_DIR/.logs"

{
  echo ""
  if bash "$REPO_DIR/scripts/health-check.sh"; then
    echo "[monitor] $(date): HEALTHY"
  else
    echo "[monitor] $(date): NEEDS-ATTENTION — notifying."
    osascript -e 'display notification "Cortex service is NEEDS-ATTENTION — see .logs/health-*.log" with title "⚠️ Cortex health" sound name "Basso"' 2>/dev/null || true
  fi
} >> "$LOG" 2>&1
