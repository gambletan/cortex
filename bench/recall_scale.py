#!/usr/bin/env python3
"""Recall-quality-at-scale benchmark for Cortex (black-box via MCP stdio).

Plants 20 "needle" memories in natural prose, buries them among ~3000
semantically-adjacent distractors, then queries with PARAPHRASES sharing no
keywords with the needle — the hard semantic-recall case. Reports recall@1/5/10
and latency. See docs/scale-test-2026-06-13.md for methodology and the
2026-06-13 result (ef_search beam widening took paraphrase recall@10 40% -> 90%).

Usage: BIN=~/.local/bin/cortex-mcp-server python3 bench/recall_scale.py
"""
import json, os, subprocess, time, statistics, random, tempfile
TMP=tempfile.mkdtemp(prefix="cortex-recall-"); DB=f"{TMP}/hard.db"
BIN=os.path.expanduser(os.environ.get("BIN","~/.local/bin/cortex-mcp-server")); random.seed(7)
def srv(db):
    e={**os.environ,"RUST_LOG":"error","CORTEX_NO_KEYCHAIN":"1"}
    p=subprocess.Popen([BIN,db],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,env=e)
    n=[0]
    def call(name,args):
        n[0]+=1;p.stdin.write(json.dumps({"jsonrpc":"2.0","id":n[0],"method":"tools/call","params":{"name":name,"arguments":args}})+"\n");p.stdin.flush()
        r=json.loads(p.stdout.readline())["result"];return r.get("isError",False),r["content"][0]["text"]
    p.stdin.write('{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}\n');p.stdin.flush();p.stdout.readline();return p,call

# Needles use natural prose; QUERIES are paraphrases with MINIMAL lexical overlap (semantic recall test).
# Each needle also has a unique id token buried so we can score unambiguously.
needles=[
 ("My cardiologist said I should cut back on espresso to at most one cup before noon.",
  "What did the heart doctor advise about my caffeine habit?"),
 ("The spare apartment key is taped under the third drawer of the workshop bench.",
  "Where can someone find the backup key to get into the flat?"),
 ("I promised Grandma I'd call her every Sunday evening after the news.",
  "What's my weekly commitment to my grandmother?"),
 ("The staging server refuses connections unless you first run the VPN profile named 'corp-west'.",
  "Why can't I reach the pre-production box, and what fixes it?"),
 ("We decided to drop the freemium tier because support costs were eating the margin.",
  "What was the reasoning behind killing our no-cost plan?"),
 ("Dad's woodworking shop closes for the whole of August every year.",
  "When is my father's furniture business not operating?"),
 ("The conference reimbursement form must be filed within 14 days or finance rejects it.",
  "How long do I have to claim travel expenses before it's too late?"),
 ("Our golden retriever can't have chocolate or grapes — the vet was very firm about grapes.",
  "Which foods are dangerous for the dog?"),
 ("I switched the bedroom thermostat schedule to drop to 18 degrees at 11pm.",
  "What temperature does the bedroom cool to overnight?"),
 ("The investor call got pushed to the first Tuesday of next month because of the holiday.",
  "When is the rescheduled meeting with the funders?"),
 ("Maria from the Lisbon office is the only one with access to the design archive.",
  "Who can let me into the old design files?"),
 ("I always get carsick unless I sit in the front passenger seat.",
  "Where do I need to sit to avoid motion sickness?"),
 ("The sourdough starter needs feeding every 12 hours when kept at room temperature.",
  "How often must I refresh the bread culture left on the counter?"),
 ("We agreed engineers rotate the on-call pager weekly, handing off on Monday mornings.",
  "How is the emergency duty shared among the dev team?"),
 ("The cabin has no cell signal so we rely on the satellite messenger for emergencies.",
  "How do we communicate from the remote lodge if something goes wrong?"),
 ("I'm saving the window seat on the night train for the mountain sunrise.",
  "Which spot did I reserve to watch dawn over the peaks from the sleeper?"),
 ("The accountant wants all receipts scanned as PDF, never photos, or she won't process them.",
  "What format does the bookkeeper require for expense documents?"),
 ("Our daughter's peanut allergy means the whole house is strictly nut-free.",
  "Why don't we keep any nuts at home?"),
 ("The backup drive lives in the fireproof safe in the garage, not the office.",
  "Where is the external disk with our backups stored?"),
 ("I quit the gym downtown and joined the pool near the station instead.",
  "Which fitness place did I move to, and where is it?"),
]
corpus=[t for t,_ in needles]
# heavy semantically-adjacent distractors (health, keys, family, servers, money, pets, travel...)
DISTRACT=[
 "The doctor mentioned my cholesterol was slightly elevated at the last checkup.",
 "I keep the car keys on the hook by the front door.",
 "Mom usually texts me on weekday afternoons about errands.",
 "The production database backups run nightly at 2am UTC.",
 "We raised the price of the premium tier by ten percent last quarter.",
 "The hardware store on Main Street closes early on Sundays.",
 "Submit the quarterly tax estimate before the fifteenth.",
 "The cat is fine with most foods but hates the salmon brand.",
 "I set the living room lights to dim at sunset automatically.",
 "The board meeting is usually the last Thursday of the month.",
 "Tom from the Berlin office handles the marketing assets.",
 "I prefer the aisle seat on long flights for the legroom.",
 "The yogurt culture should be incubated around 43 degrees.",
 "Designers rotate who runs the weekly critique session.",
 "The office has fiber internet with a backup LTE failover.",
 "I booked a standard room facing the courtyard, not the sea.",
 "Finance prefers invoices submitted through the portal.",
 "Our son loves peanut butter sandwiches for lunch.",
 "The router sits on the shelf above the office desk.",
 "I renewed my membership at the climbing gym downtown.",
]
corpus += DISTRACT*3
TOPICS=["the Q3 roadmap","the migration","a customer issue","the offsite","code review","onboarding",
        "an incident","pricing","the refactor","a contract","the campaign","a retro","the budget","hiring"]
