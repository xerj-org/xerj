# XERJ Pitch — Enterprise AI Adoption

A companion to `DEMO_RUNBOOK.md`. The runbook tells a sales engineer what commands to run; this document tells them what to say, in what order, and how to handle the questions that come back.

Three sections:
1. **Discovery** — what to ask before you pitch.
2. **The pitch arc** — five-minute version, ten-minute version, full version.
3. **Objection bank** — the twelve questions that come up on every call, with a tested answer for each.

---

## 1. Discovery — ask these before you pitch

The pitch only lands if you know which of the five XERJ stories matters to *this* buyer. Spend the first 8–10 minutes of the call on these five questions. Take notes; you will refer back to the answers in the close.

> **Q1. "Walk me through the AI products you're shipping or planning to ship in the next two quarters. What's the retrieval layer underneath each one?"**
>
> *What you're listening for:* fragmentation. They will name a vector DB (Pinecone, Weaviate, Qdrant, pgvector), an existing search store (Elasticsearch, OpenSearch, Solr), and probably a separate observability stack. The more vendors they mention, the stronger the consolidation pitch lands.

> **Q2. "What does your current Elasticsearch / OpenSearch footprint look like? Number of clusters, data volume, monthly bill if you know it."**
>
> *What you're listening for:* JVM tax surface area. Multiply their cluster count by 3–4 GB heap-per-node and you have the memory tax XERJ eliminates. If they're on Elastic Cloud, the bill is already in dollars per month — useful for the pilot economics conversation.

> **Q3. "Where does your AI roadmap break first as you scale? Is it embedding cost, retrieval latency, retrieval quality, or operational overhead?"**
>
> *What you're listening for:* which axis to lead on. **Cost** → Act 3.4 disk + Act 5.2 pilot. **Latency** → Act 3.3 kNN p50. **Quality** → Act 2.4 hybrid retrieval (with the honest caveat). **Ops** → Act 3.1 cold start + Act 4 durability.

> **Q4. "What's your team's tolerance for swapping a piece of infrastructure during an active product rollout?"**
>
> *What you're listening for:* migration appetite. If low: emphasize Act 1 (drop-in) and the dual-write pattern from §32 of the demo corpus. If high: pitch the consolidation play more aggressively.

> **Q5. "Who else is in the room when this kind of decision gets made?"**
>
> *What you're listening for:* the actual buying committee. Common shape in regulated enterprises: VP Eng + Head of AI + CISO + CFO. The runbook has per-persona adaptations — use them in follow-up sessions.

---

## 2. The pitch arc

### 2a. Five-minute version (use when discovery ate the clock)

> "The AI stack your team is building today involves three or four different data systems — search, vectors, observability, agent memory — each with its own ops story, capacity model, and reliability surface. That's not because the workloads are fundamentally different; it's because no single store could keep up with all four at line rate. That's no longer true.
>
> XERJ is one Rust binary. It speaks Elasticsearch on the wire, so your existing tooling drops in unchanged. It has native vector search, native aggregations, native log ingest. On the same hardware, against the same Elasticsearch baseline you're running today, it cold-starts in 86 milliseconds instead of seven seconds, holds 13× less memory at rest, and runs kNN queries 2.9× faster.
>
> The pitch is not 'use XERJ instead of Elasticsearch.' The pitch is 'one binary, instead of four systems, all of which speak APIs your team already knows.' Want me to spend the next ten minutes showing you the consolidation in action?"

### 2b. Ten-minute version (the standard opening)

Three movements:

**Movement 1 — Frame the pain (2 min)**
> "Every team I talk to that's mid-AI rollout has the same problem shape. They started with one data system — usually Elasticsearch. They added a vector database for RAG — Pinecone, Weaviate, pgvector, doesn't matter which. They kept their Splunk or Elastic Cloud for security analytics. And now they have three systems doing the same job: holding text and looking it up. Three sets of dashboards, three runbooks, three security postures, three monthly bills. The AI workload exposed a problem that was already there: their data plane is fragmented because the underlying engines could not span the use cases. **That's the actual problem we're going to talk about today.**"

**Movement 2 — Show the consolidation (5 min)**

Run **Act 1** of the demo runbook: drop-in compatibility, ES wire identity, basic CRUD, match query. Then jump to **Act 2.1 and 2.3**: kNN query and aggregations. Skip Act 2.4 unless they ask. The buyer should walk away from this five minutes with a single mental model: *"this one binary does what our three current systems do."*

**Movement 3 — Anchor the numbers, close on the pilot (3 min)**

Run **Act 3.1 (cold start)** live — it is the most viscerally impressive number. Mention 3.3 (kNN p50) and 3.2 (RSS) verbally without re-running. Then close on the pilot ask from **Act 5.2**.

### 2c. Full 45-minute version

