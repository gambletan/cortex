# Design — Iteration 16: Bounded Query Budget & MCP Capability Grants

**Status:** IMPLEMENTED — 2026-06-12
**Owner:** CTO
**Date:** 2026-06-12
**Tracking:** docs/ROADMAP.md → Priority 2 (security-flavored product wins)

> **Corrections applied after adversarial review.**
> 1. `expand_query` originally ran outside the budget — it now takes the query start
>    time + duration and checks the deadline before every per-entity storage query, and
>    the expansion set is capped at `MAX_EXPANSION_TERMS = 64` (bounds the downstream
>    term-match SQL regardless of store size).
> 2. `CORTEX_CAPABILITIES_FILE` set to a **missing** file fails **closed** with a
>    distinct diagnostic (not allow-all): explicitly configuring a policy expresses
>    intent to restrict, so a missing file must never widen access. Only the *implicit*
>    no-file case (no env var, no `capabilities.json`) means legacy allow-all.
> 3. Scope made explicit: the policy gates the MCP JSON-RPC surface only. CLI
>    subcommands are operator tools (shell access ⇒ filesystem access to the policy
>    file itself — gating them adds no boundary). Unifying the HTTP surface under the
>    policy is a tracked roadmap follow-up.

## 1. Goal

Two defense-in-depth features, both sourced from trend-watch and promoted by the roadmap's
recommended sequence:

1. **Bounded query budget (cortex-core/retrieval).** Cap per-query candidate work so a
   single retrieval can never scan unbounded state. This is (a) a DoS guard for the HTTP/MCP
   surfaces and (b) a privacy win: query latency stops scaling with the *full* size of the
   memory store, which shrinks the residual search-timing side-channel documented in
   `timing_leak_findings.json`.
2. **Deny-by-default capability grants (cortex-mcp-server).** Today every MCP client gets
   all 29 tools — full read/write/sync over the entire memory store — with zero
   authorization. With a capability policy in place, an agent gets **nothing** until
   explicitly granted. Tool listing is also filtered, so ungranted tools are invisible
   (progressive disclosure: smaller attack surface *and* smaller token cost).

## 2. Non-goals

- Constant-time retrieval (the budget *bounds* the timing channel; it does not flatten it —
  full padding remains tracked in the timing-leak backlog).
- Per-namespace ACLs inside cortex-core (this iteration gates the MCP *surface*; core-level
  namespace grants are a follow-up once the policy file format proves out).
- Authentication. MCP is a local stdio surface; identity of the peer process is out of
  scope. The policy constrains *what this server instance exposes*, not *who* connects.
- HTTP surface gating (cortex-http has its own input-bounds layer; unify later).

## 3. Design — query budget

### 3.1 Types (`cortex-core/src/retrieval.rs`)

```rust
pub struct QueryBudget {
    /// Hard cap on distinct candidates gathered across all phases.
    pub max_candidates: usize,        // default 10_000
    /// Soft wall-clock cap; expansion phases stop once exceeded.
    pub max_duration: Duration,       // default 250ms
}
```

`RetrievalQuery` gains `budget: QueryBudget` (populated by `Default`, builder
`with_budget`). Existing callers are unaffected — defaults are far above today's
working sizes, so behavior only changes under adversarial/pathological load.

### 3.2 Enforcement points

`retrieve()` records `Instant::now()` on entry. Between each candidate-gathering phase
(vector search → recency fallback → entity expansion → temporal → multi-hop → FTS) it
checks `over_budget()`; once true, remaining **expansion** phases are skipped and the
engine proceeds directly to scoring what it already has. Candidate insertion is capped at
`max_candidates` (insert-or-skip, never partial-phase panic). Scoring is O(candidates) and
therefore bounded by the same cap.

**Degradation is graceful:** the query still returns ranked results from the candidates
gathered before the cap — never an error. Rationale: an error channel would itself be an
oracle (an attacker could binary-search store size by observing failures), and erroring
breaks legitimate large-store users.

### 3.3 Tests

- Default budget: behavior identical to pre-change suite (regression: existing tests pass).
- `max_candidates = small` on a store with many memories: results still returned, bounded.
- `max_duration = 0`: expansion phases skipped, vector-phase results still scored/returned.

## 4. Design — MCP capability grants

### 4.1 Policy file

`CORTEX_CAPABILITIES_FILE` env var, else `<db_dir>/capabilities.json` if present:

```json
{ "version": 1, "grants": ["read", "sync_status"] }
```

Grant atoms are **groups** or **exact tool names**:

| Group     | Tools |
|-----------|-------|
| `read`    | memory_search, memory_context, fact_query, preference_query, person_list, belief_list, memory_stats, namespace_list, tag_list_taxonomy, contradiction_check |
| `write`   | memory_ingest, memory_ingest_batch, fact_add, preference_set, belief_observe, person_resolve, person_merge, relationship_extract, memory_infer, memory_consolidate, memory_compress, memory_decay, memory_archive, memory_delete, memory_restore |
| `sync`    | sync_enable, sync_pull, sync_status, sync_providers |
| `plugins` | any plugin-registered tool (non-built-in) |
| `all`     | everything |

### 4.2 Semantics

- **No policy file → legacy allow-all** with a one-line startup warning suggesting a
  policy. Shipping hard deny-by-default would brick every existing install; the secure
  posture is opt-in to *create* the file, deny-by-default *within* it.
- **Policy file present → deny-by-default.** A tool is callable iff it matches a grant
  atom. Empty `grants: []` = total lockdown (valid, intentional).
- **Malformed policy file → fail closed.** Parse error ⇒ treat as `grants: []` and log
  loudly. Never fall back to allow-all on a broken file.
- `tools/list` is filtered to granted tools only.
- Denials return a uniform `Error: tool 'X' not permitted by capability policy` (no
  existence leak: unknown tool and denied tool produce distinguishable messages today —
  keep that, since tool names are public in the repo; the policy is not secret).

### 4.3 Placement

New `cortex-mcp-server/src/capabilities.rs` (policy parse + `is_allowed(name, is_builtin)`),
checked in `McpServer::handle_request` before dispatch and in `tools/list`. Policy is
loaded once at server start (immutable thereafter — no TOCTOU on the file).

### 4.4 Tests

- Parse: groups, exact names, `all`, empty, malformed-fails-closed.
- No file ⇒ allow-all; file ⇒ deny-by-default (ungranted tool denied, granted allowed).
- `tools/list` filtering.

## 5. Rollout

Single iteration, two commits (one per feature), each landing green on
`cargo test --workspace --exclude cortex-python --exclude cortex-wasm` and
`cargo clippy --all-targets -- -D warnings` (lib targets). Roadmap updated after.
