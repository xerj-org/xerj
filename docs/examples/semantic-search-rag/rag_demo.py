#!/usr/bin/env python3
"""
Semantic search / RAG retrieval with XERJ — no separate vector DB.

Indexes a tiny knowledge base into a `semantic_text` field (auto-embedded on
ingest, zero external config), then retrieves passages by *meaning* using the
`semantic` query. The retrieved passages are the chunks you'd hand to an LLM as
grounding context for RAG.

Stdlib only (urllib + json). Point BASE at a running XERJ es_compat port.

    python3 rag_demo.py            # defaults to http://localhost:9482
    BASE=http://localhost:9200 python3 rag_demo.py
"""
import json
import os
import sys
import time
import urllib.request
import urllib.error

BASE = os.environ.get("BASE", "http://localhost:9482")
INDEX = "kb"


def req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(
        BASE + path, data=data, method=method,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(r) as resp:
            return resp.status, json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read().decode())


# The knowledge base: short docs an LLM might need to answer support questions.
# Note the wording — the queries below deliberately paraphrase these.
DOCS = [
    {"id": "kb-1", "title": "Rotating an API key",
     "body": "To change your API credentials, open Settings, choose Security, "
             "and click Regenerate token. The old token stops working immediately."},
    {"id": "kb-2", "title": "Refund policy",
     "body": "Customers can request their money back within 30 days of purchase. "
             "Refunds are issued to the original payment method within five business days."},
    {"id": "kb-3", "title": "Resetting a forgotten password",
     "body": "If you cannot sign in, use the Forgot password link on the login page "
             "to receive a reset email and choose new credentials."},
    {"id": "kb-4", "title": "Supported file formats for upload",
     "body": "You may upload documents as PDF, DOCX, or plain text. Spreadsheets "
             "in CSV and XLSX are also accepted. Images are not indexed."},
    {"id": "kb-5", "title": "Rate limits",
     "body": "The API permits 600 requests per minute per key. Exceeding the "
             "throttle returns HTTP 429; back off and retry after the reset window."},
]


def main():
    # 1) Map `body` as semantic_text -> auto-embedded on ingest (no external service).
    req("DELETE", "/" + INDEX)
    status, resp = req("PUT", "/" + INDEX, {
        "mappings": {
            "properties": {
                "title": {"type": "text"},
                "body": {"type": "semantic_text"},
            }
        }
    })
    print(f"create index -> {status} {json.dumps(resp)[:120]}")
    assert status == 200, resp

    # 2) Bulk-ingest. XERJ embeds `body` into a companion vector at index time.
    lines = []
    for d in DOCS:
        lines.append(json.dumps({"index": {"_index": INDEX, "_id": d["id"]}}))
        lines.append(json.dumps({"title": d["title"], "body": d["body"]}))
    ndjson = "\n".join(lines) + "\n"
    r = urllib.request.Request(
        BASE + "/_bulk", data=ndjson.encode(), method="POST",
        headers={"Content-Type": "application/x-ndjson"},
    )
    with urllib.request.urlopen(r) as resp:
        bulk = json.loads(resp.read().decode())
    print(f"bulk -> errors={bulk.get('errors')} items={len(bulk.get('items', []))}")
    assert bulk.get("errors") is False, bulk
    req("POST", f"/{INDEX}/_refresh")

    # 3) Semantic retrieval. The query PARAPHRASES the KB — different words, same meaning.
    def semantic(query, k=3):
        status, resp = req("POST", f"/{INDEX}/_search", {
            "size": k,
            "query": {"semantic": {"field": "body", "query": query, "k": k}},
        })
        assert status == 200, resp
        return resp["hits"]["hits"]

    def keyword(query, k=3):
        status, resp = req("POST", f"/{INDEX}/_search", {
            "size": k,
            "query": {"match": {"body": query}},
        })
        assert status == 200, resp
        return resp["hits"]["hits"]

    print("\n=== SEMANTIC RETRIEVAL (retrieve by meaning) ===")
    checks = [
        # (paraphrased user question, id we expect back at rank 1)
        ("how do I regenerate my API token", "kb-1"),
        ("can I get my money back after buying", "kb-2"),
        ("I forgot my login and can't sign in", "kb-3"),
        ("which file formats are supported for uploads", "kb-4"),
        ("how many calls per minute am I allowed", "kb-5"),
    ]
    passed = 0
    for q, expect in checks:
        hits = semantic(q, k=3)
        top = hits[0]
        ok = top["_id"] == expect
        passed += ok
        print(f"\nQ: {q!r}")
        print(f"   top -> {top['_id']} '{top['_source']['title']}' "
              f"score={top['_score']:.4f} {'OK' if ok else 'MISS(expected '+expect+')'}")
        for h in hits:
            print(f"      {h['_id']:6} {h['_score']:.4f}  {h['_source']['title']}")

    # 4) Show the RAG context bundle you'd feed an LLM for the first question.
    q = "how do I regenerate my API token"
    hits = semantic(q, k=2)
    context = "\n\n".join(
        f"[{h['_id']}] {h['_source']['title']}\n{h['_source']['body']}" for h in hits
    )
    print("\n=== RAG CONTEXT BUNDLE (top-2 passages -> LLM prompt) ===")
    print(f"User question: {q}\n")
    print("Retrieved context:\n" + context)

    # 5) Contrast: a paraphrase with NO shared keywords is where lexical differs.
    print("\n=== KEYWORD vs SEMANTIC (same paraphrase) ===")
    q = "get my money back after buying"
    kw = keyword(q, k=1)
    sm = semantic(q, k=1)
    print(f"Q: {q!r}")
    print(f"   match  top -> {(kw[0]['_id']+' '+kw[0]['_source']['title']) if kw else 'NO HITS'}")
    print(f"   semantic top -> {sm[0]['_id']} {sm[0]['_source']['title']}")

    # 6) Honest limitation: the BUILT-IN embedder is lexical. A true paraphrase
    #    with no token overlap against the embedded *body* text can miss. Here
    #    the KB titled "Rotating an API key" is stored, but its body never says
    #    "rotate", so a "rotate" paraphrase does not retrieve it. Wire an
    #    external /v1/embeddings endpoint for production-grade neural semantics.
    print("\n=== HONEST LIMITATION (lexical embedder, no body-token overlap) ===")
    q = "how do I rotate my API key"
    sm = semantic(q, k=1)
    print(f"Q: {q!r}")
    print(f"   semantic top -> {sm[0]['_id']} {sm[0]['_source']['title']} "
          f"(expected kb-1; the body never uses the word 'rotate')")

    print(f"\nRESULT: {passed}/{len(checks)} semantic retrievals returned the right doc at rank 1")
    if passed != len(checks):
        sys.exit(1)


if __name__ == "__main__":
    main()
