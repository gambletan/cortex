#!/usr/bin/env python3
"""Multi-hop relational recall benchmark for Cortex (black-box via MCP stdio).

Single-hop semantic recall (see recall_scale.py) measures whether a paraphrased
query surfaces ONE matching memory. This benchmark measures a harder case:
MULTI-HOP relational recall, where answering a query requires CHAINING facts
across entities and the answer memory shares little or no surface text with the
query.

Example chain (2-hop):
  store: "Marisol manages the Helios project."
  store: "The Helios project runs on the Aurora database."
  store: "The Aurora database is hosted in the Frankfurt datacenter."
  query: "Which datacenter does Marisol's project depend on?"
The required hop memories are the Helios->Aurora and Aurora->Frankfurt links,
which do NOT contain the word "Marisol" — pure vector similarity tends to miss
them because the query's salient tokens never appear in the target memories.

We plant ~25 such chains, bury them among ~3000 plausible distractors (reusing
the noisy-corpus style from recall_scale.py), then for each query check whether
the REQUIRED hop memories appear in memory_search top-k. We report:
  - hop-recall@1/@5/@10: fraction of required hop memories that surfaced
  - chain-completable@10: fraction of queries where ALL required hops were in top-10
  - latency p50/p95

This is the baseline metric a future "graph-edge re-ranking" change must move.

Usage: BIN=~/.local/bin/cortex-mcp-server python3 bench/recall_multihop.py
"""
import json, os, subprocess, time, statistics, random, tempfile, shutil

TMP = tempfile.mkdtemp(prefix="cortex-multihop-")
DB = f"{TMP}/multihop.db"
BIN = os.path.expanduser(os.environ.get("BIN", "~/.local/bin/cortex-mcp-server"))
random.seed(20260613)  # deterministic; NOT time/Math.random seeded


def srv(db):
    e = {**os.environ, "RUST_LOG": "error", "CORTEX_NO_KEYCHAIN": "1"}
    p = subprocess.Popen([BIN, db], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True, env=e)
    n = [0]

    def call(name, args):
        n[0] += 1
        p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": n[0], "method": "tools/call",
                                  "params": {"name": name, "arguments": args}}) + "\n")
        p.stdin.flush()
        r = json.loads(p.stdout.readline())["result"]
        return r.get("isError", False), r["content"][0]["text"]

    p.stdin.write('{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}\n')
    p.stdin.flush()
    p.stdout.readline()
    return p, call


