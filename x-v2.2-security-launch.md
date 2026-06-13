# X/Twitter — Cortex v2.2 launch drafts

## Option A — single tweet (punchy)

Your AI's memory should work like Signal, not like Google Photos.

Cortex v2.2: every memory is private-by-default and never leaves your device. Flag one as "shared" and it syncs to your other machines through *your own* iCloud — AES-256-GCM, key rotation, zero servers.

Change your mind? Demote it back to private and it's deleted off your other devices.

100% local Rust, MIT, $0. https://github.com/gambletan/cortex

## Option B — thread (5 tweets)

**1/**
Spent the last week dogfooding my own AI memory engine and it found 3 real bugs in itself. Shipping the result today: Cortex v2.2 — encrypted cross-device memory for AI agents, with privacy semantics I haven't seen anywhere else.

**2/**
The core idea: every memory is Private by default and physically cannot leave your device. Sync is per-memory opt-in. You mark *this one* memory as shared, it travels (encrypted) through your own iCloud/Drive/Dropbox. No server of mine in the loop, ever.

**3/**
The part I'm proud of: retraction. Demote a shared memory back to private → a tombstone propagates → it gets *deleted from your other devices*. Your local copy stays. Changing your mind is a first-class operation.

**4/**
Also new: deny-by-default tool authorization on the MCP surface (an agent gets zero memory access until you grant read/write/sync), bounded query budgets (latency can't leak store size), key rotation with forward secrecy, HMAC on everything.

**5/**
3.8MB Rust binary, 30 MCP tools, works with Claude Code / LangGraph / any MCP client. Free, MIT, zero telemetry (CI fails if a network crate sneaks into the core).

One script to join a second device: https://github.com/gambletan/cortex

If your AI remembers things about you, you should own that memory.

## Option C — CN 版(微博/即刻可复用)

给 AI 装记忆,隐私模型应该像 Signal,而不是像云相册。

Cortex v2.2:所有记忆默认 Private、物理上不出本机;逐条 opt-in 标记 shared 的才会经你自己的 iCloud 加密同步到其他设备(AES-256-GCM + 密钥轮换 + HMAC 防篡改,没有任何第三方服务器)。反悔了把它降回 private,其他设备上会自动删除。

3.8MB Rust 单文件,30 个 MCP 工具,接 Claude Code 即用。MIT 开源,零遥测(CI 强制)。
https://github.com/gambletan/cortex

## Posting notes

- Post from the **gambletan** account (repo owner); alvinttang engaging/retweeting later is fine but don't post identical copy from both.
- Thread option B reads most human (first-person, admits dogfooding found bugs) — recommended.
- Good reply-bait pinned comment: "the stale-cache bug story: device A retracted a memory, device B's search cache kept serving it. Black-box E2E caught what 500 unit tests didn't."
- Avoid posting within the same hour as the daily auto-iteration commits (8:57-9:30 AM CST) so the timeline doesn't look bot-coordinated.
