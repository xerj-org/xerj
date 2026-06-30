#!/usr/bin/env python3
"""Generate realistic-shaped demo corpora for the Xerj Console dashboards.

Writes one NDJSON file per intended index under demo/data/extras/:

  chat-events.ndjson   — LLM query telemetry (drives ai-overview / rag-quality)
  vector-ops.ndjson    — vector index events (drives vector-index)
  agent-memory.ndjson  — agent memory operations (drives agent-memory)
  anomalies.ndjson     — anomaly detector findings (drives anomaly-detect)
  ingest-events.ndjson — ingest pipeline events (drives ingest-pipeline)

Timestamps span the last 24h so the dashboards' default 24H range
captures everything. Distributions are seeded for reproducibility.
"""
import json, random, time, math, sys, os
random.seed(42)

OUT_DIR = os.path.join(os.path.dirname(__file__))
NOW = int(time.time())
SPAN = 24 * 3600

def iso(ts):
    return time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(ts))

def diurnal_ts(i, n):
    """Return an epoch ts for event i of n, with peak around hour 13."""
    frac = i / max(1, n - 1)
    base = NOW - SPAN + int(frac * SPAN)
    # add a small jitter
    return base + random.randint(-30, 30)

# ── chat-events ──────────────────────────────────────────────────────
MODELS = [
    ('claude-opus-4-7',    0.22, 1450),
    ('claude-sonnet-4-6',  0.35, 820),
    ('claude-haiku-4-5',   0.28, 310),
    ('gpt-5',              0.09, 990),
    ('gemini-3',           0.04, 1120),
    ('llama-4',            0.02, 540),
]
INTENTS = [
    ('semantic-search', 0.22),
    ('code-assist',     0.18),
    ('doc-qa',          0.15),
    ('summarize',       0.10),
    ('translate',       0.08),
    ('classify',        0.07),
    ('extract-json',    0.06),
    ('agent-tool',      0.05),
    ('rerank',          0.04),
    ('rewrite',         0.03),
    ('chat-freeform',   0.01),
    ('safety-check',    0.01),
]
DOCS = [
    'runbook/oncall.md','rfc/042-retention.md','arch/cluster-design.md',
    'rfc/039-hybrid-search.md','runbook/incident-1411.md','policy/pii.md',
    'arch/hnsw-internals.md','rfc/051-agent-memory.md','docs/query-dsl.md',
    'rfc/048-embed-proxy.md','runbook/billing-sync.md','docs/ingest-api.md',
]
def pick_weighted(opts):
    r = random.random()
    acc = 0
    for label, w in opts:
        acc += w
        if r <= acc:
            return label
    return opts[-1][0]

CHAT_N = 8000
with open(os.path.join(OUT_DIR, 'chat-events.ndjson'), 'w') as f:
    for i in range(CHAT_N):
        ts = diurnal_ts(i, CHAT_N)
        # diurnal weighting: queries spike at noon UTC
        hour = time.gmtime(ts).tm_hour
        weight = 0.5 + 0.5 * math.cos((hour - 13) / 24 * 2 * math.pi)
        if random.random() > weight: continue  # drop to make it diurnal
        model = pick_weighted([(m, w) for m, w, _ in MODELS])
        latency_base = next((l for m, _, l in MODELS if m == model), 800)
        intent = pick_weighted(INTENTS)
        prompt_tok = int(random.gauss(1800, 600))
        ctx_tok = int(random.gauss(9000, 4500))
        out_tok = int(random.gauss(320, 180))
        prompt_cost = max(0, prompt_tok) / 1e6 * 2.5
        ctx_cost = max(0, ctx_tok) / 1e6 * 0.2
        out_cost = max(0, out_tok) / 1e6 * 10
        total_cost = prompt_cost + ctx_cost + out_cost
        cache_hit = random.random() < 0.42
        rec = {
            '@timestamp': iso(ts),
            'model': model,
            'intent': intent,
            'prompt_tokens': max(0, prompt_tok),
            'context_tokens': max(0, ctx_tok),
            'completion_tokens': max(0, out_tok),
            'cost_usd': round(total_cost, 6),
            'latency_ms': int(random.gauss(latency_base, latency_base * 0.18)),
            'cache_hit': cache_hit,
            'top_doc': random.choice(DOCS),
            'tenant': random.choice(['acme','northwind','globex','stark','umbrella']),
            'status': 'ok' if random.random() > 0.012 else 'error',
        }
        f.write(json.dumps(rec) + '\n')