# Each chain: list of link memories (stored verbatim) + a query whose answer
# requires the LATER links (which lack the query's head-entity token).
# "hops" = the indices of link memories REQUIRED to answer the query. We treat
# every link after the head mention as required, since those are the ones a
# vector search of the query tends to miss.
#
# Structure: (links, query, required_idxs)
#   links[0] connects the head entity (named in the query) to a middle entity.
#   The query references the head entity but asks about the tail; the required
#   hop memories are the ones NOT containing the head entity's name.
CHAINS = [
    # ---- 2-hop ----
    (["Marisol manages the Helios project.",
      "The Helios project runs on the Aurora database.",
      "The Aurora database is hosted in the Frankfurt datacenter."],
     "Which datacenter does Marisol's project depend on?", [1, 2]),
    (["Devon leads the Pegasus initiative.",
      "The Pegasus initiative is funded by the Northwind grant.",
      "The Northwind grant expires at the end of the fiscal year."],
     "When does the funding for Devon's initiative run out?", [1, 2]),
    (["Priya owns the Cobalt service.",
      "The Cobalt service writes logs to the Meridian bucket.",
      "The Meridian bucket is replicated to the Osaka region."],
     "Which region holds a copy of the logs from Priya's service?", [1, 2]),
    (["Tomas chairs the Atlas committee.",
      "The Atlas committee meets in the Lighthouse building.",
      "The Lighthouse building is on the Riverside campus."],
     "On which campus does Tomas's committee gather?", [1, 2]),
    (["Yuki maintains the Sable library.",
      "The Sable library depends on the Quartz runtime.",
      "The Quartz runtime is maintained by the platform team."],
     "Which team is responsible for what Yuki's library runs on?", [1, 2]),
    (["Bianca founded the Lumen startup.",
      "The Lumen startup leases space in the Cedar tower.",
      "The Cedar tower is managed by the Hawthorne property group."],
     "Who manages the building where Bianca's startup is located?", [1, 2]),
    (["Rashid coaches the Falcons squad.",
      "The Falcons squad trains at the Birchwood arena.",
      "The Birchwood arena was built by the Stonebridge firm."],
     "Which firm constructed the venue where Rashid's squad practices?", [1, 2]),
    (["Elena curates the Vermillion collection.",
      "The Vermillion collection is stored at the Glasshouse annex.",
      "The Glasshouse annex floods during the monsoon season."],
     "When is the location of Elena's collection at risk?", [1, 2]),
    (["Kwame directs the Solstice fund.",
      "The Solstice fund invests heavily in the Marlin venture.",
      "The Marlin venture operates out of the Harbor district."],
     "In which district does the main investment of Kwame's fund operate?", [1, 2]),
    (["Ingrid wrote the Nimbus proposal.",
      "The Nimbus proposal targets the Greenfield market.",
      "The Greenfield market is regulated by the Tideland authority."],
     "Which authority regulates the market in Ingrid's proposal?", [1, 2]),
    (["Hassan runs the Tundra pipeline.",
      "The Tundra pipeline feeds the Crimson warehouse.",
      "The Crimson warehouse is audited every quarter."],
     "How often is the destination of Hassan's pipeline audited?", [1, 2]),
    (["Noor leads the Zephyr team.",
      "The Zephyr team owns the Ironwood module.",
      "The Ironwood module was deprecated last spring."],
     "What happened to the module owned by Noor's team?", [1, 2]),
    (["Leon manages the Drift account.",
      "The Drift account is billed through the Vantage system.",
      "The Vantage system charges fees in euros."],
     "In which currency are charges applied to Leon's account?", [1, 2]),

    # ---- 3-hop ----
    (["Sofia oversees the Comet program.",
      "The Comet program relies on the Basalt cluster.",
      "The Basalt cluster is powered by the Riverbend grid.",
      "The Riverbend grid draws from the Eastgate dam."],
     "Which dam ultimately supplies power to Sofia's program?", [1, 2, 3]),
    (["Mateo heads the Orchid division.",
      "The Orchid division ships through the Verde port.",
      "The Verde port connects to the Sandbar highway.",
      "The Sandbar highway crosses the Pinecrest county."],
     "Which county does the shipping route of Mateo's division pass through?", [1, 2, 3]),
    (["Aisha sponsors the Lattice scholarship.",
      "The Lattice scholarship is administered by the Brightwater foundation.",
      "The Brightwater foundation banks with the Sterling trust.",
      "The Sterling trust is headquartered in Zurich."],
     "Where is the bank of the foundation behind Aisha's scholarship based?", [1, 2, 3]),
    (["Felix architects the Onyx platform.",
      "The Onyx platform authenticates via the Kestrel gateway.",
      "The Kestrel gateway forwards to the Halcyon directory.",
      "The Halcyon directory syncs nightly with the legacy mainframe."],
     "What does the auth backend of Felix's platform sync with?", [1, 2, 3]),
    (["Greta launched the Pinnacle campaign.",
      "The Pinnacle campaign is tracked in the Cobblestone dashboard.",
      "The Cobblestone dashboard pulls from the Driftwood feed.",
      "The Driftwood feed updates only on weekdays."],
     "How frequently does the underlying data feed of Greta's campaign refresh?", [1, 2, 3]),
    (["Omar manages the Vesper rollout.",
      "The Vesper rollout depends on the Amber toolkit.",
      "The Amber toolkit is licensed from the Quill vendor.",
      "The Quill vendor sunsets support next year."],
     "When does support end for the toolkit behind Omar's rollout?", [1, 2, 3]),
    (["Lena directs the Cascade study.",
      "The Cascade study recruits from the Hollow clinic.",
      "The Hollow clinic partners with the Ridgeline university.",
      "The Ridgeline university is in the Maplewood town."],
     "In which town is the university partnered with the clinic for Lena's study?", [1, 2, 3]),
    (["Bruno owns the Slate franchise.",
      "The Slate franchise sources beans from the Acacia farm.",
      "The Acacia farm sits in the Verdant valley.",
      "The Verdant valley suffers droughts every few years."],
     "What climate problem affects the source region for Bruno's franchise?", [1, 2, 3]),
    (["Carmen leads the Beacon coalition.",
      "The Beacon coalition lobbies the Granite assembly.",
      "The Granite assembly convenes in the Capitol annex.",
      "The Capitol annex is closed for renovation."],
     "What is the status of the venue where the body lobbied by Carmen's coalition meets?", [1, 2, 3]),
    (["Viktor maintains the Cinder framework.",
      "The Cinder framework bundles the Pewter parser.",
      "The Pewter parser was forked from the Driftless project.",
      "The Driftless project is no longer maintained."],
     "What is the state of the upstream project behind Viktor's framework's parser?", [1, 2, 3]),
    (["Tara coordinates the Harbor festival.",
      "The Harbor festival is sponsored by the Goldleaf brewery.",
      "The Goldleaf brewery sources hops from the Willow estate.",
      "The Willow estate was sold to a foreign buyer."],
     "What recently happened to the hop supplier of Tara's festival sponsor?", [1, 2, 3]),
    (["Idris runs the Mosaic exchange.",
      "The Mosaic exchange clears trades through the Pillar bank.",
      "The Pillar bank reports to the Summit regulator.",
      "The Summit regulator imposed new rules this month."],
     "Which oversight body recently changed rules affecting Idris's exchange?", [1, 2, 3]),
]


