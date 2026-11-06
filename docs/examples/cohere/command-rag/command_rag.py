#!/usr/bin/env python3
"""Cohere Command answering over files XERJ indexed, with citations that point
back to the file and page they came from.

Two steps, standard library only:

  index  Read the records `xerj autoindex` wrote (one per PDF page, HTML
         section, ...), embed their text with Cohere Embed
         (input_type=search_document), and write {body, title, path, page, vec}
         to a new XERJ index with the SAME _id as the source record.

  ask    Embed the question (input_type=search_query), run one XERJ hybrid
         query (BM25 on `body` + kNN on `vec`, fused with RRF), rerank the top
         candidates with Cohere Rerank, send the best ones to Cohere Command as
         `documents` (id = XERJ _id), and print the answer with every citation
         resolved to its file and page through GET /{index}/_doc/{id}.

Environment:
  XERJ_URL        default http://127.0.0.1:9200
  CO_API_KEY      Cohere API key (required)
  CO_BASE_URL     default https://api.cohere.com (point it at a private deployment
                  to keep text inside your network; not tested here)
  CO_EMBED_MODEL  default embed-v5.0-pro
  CO_RERANK_MODEL default rerank-v5.0-fast
  CO_CHAT_MODEL   default command-a-plus-05-2026
Model ids are the ones on Cohere's models page on 2026-11-05; check it before use.

Rerank runs here, on the client. A server-side Cohere rerank stage on _search
is a separate change and is not used by this script.
"""
import argparse
import json
import os
import sys
import urllib.error
import urllib.request

XERJ = os.environ.get("XERJ_URL", "http://127.0.0.1:9200").rstrip("/")
CO = os.environ.get("CO_BASE_URL", "https://api.cohere.com").rstrip("/")
EMBED_MODEL = os.environ.get("CO_EMBED_MODEL", "embed-v5.0-pro")
RERANK_MODEL = os.environ.get("CO_RERANK_MODEL", "rerank-v5.0-fast")
CHAT_MODEL = os.environ.get("CO_CHAT_MODEL", "command-a-plus-05-2026")
EMBED_BATCH = 96          # Cohere's per-call limit for /v2/embed
MAX_CHARS = 2000          # text sent per record; longer bodies are cut


def http(method, url, body=None, headers=None):
    data = None if body is None else (body if isinstance(body, bytes) else json.dumps(body).encode())
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json", **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            raw = resp.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        sys.exit(f"{method} {url} -> HTTP {e.code}: {e.read().decode(errors='replace')[:400]}")


def cohere(path, body):
    key = os.environ.get("CO_API_KEY")
    if not key:
        sys.exit("CO_API_KEY is not set")
    return http("POST", f"{CO}{path}", body, {"Authorization": f"Bearer {key}"})


def embed(texts, input_type):
    out = []
    for i in range(0, len(texts), EMBED_BATCH):
        r = cohere("/v2/embed", {"model": EMBED_MODEL, "texts": texts[i:i + EMBED_BATCH],
                                 "input_type": input_type, "embedding_types": ["float"]})
        out.extend(r["embeddings"]["float"])
    return out


def scroll(index):
    """Every record of `index` that has text in `body`."""
    r = http("POST", f"{XERJ}/{index}/_search?scroll=1m",
             {"size": 500, "query": {"exists": {"field": "body"}}})
    while r.get("hits", {}).get("hits"):
        yield from r["hits"]["hits"]
        r = http("POST", f"{XERJ}/_search/scroll", {"scroll": "1m", "scroll_id": r["_scroll_id"]})


