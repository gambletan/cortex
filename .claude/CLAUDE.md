# Cortex Project Instructions

## Decision Protocol — get a Codex second opinion

Before handing the user a **substantive** decision (architecture, a release, a risky or
hard-to-reverse change, an ambiguous tradeoff, or a non-trivial design choice), first spawn
the **`codex-advisor`** agent with the decision + full inline context, and fold its
recommendation into what you present — note where Codex agrees or pushes back.

Skip it for trivial confirmations and fast back-and-forth (don't add a Codex round-trip to
every micro-step — that kills the loop). Use judgment: high-stakes or genuinely uncertain →
consult Codex; routine → proceed.

## Project Goal
Build the **most privacy-advanced memory engine** for personal AI agents.

**Mission**: 100% local, zero telemetry, cryptographically hardened, defense-in-depth security.

## Self-Evolution Mode

**Daily Auto-Iteration Enabled** (8:57 AM each day)

### Iteration Focus
- **Primary**: Security & privacy hardening (隐私方向最先进)
- **Method**: Autonomous code review → find issues → fix → test → commit
- **Scope**: TOP 3 critical issues per iteration

### Defense-in-Depth Principles
When fixing issues, apply protection at multiple layers:
1. **Query Layer** — Filter private data before returning
2. **Storage Layer** — Validate at database access
3. **Sync Layer** — Validate operations from peers
4. **Crypto Layer** — Protect sensitive data in transit and at rest

### What Gets Auto-Fixed
- Security vulnerabilities (HIGH/CRITICAL only)
- Privacy leaks (query, sync, cache)
- Memory safety issues (zeroization)
- Data integrity issues (crypto, sync)

### What Gets Skipped
- Code style/formatting
- Minor performance tweaks
- UI/UX improvements
- Non-critical warnings

## File Structure

```
cortex/
├── cortex-core/          # Core Rust library
│   └── src/
│       ├── sync/         # Cloud sync & encryption
│       ├── storage/      # SQLite backend
│       ├── retrieval.rs  # Query engine
│       └── ...
├── cortex-http/          # HTTP API
├── cortex-mcp-server/    # MCP integration
├── SECURITY.md           # Security model
├── SECURITY_AUDIT_*.md   # Iteration audits
└── .claude/
    └── CLAUDE.md         # This file
```

## Testing Standards

All changes must pass the **full workspace suite** (`--lib` alone silently skips
integration tests and has masked a red CI before):
```bash
cargo test --workspace --exclude cortex-python --exclude cortex-wasm
```

No test regressions allowed. If a test fails due to your change, fix it before committing.

**Test isolation (mandatory for iterations):** acceptance/black-box tests are written by a
**context-isolated subagent** that sees only the design doc, tool schemas, and public docs —
never the implementation diff or the implementer's unit tests. The implementer writes unit
tests; the isolated agent writes acceptance tests; adversarial review is a third independent
context. Rationale: the implementer's tests inherit the implementation's assumptions
(运动员不能当裁判) — Iteration 17's stale-cache privacy bug was found exactly this way.

## Commit Message Format

```
fix: brief description (category: security|privacy|perf)

Longer explanation of what was wrong and how it's fixed.
- Point 1
- Point 2

Fixes self-evolution iteration N.

Co-Authored-By: Claude Haiku 4.5 <noreply@anthropic.com>
```

## Known Issues / Deferred Work

### High Priority (Next Iterations)
- [ ] Timing attack vectors (search operations leak patterns)
- [ ] Key rotation capability (forward secrecy)
- [ ] Manifest integrity protection (HMAC)

### Medium Priority
- [ ] Embedding vector auto-zeroization
- [ ] Snapshot HLC versioning
- [ ] Device ID path validation

### Low Priority
- [ ] Quantum-resistant encryption (hybrid KEM)
- [ ] Zero-knowledge proofs (advanced privacy)
- [ ] Differential privacy (for aggregations)

## Feedback Rules (From Memory)

> **Rule**: ALL issues found by review tools must be fixed immediately, never deferred.
>
> **Why**: Technical debt compounds; small issues multiply.
>
> **How to Apply**: When code-review, adversarial-review, or security audits find anything HIGH/CRITICAL, fix it before moving forward.

## Performance Targets

- Ingest: < 200µs
- Search: < 600µs
- Memory overhead: < 50MB for 10K memories

Optimize only if needed; correctness & security first.

## Related Documentation

- [SECURITY.md](../SECURITY.md) — Threat model & crypto details
- [README.md](../README.md) — Architecture & features
- [SECURITY_AUDIT_ITER_10.md](../SECURITY_AUDIT_ITER_10.md) — Latest iteration results

---

**Last Updated**: 2026-06-02 (Iteration 10 complete)

**Status**: 🟢 Production-ready privacy hardening complete. Ready for next phase.