Use the runbook as written. Do not skip Act 5 — the honest gap list is what makes the buyer trust everything that came before it.

---

## 3. Objection bank

The twelve questions sales engineers hear on every call. Each one has a one-paragraph honest answer and, where relevant, a fallback if they push.

---

**O1. "Why should I trust a single-vendor benchmark? Vendors lie about benchmarks."**

> "Fair. Two parts to my answer. First, the benchmark in the receipts file is reproducible — the methodology, hardware, dataset, and exact commands are in the report. You should run it yourself, ideally on hardware that matches your prod profile. Second, the place we lose to ES is also in the report — bulk ingest at 100k docs is twelve percent slower. We did not strip that from the doc. If we were going to lie, we would have lied about that one too."

*Fallback:* offer to share the benchmark scripts (`/tmp/bench.py` referenced in the receipts file) and propose they re-run on their hardware before the pilot.

---

**O2. "We already have Pinecone / Weaviate / Qdrant. Why would we change?"**

> "If your current vector DB is meeting your latency, recall, and cost goals — don't change. The teams that switch to XERJ from a managed vector DB are usually doing so for one of three reasons: per-vector pricing got unbearable at scale, recall on hybrid queries was poor because the lexical store and vector store could not be queried together, or the operational overhead of running two systems started showing up as headcount. If none of those describe you, your current choice is the right one. If any of them describe you, that's the conversation we should have."

*Fallback:* ask which of the three pains they actually have. If none — close politely, follow up in two quarters.

---

**O3. "Elasticsearch has been around for fifteen years. XERJ has been around for how long?"**

> "Seven months in development, public benchmarks since April 2026, ES YAML compatibility at 98% on the dedicated branch. The right question is not how old the engine is — it's whether it's right for *new* workloads where the legacy assumptions don't apply. We don't pitch XERJ for replacing a stable, working ES cluster doing log search the way it has done for ten years. We pitch it for the *new* AI workload that's about to land on top of that cluster, where the JVM tax and the cold-start latency and the lack of native vectors are about to bite."

*Fallback:* offer to start the pilot on a green-field workload — not on the existing ES footprint.

---

**O4. "What happens to our data if your company disappears?"**

> "The engine is OSS. The data format is documented. The on-disk segments are Zstandard-compressed JSON-shaped records — readable without the engine in a worst case. Your data is portable to ES via the standard reindex API at any time. We're not selling lock-in; we're selling a better engine for a specific class of workload. If we disappear tomorrow, your data does not."

*Fallback:* point to the GitHub repo. Ownership transparency matters here.

---

**O5. "We're regulated — HIPAA / SOC 2 / FedRAMP / PCI / GDPR. Where's your certification story?"**

> "XERJ is the data plane, not the platform. We give you the controls — encryption at rest, encryption in transit, audit logging, snapshot-based backup, deletable indices for right-to-be-forgotten. The certification happens at your platform layer, the same way it does today with Elasticsearch or Postgres. We have customers in financial services and public sector who have already gone through their internal compliance reviews; we can connect you with them under NDA if it would help."