def cmd_index(args):
    records = [h for h in scroll(args.source) if (h["_source"].get("body") or "").strip()]
    if not records:
        sys.exit(f"no records with text in index {args.source}")
    texts = [h["_source"]["body"][:MAX_CHARS] for h in records]
    vectors = embed(texts, "search_document")
    dims = len(vectors[0])
    http("PUT", f"{XERJ}/{args.target}", {"mappings": {"properties": {
        "body": {"type": "text"}, "title": {"type": "keyword"}, "path": {"type": "keyword"},
        "page": {"type": "keyword"}, "vec": {"type": "dense_vector", "dims": dims, "similarity": "cosine"}}}})
    lines = []
    for h, v in zip(records, vectors):
        s = h["_source"]
        lines.append(json.dumps({"index": {"_index": args.target, "_id": h["_id"]}}))
        lines.append(json.dumps({"body": s["body"][:MAX_CHARS], "title": s.get("title"), "path": s.get("path"),
                                 "page": s.get("page"), "vec": v}))
    r = http("POST", f"{XERJ}/_bulk", ("\n".join(lines) + "\n").encode())
    if r.get("errors"):
        sys.exit("bulk reported errors: " + json.dumps(r)[:400])
    http("POST", f"{XERJ}/{args.target}/_refresh")
    print(f"indexed {len(records)} records into {args.target} ({dims}-dim Cohere vectors from {EMBED_MODEL})")


def resolve_citation(source, docs):
    """Map a citation source to the XERJ _id we sent.

    Cohere echoes the custom document id in some responses and a positional
    id ("doc:0" / "doc_0") in others; handle both instead of assuming one.
    """
    ident = str(source.get("id", ""))
    ids = [d["id"] for d in docs]
    if ident in ids:
        return ident
    tail = ident.replace("doc:", "").replace("doc_", "").split(":")[-1]
    if tail.isdigit() and int(tail) < len(docs):
        return ids[int(tail)]
    return None


def cmd_ask(args):
    qvec = embed([args.question], "search_query")[0]
    hits = http("POST", f"{XERJ}/{args.target}/_search", {"size": args.candidates, "_source": ["body", "title", "path", "page"],
        "query": {"hybrid": {"queries": [
            {"query": {"match": {"body": args.question}}, "weight": 1.0},
            {"query": {"knn": {"field": "vec", "query_vector": qvec, "num_candidates": max(50, args.candidates)}}, "weight": 1.0}],
            "fusion": {"type": "rrf", "k": 60}}}})["hits"]["hits"]
    if not hits:
        sys.exit("no candidates found")
    ranked = cohere("/v2/rerank", {"model": RERANK_MODEL, "query": args.question,
                                   "documents": [h["_source"]["body"] for h in hits], "top_n": args.top})["results"]
    chosen = [hits[r["index"]] for r in ranked]
    docs = [{"id": h["_id"], "data": {"title": h["_source"].get("title") or "", "snippet": h["_source"]["body"],
                                       "source": f'{h["_source"].get("path")}#page={h["_source"].get("page")}'}} for h in chosen]
    resp = cohere("/v2/chat", {"model": CHAT_MODEL, "messages": [{"role": "user", "content": args.question}], "documents": docs})
    msg = resp["message"]
    print("".join(c.get("text", "") for c in msg.get("content", [])))
    print("\nCitations:")
    unresolved = 0
    for c in msg.get("citations") or []:
        for s in c.get("sources", []):
            doc_id = resolve_citation(s, docs)
            if doc_id is None:
                unresolved += 1
                print(f"  {c.get('text')!r} -> UNRESOLVED source id {s.get('id')!r}")
                continue
            got = http("GET", f"{XERJ}/{args.target}/_doc/{doc_id}")
            src = got.get("_source") or {}
            if not got.get("found"):
                unresolved += 1
            print(f"  {c.get('text')!r} -> {src.get('path')} page {src.get('page')} (_id {doc_id})")
    if args.assert_citations and (unresolved or not msg.get("citations")):
        sys.exit(f"citation check failed: {unresolved} unresolved, {len(msg.get('citations') or [])} citations")


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("index", help="embed autoindexed records into a vector index")
    i.add_argument("--source", required=True, help="index written by xerj autoindex, e.g. docs-docs")
    i.add_argument("--target", default="cohere_rag")
    a = sub.add_parser("ask", help="hybrid search + Cohere rerank + Command answer with citations")
    a.add_argument("question")
    a.add_argument("--target", default="cohere_rag")
    a.add_argument("--candidates", type=int, default=30)
    a.add_argument("--top", type=int, default=5)
    a.add_argument("--assert-citations", action="store_true", help="exit 1 unless every citation resolves to a stored record")
    args = p.parse_args()
    {"index": cmd_index, "ask": cmd_ask}[args.cmd](args)


if __name__ == "__main__":
    main()
