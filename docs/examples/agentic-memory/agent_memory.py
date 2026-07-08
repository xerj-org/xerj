#!/usr/bin/env python3
"""Long-term memory for an AI agent, backed by XERJ's /_memory API.

Simulates a coding-assistant agent that, across a session:
  1. STORES observations/preferences it learns about the project
     (each as text + a small embedding vector + metadata),
  2. later RECALLS the most relevant memories to answer a new question
     -- by vector similarity AND by plain text,
  3. does a metadata-filtered recall (only "preference" memories),
  4. FORGETS a memory that is no longer true,
  5. proves multi-namespace isolation: a second agent cannot see the
     first agent's memories.

No LLM and no pip installs -- stdlib only. The "embedder" here is a tiny
deterministic hashed-character-trigram vectorizer so the demo is fully
self-contained and cosine similarity is meaningful (it is lexical, not
neural -- see the note by embed()). In production you bring your own vectors
(OpenAI/Cohere/your model) -- XERJ stores and kNN-searches them the same way,
which is exactly what lets XERJ replace a bolt-on vector DB for agent memory.
"""

import os
import json
import math
import re
import urllib.request
from urllib.error import HTTPError

# Server URL: read XERJ_URL (or the legacy BASE alias); default to the stock
# XERJ port 9200. No un-overridable hardcoded port -- point this at any node.
BASE = os.environ.get("XERJ_URL") or os.environ.get("BASE") or "http://localhost:9200"
DIM = 96  # embedding dimensionality for the toy hashing embedder


# --------------------------------------------------------------------------- #
# Tiny self-contained "embedder": deterministic hashed character trigrams.
# Character n-grams give the demo enough lexical-semantic overlap that related
# wording (service/services, prefer/prefers) lands nearby -- WITHOUT any model
# download. It is NOT a neural embedder: real synonymy ("car" ~ "automobile")
# needs real embeddings. In production you pass vectors from your own model
# (OpenAI/Cohere/local); XERJ stores and kNN-searches them exactly the same,
# which is what lets it stand in for a bolt-on vector DB.
# --------------------------------------------------------------------------- #
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


# --------------------------------------------------------------------------- #
# Thin HTTP helpers over XERJ's REST surface.
# --------------------------------------------------------------------------- #
def _req(method: str, path: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        BASE + path, data=data, method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req) as r:
            return json.loads(r.read() or b"{}")
    except HTTPError as e:
        return json.loads(e.read() or b"{}")


def remember(ns: str, text: str, metadata: dict, mem_id: str) -> dict:
    """Store one memory: text is BM25-indexed, vector enables kNN recall."""
    return _req("POST", f"/_memory/{ns}", {
        "text": text, "vector": embed(text), "metadata": metadata, "id": mem_id,
    })


def recall_vec(ns: str, query: str, k: int = 3, filt: dict | None = None) -> list:
    """Semantic recall: embed the query, kNN over stored vectors."""
    body = {"vector": embed(query), "k": k}
    if filt:
        body["filter"] = filt
    return _req("POST", f"/_memory/{ns}/_recall", body)["hits"]


def recall_text(ns: str, query: str, k: int = 3) -> list:
    """Lexical recall: BM25 over the stored memory text."""
    return _req("POST", f"/_memory/{ns}/_recall", {"query": query, "k": k})["hits"]


def forget(ns: str, mem_id: str) -> dict:
    return _req("DELETE", f"/_memory/{ns}/{mem_id}")


def drop(ns: str) -> dict:
    return _req("DELETE", f"/_memory/{ns}")


def show(label: str, hits: list) -> None:
    print(f"\n{label}")
    for h in hits:
        kind = (h.get("metadata") or {}).get("kind", "-")
        print(f"  [{h['score']:.3f}] ({kind:10}) {h['text']}")


# --------------------------------------------------------------------------- #
# Quantified recall quality. The point of agent memory is to recall the RIGHT
# memory for a new question, so we MEASURE that instead of eyeballing it: a set
# of probe questions, each labeled with the one memory that correctly answers
# it, scored by recall@1 (was the right memory ranked first?) and recall@3
# (was it in the top 3?). We report the number both recall paths actually hit.
# --------------------------------------------------------------------------- #
PROBES = [
    ("What language should we choose for a new backend service?", "ada-1"),
    ("Which region is production deployed in?",                   "ada-2"),
    ("Are we using tabs or spaces, and what line width?",         "ada-3"),
    ("Which service owns the Postgres ledger database?",          "ada-4"),
    ("What has to pass in CI before a deploy?",                   "ada-5"),
    ("What do we use for tracing?",                               "ada-6"),
]


