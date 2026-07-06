# Give an AI agent long-term memory with XERJ

**Use case:** your agent learns things during a session — user preferences, project
facts, decisions — and needs to *remember* them on a later turn to answer well.
The usual answer is to bolt a separate vector database next to your search stack.
XERJ folds that job into the engine you already run: a namespaced, offline
**agent-memory API** (`/_memory/{ns}`) that stores text + an embedding + metadata
and recalls the most relevant memories by **vector similarity** or **plain text**,
with metadata filtering, forgetting, and hard per-agent isolation.

This recipe builds a tiny simulated coding-assistant agent (no LLM required) that:

1. **stores** what it learns as memories (`POST /_memory/{ns}`),
2. **recalls** the right memory to answer a new question — semantically (kNN) and
   lexically (BM25),
3. does a **metadata-filtered** recall ("only surface team *preferences*"),
4. **forgets** a memory that is no longer true, and
5. proves **multi-namespace isolation**: a second agent can never see the first
   agent's memories.

Everything below was run end-to-end against a live XERJ. No pip installs — the
example is Python 3 stdlib only.

---

## Why this replaces a bolt-on vector DB

A memory namespace is just a reserved XERJ index (`.xerj-memory-{ns}`) under the
hood, so recall reuses the same `dense_vector`/HNSW kNN, BM25, and metadata-filter
paths that serve the rest of the engine. That means:

- **One system to run and back up.** No second datastore, no sync job keeping a
  vector index consistent with your source of truth.
- **kNN *and* BM25 in the same store.** Recall by embedding similarity, by keyword,
  or filter both by metadata — no glue code.
- **Namespaces are physical isolation.** Each agent (or tenant, or user) gets its
  own backing index; a recall in namespace `A` literally cannot read namespace `B`.
- **Offline and zero-config.** You supply the vectors (from your own model); XERJ
  never phones out to embed.

---

## The memory API, in four calls

| Verb + path | What it does | Body / result |
|---|---|---|
| `POST /_memory/{ns}` | store a memory | `{text, vector?, metadata?, id?}` → `{id, namespace, created}` |
| `POST /_memory/{ns}/_recall` | recall top-k | `{vector? \| query?, k?, filter?}` → `{hits:[{id,text,metadata,score}]}` |
| `GET /_memory/{ns}` | list recent (bounded) | → `{count, entries:[…]}` |
| `DELETE /_memory/{ns}/{id}` | forget one | → `{id, forgotten}` |
| `DELETE /_memory/{ns}` | drop the namespace | → `{namespace, dropped}` |

`_recall` picks the path from the body: a `vector` runs kNN; otherwise `query`
runs BM25 over the text; an empty body returns recent memories (`match_all`).
`filter` is a normal ES query clause applied as a `bool` filter, so it narrows
without affecting the score. Recall of an unknown namespace returns `{"hits": []}` —
that is how isolation stays clean.

### Storing a memory (raw wire)

```bash
curl -XPOST localhost:9481/_memory/agent-ada -H 'content-type: application/json' -d '{
  "text": "The team prefers Rust over Go for new backend services.",
  "vector": [0.12, 0.0, 0.34],
  "metadata": {"kind": "preference", "topic": "language"},
  "id": "demo-1"
}'
```
```json
{"created":true,"id":"demo-1","namespace":"agent-ada"}
```

### Recalling by vector, filtered to preferences (raw wire)

```bash
curl -XPOST localhost:9481/_memory/agent-ada/_recall -H 'content-type: application/json' -d '{
  "vector": [0.12, 0.0, 0.34],
  "k": 2,
  "filter": {"term": {"metadata.kind": "preference"}}
}'
```
```json
{"hits":[{"id":"demo-1",
          "metadata":{"kind":"preference","topic":"language"},
          "score":1.0,
          "text":"The team prefers Rust over Go for new backend services."}],
 "namespace":"agent-ada"}
```

Note `metadata.kind` filters straight out of the box — nested metadata subfields
are queryable with a normal `term` clause, no explicit mapping needed.

---

## The full agent

The script below is the whole example. The one thing to understand about it: it
carries a **tiny self-contained "embedder"** (deterministic hashed character
trigrams) so the demo needs no model download and cosine similarity is meaningful.

> **Honesty check.** That toy embedder is *lexical*, not neural: it rewards shared
> character n-grams (so `service`/`services`, `prefer`/`prefers` land near each
> other), but it does not know that "car" ≈ "automobile". For real semantic recall
> you pass vectors from your own model (OpenAI/Cohere/a local model) — the store and
> recall calls are byte-for-byte the same. XERJ's own built-in `semantic_text`
> embedder is likewise lexical unless you configure an external `/v1/embeddings`
> endpoint; the memory API deliberately takes *your* vectors so quality is your
> choice.

