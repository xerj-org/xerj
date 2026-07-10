#!/usr/bin/env python3
"""Semantic + hybrid search over a real knowledge base — zero dependencies.

Indexes the 40-article AI engineering KB that ships in this repo
(demo/data/ai_kb.ndjson) into a `semantic_text` field. XERJ embeds every
document at ingest with its built-in embedder (no external services, no
API keys), then answers the same question three ways so you can see what
each retrieval mode is good at:

  1. match     — classic BM25 keyword search
  2. semantic  — embed the query, k-NN over the document vectors
  3. hybrid    — BM25 + semantic fused with Reciprocal Rank Fusion

Run it:
    xerj --insecure --data-dir ./data &        # start XERJ
    python3 recipes/semantic_search.py

Optional env:
    XERJ_URL   (default http://localhost:9200)

The built-in default embedder is *lexical* (feature-hashing), not neural.
For real neural semantics — with no change to the index, mapping, or the
three queries below — pick a different backend when you start the server:

    # Built-in neural BERT (all-MiniLM-L6-v2), in-process — shipped in the
    # binary; the model auto-downloads (~90 MB) on first use, then caches.
    xerj --insecure --data-dir ./data --embed-mode neural

    # …or any external OpenAI-compatible endpoint (bring your own model):
    xerj --insecure --data-dir ./data --embed-mode proxy
    # with, in xerj.toml:
    #   [embedding]
    #   default_endpoint = "https://api.openai.com/v1/embeddings"
    #   default_model    = "text-embedding-3-small"
"""

import json
import os
import pathlib
import urllib.error
import urllib.request

XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")
KB = pathlib.Path(__file__).resolve().parent.parent / "demo" / "data" / "ai_kb.ndjson"
INDEX = "ai-kb"


def call(method: str, path: str, body=None):
    """One tiny HTTP helper instead of a client library."""
    data = None
    headers = {}
    if body is not None:
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        headers["Content-Type"] = (
            "application/x-ndjson" if isinstance(body, str) else "application/json"
        )
    req = urllib.request.Request(f"{XERJ}{path}", data=data, headers=headers, method=method)
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def main():
    # ── 1. Fresh index: `content` is semantic_text, so XERJ embeds it at
    #       ingest time and `semantic` queries embed the question the same
    #       way. `category`/`source` stay keyword for exact filtering.
    try:
        call("DELETE", f"/{INDEX}")
    except urllib.error.HTTPError:
        pass  # didn't exist yet
    call("PUT", f"/{INDEX}", {
        "mappings": {
            "properties": {
                "title":    {"type": "text"},
                "content":  {"type": "semantic_text", "dimensions": 384},
                "category": {"type": "keyword"},
                "source":   {"type": "keyword"},
            }
        }
    })

    # ── 2. Bulk-ingest the real KB (40 hand-written articles).
    lines = []
    for raw in KB.read_text().splitlines():
        doc = json.loads(raw)
        doc.pop("embedding", None)  # let semantic_text embed the content itself
        lines.append(json.dumps({"index": {"_index": INDEX, "_id": str(doc["id"])}}))
        lines.append(json.dumps(doc))
    call("POST", "/_bulk?refresh=true", "\n".join(lines) + "\n")
    total = call("GET", f"/{INDEX}/_count")["count"]
    print(f"indexed {total} KB articles into `{INDEX}`\n")

    # ── 3. Ask the same question three ways.
    question = "how do I keep an LLM agent's context from overflowing?"

    modes = {
        "match (BM25)": {"match": {"content": question}},
        "semantic":     {"semantic": {"field": "content", "query": question, "k": 5}},
        "hybrid (RRF)": {"hybrid": {"queries": [
            {"query": {"match": {"content": question}}, "weight": 1.0},
            {"query": {"semantic": {"field": "content", "query": question, "k": 10}}, "weight": 1.0},
        ]}},
    }

    print(f"Q: {question}\n")
    for name, query in modes.items():
        hits = call("POST", f"/{INDEX}/_search", {"query": query, "size": 3})["hits"]["hits"]
        print(f"── {name}")
        for h in hits:
            src = h["_source"]
            print(f"   {h['_score']:6.3f}  [{src['category']:>9}]  {src['title']}")
        print()


if __name__ == "__main__":
    main()