def evaluate_recall(ns: str, recall_fn) -> tuple:
    """Return (recall@1, recall@3) as fractions in [0,1] over PROBES."""
    at1 = at3 = 0
    for question, want in PROBES:
        ids = [h["id"] for h in recall_fn(ns, question, k=3)]
        if ids[:1] == [want]:
            at1 += 1
        if want in ids[:3]:
            at3 += 1
    n = len(PROBES)
    return at1 / n, at3 / n


# --------------------------------------------------------------------------- #
# The scenario.
# --------------------------------------------------------------------------- #
def main() -> None:
    ADA = "agent-ada"        # our coding assistant
    BILLING = "agent-billing"  # an unrelated agent, to prove isolation

    # Clean slate for a repeatable demo.
    drop(ADA)
    drop(BILLING)

    # --- Turn 1..N: the agent learns things and writes them to memory. ------ #
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

    # A different agent stores its own (finance) memory in its own namespace.
    remember(BILLING, "Invoices over $10k require CFO approval.",
             {"kind": "policy"}, "bill-1")

    # --- A later turn: answer a question by recalling relevant memory. ------ #
    q1 = "What programming language should I use for a new backend service?"
    show(f'== Semantic recall for: "{q1}"', recall_vec(ADA, q1, k=3))

    q2 = "Which region is production deployed in?"
    show(f'== Lexical (BM25) recall for: "{q2}"', recall_text(ADA, q2, k=3))

    # --- Metadata-filtered recall: only surface team *preferences*. --------- #
    q3 = "How should I set up the new service?"
    show(f'== Preference-only recall for: "{q3}"',
         recall_vec(ADA, q3, k=5, filt={"term": {"metadata.kind": "preference"}}))

    # --- Measured recall quality over a labeled probe set. ------------------ #
    # All 6 memories are still present here (before the forget below).
    v1, v3 = evaluate_recall(ADA, recall_vec)   # semantic (kNN over the vectors)
    b1, b3 = evaluate_recall(ADA, recall_text)  # lexical  (BM25 over the text)
    n = len(PROBES)
    print(f"\n== Recall quality over {n} labeled probe questions ==")
    print(f"  semantic (kNN):  recall@1 = {v1*100:5.1f}%  ({round(v1*n)}/{n})"
          f"   recall@3 = {v3*100:5.1f}%  ({round(v3*n)}/{n})")
    print(f"  lexical  (BM25): recall@1 = {b1*100:5.1f}%  ({round(b1*n)}/{n})"
          f"   recall@3 = {b3*100:5.1f}%  ({round(b3*n)}/{n})")

    # --- Isolation: agent-billing cannot see agent-ada's memories. ---------- #
    print("\n== Namespace isolation ==")
    leak = recall_vec(BILLING, q1, k=3)
    billing_texts = [h["text"] for h in leak]
    print(f"  agent-billing recall for a coding question -> {billing_texts}")
    assert all("Rust" not in t and "AWS" not in t for t in billing_texts), \
        "ISOLATION VIOLATION: agent-billing saw agent-ada's memories"
    print("  OK: agent-billing only ever sees its own namespace.")

    # --- Forgetting: the team switched tracing tools; retract that memory. -- #
    print("\n== Forgetting a memory that is no longer true ==")
    before = recall_vec(ADA, "what do we use for tracing", k=3)
    show("  before forget:", before)
    print("  forget ->", forget(ADA, "ada-6"))
    after = recall_vec(ADA, "what do we use for tracing", k=3)
    show("  after forget:", after)
    assert all(h["id"] != "ada-6" for h in after), "ada-6 was not forgotten"
    print("  OK: the retracted memory is gone.")

    # --- Correctness checks so a green run really means it worked. ---------- #
    top_lang = recall_vec(ADA, q1, k=1)[0]
    assert "Rust" in top_lang["text"], f"expected Rust preference on top, got {top_lang}"
    top_region = recall_text(ADA, q2, k=1)[0]
    assert "us-east-1" in top_region["text"], f"expected region fact on top, got {top_region}"
    prefs = recall_vec(ADA, q3, k=5, filt={"term": {"metadata.kind": "preference"}})
    assert prefs and all((h["metadata"] or {}).get("kind") == "preference" for h in prefs), \
        "preference filter leaked non-preferences"
    print("\nAll assertions passed.")


if __name__ == "__main__":
    main()