```python
#!/usr/bin/env python3
"""Long-term memory for an AI agent, backed by XERJ's /_memory API."""

import json
import math
import re
import urllib.request
from urllib.error import HTTPError

BASE = "http://localhost:9481"
DIM = 96  # embedding dimensionality for the toy hashing embedder


# Tiny self-contained "embedder": deterministic hashed character trigrams.
# NOT neural -- real synonymy needs real embeddings. In production you pass
# vectors from your own model; XERJ stores and kNN-searches them the same way.
def embed(text: str) -> list[float]:
    s = " " + re.sub(r"[^a-z0-9]+", " ", text.lower()) + " "
    vec = [0.0] * DIM
    for i in range(len(s) - 2):
        h = 0
        for ch in s[i:i + 3]:  # hash each character trigram into a bucket
            h = (h * 131 + ord(ch)) & 0xFFFFFFFF
        vec[h % DIM] += 1.0
    norm = math.sqrt(sum(v * v for v in vec)) or 1.0
    return [round(v / norm, 6) for v in vec]


def _req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method,
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req) as r:
            return json.loads(r.read() or b"{}")
    except HTTPError as e:
        return json.loads(e.read() or b"{}")


def remember(ns, text, metadata, mem_id):
    return _req("POST", f"/_memory/{ns}",
                {"text": text, "vector": embed(text), "metadata": metadata, "id": mem_id})

def recall_vec(ns, query, k=3, filt=None):          # semantic (kNN)
    body = {"vector": embed(query), "k": k}
    if filt:
        body["filter"] = filt
    return _req("POST", f"/_memory/{ns}/_recall", body)["hits"]

def recall_text(ns, query, k=3):                    # lexical (BM25)
    return _req("POST", f"/_memory/{ns}/_recall", {"query": query, "k": k})["hits"]

def forget(ns, mem_id):
    return _req("DELETE", f"/_memory/{ns}/{mem_id}")

def drop(ns):
    return _req("DELETE", f"/_memory/{ns}")

def show(label, hits):
    print(f"\n{label}")
    for h in hits:
        kind = (h.get("metadata") or {}).get("kind", "-")
        print(f"  [{h['score']:.3f}] ({kind:10}) {h['text']}")


def main():
    ADA, BILLING = "agent-ada", "agent-billing"
    drop(ADA); drop(BILLING)  # clean slate for a repeatable demo

    print("== Storing what the agent learned this session ==")
    facts = [
        ("The team prefers Rust over Go for new backend services.",
         {"kind": "preference", "topic": "language"}),
        ("Production runs on AWS in us-east-1; staging is us-west-2.",
         {"kind": "fact", "topic": "infra"}),
        ("The team prefers tabs over spaces and a 100-column line limit.",
         {"kind": "preference", "topic": "style"}),
        ("The payments service owns the Postgres 'ledger' database.",
         {"kind": "fact", "topic": "ownership"}),
        ("Deploys are gated on the full integration suite passing in CI.",
         {"kind": "fact", "topic": "process"}),
        ("The team prefers OpenTelemetry for tracing, not vendor SDKs.",
         {"kind": "preference", "topic": "observability"}),
    ]
    for i, (text, md) in enumerate(facts, 1):
        r = remember(ADA, text, md, f"ada-{i}")
        print(f"  stored {r['id']:8} created={r['created']}")

    remember(BILLING, "Invoices over $10k require CFO approval.", {"kind": "policy"}, "bill-1")

    q1 = "What programming language should I use for a new backend service?"
    show(f'== Semantic recall for: "{q1}"', recall_vec(ADA, q1, k=3))

    q2 = "Which region is production deployed in?"
    show(f'== Lexical (BM25) recall for: "{q2}"', recall_text(ADA, q2, k=3))

    q3 = "How should I set up the new service?"
    show(f'== Preference-only recall for: "{q3}"',
         recall_vec(ADA, q3, k=5, filt={"term": {"metadata.kind": "preference"}}))

    print("\n== Namespace isolation ==")
    leak = recall_vec(BILLING, q1, k=3)
    billing_texts = [h["text"] for h in leak]
    print(f"  agent-billing recall for a coding question -> {billing_texts}")
    assert all("Rust" not in t and "AWS" not in t for t in billing_texts)
    print("  OK: agent-billing only ever sees its own namespace.")

    print("\n== Forgetting a memory that is no longer true ==")
    show("  before forget:", recall_vec(ADA, "what do we use for tracing", k=3))
    print("  forget ->", forget(ADA, "ada-6"))
    after = recall_vec(ADA, "what do we use for tracing", k=3)
    show("  after forget:", after)
    assert all(h["id"] != "ada-6" for h in after)
    print("  OK: the retracted memory is gone.")

    # Correctness checks so a green run really means it worked.
    assert "Rust" in recall_vec(ADA, q1, k=1)[0]["text"]
    assert "us-east-1" in recall_text(ADA, q2, k=1)[0]["text"]
    prefs = recall_vec(ADA, q3, k=5, filt={"term": {"metadata.kind": "preference"}})
    assert prefs and all((h["metadata"] or {}).get("kind") == "preference" for h in prefs)
    print("\nAll assertions passed.")


if __name__ == "__main__":
    main()
```

