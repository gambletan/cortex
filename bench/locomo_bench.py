#!/usr/bin/env python3
"""LOCOMO benchmark for Cortex memory engine.

Uses cortex-python native binding + Gemini for QA + judging.

Flow per conversation:
1. Pre-compute embeddings for all dialogue chunks and questions (via ollama)
2. Ingest chunks into Cortex with embeddings
3. Retrieve relevant context for each question via semantic search
4. Use Gemini to answer questions from retrieved context
5. Use Gemini as judge to score answers

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
from cortex_ai_memory import Cortex

# ── Config ───────────────────────────────────────────────────────────────────

LOCOMO_PATH = Path(__file__).parent / "data" / "locomo10.json"
RESULTS_DIR = Path(__file__).parent / "results"
CLAUDE_MODEL = "claude-sonnet-4-20250514"
EMBED_MODEL = "nomic-embed-text"
OLLAMA_URL = "http://localhost:11434/api/embeddings"
SEARCH_LIMIT = 20
MAX_CONCURRENT_LLM = 5
INGEST_CHUNK_SIZE = 3
CATEGORY_NAMES = {1: "single-hop", 2: "temporal", 3: "multi-hop", 4: "open-domain"}


@dataclass
class QAResult:
    conv_id: str
    question: str
    ground_truth: str
    predicted: str
    category: int
    score: int
    context_snippets: list = field(default_factory=list)


# ── Embedding via Ollama ─────────────────────────────────────────────────────

def embed_text(text: str) -> list:
    payload = json.dumps({"model": EMBED_MODEL, "prompt": text}).encode()
    req = Request(OLLAMA_URL, data=payload, headers={"Content-Type": "application/json"})
    with urlopen(req, timeout=60) as resp:
        data = json.loads(resp.read())
    return data["embedding"]


# ── LLM helpers (Gemini) ─────────────────────────────────────────────────────

sem = None
aclient = None


def init_claude():
    global aclient
    api_key = os.environ.get("ANTHROPIC_API_KEY")
    if not api_key:
        env_path = Path.home() / "work" / "X-Auto" / ".env"
        if env_path.exists():
            for line in env_path.read_text().splitlines():
                if line.startswith("ANTHROPIC_API_KEY="):
                    api_key = line.split("=", 1)[1].strip()
                    break
    if not api_key:
        print("ERROR: No ANTHROPIC_API_KEY found")
        sys.exit(1)
    os.environ["ANTHROPIC_API_KEY"] = api_key
    aclient = anthropic.AsyncAnthropic()
    print(f"Claude model: {CLAUDE_MODEL}")


async def answer_question(question: str, context: str) -> str:
    async with sem:
        resp = await aclient.messages.create(
            model=CLAUDE_MODEL,
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
            model=CLAUDE_MODEL,
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

def load_conversations() -> list:
    with open(LOCOMO_PATH) as f:
        return json.load(f)


def extract_dialogue_turns(conv: dict) -> list:
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
                "session": i,
                "date_time": date_time,
            })
    return turns


def build_chunks(turns: list) -> list:
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


def get_qa_pairs(conv: dict) -> list:
    return [qa for qa in conv["qa"] if qa["category"] in (1, 2, 3, 4)]


async def evaluate_conversation(conv: dict, conv_idx: int, total: int) -> list:
    conv_id = conv["sample_id"]
    qa_pairs = get_qa_pairs(conv)
    print(f"\n[{conv_idx+1}/{total}] Conversation {conv_id}: {len(qa_pairs)} QA pairs")

    turns = extract_dialogue_turns(conv)
    chunks = build_chunks(turns)

    # Embed chunks
    print(f"  Embedding {len(chunks)} chunks...", end=" ", flush=True)
    t0 = time.time()
    chunk_embeddings = [embed_text(c) for c in chunks]
    print(f"{time.time()-t0:.1f}s")

    # Embed questions
    print(f"  Embedding {len(qa_pairs)} questions...", end=" ", flush=True)
    t0 = time.time()
    question_embeddings = [embed_text(qa["question"]) for qa in qa_pairs]
    print(f"{time.time()-t0:.1f}s")

    # Ingest into Cortex
    print(f"  Ingesting...", end=" ", flush=True)
    t0 = time.time()
    db_path = f"/tmp/cortex_locomo_{conv_id}.db"
    for suffix in ["", "-shm", "-wal"]:
        p = Path(db_path + suffix)
        if p.exists():
            p.unlink()
    cortex = Cortex(db_path)
    for chunk, emb in zip(chunks, chunk_embeddings):
        cortex.ingest(chunk, "locomo", embedding=emb)
    print(f"{len(chunks)} chunks in {time.time()-t0:.1f}s")

    # Search
    print(f"  Searching...", end=" ", flush=True)
    t0 = time.time()
    contexts = []
    for qa, emb in zip(qa_pairs, question_embeddings):
        results = cortex.retrieve(qa["question"], SEARCH_LIMIT, embedding=emb)
        snippets = [text for _, _, text in results]
        contexts.append(("\n\n".join(snippets), snippets))
    print(f"{time.time()-t0:.1f}s")

    # Answer
    print(f"  Answering...", end=" ", flush=True)
    t0 = time.time()
    tasks = []
    for i, (qa, (ctx, _)) in enumerate(zip(qa_pairs, contexts)):
        tasks.append(answer_question(qa["question"], ctx))
    answers = await asyncio.gather(*tasks)
    print(f"{time.time()-t0:.1f}s")

    # Judge
    print(f"  Judging...", end=" ", flush=True)
    t0 = time.time()
    judge_tasks = []
    for i, (qa, ans) in enumerate(zip(qa_pairs, answers)):
        judge_tasks.append(judge_answer(qa["question"], qa["answer"], ans))
    scores = await asyncio.gather(*judge_tasks)
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
            context_snippets=contexts[i][1][:3],
        ))

    for cat in sorted(CATEGORY_NAMES.keys()):
        cat_results = [r for r in results if r.category == cat]
        if cat_results:
            acc = sum(r.score for r in cat_results) / len(cat_results)
            print(f"    Cat {cat} ({CATEGORY_NAMES[cat]}): {acc:.1%}")

    return results


async def main():
    global sem
    sem = asyncio.Semaphore(MAX_CONCURRENT_LLM)
    init_claude()
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)

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
        c = Cortex(db)
        c.ingest("test", "test", embedding=[0.1] * 768)
        r = c.retrieve("test", 1, embedding=[0.1] * 768)
        assert len(r) == 1
        print("Cortex Python binding OK")
    except Exception as e:
        print(f"ERROR: Cortex binding: {e}")
        sys.exit(1)

    conversations = load_conversations()
    print(f"Loaded {len(conversations)} conversations")

    all_results = []
    for i, conv in enumerate(conversations):
        try:
            results = await evaluate_conversation(conv, i, len(conversations))
            all_results.extend(results)
        except Exception as e:
            print(f"\n  ERROR: {e}")
            import traceback
            traceback.print_exc()

    # Final results
    print("\n" + "=" * 60)
    print("LOCOMO BENCHMARK RESULTS — Cortex v1.7.0")
    print("=" * 60)

    cat_scores = {}
    for cat in sorted(CATEGORY_NAMES.keys()):
        cat_results = [r for r in all_results if r.category == cat]
        if cat_results:
            acc = sum(r.score for r in cat_results) / len(cat_results)
            cat_scores[cat] = {"name": CATEGORY_NAMES[cat], "accuracy": round(acc, 4),
                               "correct": sum(r.score for r in cat_results), "total": len(cat_results)}
            print(f"  Category {cat} ({CATEGORY_NAMES[cat]:>12}): {acc:6.1%}  ({cat_scores[cat]['correct']}/{cat_scores[cat]['total']})")

    overall = sum(s["accuracy"] for s in cat_scores.values()) / len(cat_scores) if cat_scores else 0
    print(f"\n  {'Overall (macro-avg)':>26}: {overall:6.1%}")

    total_correct = sum(r.score for r in all_results)
    total_qs = len(all_results)
    micro = total_correct / total_qs if total_qs > 0 else 0
    print(f"  {'Overall (micro-avg)':>26}: {micro:6.1%}  ({total_correct}/{total_qs})")

    output = {
        "benchmark": "LOCOMO", "memory_engine": "Cortex v1.7.0",
        "llm_model": CLAUDE_MODEL, "embedding_model": EMBED_MODEL,
        "search_limit": SEARCH_LIMIT, "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "per_category": cat_scores, "overall_macro_avg": round(overall, 4),
        "overall_micro_avg": round(micro, 4), "total_questions": total_qs, "total_correct": total_correct,
    }
    output_path = RESULTS_DIR / "locomo_results.json"
    with open(output_path, "w") as f:
        json.dump(output, f, indent=2)
    print(f"\nResults saved to {output_path}")


if __name__ == "__main__":
    asyncio.run(main())
