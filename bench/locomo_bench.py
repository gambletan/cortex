#!/usr/bin/env python3
"""LOCOMO benchmark for Cortex memory engine.

Uses cortex-python native binding (no subprocess needed).

Flow per conversation:
1. Pre-compute embeddings for all dialogue chunks and questions (via ollama)
2. Ingest chunks into Cortex with embeddings
3. Retrieve relevant context for each question via semantic search
4. Use Claude to answer questions from retrieved context
5. Use Claude as judge to score answers

Categories (1-4 scored, 5 excluded):
  1: Single-hop factual
  2: Temporal reasoning
  3: Multi-hop reasoning
  4: Open-domain / summary
  5: Adversarial (excluded)
"""

import asyncio
import json
import os
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from urllib.request import urlopen, Request

import anthropic
from cortex_python import PyCortex

# ── Config ───────────────────────────────────────────────────────────────────

LOCOMO_PATH = Path("/Users/tan/work/locomo/data/locomo10.json")
RESULTS_DIR = Path("/Users/tan/work/cortex/bench/results")
MODEL = "claude-sonnet-4-20250514"
EMBED_MODEL = "nomic-embed-text"
OLLAMA_URL = "http://localhost:11434/api/embeddings"
SEARCH_LIMIT = 20
MAX_CONCURRENT_LLM = 10
INGEST_CHUNK_SIZE = 3  # dialogue turns per chunk
CATEGORY_NAMES = {1: "single-hop", 2: "temporal", 3: "multi-hop", 4: "open-domain"}


@dataclass
class QAResult:
    conv_id: str
    question: str
    ground_truth: str
    predicted: str
    category: int
    score: int
    context_snippets: list[str] = field(default_factory=list)


# ── Embedding via Ollama ─────────────────────────────────────────────────────


def embed_text(text: str) -> list[float]:
    """Get embedding from ollama's nomic-embed-text model."""
    payload = json.dumps({"model": EMBED_MODEL, "prompt": text}).encode()
    req = Request(OLLAMA_URL, data=payload, headers={"Content-Type": "application/json"})
    with urlopen(req, timeout=60) as resp:
        data = json.loads(resp.read())
    return data["embedding"]


# ── LLM helpers ──────────────────────────────────────────────────────────────

sem = asyncio.Semaphore(MAX_CONCURRENT_LLM)
aclient = anthropic.AsyncAnthropic()


async def answer_question(question: str, context: str) -> str:
    async with sem:
        resp = await aclient.messages.create(
            model=MODEL,
            max_tokens=300,
            messages=[{"role": "user", "content": f"""You are answering questions about a conversation between two people.
Use ONLY the provided memory context to answer. If the context doesn't contain enough information, give your best guess based on what's available.
Be concise - answer in 1-2 sentences maximum.

Memory context:
{context}

Question: {question}

Answer:"""}],
        )
        return resp.content[0].text.strip()


async def judge_answer(question: str, ground_truth: str, predicted: str) -> int:
    async with sem:
        resp = await aclient.messages.create(
            model=MODEL,
            max_tokens=50,
            messages=[{"role": "user", "content": f"""You are a judge evaluating whether a predicted answer is correct given the ground truth.
Consider an answer correct if it captures the essential meaning, even if worded differently.
For dates/times, minor format differences are acceptable if the information is the same.
For open-ended questions, the answer should align with the ground truth's key points.

Question: {question}
Ground truth: {ground_truth}
Predicted answer: {predicted}

Respond with ONLY "1" if correct or "0" if incorrect."""}],
        )
        text = resp.content[0].text.strip()
        for ch in text:
            if ch in ("0", "1"):
                return int(ch)
        return 0


# ── Conversation processing ─────────────────────────────────────────────────


def load_conversations() -> list[dict]:
    with open(LOCOMO_PATH) as f:
        return json.load(f)


def extract_dialogue_turns(conv: dict) -> list[dict]:
    conversation = conv["conversation"]
    turns = []
    for i in range(1, 100):
        session_key = f"session_{i}"
        date_key = f"session_{i}_date_time"
        if session_key not in conversation:
            break
        date_time = conversation.get(date_key, f"Session {i}")
        for utt in conversation[session_key]:
            turns.append({
                "speaker": utt["speaker"],
                "text": utt["text"],
                "dia_id": utt.get("dia_id", ""),
                "session": i,
                "date_time": date_time,
            })
    return turns


def build_chunks(turns: list[dict]) -> list[str]:
    """Group dialogue turns into chunks with session headers."""
    chunks = []
    current_session = None
    current_lines = []

    for turn in turns:
        if turn["session"] != current_session:
            if current_lines:
                chunks.append("\n".join(current_lines))
                current_lines = []
            current_session = turn["session"]
            current_lines.append(f"[Session {current_session} - {turn['date_time']}]")

        current_lines.append(f"{turn['speaker']}: {turn['text']}")

        if len(current_lines) >= INGEST_CHUNK_SIZE + 1:
            chunks.append("\n".join(current_lines))
            current_lines = []

    if current_lines:
        chunks.append("\n".join(current_lines))
    return chunks