# ── vector-ops ───────────────────────────────────────────────────────
SHARDS = [f'shard-{i:02d}' for i in range(16)]
VEC_N = 4000
with open(os.path.join(OUT_DIR, 'vector-ops.ndjson'), 'w') as f:
    for i in range(VEC_N):
        ts = diurnal_ts(i, VEC_N)
        op = random.choices(['knn-search','upsert','delete','reindex'], [0.78, 0.18, 0.03, 0.01])[0]
        rec = {
            '@timestamp': iso(ts),
            'op': op,
            'shard': random.choice(SHARDS),
            'dim': random.choice([384, 768, 1024, 1536, 3072]),
            'k': random.choice([5,10,20,50,100]) if op == 'knn-search' else None,
            'ef_search': random.choice([24, 48, 96, 192]) if op == 'knn-search' else None,
            'recall_at_10': round(random.gauss(0.94, 0.02), 4) if op == 'knn-search' else None,
            'latency_ms': int(random.gauss(28, 12)),
            'index': random.choice(['ai-kb','agent-memory','code-embeddings','docs-rag']),
        }
        rec = {k: v for k, v in rec.items() if v is not None}
        f.write(json.dumps(rec) + '\n')

# ── agent-memory ─────────────────────────────────────────────────────
AGENT_N = 5000
NAMES = ['cluster-reset','oncall-triage','search-tuning','perf-debug','schema-migrate',
         'incident-postmortem','feature-flag','rollout-plan','customer-request','q4-roadmap']
with open(os.path.join(OUT_DIR, 'agent-memory.ndjson'), 'w') as f:
    for i in range(AGENT_N):
        ts = diurnal_ts(i, AGENT_N)
        op = random.choices(['insert','recall','update','delete','expire'], [0.40, 0.45, 0.08, 0.02, 0.05])[0]
        rec = {
            '@timestamp': iso(ts),
            'op': op,
            'agent': random.choice(['ops-agent','support-agent','triage-agent','planning-agent']),
            'memory_key': random.choice(NAMES),
            'tokens': int(random.gauss(450, 200)) if op != 'expire' else 0,
            'score': round(random.gauss(0.78, 0.14), 4) if op == 'recall' else None,
            'ttl_hours': random.choice([24, 72, 168, 720]) if op in ('insert','update') else None,
        }
        rec = {k: v for k, v in rec.items() if v is not None}
        f.write(json.dumps(rec) + '\n')

# ── anomalies ────────────────────────────────────────────────────────
ANOM_N = 600
with open(os.path.join(OUT_DIR, 'anomalies.ndjson'), 'w') as f:
    for i in range(ANOM_N):
        ts = diurnal_ts(i, ANOM_N)
        kind = random.choice(['latency-spike','error-burst','traffic-drop','schema-drift','cost-spike'])
        sev = random.choices(['critical','warning','info'], [0.12, 0.40, 0.48])[0]
        rec = {
            '@timestamp': iso(ts),
            'kind': kind,
            'severity': sev,
            'service': random.choice(['ingest-pipeline','search-api','vector-index','llm-proxy','sshd']),
            'score': round(random.gauss(0.72, 0.15), 3),
            'duration_s': int(random.gauss(180, 90)),
        }
        f.write(json.dumps(rec) + '\n')

# ── ingest-events ────────────────────────────────────────────────────
INGEST_N = 12000
with open(os.path.join(OUT_DIR, 'ingest-events.ndjson'), 'w') as f:
    for i in range(INGEST_N):
        ts = diurnal_ts(i, INGEST_N)
        stage = random.choices(['parse','enrich','index','dlq'], [0.25, 0.25, 0.46, 0.04])[0]
        ok = stage != 'dlq' and random.random() > 0.008
        rec = {
            '@timestamp': iso(ts),
            'stage': stage,
            'pipeline': random.choice(['logs-pipeline','metrics-pipeline','traces-pipeline','events-pipeline']),
            'docs': int(random.gauss(420, 110)),
            'duration_ms': int(random.gauss(38, 14)),
            'status': 'ok' if ok else 'failed',
            'reason': None if ok else random.choice(['parse_error','schema_mismatch','timeout','rate_limited']),
        }
        rec = {k: v for k, v in rec.items() if v is not None}
        f.write(json.dumps(rec) + '\n')

print('chat-events:',  sum(1 for _ in open(os.path.join(OUT_DIR, 'chat-events.ndjson'))))
print('vector-ops:',   sum(1 for _ in open(os.path.join(OUT_DIR, 'vector-ops.ndjson'))))
print('agent-memory:', sum(1 for _ in open(os.path.join(OUT_DIR, 'agent-memory.ndjson'))))
print('anomalies:',    sum(1 for _ in open(os.path.join(OUT_DIR, 'anomalies.ndjson'))))
print('ingest-events:', sum(1 for _ in open(os.path.join(OUT_DIR, 'ingest-events.ndjson'))))