i=0
while len(corpus)<3000:
    corpus.append(f"On day {i%365} we handled {TOPICS[i%len(TOPICS)]}; note {i} for team {i%6}."); i+=1
random.shuffle(corpus)

p,call=srv(DB)
stored=0
for b in range(0,len(corpus),100):
    err,out=call("memory_ingest_batch",{"items":[{"text":x,"channel":"hard"} for x in corpus[b:b+100]]})
    stored+=json.loads(out).get("stored",0)
err,stats=call("memory_stats",{});st=json.loads(stats)
print(f"ingested {stored} | index_size={st['index_size']} total={st['total']}\n")
print(f"=== SEMANTIC RECALL (paraphrased queries, ~{st['total']} memories) ===")
r1=r5=r10=0; lat=[]; miss=[]
for needle,query in needles:
    t=time.time();err,out=call("memory_search",{"query":query,"limit":10});lat.append((time.time()-t)*1000)
    res=json.loads(out).get("results",[])
    key=needle[:30]
    rk=next((idx for idx,r in enumerate(res) if key in r.get("text","")),-1)
    if rk==0:r1+=1
    if 0<=rk<5:r5+=1
    if 0<=rk<10:r10+=1
    if rk<0: miss.append(query[:45])
N=len(needles)
print(f"recall@1  : {r1}/{N} = {r1/N*100:.0f}%")
print(f"recall@5  : {r5}/{N} = {r5/N*100:.0f}%")
print(f"recall@10 : {r10}/{N} = {r10/N*100:.0f}%")
print(f"latency: p50={statistics.median(lat):.1f}ms p95={sorted(lat)[int(0.95*N)]:.1f}ms")
if miss:
    print(f"\nMISSED (not in top-10), {len(miss)}:")
    for m in miss: print(f"  - {m}")
p.stdin.close();p.wait(timeout=10)