def get_qa_pairs(conv: dict) -> list[dict]:
    return [qa for qa in conv["qa"] if qa["category"] in (1, 2, 3, 4)]


async def evaluate_conversation(conv: dict, conv_idx: int, total: int) -> list[QAResult]:
    conv_id = conv["sample_id"]
    qa_pairs = get_qa_pairs(conv)
    print(f"\n[{conv_idx+1}/{total}] Conversation {conv_id}: {len(qa_pairs)} QA pairs (cat 1-4)")

    turns = extract_dialogue_turns(conv)
    chunks = build_chunks(turns)

    # Step 1: Compute embeddings for chunks
    print(f"  Embedding {len(chunks)} chunks...", end=" ", flush=True)
    t0 = time.time()
    chunk_embeddings = []
    for ci, chunk in enumerate(chunks):
        chunk_embeddings.append(embed_text(chunk))
        if (ci + 1) % 50 == 0:
            print(f"{ci+1}...", end=" ", flush=True)
    print(f"{time.time()-t0:.1f}s")

    # Step 2: Compute embeddings for questions
    print(f"  Embedding {len(qa_pairs)} questions...", end=" ", flush=True)
    t0 = time.time()
    question_embeddings = []
    for qi, qa in enumerate(qa_pairs):
        question_embeddings.append(embed_text(qa["question"]))
        if (qi + 1) % 50 == 0:
            print(f"{qi+1}...", end=" ", flush=True)
    print(f"{time.time()-t0:.1f}s")

    # Step 3: Create fresh Cortex DB and ingest
    print(f"  Ingesting into Cortex...", end=" ", flush=True)
    t0 = time.time()
    db_path = f"/tmp/cortex_locomo_{conv_id}.db"
    for suffix in ["", "-shm", "-wal"]:
        p = Path(db_path + suffix)
        if p.exists():
            p.unlink()

    cortex = PyCortex(db_path)
    for chunk, emb in zip(chunks, chunk_embeddings):
        cortex.ingest(chunk, "locomo", embedding=emb)
    print(f"{len(chunks)} chunks in {time.time()-t0:.1f}s")

    # Step 4: Search for each question
    print(f"  Searching {len(qa_pairs)} questions...", end=" ", flush=True)
    t0 = time.time()
    contexts = []
    for qi, (qa, emb) in enumerate(zip(qa_pairs, question_embeddings)):
        results = cortex.retrieve(qa["question"], SEARCH_LIMIT, embedding=emb)
        snippets = [text for _, _, text in results]
        context = "\n\n".join(snippets)
        contexts.append((context, snippets))
        if (qi + 1) % 50 == 0:
            print(f"{qi+1}...", end=" ", flush=True)
    print(f"{time.time()-t0:.1f}s")

    # Step 5: Answer questions concurrently
    print(f"  Answering questions...", end=" ", flush=True)
    t0 = time.time()

    async def answer_one(idx: int):
        qa = qa_pairs[idx]
        ctx, snippets = contexts[idx]
        answer = await answer_question(qa["question"], ctx)
        return idx, answer, snippets

    tasks = [answer_one(i) for i in range(len(qa_pairs))]
    answer_results = await asyncio.gather(*tasks)
    answers = {}
    snippet_map = {}
    for idx, answer, snippets in answer_results:
        answers[idx] = answer
        snippet_map[idx] = snippets
    print(f"{time.time()-t0:.1f}s")

    # Step 6: Judge answers concurrently
    print(f"  Judging answers...", end=" ", flush=True)
    t0 = time.time()

    async def judge_one(idx: int):
        qa = qa_pairs[idx]
        score = await judge_answer(qa["question"], qa["answer"], answers[idx])
        return idx, score

    judge_tasks = [judge_one(i) for i in range(len(qa_pairs))]
    judge_results = await asyncio.gather(*judge_tasks)
    scores = {}
    for idx, score in judge_results:
        scores[idx] = score
    print(f"{time.time()-t0:.1f}s")

    # Build results
    results = []
    for i, qa in enumerate(qa_pairs):
        results.append(QAResult(
            conv_id=conv_id,
            question=qa["question"],
            ground_truth=qa["answer"],
            predicted=answers[i],
            category=qa["category"],
            score=scores[i],
            context_snippets=snippet_map[i][:3],
        ))

    # Per-conversation summary
    for cat in sorted(CATEGORY_NAMES.keys()):
        cat_results = [r for r in results if r.category == cat]
        if cat_results:
            acc = sum(r.score for r in cat_results) / len(cat_results)
            print(f"    Cat {cat} ({CATEGORY_NAMES[cat]}): {acc:.1%} ({sum(r.score for r in cat_results)}/{len(cat_results)})")

    return results