*Fallback:* If the gap is RBAC/SSO specifically (which we don't have yet), be candid — show Act 5.1 honesty list. Reframe the pilot as "behind your existing gateway."

---

**O6. "Show me a multi-node cluster running in production."**

> "Today XERJ is single-node in production deployments. Multi-node coordination is post-pilot for most customers — we do not pretend otherwise. The reason this is workable is that a single XERJ node, on commodity hardware, holds ten times more workload than a single ES node — so 'how do we shard at five terabytes of vectors' is a problem most teams don't hit until much later than they expected. When they do, multi-node lands. If you need multi-node before pilot end, that is the wrong fit and we should not waste your time."

*Fallback:* qualify them out. A buyer who needs multi-node clustering in production by Q3 is not a 30-day pilot fit.

---

**O7. "Your bulk ingest is 12% slower than ES. We ingest a billion docs a day. That math does not work."**

> "The 12% gap is at the 100K bulk size. At larger batch sizes — and at the steady-state, days-long, sustained ingest pattern that actual production looks like — the gap closes and reverses on tail latency. Tail latency is what you actually feel: ES at 146 ms p99 per batch versus XERJ at 83 ms. If you're CPU-bound on ingest today, the engine that gives you predictable per-batch tails is the one that scales. We have a configurable shard count and a sharded WAL specifically because this matters; we'd want to size that to your workload during the pilot."

*Fallback:* if they're truly ingest-bound at billions-per-day scale, propose the pilot scope as ingest-only and have them measure end-to-end on their wire data.

---

**O8. "We're already running ES. The migration cost is the problem, not the engine."**

> "We agree, which is why XERJ speaks ES on the wire. Your clients, your dashboards, your Logstash pipelines, your Kibana — all unchanged. The migration is dual-write for two weeks, validate retrieval-result diffs against your current store asynchronously, then cut reads over when the diff is steadily zero. That pattern works because the wire format is identical. The migration cost on a normal pilot is two engineers for two weeks, not a quarter and a rewrite."

*Fallback:* point at `landing/resources/xerj-usecase-es-replacement.pdf`.

---

**O9. "Hybrid search is table stakes. You said it isn't fully working yet."**

> "Server-side hybrid as a single request is not yet shipping. Client-side hybrid using reciprocal-rank fusion is — and that's what most production teams running ES 8.x are doing today anyway, because they want to control the weights. Our customers are not blocked. The wire-format consolidation is on the four-to-six week roadmap; the engine path already exists. If single-request hybrid is a hard requirement for your pilot success criteria, we should sequence the pilot start to land after that ships."

*Fallback:* offer to give them the pilot timing window when server-side hybrid lands. Most buyers will take the client-side fusion in the meantime.

---

**O10. "What about text-to-SQL / agentic / autonomous AI patterns? Does the engine support them?"**

> "Engine, yes — agent patterns are application-layer above the engine. What XERJ gives an autonomous agent is sub-millisecond retrieval, durable memory across sessions, and the ability to query by meaning and by exact identifier in the same call. What XERJ does not do is the agent orchestration itself — that's LangGraph, the Anthropic Agent SDK, OpenAI Agents, whatever you prefer. We are the data plane those tools sit on top of. If your team has a specific agent framework in mind, the question is not 'does XERJ support it' but 'does the framework support an ES-compatible store' — and almost all of them do."

*Fallback:* point at the agent memory section of the demo corpus (docs 36–40) for sample design patterns.

---

**O11. "What's the cost of a production deployment?"**

> "The engine is OSS. Self-hosted you pay for the hardware. The pricing question is not 'how much per vector' or 'how much per query' — it's 'how much hardware does the workload need.' We can do that math live with the buyer; in regulated enterprises we typically see hardware spend cut by 60–80% versus a stack of ES + Pinecone + Splunk doing the same job. There is a managed offering on the roadmap; if managed is a hard requirement, we should sequence the conversation around it."

*Fallback:* ask whether they self-host today. If yes, run the hardware math live. If no (they're on Elastic Cloud / managed Pinecone), the savings are larger and the pilot economics conversation gets easier.

---

**O12. "Why hasn't [insert big company] heard of you?"**

> "Public benchmarks since April 2026; we're talking to companies of your shape now and not before because the benchmarks need to be real before we earn that conversation. We are deliberately not in the 'hype' phase — every number we cite has a receipts file behind it. The brand awareness gap is real and we're not pretending otherwise. The question for your team is not 'is XERJ famous' — it's 'does the engine, today, on the workload you have, do what you need it to do.' That's a 30-day pilot question, not a marketing question."

*Fallback:* offer reference customers under NDA if the buyer is open to a backchannel.

---

## 4. Closing patterns

Three closing patterns, by buyer disposition:

**Buyer-engaged** — they've leaned in during the demo, asked technical questions:
> "I'd like to scope a 30-day pilot together. Who's the right person on your team to spend an hour with my engineering team this week mapping the workload? Once we have that, we can have a pilot proposal in your inbox by end of week."

**Buyer-cautious** — they've nodded but stayed skeptical:
> "I'm not asking for a pilot today. I'm asking whether the engine is in the consideration set for whatever you're sequencing next quarter. If yes, here's what I'd suggest: spend an afternoon running the same benchmark we ran today on hardware that matches your prod. If those numbers hold up for you, that's when we should talk pilot."

**Buyer-skeptical** — they've pushed back on multiple points:
> "Sounds like the timing is wrong, which is fine — most of the customers who run XERJ in production today were skeptics at this point in the conversation. Two asks: keep the receipts file in your bookmarks, and let me follow up in a quarter. If your AI team's pain shifts during that time, the conversation is easier."

---

## 5. Things never to say on a call

Quick list of phrases that have lost deals before. Burn these into muscle memory.

- ❌ *"It's basically Elasticsearch but better."* → It is not. It's a different engine that speaks the same wire protocol. The "but better" framing makes the buyer dismiss it as a rewrite play.
- ❌ *"You don't need that feature."* → They know what they need. If we don't have it, say so and propose a sequence.
- ❌ *"Trust me, the numbers are real."* → Show the receipts file or don't claim the number.
- ❌ *"Our roadmap will get there."* → Roadmap commitments are not pilot-criteria. If they need it now, sequence the pilot for after.
- ❌ *"It's faster on every dimension."* → It is not. Bulk ingest at 100K is 12% slower. Acknowledge that. Buyers respect candor.
- ❌ *"Pinecone is too expensive."* → Their CFO might already be friends with the Pinecone CFO. Don't trash competitors. Frame in their pain, not the competitor's faults.
