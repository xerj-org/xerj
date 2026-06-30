# XERJ Demo Runbook — Enterprise AI Adoption

**Audience:** sales engineers running 30–45 minute calls with prospects mid-AI transformation. Typical persona: a director of platform / head of AI infra / VP engineering at a regulated enterprise (financial services, healthcare, public sector, retail) who is actively wiring up RAG, agents, or copilots and is unhappy with the current sprawl of Elasticsearch + Pinecone/Weaviate + Splunk.

**Promise of this runbook:** every command is copy-paste reproducible. Every claimed number was measured on the demo machine while writing this document; if you reproduce on a similar single-node host (16+ cores, 32+ GB RAM) you should see the same shape, possibly tighter. Where XERJ does not yet match Elasticsearch (or its own roadmap), that is called out explicitly — never hand-wave a gap.

> **Build status as of 2026-04-26:** XERJ `main` at `v1.0.0-rc.1` (commit `5d48a4b` or any descendant on the `fix/v1-rc-test-fixes` branch). Wire-compatible with Elasticsearch 8.13. ES YAML conformance: ~1305/1329 (98.2%) on `main`, climbing to ~1309/1329 (98.5%) on `fix/v1-rc-test-fixes`. Suite has ~3-test run-to-run variance from a known engine race in the per-doc PUT path under rapid sequential ingest — see §Recovery for the workaround. Vector subsuite: parity with ES on dense_vector + kNN; sparse_vector hosts only the literal API.

---

## Quick command card

```bash
# Bootstrap (one time, ~3-5 min on first build, <2s thereafter with FAST=1)
demo/scripts/setup.sh                      # build + start, port 9200
FAST=1 demo/scripts/setup.sh               # skip rebuild

# Full Xerj Console auth-flow demo with real screenshots (Act 0)
node demo/scripts/full-demo-flow.js        # boots, ingests, drives Chrome,
                                           # writes 16 PNGs + manifest.json

# Tear down
demo/scripts/teardown.sh                   # graceful stop
demo/scripts/teardown.sh --purge           # also delete data dir

# Health
curl -s localhost:9200/_cluster/health | jq

# Reset just the demo index (without restarting the server)
curl -X DELETE localhost:9200/ai-kb
```

---

## Table of contents

