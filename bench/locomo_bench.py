#!/usr/bin/env python3
"""LOCOMO benchmark for Cortex memory engine.

Evaluates long-term conversational memory by:
1. Ingesting multi-session dialogues into Cortex (with embeddings)
2. Retrieving relevant context for each question via semantic search
3. Using Claude to answer questions from retrieved context
4. Using Claude as judge to score answers against ground truth

Categories (1-4 scored, 5 excluded):
  1: Single-hop factual
  2: Temporal reasoning
  3: Multi-hop reasoning
  4: Open-domain / summary
  5: Adversarial (excluded)

Requires:
  - cortex-mcp-server binary
  - ollama running with nomic-embed-text model (for embeddings)
  - ANTHROPIC_API_KEY env var
"""

import asyncio
import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from urllib.request import urlopen, Request
from urllib.error import URLError

import anthropic

# ── Config ───────────────────────────────────────────────────────────────────

LOCOMO_PATH = Path("/Users/tan/work/locomo/data/locomo10.json")
CORTEX_BIN = Path("/Users/tan/.local/bin/cortex-mcp-server")
RESULTS_DIR = Path("/Users/tan/work/cortex/bench/results")
MODEL = "claude-sonnet-4-20250514"
EMBED_MODEL = "nomic-embed-text"
OLLAMA_URL = "http://localhost:11434/api/embeddings"
SEARCH_LIMIT = 20  # memories to retrieve per question
MAX_CONCURRENT_LLM = 10  # concurrent Claude API calls
CATEGORY_NAMES = {1: "single-hop", 2: "temporal", 3: "multi-hop", 4: "open-domain"}

# Batch dialogue turns into chunks of N turns for ingestion (reduces API calls)
INGEST_CHUNK_SIZE = 3


@dataclass
class QAResult:
    conv_id: str
    question: str
    ground_truth: str
    predicted: str
    category: int
    score: int  # 0 or 1
    context_snippets: list[str] = field(default_factory=list)


# ── Embedding via Ollama ─────────────────────────────────────────────────────


def embed_text(text: str) -> list[float]:
    """Get embedding from ollama's nomic-embed-text model."""
    payload = json.dumps({"model": EMBED_MODEL, "prompt": text}).encode()
    req = Request(OLLAMA_URL, data=payload, headers={"Content-Type": "application/json"})
    with urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read())
    return data["embedding"]


def embed_texts_batch(texts: list[str]) -> list[list[float]]:
    """Embed multiple texts sequentially (ollama doesn't support batching)."""
    return [embed_text(t) for t in texts]


# ── Cortex MCP Client ───────────────────────────────────────────────────────


class CortexClient:
    """Communicate with cortex-mcp-server over stdio JSON-RPC."""

    def __init__(self, db_path: str):
        self.db_path = db_path
        self.proc: subprocess.Popen | None = None
        self._req_id = 0

    def start(self):
        # Clean up any leftover DB files
        for suffix in ["-shm", "-wal"]:
            p = Path(self.db_path + suffix)
            if p.exists():
                p.unlink()

        self.proc = subprocess.Popen(
            [str(CORTEX_BIN), self.db_path],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        # Initialize MCP
        resp = self._call("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "locomo-bench", "version": "1.0"},
        })
        return resp

    def stop(self):
        if self.proc:
            try:
                self.proc.stdin.close()
                self.proc.wait(timeout=5)
            except Exception:
                self.proc.kill()
                self.proc.wait(timeout=2)
            self.proc = None

    def _call(self, method: str, params: dict) -> dict:
        self._req_id += 1
        req = {"jsonrpc": "2.0", "id": self._req_id, "method": method, "params": params}
        line = json.dumps(req) + "\n"
        try:
            self.proc.stdin.write(line.encode())
            self.proc.stdin.flush()
        except BrokenPipeError:
            raise RuntimeError(f"Cortex process died (BrokenPipe on {method})")
        resp_line = self.proc.stdout.readline().decode().strip()
        if not resp_line:
            raise RuntimeError(f"Empty response for {method} (process may have died)")
        return json.loads(resp_line)

    def ingest(self, text: str, channel: str, user_id: str | None = None,
               embedding: list[float] | None = None):
        args = {"text": text, "channel": channel}
        if user_id:
            args["user_id"] = user_id
        if embedding:
            args["embedding"] = embedding
        return self._call("tools/call", {"name": "memory_ingest", "arguments": args})

    def search(self, query: str, limit: int = 10,
               embedding: list[float] | None = None) -> list[dict]:
        args = {"query": query, "limit": limit}
        if embedding:
            args["embedding"] = embedding
        resp = self._call("tools/call", {"name": "memory_search", "arguments": args})
        content = resp.get("result", {}).get("content", [{}])
        text = content[0].get("text", "{}") if content else "{}"
        data = json.loads(text)
        return data.get("results", [])


# ── LLM helpers ──────────────────────────────────────────────────────────────

sem = asyncio.Semaphore(MAX_CONCURRENT_LLM)
aclient = anthropic.AsyncAnthropic()


