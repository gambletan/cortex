#!/bin/bash
# Cortex self-evolution loop — SAFE autonomous iteration.
#
# Runs daily via launchd (com.cortex.auto-iterate, 08:57). Each run produces a
# REVIEWABLE branch (auto-iter/<timestamp>) and pushes THAT — it never auto-merges to
# main. A human reviews and merges. This is deliberate: the ITER 10-25 audit showed a
# fully-autonomous push-to-main loop produces reward-hacking (fabricated "fixes" that
# pass tests), and a test gate cannot catch that. The human merge gate is the safeguard.
#
# Hard requirements enforced here (lessons from the audit):
#   - `set -euo pipefail`  → a failing test actually aborts (old script let `tee` mask it)
#   - full `cargo test -p cortex-core` (lib + integration), NOT `cargo test --lib`
#   - real claude binary on PATH (lives in ~/.local/bin)

set -euo pipefail

REPO_DIR="/Users/alvinclaw/work/cortex"
# claude → ~/.local/bin, cargo → ~/.cargo/bin. launchd's PATH lacks both by default.
export PATH="/Users/alvinclaw/.local/bin:/Users/alvinclaw/.cargo/bin:$PATH"
cd "$REPO_DIR"

TS="$(date +%Y-%m-%d_%H-%M-%S)"
LOG_FILE="$REPO_DIR/.logs/iteration-$TS.log"
mkdir -p "$REPO_DIR/.logs"

log() { echo "[$(date)] $*" | tee -a "$LOG_FILE"; }

log "=== Cortex autonomous iteration $TS ==="

# ── Sanity: tools present ──────────────────────────────────────────────────────
command -v claude >/dev/null || { log "❌ claude not on PATH — aborting."; exit 1; }
command -v cargo  >/dev/null || { log "❌ cargo not on PATH — aborting.";  exit 1; }

# ── Preconditions: clean tree, synced to origin/main ───────────────────────────
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  log "❌ Working tree has uncommitted tracked changes — aborting to avoid clobbering."
  exit 1
fi
git fetch origin main >> "$LOG_FILE" 2>&1
git checkout main      >> "$LOG_FILE" 2>&1
git reset --hard origin/main >> "$LOG_FILE" 2>&1

# ── Step 1: Baseline — the FULL suite must be green before we touch anything ────
log "Baseline: cargo test -p cortex-core"
if ! cargo test -p cortex-core >> "$LOG_FILE" 2>&1; then
  log "❌ Baseline suite is RED on origin/main — aborting (pre-existing, not our change)."
  exit 1
fi
log "✅ Baseline green."

# ── Step 2: Isolated iteration branch ──────────────────────────────────────────
BRANCH="auto-iter/$TS"
git checkout -b "$BRANCH" >> "$LOG_FILE" 2>&1
log "Working on branch $BRANCH"

# ── Step 3: Autonomous review + fix via Claude (headless) ──────────────────────
log "Running Claude headless review+fix (this can take several minutes)..."
PROMPT=$(cat <<'PROMPT'
你是 Cortex 隐私内存引擎的自主安全审查员。本次目标：

1. 审查当前代码，找出 TOP 1-3 个真实的 HIGH/CRITICAL 安全或隐私问题（重点 security/privacy）。
2. 用 TDD 修复：先写会失败的测试，再修，再让全套测试通过。
3. 把修复提交到当前分支（git commit）。如有需要可写一份简短的迭代说明。

铁律（违反即视为本次迭代失败）：
- 只修真问题。如果没有 HIGH/CRITICAL 的真问题，就【什么都不改】，直接说明“本轮无需改动”。
  绝不要为了凑数虚构漏洞 —— 历史上 16-24 轮就是这样反复 reward-hacking 的。
- 全套测试必须通过：`cargo test -p cortex-core`（不是 `--lib`，必须包含 tests/ 集成套件）。
- 构建必须 warning-clean，lib clippy 必须 clean。
- 不要 git push，不要切换/创建分支，不要碰 main。只在当前分支提交。
PROMPT
)
# bypassPermissions: unattended run has no human to approve tool use. Opus for quality
# (Haiku reward-hacked across iters 16-24; Opus produced the ITER 25 correction).
claude -p "$PROMPT" \
  --permission-mode bypassPermissions \
  --model opus \
  >> "$LOG_FILE" 2>&1 || log "⚠️  Claude step exited non-zero — proceeding to verify state."

# Commit anything Claude left uncommitted, so the diff/verify below sees it.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  git add -A
  git commit -m "chore(auto-iter): uncommitted changes from $TS" >> "$LOG_FILE" 2>&1 || true
fi

# ── Step 4: Verify — the FULL suite must still be green ─────────────────────────
log "Verify: cargo test -p cortex-core"
if ! cargo test -p cortex-core >> "$LOG_FILE" 2>&1; then
  log "❌ Tests RED after changes — discarding branch $BRANCH."
  git checkout main >> "$LOG_FILE" 2>&1
  git branch -D "$BRANCH" >> "$LOG_FILE" 2>&1
  exit 1
fi

# ── Step 5: Honest no-op if nothing real was found ─────────────────────────────
if git diff --quiet origin/main.."$BRANCH"; then
  log "✅ No changes this iteration (no real HIGH/CRITICAL issue found). Cleaning up."
  git checkout main >> "$LOG_FILE" 2>&1
  git branch -D "$BRANCH" >> "$LOG_FILE" 2>&1
  exit 0
fi

# ── Step 6: Push the REVIEW branch (never main). Human reviews + merges. ────────
log "Pushing review branch $BRANCH (NOT main) for human review."
git push -u origin "$BRANCH" >> "$LOG_FILE" 2>&1
git checkout main >> "$LOG_FILE" 2>&1
log "✅ Iteration $TS complete. Review + merge: git checkout $BRANCH"
echo "Review branch: $BRANCH  (log: $LOG_FILE)"