---

## Run it

Start XERJ (single node, no TLS/auth) and run the script:

```bash
# 1. boot XERJ
printf '[server]\nes_compat_port = 9481\n' > /tmp/xerj.toml
./engine/target/release/xerj --insecure --data-dir /tmp/xerj-mem --config /tmp/xerj.toml &

# 2. run the agent
python3 docs/examples/agentic-memory/agent_memory.py
```

### Real output

```text
== Storing what the agent learned this session ==
  stored ada-1    created=True
  stored ada-2    created=True
  stored ada-3    created=True
  stored ada-4    created=True
  stored ada-5    created=True
  stored ada-6    created=True

== Semantic recall for: "What programming language should I use for a new backend service?"
  [0.779] (preference) The team prefers Rust over Go for new backend services.
  [0.741] (fact      ) The payments service owns the Postgres 'ledger' database.
  [0.731] (preference) The team prefers tabs over spaces and a 100-column line limit.

== Lexical (BM25) recall for: "Which region is production deployed in?"
  [3.746] (fact      ) Production runs on AWS in us-east-1; staging is us-west-2.
  [1.010] (fact      ) Deploys are gated on the full integration suite passing in CI.

== Preference-only recall for: "How should I set up the new service?"
  [0.765] (preference) The team prefers Rust over Go for new backend services.
  [0.692] (preference) The team prefers tabs over spaces and a 100-column line limit.
  [0.666] (preference) The team prefers OpenTelemetry for tracing, not vendor SDKs.

== Namespace isolation ==
  agent-billing recall for a coding question -> ['Invoices over $10k require CFO approval.']
  OK: agent-billing only ever sees its own namespace.

== Forgetting a memory that is no longer true ==

  before forget:
  [0.755] (preference) The team prefers OpenTelemetry for tracing, not vendor SDKs.
  [0.692] (fact      ) Production runs on AWS in us-east-1; staging is us-west-2.
  [0.683] (preference) The team prefers Rust over Go for new backend services.
  forget -> {'forgotten': True, 'id': 'ada-6', 'namespace': 'agent-ada'}

  after forget:
  [0.692] (fact      ) Production runs on AWS in us-east-1; staging is us-west-2.
  [0.683] (preference) The team prefers Rust over Go for new backend services.
  [0.661] (preference) The team prefers tabs over spaces and a 100-column line limit.
  OK: the retracted memory is gone.

All assertions passed.
```

Read the run top to bottom:

- **Semantic recall** put the Rust preference on top for a question that never
  says "Rust" or "prefer" — trigram overlap on *backend / service / new* pulled it
  up over unrelated facts.
- **BM25 recall** answered the region question exactly, no vectors involved.
- **Preference-only recall** returned three memories, *all* `kind:"preference"` —
  the `metadata.kind` filter did its job.
- **Isolation** held: `agent-billing`, asked a coding question, only saw its own
  single finance memory.
- **Forgetting** removed `ada-6` (the tracing preference); the follow-up recall no
  longer surfaces it.

---

## Notes, limits, and going to production

- **Bring your own embeddings.** The memory API stores whatever vector you pass and
  kNN-searches it with cosine similarity. Swap the toy `embed()` for a call to your
  model's embeddings endpoint and everything else is unchanged. Keep the vector
  dimension consistent within a namespace — the backing index's `dense_vector`
  field is sized from the first stored vector.
- **Vector *or* text.** A memory needs at least `text` or a `vector`. Text is always
  BM25-indexed; a vector additionally enables kNN recall. Recall with `vector` uses
  kNN; without it, `query` uses BM25.
- **Recency.** The store blends similarity with a recency signal internally, so
  fresh memories surface even when marginally less similar — useful for agents whose
  latest observation should win ties.
- **Isolation is structural, not a filter.** Namespaces are separate physical
  indices, so there is no query you can write in one namespace that reads another.
  Use one namespace per agent/tenant/user.
- **`GET /_memory/{ns}`** lists up to 100 most-recent entries for debugging/audit;
  it is bounded and recent-first, not a full export.
- **Wire-compatible.** Because a namespace is a real index, you can inspect or
  operate on it with the standard ES-compatible surface if you ever need to — the
  memory API is a thin, honest adapter over the same machinery.