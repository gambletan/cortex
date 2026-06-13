# Scale Test — Recall Quality at ~5K Memories (2026-06-13)

**Question:** the roadmap claims "recall quality at scale" is the real gap. Is it? Where exactly?

**Setup:** installed release binary (embeddings enabled, all-MiniLM-class local model + HNSW).
Black-box via MCP stdio. Temp DBs, real auto-embedding on every ingest. ~3000 ingested texts →
~5170 total memories (auto-inference adds ~2200 facts). Two query styles measured against
50 planted "needles" buried among thousands of semantically-adjacent distractors.

## Results

| Query style | recall@1 | recall@5 | recall@10 | latency p50 / p95 |
|---|---|---|---|---|
| **Lexical** (query shares the memory's keywords) | **100%** | 100% | 100% | 4.2ms / 7.7ms |
| **Paraphrase** (zero keyword overlap, pure semantic) | **~40%** | ~40% | ~40% | 5.3ms / first-query model-load spike |

Ingest: 3.88ms/item with real embeddings (3000 items in 11.6s). Search stays sub-10ms at 5K.

## Diagnosis (this is the useful part)

The mechanism is **correct**: `retrieve_with_namespace` auto-embeds the query
(`auto_embed`), the HNSW vector phase runs, and ranking is sound — every needle that
*is* recalled lands at **rank 0**, never buried at rank 7/20. So **ranking is not the
problem.**

Two controlled probes isolate the cause:
1. **Small-N control:** the exact paraphrase queries that score 40% at N=5000 score
   **5/5 at N=5** — so the embedder works and the query is embedded. The drop is purely
   a function of scale.
2. **Candidate-depth probe:** raising the MCP `limit` 10 → 100 (vector candidate pool
   50 → 500+) brings in **zero** additional missed needles. The ~7 hard misses are not
   in the top-hundreds of nearest neighbors at all.

**Conclusion: the bottleneck is candidate recall — the embedding model + ANN beam can't
place hard paraphrases near their answers in vector space at 5K scale.** Not plumbing,
not ranking, not candidate-pool size.

Nuance worth keeping in perspective: the **lexical** case is 100% even at 5K, and in real
usage people refer to things with consistent vocabulary far more often than in full
paraphrase. The 40% is the genuinely-hard tail (zero lexical overlap), which is also where
LoCoMo multi-hop lives.

## Levers (Iteration 18 — retrieval quality, ranked by expected impact)

1. **Stronger embedding model** — biggest lever. Pluggable embedder + a better default
   (e.g. bge-small / gte-small / e5-small) measured on a fixed paraphrase set + LoCoMo.
2. **HNSW search beam (ef_search)** — `search_inner_readonly` uses `Search::default()`;
   the depth probe suggests the beam, not the take-limit, caps candidate recall. Expose
   and raise ef_search; cheap to try, measure recall delta.
3. **Graph-edge re-ranking** — traverse relationship/link edges to rescue multi-hop
   answers the embedder alone misses.
4. **Hybrid fusion tuning** — lexical is already 100%; make sure the semantic path is
   additive (RRF-style fusion of FTS + vector) so a paraphrase miss can still be saved by
   any shared token.

## Regression gate

Bake this exact paraphrase set (20–50 needles) + a LoCoMo subset into CI as a recall
gate, so retrieval quality can never silently regress (same lesson as the sync tests).
Acceptance tests authored by a context-isolated subagent per .claude/CLAUDE.md.

**Target:** paraphrase recall@10 ≥ 75% at 5K (from ~40%), LoCoMo overall ≥ 80%.
