# Iteration 19 — Graph-Edge Re-ranking: Negative Result + Sharpened Problem

**Date:** 2026-06-13
**Status:** attempted, not shipped (would regress); benchmark + diagnosis retained.

## What we set out to do

The scale test (docs/scale-test-2026-06-13.md) showed the remaining recall gap is
**multi-hop / relational** — answers that require chaining facts across entities, where
the decisive "tail" link shares no surface text with the query. A context-isolated
subagent (per the testing protocol in .claude/CLAUDE.md) built a black-box benchmark,
`bench/recall_multihop.py`: 25 chains (13 two-hop, 12 three-hop) buried among ~3000
distractors that **deliberately collide on entity tokens** (many memories mention
"Aurora"/"Helios"/"Frankfurt").

## Baseline (vector + existing fact-expansion)

```
hop-recall@1  : 2%    hop-recall@5 : 50%    hop-recall@10 : 56%
chain-completable@10 : 36%   (all required hops of a chain present in top-10)
```

## Two re-ranking approaches tried — both failed to beat baseline

1. **Fact-graph 2-hop + proximity boost.** Walk `MemContent::Fact` subject/object edges
   out two hops, boost graph-reachable candidates. **Result: no change (56%).** The seed
   candidates and the chain links are *episodic text* memories, not structured Facts, so
   the Fact-only walk never touches them.

2. **Episodic-text 2-hop via entity co-occurrence (`search_episodic_by_terms`) + boost.**
   Extract entities from seed text, pull memories that *mention* them, boost by hop
   proximity. **Result: regression to 5%.** The benchmark's deliberate entity collisions
   are the killer: `LIKE '%Aurora%'` returns the true tail link *and* dozens of
   distractors, all boosted equally (+0.30), flooding the top-10 and burying the answer.

## The real lesson

**Entity co-occurrence ≠ a graph edge.** Co-occurrence cannot distinguish the one memory
that truly continues the chain from the many that merely share a token — exactly what the
colliding-distractor benchmark was designed to expose. A boost applied on co-occurrence is
net-negative noise.

Real graph-edge re-ranking needs **precise edges**: "this memory continues that memory's
chain", which requires either
- **ingest-time edge construction** — when ingesting "Helios runs on Aurora", create an
  explicit link to the memory establishing Helios and to the one about Aurora (populate
  the existing `MemObject.links` / relationship graph), so retrieval can traverse *real*
  edges instead of guessing from text; or
- a **graph-native retrieval pass** over the relationship store with edge-typed weights.

That is a substantially larger design than a retrieval-time re-rank weight, and it is the
honest reframing of this roadmap item. Shipping the co-occurrence version would have
regressed real recall, so it was reverted.

## Retained assets

- `bench/recall_multihop.py` — durable, deterministic, black-box multi-hop benchmark
  (seed 20260613, ~5K memories, sub-10ms baseline). Use it as the gate for any future
  relational-chaining work and add it to the CI recall suite.
- Sharpened roadmap item: "relational chaining" needs ingest-time edge construction, not
  a retrieval re-rank tweak.

## Net for the session

A verified negative result that prevents a real regression, a permanent benchmark, and a
correct problem definition. The Iteration 18 beam fix (paraphrase recall 40%→90%) remains
the shipped win; single-hop/paraphrase recall at 5K is now strong, multi-hop is the open
frontier and now has both a benchmark and a concrete plan.