async def answer_question(question: str, context: str) -> str:
    """Use Claude to answer a question given retrieved memory context."""
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
    """Use Claude as judge: 1=correct, 0=incorrect."""
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


# ── Main benchmark ───────────────────────────────────────────────────────────


def load_conversations() -> list[dict]:
    with open(LOCOMO_PATH) as f:
        return json.load(f)


def extract_dialogue_turns(conv: dict) -> list[dict]:
    """Extract all dialogue turns in order, with session metadata."""
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


def ingest_conversation(conv: dict) -> CortexClient:
    """Ingest all turns of a conversation into a fresh Cortex DB with embeddings."""
    conv_id = conv["sample_id"]
    db_path = f"/tmp/cortex_locomo_{conv_id}.db"

    # Remove old DB
    for suffix in ["", "-shm", "-wal"]:
        p = Path(db_path + suffix)
        if p.exists():
            p.unlink()

    cortex = CortexClient(db_path)
    cortex.start()

    turns = extract_dialogue_turns(conv)

    # Build text chunks: group consecutive turns into chunks for better context
    chunks = []
    current_session = None
    current_chunk_lines = []

    for turn in turns:
        # Add session marker on session change
        if turn["session"] != current_session:
            # Flush current chunk
            if current_chunk_lines:
                chunks.append("\n".join(current_chunk_lines))
                current_chunk_lines = []
            current_session = turn["session"]
            current_chunk_lines.append(f"[Session {current_session} - {turn['date_time']}]")

        current_chunk_lines.append(f"{turn['speaker']}: {turn['text']}")

        if len(current_chunk_lines) >= INGEST_CHUNK_SIZE + 1:  # +1 for session header
            chunks.append("\n".join(current_chunk_lines))
            current_chunk_lines = []

    if current_chunk_lines:
        chunks.append("\n".join(current_chunk_lines))

    # Compute embeddings for all chunks
    embeddings = embed_texts_batch(chunks)

    # Ingest with embeddings
    for chunk_text, emb in zip(chunks, embeddings):
        cortex.ingest(chunk_text, channel="locomo", embedding=emb)

    return cortex, len(chunks)


def get_qa_pairs(conv: dict) -> list[dict]:
    """Get QA pairs for categories 1-4 only."""
    return [qa for qa in conv["qa"] if qa["category"] in (1, 2, 3, 4)]


async def evaluate_conversation(conv: dict, conv_idx: int, total: int) -> list[QAResult]:
    """Evaluate all QA pairs for one conversation."""
    conv_id = conv["sample_id"]
    qa_pairs = get_qa_pairs(conv)
    print(f"\n[{conv_idx+1}/{total}] Conversation {conv_id}: {len(qa_pairs)} QA pairs (cat 1-4)")

    # Step 1: Ingest with embeddings
    print(f"  Ingesting dialogue turns...", end=" ", flush=True)
    t0 = time.time()
    cortex, n_chunks = ingest_conversation(conv)
    turns = extract_dialogue_turns(conv)
    print(f"{len(turns)} turns -> {n_chunks} chunks in {time.time()-t0:.1f}s")

    # Step 2: Retrieve context for all questions (with query embeddings)
    print(f"  Retrieving context for {len(qa_pairs)} questions...", end=" ", flush=True)
    t0 = time.time()
    contexts = []
    for qi, qa in enumerate(qa_pairs):
        q_emb = embed_text(qa["question"])
        results = cortex.search(qa["question"], limit=SEARCH_LIMIT, embedding=q_emb)
        snippets = [r["text"] for r in results]
        context = "\n\n".join(snippets)
        contexts.append((context, snippets))
        if (qi + 1) % 50 == 0:
            print(f"{qi+1}...", end=" ", flush=True)
    print(f"{time.time()-t0:.1f}s")

    cortex.stop()

    # Step 3: Answer questions concurrently
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

    # Step 4: Judge answers concurrently
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

    # Print per-conversation summary
    for cat in sorted(CATEGORY_NAMES.keys()):
        cat_results = [r for r in results if r.category == cat]
        if cat_results:
            acc = sum(r.score for r in cat_results) / len(cat_results)
            print(f"    Cat {cat} ({CATEGORY_NAMES[cat]}): {acc:.1%} ({sum(r.score for r in cat_results)}/{len(cat_results)})")

    return results


async def main():
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    checkpoint_path = RESULTS_DIR / "checkpoint.json"

    # Verify ollama is accessible
    try:
        test_emb = embed_text("test")
        print(f"Ollama embedding OK (dim={len(test_emb)})")
    except Exception as e:
        print(f"ERROR: Cannot connect to ollama for embeddings: {e}")
        print("Make sure ollama is running with nomic-embed-text model")
        sys.exit(1)

    conversations = load_conversations()
    print(f"Loaded {len(conversations)} conversations from LOCOMO")

    # Count total QA pairs
    total_qa = sum(len(get_qa_pairs(c)) for c in conversations)
    print(f"Total QA pairs (cat 1-4): {total_qa}")

    # Load checkpoint if exists
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
        "overall_macro_avg": round(overall, 4) if cat_scores else 0,
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