async def main():
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    checkpoint_path = RESULTS_DIR / "checkpoint.json"

    # Verify ollama
    try:
        test_emb = embed_text("test")
        print(f"Ollama embedding OK (dim={len(test_emb)})")
    except Exception as e:
        print(f"ERROR: Cannot connect to ollama: {e}")
        sys.exit(1)

    # Verify cortex
    try:
        db = "/tmp/cortex_verify.db"
        for s in ["", "-shm", "-wal"]:
            try: os.unlink(db + s)
            except: pass
        c = PyCortex(db)
        c.ingest("test", "test", embedding=[0.1] * 768)
        r = c.retrieve("test", 1, embedding=[0.1] * 768)
        assert len(r) == 1
        print("Cortex Python binding OK")
    except Exception as e:
        print(f"ERROR: Cortex binding check failed: {e}")
        sys.exit(1)

    conversations = load_conversations()
    print(f"Loaded {len(conversations)} conversations from LOCOMO")
    total_qa = sum(len(get_qa_pairs(c)) for c in conversations)
    print(f"Total QA pairs (cat 1-4): {total_qa}")

    # Load checkpoint
    all_results: list[QAResult] = []
    completed_convs: set[str] = set()
    if checkpoint_path.exists():
        with open(checkpoint_path) as f:
            checkpoint = json.load(f)
        for r in checkpoint["results"]:
            all_results.append(QAResult(**{k: v for k, v in r.items()}))
        completed_convs = {r.conv_id for r in all_results}
        print(f"Resuming from checkpoint: {len(completed_convs)} conversations done, {len(all_results)} QA pairs")

    total = len(conversations)
    for i, conv in enumerate(conversations):
        conv_id = conv["sample_id"]
        if conv_id in completed_convs:
            print(f"\n[{i+1}/{total}] Skipping {conv_id} (already done)")
            continue

        try:
            results = await evaluate_conversation(conv, i, total)
            all_results.extend(results)
        except Exception as e:
            print(f"\n  ERROR on {conv_id}: {e}")
            import traceback
            traceback.print_exc()
            continue

        # Save checkpoint
        with open(checkpoint_path, "w") as f:
            json.dump({
                "results": [
                    {
                        "conv_id": r.conv_id,
                        "question": r.question,
                        "ground_truth": r.ground_truth,
                        "predicted": r.predicted,
                        "category": r.category,
                        "score": r.score,
                        "context_snippets": r.context_snippets,
                    }
                    for r in all_results
                ]
            }, f, indent=2)
        print(f"  Checkpoint saved ({len(all_results)} total QA pairs)")

    # ── Final results ────────────────────────────────────────────────────────
    print("\n" + "=" * 60)
    print("LOCOMO BENCHMARK RESULTS -- Cortex Memory Engine")
    print("=" * 60)

    cat_scores = {}
    for cat in sorted(CATEGORY_NAMES.keys()):
        cat_results = [r for r in all_results if r.category == cat]
        if cat_results:
            acc = sum(r.score for r in cat_results) / len(cat_results)
            cat_scores[cat] = {
                "name": CATEGORY_NAMES[cat],
                "accuracy": round(acc, 4),
                "correct": sum(r.score for r in cat_results),
                "total": len(cat_results),
            }
            print(f"  Category {cat} ({CATEGORY_NAMES[cat]:>12}): {acc:6.1%}  ({cat_scores[cat]['correct']}/{cat_scores[cat]['total']})")

    if cat_scores:
        overall = sum(s["accuracy"] for s in cat_scores.values()) / len(cat_scores)
        print(f"\n  {'Overall (macro-avg)':>26}: {overall:6.1%}")
    else:
        overall = 0

    total_correct = sum(r.score for r in all_results)
    total_qs = len(all_results)
    micro = total_correct / total_qs if total_qs > 0 else 0
    print(f"  {'Overall (micro-avg)':>26}: {micro:6.1%}  ({total_correct}/{total_qs})")

    # Save final JSON
    final_output = {
        "benchmark": "LOCOMO",
        "memory_engine": "Cortex",
        "model": MODEL,
        "embedding_model": EMBED_MODEL,
        "search_limit": SEARCH_LIMIT,
        "ingest_chunk_size": INGEST_CHUNK_SIZE,
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "per_category": cat_scores,
        "overall_macro_avg": round(overall, 4),
        "overall_micro_avg": round(micro, 4),
        "total_questions": total_qs,
        "total_correct": total_correct,
    }

    output_path = RESULTS_DIR / "locomo_results.json"
    with open(output_path, "w") as f:
        json.dump(final_output, f, indent=2)
    print(f"\nResults saved to {output_path}")


if __name__ == "__main__":
    asyncio.run(main())