def main():
    # Build corpus: all chain link memories + heavy distractors, shuffled.
    chain_mems = []  # flat list of every link memory text
    for links, _q, _req in CHAINS:
        chain_mems.extend(links)

    corpus = list(chain_mems)

    # Plausible distractors echoing the same "entity does X / X relates to Y"
    # relational shape, so distractors are semantically adjacent to the chains.
    DISTRACT = [
        "Carlos manages the Aurora reporting suite.",        # collides on 'Aurora'
        "The Helios mascot appears at every company event.",  # collides on 'Helios'
        "The Frankfurt office hosts the annual summit.",      # collides on 'Frankfurt'
        "Nadia leads the Borealis project.",
        "The Borealis project runs on the Cascade database.",
        "The Cascade database lives in the Dublin datacenter.",
        "The Pegasus statue stands in the lobby.",
        "The platform team also owns the build system.",
        "The Harbor district is famous for its seafood.",
        "The Sterling award is given out each spring.",
        "Many startups lease space in the Cedar district downtown.",
        "The Riverbend cafe serves the best coffee nearby.",
        "The Quartz countertops were installed last year.",
        "The Falcons mascot is a large foam bird.",
        "The monsoon season brings heavy rain to the coast.",
        "The euro strengthened against the dollar last week.",
        "The mainframe migration is scheduled for next quarter.",
        "The drought relief fund opened applications today.",
        "The brewery tour runs every Saturday afternoon.",
        "The regulator published its annual report online.",
    ]
    corpus += DISTRACT * 3

    TOPICS = ["the Q3 roadmap", "the migration", "a customer issue", "the offsite",
              "code review", "onboarding", "an incident", "pricing", "the refactor",
              "a contract", "the campaign", "a retro", "the budget", "hiring"]
    ENTS = ["Orion", "Vega", "Nova", "Delta", "Echo", "Sierra", "Tango", "Lima"]
    i = 0
    while len(corpus) < 3000:
        ent = ENTS[i % len(ENTS)]
        corpus.append(f"On day {i % 365} the {ent} group handled {TOPICS[i % len(TOPICS)]}; note {i} for team {i % 6}.")
        i += 1
    random.shuffle(corpus)

    p, call = srv(DB)
    stored = 0
    for b in range(0, len(corpus), 100):
        err, out = call("memory_ingest_batch",
                        {"items": [{"text": x, "channel": "multihop"} for x in corpus[b:b + 100]]})
        stored += json.loads(out).get("stored", 0)

    err, stats = call("memory_stats", {})
    st = json.loads(stats)
    print(f"ingested {stored} | index_size={st['index_size']} total={st['total']}\n")

    n2 = sum(1 for _l, _q, req in CHAINS if len(req) == 2)
    n3 = sum(1 for _l, _q, req in CHAINS if len(req) == 3)
    print(f"=== MULTI-HOP RELATIONAL RECALL ({len(CHAINS)} chains: {n2} 2-hop, {n3} 3-hop, ~{st['total']} memories) ===")

    # hop-recall: over EVERY required hop memory across all chains, what fraction
    # surfaced at rank 0 / <5 / <10. chain-completable: all required hops in top-10.
    hop_total = 0
    hop_r1 = hop_r5 = hop_r10 = 0
    chains_complete = 0
    lat = []
    incomplete = []

    for links, query, req in CHAINS:
        t = time.time()
        err, out = call("memory_search", {"query": query, "limit": 10})
        lat.append((time.time() - t) * 1000)
        res = json.loads(out).get("results", [])
        ranks = {}  # required link idx -> rank (or -1)
        all_in_top10 = True
        for idx in req:
            target = links[idx]
            rk = next((r_i for r_i, r in enumerate(res) if r.get("text", "") == target), -1)
            ranks[idx] = rk
            hop_total += 1
            if rk == 0:
                hop_r1 += 1
            if 0 <= rk < 5:
                hop_r5 += 1
            if 0 <= rk < 10:
                hop_r10 += 1
            else:
                all_in_top10 = False
        if all_in_top10:
            chains_complete += 1
        else:
            missed = [links[idx][:40] for idx in req if not (0 <= ranks[idx] < 10)]
            incomplete.append((query[:50], missed))

    C = len(CHAINS)
    print(f"hop-recall@1  : {hop_r1}/{hop_total} = {hop_r1 / hop_total * 100:.0f}%   (required hop memory at rank 0)")
    print(f"hop-recall@5  : {hop_r5}/{hop_total} = {hop_r5 / hop_total * 100:.0f}%   (required hop memory in top-5)")
    print(f"hop-recall@10 : {hop_r10}/{hop_total} = {hop_r10 / hop_total * 100:.0f}%   (required hop memory in top-10)")
    print(f"chain-completable@10 : {chains_complete}/{C} = {chains_complete / C * 100:.0f}%   (ALL required hops in top-10)")
    print(f"latency: p50={statistics.median(lat):.1f}ms p95={sorted(lat)[min(int(0.95 * C), C - 1)]:.1f}ms")

    if incomplete:
        print(f"\nINCOMPLETE CHAINS (missing >=1 required hop in top-10), {len(incomplete)}:")
        for q, missed in incomplete:
            print(f"  - q: {q}")
            for m in missed:
                print(f"      missing hop: {m}")

    p.stdin.close()
    p.wait(timeout=10)


if __name__ == "__main__":
    try:
        main()
    finally:
        shutil.rmtree(TMP, ignore_errors=True)