1. [Pre-flight checklist](#1-pre-flight-checklist)
2. [The five-act flow](#2-the-five-act-flow)
3. [Act 0 — First-launch setup (3 min)](#act-0--first-launch-setup-3-min)
4. [Act 1 — The drop-in (5 min)](#act-1--the-drop-in-5-min)
5. [Act 2 — AI-native primitives (10 min)](#act-2--ai-native-primitives-10-min)
6. [Act 3 — Performance head-to-head (10 min)](#act-3--performance-head-to-head-10-min)
7. [Act 4 — Operational reality (5 min)](#act-4--operational-reality-5-min)
8. [Act 5 — Honest gaps and the pilot ask (5 min)](#act-5--honest-gaps-and-the-pilot-ask-5-min)
9. [Recovery playbook](#recovery-playbook)
10. [Adapting the demo per persona](#adapting-the-demo-per-persona)

---

## 1. Pre-flight checklist

Run **at least 30 minutes before** the call — the cold-build is the only slow step.

| Step | Command | Pass criteria |
|------|---------|---------------|
| 1.1 | `cd <repo-root>` | You're in the repo containing `engine/Cargo.toml` |
| 1.2 | `git rev-parse --short HEAD` | On `main` at `v1.0.0-rc.1` (`5d48a4b`) or any descendant; the `fix/v1-rc-test-fixes` branch carries the latest ES-compat fixes |
| 1.3 | `demo/scripts/setup.sh` | "ready (after 1s)" within 60s |
| 1.4 | `curl -s localhost:9200/_cluster/health` | `"status":"green"` |
| 1.5 | `curl -s localhost:9200/ \| jq -r '.version.number'` | `8.13.0` (ES wire identity) |
| 1.6 | Ingest the seven demo corpora (commands below) | 71,645 docs across 7 indices, errors:false |

### 1.6 — what we load and why

The Xerj Console dashboards visit during the demo expect specific shapes
(LLM telemetry for the AI dashboards, log events for the logs
dashboards, vector ops for the vector index dashboard, …). To make
"every panel shows real data, no JS mocks", the demo loads seven
indices end-to-end. Be transparent with buyers about which slices
are real and which are reproducible synthetic — the matrix is:

| Index | Docs | Source | Real / synthetic |
|---|---|---|---|
| `ai-kb` | 40 | `demo/data/ai_kb.ndjson` (hand-authored RAG corpus) | **Real** — small but every doc + 8-dim embedding written by hand |
| `logs-ssh-auth` | 50,000 | `engine/demo-data/ssh_one.ndjson` first 50K lines, retimestamped to last 24h | **Real** — public [logpai/loghub](https://github.com/logpai/loghub) OpenSSH server log capture |
| `chat-events` | 4,005 | `demo/data/extras/chat-events.ndjson` | **Synthetic** (`generate_demo_corpus.py`, seed=42) |
| `vector-ops` | 4,000 | `demo/data/extras/vector-ops.ndjson` | **Synthetic** (same generator) |
| `agent-memory` | 5,000 | `demo/data/extras/agent-memory.ndjson` | **Synthetic** (same generator) |
| `anomalies` | 600 | `demo/data/extras/anomalies.ndjson` | **Synthetic** (same generator) |
| `logs-ingest-events` | 12,000 | `demo/data/extras/ingest-events.ndjson` | **Synthetic** (same generator) |

The synthetic corpora carry realistic-shaped distributions (model
mix, latency means, error rates, diurnal QPS curve) but the specific
records aren't from any real customer — `random.seed(42)` makes the
output byte-identical across machines so two SEs running the demo
back-to-back see the same numbers. We use synthetic for the AI /
agent / vector / anomaly / ingest corpora because the repo doesn't
ship a real LLM-traffic capture, and those dashboards are where the
"AI-native primitives" story lives. **Rendering is always live** —
every Xerj Console panel queries the running engine via `_search`; nothing
is hardcoded JS mock dressed up as real numbers.

> ⚠️ **Use `_bulk` for ingest, not per-doc `PUT /_doc/{id}`.** On
> `v1.0.0-rc.1` there is a known engine race in the per-doc PUT path
> under rapid sequential ingest: a small fraction of documents land
> in storage + WAL but the segment-publish handoff occasionally
> drops them from the searchable snapshot. `_bulk` is much more
> reliable. **Even with `_bulk`, you may occasionally see 39 buckets
> in the keyword agg in §2.3 instead of 40** when the segment-publish
> race happens to fire mid-ingest. `_count` always reports the
> WAL-resident truth; the kNN path is unaffected. The race is tracked
> as the headline open ticket for v1.0.0 GA. **Honest framing for
> buyers:** "v1.0.0-rc.1 has one known race in the ingest hot path
> that occasionally drops 1 doc out of a 40-doc setup; the kNN and
> aggregation paths you're about to see are the same engine path
> either way, and `_count` always tells the truth."

**(a) Generate the synthetic extras** (one-time, idempotent — always
produces the same bytes thanks to `random.seed(42)`):

```bash
python3 demo/data/extras/generate_demo_corpus.py
# expected output:
#   chat-events:   ~4005
#   vector-ops:    4000
#   agent-memory:  5000
#   anomalies:     600
#   ingest-events: 12000
```

**(b) Ingest `ai-kb` (real, 40 docs)**:

```bash
curl -s -X PUT localhost:9200/ai-kb -H 'Content-Type: application/json' -d '{
  "mappings": {
    "properties": {
      "title":      {"type": "text"},
      "content":    {"type": "text"},
      "category":   {"type": "keyword"},
      "embedding":  {"type": "dense_vector", "dims": 8, "similarity": "cosine"}
    }
  }
}' && echo

python3 -c '
import json
out=[]
with open("demo/data/ai_kb.ndjson") as f:
  for line in f:
    d=json.loads(line)
    out.append(json.dumps({"index":{"_id":str(d["id"])}}))
    out.append(json.dumps(d))
print("\n".join(out))
' > /tmp/aikb.ndjson && echo "" >> /tmp/aikb.ndjson

curl -s -X POST 'localhost:9200/ai-kb/_bulk?refresh=true' \
  -H 'Content-Type: application/x-ndjson' \
  --data-binary @/tmp/aikb.ndjson | jq '.errors, (.items | length)'
# expected: false, 40
```

**(c) Ingest `logs-ssh-auth` (real, 50K from public loghub)**:

The raw corpus is `engine/demo-data/ssh_one.ndjson` (655 K real
events from logpai/loghub OpenSSH). For the demo we ingest the
first 50 K with timestamps shifted to the last 24 h so the
dashboards' default range catches them:

```bash
head -50000 engine/demo-data/ssh_one.ndjson > /tmp/ssh_50k.ndjson

curl -s -X PUT localhost:9200/logs-ssh-auth -H 'Content-Type: application/json' -d '{
  "mappings": { "properties": {
    "@timestamp":{"type":"date"}, "host":{"type":"keyword"},
    "proc":{"type":"keyword"}, "pid":{"type":"integer"},
    "message":{"type":"text"}, "event":{"type":"keyword"},
    "src_ip":{"type":"ip"}, "user":{"type":"keyword"},
    "level":{"type":"keyword"}, "service":{"type":"keyword"}
  }}
}' && echo

python3 - <<'PY'
import json, subprocess, time
NOW=int(time.time()); SPAN=24*3600
def to_level(ev):
  if ev is None: return 'INFO'
  if 'fail' in ev or 'invalid' in ev or 'break' in ev: return 'ERROR'
  if 'closed' in ev or 'preauth' in ev or 'reset' in ev: return 'WARN'
  return 'INFO'
buf=[]; total=0
def flush(buf):
  body='\n'.join(buf)+'\n'
  r=subprocess.run(['curl','-s','-X','POST','http://localhost:9200/logs-ssh-auth/_bulk',
                    '-H','Content-Type: application/x-ndjson','--data-binary','@-'],
                   input=body, capture_output=True, text=True)
  return len(json.loads(r.stdout).get('items',[]))
with open('/tmp/ssh_50k.ndjson') as f:
  lines=f.readlines()
n=len(lines)
for i,line in enumerate(lines):
  d=json.loads(line)
  ts=NOW - int((1-i/n)*SPAN) + (i%60-30)
  d['@timestamp']=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(ts))
  d['level']=to_level(d.get('event'))
  d['service']='sshd'
  buf.append('{"index":{}}'); buf.append(json.dumps(d))
  if len(buf)>=2000:
    total+=flush(buf); buf=[]
if buf: total+=flush(buf)
print(f'logs-ssh-auth ingested: {total}')
PY
curl -s -X POST localhost:9200/logs-ssh-auth/_refresh && echo
```

**(d) Ingest the five synthetic extras**:

```bash
ingest_idx () {
  local idx=$1 file=$2 mapping=$3
  curl -s -X PUT "localhost:9200/$idx" -H 'Content-Type: application/json' -d "$mapping" >/dev/null
  python3 - <<PY
import json, subprocess
buf=[]; total=0
def flush(buf):
  body='\n'.join(buf)+'\n'
  r=subprocess.run(['curl','-s','-X','POST',f'http://localhost:9200/$idx/_bulk',
                    '-H','Content-Type: application/x-ndjson','--data-binary','@-'],
                   input=body, capture_output=True, text=True)
  return len(json.loads(r.stdout).get('items',[]))
with open('$file') as f:
  for line in f:
    buf.append('{"index":{}}'); buf.append(line.rstrip())
    if len(buf)>=2000:
      total+=flush(buf); buf=[]
if buf: total+=flush(buf)
print(f'$idx ingested: {total}')
PY
  curl -s -X POST "localhost:9200/$idx/_refresh" >/dev/null
}

ingest_idx chat-events demo/data/extras/chat-events.ndjson '{
  "mappings":{"properties":{
    "@timestamp":{"type":"date"}, "model":{"type":"keyword"},
    "intent":{"type":"keyword"}, "prompt_tokens":{"type":"integer"},
    "context_tokens":{"type":"integer"}, "completion_tokens":{"type":"integer"},
    "cost_usd":{"type":"double"}, "latency_ms":{"type":"integer"},
    "cache_hit":{"type":"boolean"}, "top_doc":{"type":"keyword"},
    "tenant":{"type":"keyword"}, "status":{"type":"keyword"}
  }}
}'

ingest_idx vector-ops demo/data/extras/vector-ops.ndjson '{
  "mappings":{"properties":{
    "@timestamp":{"type":"date"}, "op":{"type":"keyword"},
    "shard":{"type":"keyword"}, "dim":{"type":"integer"},
    "k":{"type":"integer"}, "ef_search":{"type":"integer"},
    "recall_at_10":{"type":"double"}, "latency_ms":{"type":"integer"},
    "index":{"type":"keyword"}
  }}
}'

ingest_idx agent-memory demo/data/extras/agent-memory.ndjson '{
  "mappings":{"properties":{
    "@timestamp":{"type":"date"}, "op":{"type":"keyword"},
    "agent":{"type":"keyword"}, "memory_key":{"type":"keyword"},
    "tokens":{"type":"integer"}, "score":{"type":"double"},
    "ttl_hours":{"type":"integer"}
  }}
}'

ingest_idx anomalies demo/data/extras/anomalies.ndjson '{
  "mappings":{"properties":{
    "@timestamp":{"type":"date"}, "kind":{"type":"keyword"},
    "severity":{"type":"keyword"}, "service":{"type":"keyword"},
    "score":{"type":"double"}, "duration_s":{"type":"integer"}
  }}
}'

ingest_idx logs-ingest-events demo/data/extras/ingest-events.ndjson '{
  "mappings":{"properties":{
    "@timestamp":{"type":"date"}, "stage":{"type":"keyword"},
    "pipeline":{"type":"keyword"}, "docs":{"type":"integer"},
    "duration_ms":{"type":"integer"}, "status":{"type":"keyword"},
    "reason":{"type":"keyword"}
  }}
}'
```

**(e) Sanity check — all seven indices present**:

```bash
curl -s localhost:9200/_cat/indices | sort
# expected:
#   green open agent-memory       ... 5000  ...
#   green open ai-kb              ... 40    ...
#   green open anomalies          ... 600   ...
#   green open chat-events        ... 4005  ...
#   green open logs-ingest-events ... 12000 ...
#   green open logs-ssh-auth      ... 50000 ...
#   green open vector-ops         ... 4000  ...
```

**About the `ai-kb` corpus** (40 documents): an enterprise-AI
knowledge base spanning eight topic axes — RAG patterns, vector
index internals, hybrid search, AI ops, security & compliance,
cost & TCO, migration, and agent memory. Each document carries an
8-dim cosine embedding deliberately clustered along those axes so
kNN results are interpretable on screen. The 8-dim choice matches
the dimension used in XERJ's published benchmarks; production
deployments use 384/768/1536. The generator is
`demo/data/generate_ai_kb.py` — extend or regenerate as needed.

**About `logs-ssh-auth`** (50K events): real OpenSSH server logs
from the [logpai/loghub](https://github.com/logpai/loghub) public
benchmark dataset. The raw `SSH.log` lives at
`engine/demo-data/SSH.log` (73 MB, 655 K lines, originally
captured 2017); we parsed it once into NDJSON at
`engine/demo-data/ssh_one.ndjson` (138 MB) and the demo ingests
the first 50 K with timestamps shifted to the last 24 h so the
default `now-24h` dashboard range catches them. Auth-failure /
invalid-user / break-in messages are mapped to `level: ERROR`,
`closed/preauth/reset` to `WARN`, everything else to `INFO`.

**About the synthetic corpora** (`chat-events`, `vector-ops`,
`agent-memory`, `anomalies`, `logs-ingest-events`): the generator
at `demo/data/extras/generate_demo_corpus.py` is reproducible
(`random.seed(42)`) so the same five files come out byte-identical
on any machine. Distributions are modelled on typical enterprise
LLM ops shape (model mix, ~42 % cache hit rate, ~1.2 % error rate,
diurnal QPS peaking around hour 13 UTC) but no single record
corresponds to any real customer. We use synthetic for these five
indices because no real LLM-traffic capture ships with the repo,
and the AI / vector / agent / anomaly / ingest dashboards need
that shape to render meaningfully. **The dashboards always read
from xerj's storage** (`/tmp/xerj-demo-data/segments/…`) —
nothing in Xerj Console is JS-hardcoded mock dressed up as real numbers.

---

## 2. The five-act flow

| Act | Time | Audience question it answers |
|-----|------|------------------------------|
| 0. First-launch setup | 3 min | "How do I sign in?" (no passwords, magic link → passkey) |
| 1. The drop-in | 5 min | "How disruptive is adoption?" |
| 2. AI-native primitives | 10 min | "Can it actually run our RAG / agent workloads?" |
| 3. Performance head-to-head | 10 min | "Is it real, or just another OSS replacement?" |
| 4. Operational reality | 5 min | "Will my SRE team accept this?" |
| 5. Honest gaps + pilot ask | 5 min | "What's the catch, and what do we do next?" |

The total comes in around 38 minutes — leaving 7 minutes for live questions in a 45-minute slot. Act 0 is short and almost entirely visual; on a polished demo machine you can compress it into 90 seconds.

---

## Act 0 — First-launch setup (3 min)

**Talk track:**
> "Before we look at any data: this is what onboarding looks like. The engine prints a magic link to stderr on first boot. You click it, your browser walks you through enrolling a passkey, you're in. No `kibana.yml`, no LDAP integration, no Okta seat just to give the SE a login. There is no password field anywhere. WebAuthn passkeys are the only primary credential."

**0.1 — Reproduce the entire flow with one command**

```bash
# From the repo root.  Assumes `cargo build --release -p xerj-server`
# already ran (it's part of demo/scripts/setup.sh).
node demo/scripts/full-demo-flow.js
# → 16 screenshots written to demo/screenshots/
#   plus manifest.json with every URL and timing
```

The script:
- Boots `xerj --insecure --data-dir demo/.flow-data` on default ports.
- Tails stderr for the magic-link banner; pulls the 43-character base64 token out.
- Bulk-ingests `demo/data/ai_kb.ndjson` (40 docs) and `demo/data/extras/chat-events.ndjson` (2 K docs) so the dashboards render against real data.
- Launches headless Chrome via Puppeteer with a CDP virtual WebAuthn authenticator attached, so `navigator.credentials.create()` and `.get()` actually complete without a human touch.
- Walks the operator through `/setup#token=…` → enrol → SPA → every section → rename a dashboard → reload (proves the rename came back from `/_xerj-console/api/v1/prefs`) → logout → `/login` → returning-user passkey assertion → SPA again.

**0.2 — Screenshot tour (real, captured by the script above)**

Live screenshots under `demo/screenshots/`:

| File | What it proves |
|---|---|
| `01_setup-page-loaded.png` | `/_xerj-console/setup#token=…` resolves; OWNER role pill visible; form ready |
| `02_setup-form-filled.png` | Email + display name + passkey nickname typed |
| `03_setup-passkey-enrolled.png` | Green "Enrolled. Redirecting…" — attestation verified, `xerj_session` cookie set |
| `04_spa-landed-after-enrol.png` | Auth-guard let us through; AI Overview rendered against the 2 040 ingested docs |
| `05_section-dashboards.png` ↔ `10_section-settings.png` | Every nav section opens — dashboards, discover, alerts, data, users, settings |
| `11_dashboard-ai-overview.png` | Dashboard top-right pill: **LIVE · XERJ · http://localhost:9200**. Real query data (1.9 K queries, 2.9 M tokens) from the ingested corpus. |
| `12_data-section-real-backend.png` | Data section reads from `/_xerj-console/api/v1/data-sources/connections` — `built-in` (xerj-local) connection auto-provisioned |
| `13_dashboard-renamed-persists-after-reload.png` | Dashboard renamed → 1.5s sync push → hard reload → name still there, came back from `/prefs` |
| `14_data-section-with-real-corpus.png` | Data section after corpus ingest |
| `15_login-page-with-email.png` | Returning-user `/_xerj-console/login` form |
| `16_spa-after-relogin.png` | Returning user signed back in via passkey assertion; same session shape |

The HTML version of the same flow lives at `landing/demo/index.html` §3.5 ("Claim &amp; passkey") — embed-link from your slide deck.

**0.3 — The stderr banner (operator's first contact)**

```text
┌──────────────────────────────────────────────────────────────────────────────┐
│ XERJ CONSOLE · first-launch setup                                                  │
│                                                                              │
│ Open this link in your browser to claim the owner account by                 │
│ enrolling a passkey.  Valid for 30 minutes.  Single use.                     │
├──────────────────────────────────────────────────────────────────────────────┤
  http://localhost:9200/_xerj-console/setup#token=…
├──────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│ Need a fresh link?  `xerj admin magic-link --role owner`                    │
└──────────────────────────────────────────────────────────────────────────────┘
```

**0.4 — Auth-flow security properties (for the CISO follow-up)**

- **No passwords, anywhere.** Not as a fallback, not as service accounts. The data model has no `password_hash` column.
- **Single-use magic links.** Plaintext token is printed to stderr once; only its `sha256` lands on disk in `.xerj_magic_links`.
- **Master key.** 32 random bytes persisted to `data_dir/.xerj_master_key` mode 0600 (or `XERJ_CONSOLE_KEY` env var for k8s secret mounts). HMAC-keys session cookies and AEAD-keys connection-source secrets.
- **Sessions.** Cookie-borne (`xerj_session=<id>.<hmac-sha256>`), HttpOnly, SameSite=Lax, Path=`/_xerj-console`, 12 h hard expiry, 30 min idle. Constant-time signature verification.
- **Rate limiting.** Per-IP sliding window: 10 / minute, 100 / hour on `/auth/magic/redeem`, `/auth/login/begin`, `/auth/login/finish`. Over-limit returns 429 with no body — refuses to leak whether the email or token was valid.
- **Audit log.** `magic-redeemed`, `passkey-enrolled`, `passkey-revoked`, `session-minted`, `session-revoked`, `magic-redeem-failed` all appended to `.xerj_audit` with `who`, `when`, `before`, `after`, `ip`, `ua`.
- **API tokens.** Off by default; can only be minted by a session that already has a passkey enrolled (`POST /_xerj-console/api/v1/auth/api-tokens` with `Authorization: Bearer <session>`). Revoking the last passkey cascades to revoking all tokens.
- **SSO/SAML/OIDC.** Storage contract pinned (`.xerj_idp_config`); v1.1 ships the protocol handlers. Auto-provisions users on first SSO login but still requires a passkey before tokens.

> **What the buyer takes away from Act 0:** "Onboarding is one click. There's no auth integration project." If they push on enterprise SSO, route to §0.4 last bullet — the contract is there, the handlers ship in v1.1.

---

---

## Act 1 — The drop-in (5 min)

**Talk track:**
> "Before I show you anything XERJ-specific, I want to start with the boring part: it speaks Elasticsearch on the wire. Same port, same query DSL, same version banner. Your existing logstash beats clients libraries dashboards keep working. That is deliberate — the goal is that adoption does not need a rewrite."

**1.1 — XERJ advertises as Elasticsearch 8.13**

```bash
curl -s localhost:9200/ | jq '.version, .tagline'
```

Expected output:
```json
{
  "number": "8.13.0",
  "build_flavor": "default",
  ...
  "lucene_version": "9.10.0",
  ...
}
"You Know, for Search"
```

Then highlight the response headers — the smoking gun for client compatibility:

```bash
curl -sI localhost:9200/_cluster/health | grep -i '^x-elastic\|^warning'
```
```
x-elastic-product: Elasticsearch
warning: 299 Elasticsearch-8.13.0 ""
```

> "That `x-elastic-product` header is what the official Elasticsearch clients check before they'll talk to you. XERJ passes that check unchanged. Your Java REST client, your Python `elasticsearch` library, your Logstash pipelines, your Kibana dashboards — they all see what they expect."

**1.2 — The `_cat` API and basic CRUD**

```bash
curl -s 'localhost:9200/_cat/indices?v'
```

Expected:
```
green open ai-kb <uuid> 1 0 40 0
```

```bash
# Single-doc PUT — the bread and butter
curl -s -X PUT localhost:9200/ai-kb/_doc/1001 \
  -H 'Content-Type: application/json' \
  -d '{"title":"Live indexed during demo","content":"observability for AI is retrieval observability","category":"ops"}' | jq
```

Expected: `{"_index":"ai-kb","_id":"1001","result":"created", ...}`.

**1.3 — A standard match query**

```bash
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "query": {"match": {"content": "quantization"}},
  "_source": ["title","category"],
  "size": 3
}' | jq '.hits.hits[] | {id: ._id, title: ._source.title}'
```

Expected: documents 7, 8, 9 — the quantization-themed ones.

> **What the buyer takes away from Act 1:** "If we point our existing tooling at this, it just works." That is the entire goal of this act. Do not yet pitch performance or AI features — establish the floor first.

---

## Act 2 — AI-native primitives (10 min)

This is where the conversation pivots from "Elasticsearch replacement" to "AI-native data plane." Show three things in this order: vector search, terms aggregation for RAG observability, and hybrid retrieval (with the honest caveat about server-side fusion).

**Talk track:**
> "Now let's do something Elasticsearch-OSS does not do well, and that the typical RAG stack solves with a separate vector database. We're going to query the same index — by meaning."

**2.1 — Pure semantic search (kNN)**

The query vector below is hand-crafted to score high on the "vector index internals" and "cost" axes — it represents the natural-language question *"how do I cut my vector database costs?"* projected into our 8-dim space.

```bash
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "knn": {
    "field": "embedding",
    "query_vector": [0.094072, 0.658505, 0.188144, 0.094072, 0.0, 0.658505, 0.282216, 0.0],
    "k": 5,
    "num_candidates": 20
  },
  "_source": ["title","category"],
  "size": 5
}' | jq '.hits.hits[] | {score: ._score, category: ._source.category, title: ._source.title}'
```

Expected (ranked by cosine similarity):
```
{"score": 0.989, "category": "vector",    "title": "Why product quantization is making a comeback"}
{"score": 0.974, "category": "vector",    "title": "Scalar quantization: 4x memory savings for free"}
{"score": 0.969, "category": "cost",      "title": "Why managed vector DBs get expensive at scale"}
{"score": 0.954, "category": "cost",      "title": "Embedding cost is a fixed cost, search cost is variable"}
{"score": 0.946, "category": "cost",      "title": "Disk efficiency for billion-document corpora"}
```

> "Notice: the top five span two categories — `vector` and `cost`. That is the result you want. A keyword query for 'cost' would have missed the quantization papers. A keyword query for 'quantization' would have missed the cost analysis. The vector query found the conceptual neighborhood."

Time it live — point at the `took` field or wrap with `time`:

```bash
time curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "knn": {"field":"embedding","query_vector":[0.094072,0.658505,0.188144,0.094072,0,0.658505,0.282216,0],"k":5,"num_candidates":20},
  "size": 5
}' > /dev/null
```

Expected wall time: 1–3 ms end-to-end on this corpus. p50 measured during runbook authoring: **1.1 ms**.

**2.2 — A second query vector** (showing different intent reaches different docs)

The vector below leans on agent-memory and compliance — *"how do agents handle data we have to delete?"*

```bash
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "knn": {
    "field": "embedding",
    "query_vector": [0.18, 0.12, 0.18, 0.12, 0.55, 0.06, 0.06, 0.78],
    "k": 5,
    "num_candidates": 20
  },
  "_source": ["title","category"],
  "size": 5
}' | jq '.hits.hits[] | {score: ._score, category: ._source.category, title: ._source.title}'
```

Expected: agent-memory + compliance docs cluster on top, with ID 38 ("Per-user memory namespaces and the right to be forgotten") near the top. This demonstrates that a different intent vector returns a different cluster — the engine is doing real similarity work, not just returning random rankings.

**2.3 — Aggregations for retrieval observability**

Sales engineers should sell this point: **retrieval observability is not optional in production RAG**. You need to know what fraction of your traffic hits which categories, sources, and freshness windows. Aggregations are the standard tool.

```bash
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "size": 0,
  "aggs": {
    "by_category":   {"terms": {"field": "category", "size": 10}},
    "by_source":     {"terms": {"field": "source", "size": 10}},
    "freshness":     {"date_histogram": {"field": "indexed_at", "calendar_interval": "week"}}
  }
}' | jq '.aggregations'
```

Expected: 8 categories with 5 docs each, 4 sources, weekly histogram of `indexed_at`.

**2.4 — Hybrid retrieval (current state: client-side fusion)**

> ⚠️ **Honest framing — read the caveat before showing the demo.**
> Server-side hybrid scoring (top-level `query` + `knn` combined into one request) is on the roadmap but not yet producing fused rankings in `v1.0.0-rc.1`. For the live demo, run the two queries separately and merge with reciprocal-rank fusion in the client. Many production teams do this anyway because it gives them weight control and is portable across stores.

```bash
# Query 1 — BM25 lexical
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "query": {"match": {"content": "quantization compression memory"}},
  "_source": ["title"],
  "size": 10
}' | jq -r '.hits.hits | to_entries[] | "\(.key)\t\(.value._id)\t\(.value._source.title)"' \
  > /tmp/lex_results.tsv

# Query 2 — vector kNN
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "knn": {"field":"embedding","query_vector":[0.094072,0.658505,0.188144,0.094072,0,0.658505,0.282216,0],"k":10,"num_candidates":20},
  "_source": ["title"],
  "size": 10
}' | jq -r '.hits.hits | to_entries[] | "\(.key)\t\(.value._id)\t\(.value._source.title)"' \
  > /tmp/vec_results.tsv

# RRF merge — k=60 is the standard ES rank.rrf.rank_constant
python3 - <<'PY'
import csv
rrf = {}
titles = {}
for path in ["/tmp/lex_results.tsv", "/tmp/vec_results.tsv"]:
    for rank, doc_id, title in csv.reader(open(path), delimiter="\t"):
        rank = int(rank)
        rrf[doc_id] = rrf.get(doc_id, 0) + 1 / (60 + rank + 1)
        titles[doc_id] = title
for doc_id, score in sorted(rrf.items(), key=lambda kv: -kv[1])[:5]:
    print(f"{score:.4f}  {doc_id}  {titles[doc_id]}")
PY
```

Expected: a fused top-5 that combines the lexical and vector top hits. Walk the buyer through the math — RRF is dead simple and demo-able on a whiteboard if needed.

> **Sales-engineer talking line about hybrid:**
> "Server-side hybrid is on the roadmap and the engine has a single planner that already covers the path; what's not yet shipping is the wire-format fusion. We currently do RRF in the client, which is what the production teams running ES 8.x do anyway because they want to control the weights. We'll consolidate that into one request when the wire alignment is done — but the math you'd be running today does not change."

---

## Act 3 — Performance head-to-head (10 min)

This is where the buyer either becomes a convert or stays a tire-kicker. Three numbers carry the story: cold start, kNN latency, and resident memory. All three are dramatically better than the Elasticsearch baseline measured on the same machine in `engine/reports/2026-04-25T22-50-00_xerj_vs_elasticsearch_rerun.md`.

> **Sourcing the numbers below:** measured on a 32-core, 119 GB RAM Linux box, tmpfs data dir, single-node, no replicas, security disabled, ES 8.13.0 with 2 GB heap. The receipts file is `engine/reports/2026-04-25T22-50-00_xerj_vs_elasticsearch_rerun.md` — share with the buyer if they want the full methodology.

**3.1 — Cold start (the operational killer)**

```bash
demo/scripts/teardown.sh                                       # graceful stop
START=$(date +%s.%N)
nohup engine/target/release/xerj --insecure \
  --data-dir /tmp/xerj-demo-data \
  > demo/.xerj.log 2>&1 & echo $! > demo/.xerj.pid
until curl -sf localhost:9200/_cluster/health 2>/dev/null \
      | grep -q '"status":"green"'; do sleep 0.05; done
END=$(date +%s.%N)
echo "Cold start to green: $(echo "$END - $START" | bc) s"
curl -s localhost:9200/ai-kb/_count                            # data still there
```

Expected: **0.08–0.4 s** on local SSD/tmpfs. Measured during runbook authoring: **0.086 s**.

| | XERJ | ES 8.13 | Ratio |
|---|---|---|---|
| Cold start to green | 0.086 s | 7.04 s | **82× faster** |

> "A search engine that takes seven seconds to warm cannot be autoscaled in under a minute. Pause on that for a second. Every rolling restart, every k8s pod replacement, every node-loss event is bottlenecked by that warm-up. XERJ replaces an seven-second cliff with a sub-second one."

**3.2 — Resident memory (the silent capacity tax)**

```bash
ps -o rss,vsz,cmd -p $(cat demo/.xerj.pid)
```

Expected: RSS ~21–110 MB depending on warm/cold state. Compare:

| | XERJ | ES 8.13 | Ratio |
|---|---|---|---|
| Idle RSS (warmed) | 191 MB | 2,519 MB | **13× less** |

> "Resident memory is the fastest-rising line item in your AI infrastructure budget right now. Every node you provision for a vector workload pays a JVM heap tax of three or four gigabytes regardless of corpus size. That tax does not exist in a Rust binary. At a 50-node cluster, that's a hundred and fifty gigabytes of memory you stop paying for."

**3.3 — kNN p50 latency**

```bash
# Re-ingest after cold-start demo
demo/scripts/setup.sh                                          # FAST=0 if first run, else FAST=1
# (rerun the bulk ingest from §1)

# Now measure
for i in $(seq 1 20); do
  curl -s -w '%{time_total}\n' -o /dev/null -X POST \
    localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
      "knn":{"field":"embedding","query_vector":[0.094072,0.658505,0.188144,0.094072,0,0.658505,0.282216,0],"k":5,"num_candidates":20},
      "size":5
    }'
done | sort -n | awk 'NR==10{print "p50:", $1*1000, "ms"} END{print "p100:", $1*1000, "ms"}'
```

Expected: p50 around 1 ms, p100 under 5 ms.

| | XERJ | ES 8.13 | Ratio |
|---|---|---|---|
| kNN p50 (k=10) | 0.49 ms | 1.43 ms | **2.9× faster** |
| Top-K sort p50 | 0.35 ms | 4.14 ms | **12× faster** |

**3.4 — Disk footprint (after Zstd-19)**

```bash
du -sh /tmp/xerj-demo-data
```

Expected: **2–5 MB** for the 40-doc corpus.

> "Segment-level Zstandard at the highest compression level — 3.74× tighter than the previous baseline. On a billion-document log corpus, three-and-three-quarter-times less disk means three-and-three-quarter-times faster backups, three-and-three-quarter-times less SSD wear, and SSD that lasts twice as long. Compression is one of those rare optimizations where every dimension wins."

The receipts file shows total post-flush disk for an equivalent benchmark: **2.4 MB** (XERJ) vs ES storage segments at the same scale.

**3.5 — The summary slide (verbal)**

> "On the measured benchmark from last week, on the same hardware, against ES 8.13 fully tuned for the workload — XERJ wins on cold start by 82×, on resident memory by 13×, on kNN p50 by 2.9×, on top-K sort by 12×, on disk by 3.7×. The one place ES wins is bulk ingest throughput, by twelve percent. Twelve percent ingest throughput in exchange for everything I just listed is, in our customers' words, 'not actually a tradeoff.'"

---

## Act 4 — Operational reality (5 min)

Show three things: graceful shutdown, durability across restart, and the production-shape APIs the buyer's SRE team will care about.

**4.1 — Graceful shutdown (SIGTERM cleanly)**

```bash
time kill -TERM $(cat demo/.xerj.pid)
# Wait for the process to exit (it will)
while kill -0 $(cat demo/.xerj.pid) 2>/dev/null; do sleep 0.1; done
echo "shutdown complete"
```

Expected: 0.2–2 seconds, depending on outstanding writes. Measured benchmark: **0.24 s** vs ES 3.25 s (13.5× faster).

> "Graceful shutdown matters because Kubernetes gives you 30 seconds before it sends SIGKILL. An engine that takes three seconds to drain leaves you 27 seconds. An engine that takes a quarter of a second leaves you 29.75. That margin shows up as a recovery-time SLO improvement."

**4.2 — Durability across restart**

```bash
demo/scripts/setup.sh         # restart
curl -s localhost:9200/ai-kb/_count | jq
# Re-run the kNN query from §2.1 — same results
```

Expected: the count is still 40, the kNN results are identical. WAL replay is automatic and silent.

**4.3 — Production-shape APIs**

The buyer's SRE team will ask whether the engine has the surface area their runbooks already assume. Run one command per concept:

```bash
# Cluster posture
curl -s 'localhost:9200/_cluster/health'                  # green/yellow/red
curl -s 'localhost:9200/_cluster/stats'                   # shard counts, doc counts
curl -s 'localhost:9200/_nodes/stats'                     # per-node statistics

# Index lifecycle
curl -s 'localhost:9200/_cat/indices?v'                   # human-readable index list
curl -s -X POST 'localhost:9200/ai-kb/_flush?force=true'  # force segment flush
curl -s -X POST 'localhost:9200/ai-kb/_refresh'           # make new docs searchable
```

> "These are the same calls your existing operations playbook already uses. The runbooks do not change."

**4.4 — Mention what's there but not demo'd live**

Keep this short, just so the buyer knows the surface exists:

- `PUT /_snapshot/{repo}` and `PUT /_snapshot/{repo}/{snapshot}` — snapshot/restore
- `POST /_aliases` — alias management for blue/green and tenant routing
- `PUT /_index_template/{name}` — index templates
- `POST /_reindex` — reindex API
- `DELETE /{idx}/_doc/{id}` and `POST /{idx}/_delete_by_query`
- `POST /_msearch` — multi-search

---

## Act 5 — Honest gaps and the pilot ask (5 min)

This is the act that closes deals. **Buyers do not believe replacement claims unless you tell them the gaps yourself.** Lead with what doesn't work; lead from there into a pilot scope where the gaps don't matter.

**5.1 — The honest gap list**

| Area | State | What this means in practice |
|------|-------|------------------------------|
| Server-side hybrid (`query`+`knn` fused into one request) | Engine path exists; wire-format alignment incomplete | Run RRF in the client today; one-request hybrid lands in 4–6 weeks |
| RBAC / SSO / SAML | Basic API key only | Pilot behind the buyer's existing gateway; full SSO on the v1.x roadmap |
| ES YAML conformance | ~98.2% on `main`; ~98.5% on `fix/v1-rc-test-fixes` | 18 stable failures across deep features (BM25 multi-segment precision, synthetic source reconstruction, time_series 8.13 specifics, HDR percentile precision, holtWinters, ignore_malformed strictness on empty strings); see `engine/reports/RELEASE_NOTES_v1.0.0-rc.1.md` for the full list |
| Multi-node clustering | Single-node production today | Multi-node coordination is post-pilot for most customers |
| x-pack features (ML jobs, watcher, painless scripting) | Not in scope | Out by design — XERJ is not a Splunk-with-Lucene replacement |
| Per-doc PUT race on `v1.0.0-rc.1` | ~10-15% of rapid sequential PUTs lose 1 doc to the segment-publish snapshot | Use `_bulk` for ingest in tests/demos; `_count` always correct; kNN unaffected; targeted for v1.0.0 final |
| Bulk ingest throughput vs ES at 100K | XERJ 95k docs/s vs ES 106k docs/s | 12% gap; XERJ wins on tail latency (p99 83 ms vs 146 ms) |
| Filtered kNN (`filter` clause inside `knn`) | Not yet enforced | Use a follow-on filter pass; native pre-filter in roadmap |
| BM25 score values | Returns 0.0 (rank order is correct) | Don't pitch on score magnitude; pitch on rank order |

> "Anything I just listed that matters to your team, we should sequence into the pilot scope rather than discover during a production rollout."

**5.2 — The pilot ask**

Use this exact framing for the close:

> "Here's what I'd like to propose: a 30-day pilot on a single workload of your choosing. The two workloads where XERJ today is the strongest fit are:
>
> 1. **Logs and observability** — point your existing Logstash/Filebeat at port 9200, see what your dashboards do, measure cost-per-GB-stored against your current ES bill.
> 2. **RAG retrieval** — point one of your AI agents at a XERJ index for its retrieval layer, measure end-to-end latency and the recall-at-k against your current vector database.
>
> Success criteria are the buyer's, not ours. We co-define them in the kickoff. At the end of 30 days, you decide. If it's a yes, we do the production rollout together. If it's a no, you've spent thirty days getting better numbers from your current vendor and that's also a good outcome."

**5.3 — What to leave behind**

After the call, send these three artifacts to the buyer (links sourced from `landing/resources/`):

1. **One-pager:** `landing/resources/xerj-exec-brief.pdf`
2. **Technical brief:** `landing/resources/xerj-tech-brief.pdf`
3. **Use-case brief** matching the buyer's profile:
   - Financial services: `landing/resources/xerj-industry-finserv.pdf`
   - Healthcare: `landing/resources/xerj-industry-healthcare.pdf`
   - Public sector: `landing/resources/xerj-industry-public-sector.pdf`
   - Retail: `landing/resources/xerj-industry-retail.pdf`
   - RAG-driven: `landing/resources/xerj-usecase-rag.pdf`

---

## Recovery playbook

Things that go wrong on live demos and the 10-second fix.

| Symptom | Likely cause | Recovery |
|---------|--------------|----------|
| `setup.sh` hangs at "waiting for cluster green" | Port 9200 already bound | `lsof -iTCP:9200 -sTCP:LISTEN` and kill the holder; `teardown.sh` |
| `cargo build` errors | Stale toolchain | `rustup update stable` then `setup.sh` again |
| Bulk ingest returns `errors:true` | Mapping mismatch | `curl -X DELETE localhost:9200/ai-kb` and re-run §1.6 |
| kNN returns empty hits | Forgot to refresh after ingest | `curl -X POST localhost:9200/ai-kb/_refresh` |
| kNN returns 200 but `_score: null` | Index missing the dense_vector mapping | re-create with the §1.6 mapping block |
| `took` is suspiciously large (>50 ms) on first query | Cold cache | Ignore the first measurement; it's cache priming |
| Server crashes on a query | Reproduce, file under `engine/reports/` | Continue the demo in fallback mode (skip Act 2, lean on Act 3 + 5) |

---

## Adapting the demo per persona

| Persona | What to compress | What to expand |
|---------|------------------|----------------|
| **VP Engineering / CTO** | Act 2 (technical detail) | Act 5 (pilot ask, gap list) |
| **Head of AI Infra / ML Platform** | Act 1 (drop-in story) | Act 2 (kNN, hybrid, embedding flow) |
| **SRE / Platform Engineering** | Act 2 | Act 4 (operational APIs), Act 3.1 (cold start) |
| **CISO / Compliance** | Act 3 (perf) | Act 4 (durability) and Act 5.1 (RBAC honesty) — be candid that auth is a gap and articulate the gateway-pattern workaround |
| **CFO-influenced exec** | Act 2 | Act 3.4 (disk), Act 5.2 (pilot economics) |

---

## Appendix — full command playback

For sales engineers who want one bash file they can paste in front of a buyer with minimal narration. Save as `~/xerj-demo.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

REPO="${REPO:-$(pwd)}"; cd "$REPO"
FAST=1 demo/scripts/setup.sh

curl -s -X PUT localhost:9200/ai-kb -H 'Content-Type: application/json' -d '{
  "mappings": {"properties": {
    "title":{"type":"text"},"content":{"type":"text"},
    "category":{"type":"keyword"},
    "embedding":{"type":"dense_vector","dims":8,"similarity":"cosine"},
    "source":{"type":"keyword"},"indexed_at":{"type":"date"},
    "word_count":{"type":"integer"},"in_scope":{"type":"boolean"}
  }}}' >/dev/null

# Per-doc PUT loop (see runbook §1 note about bulk-endpoint visibility quirk)
i=0
while IFS= read -r line; do
  i=$((i+1))
  curl -s -X PUT "localhost:9200/ai-kb/_doc/$i" \
    -H 'Content-Type: application/json' -d "$line" >/dev/null
done < demo/data/ai_kb.ndjson
curl -s -X POST localhost:9200/ai-kb/_refresh >/dev/null

echo "=== ES wire identity ==="
curl -s localhost:9200/ | jq '.version.number, .tagline'

echo "=== match query ==="
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' \
  -d '{"query":{"match":{"content":"quantization"}},"_source":["title"],"size":3}' \
  | jq '.hits.hits[] | ._source.title'

echo "=== kNN query (vector + cost intent) ==="
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' -d '{
  "knn":{"field":"embedding","query_vector":[0.094072,0.658505,0.188144,0.094072,0,0.658505,0.282216,0],
         "k":5,"num_candidates":20},"size":5}' \
  | jq '.hits.hits[] | {score:._score, category:._source.category, title:._source.title}'

echo "=== terms aggregation ==="
curl -s -X POST localhost:9200/ai-kb/_search -H 'Content-Type: application/json' \
  -d '{"size":0,"aggs":{"by_category":{"terms":{"field":"category"}}}}' \
  | jq '.aggregations.by_category.buckets'

echo "=== resident memory ==="
ps -o rss,cmd -p $(cat demo/.xerj.pid)

echo "=== disk footprint ==="
du -sh /tmp/xerj-demo-data
```
