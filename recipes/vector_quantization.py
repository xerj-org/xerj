#!/usr/bin/env python3
"""Scalar8 (int8) vector quantization on a real corpus — zero dependencies.

XERJ stores dense vectors at full float32 precision by default. Opting a
`dense_vector` field into scalar8 quantization (`index_options.type:
int8_hnsw`) makes the kNN *serving* path score against 1-byte-per-dimension
codes instead of 4-byte floats — a ~4x smaller vector working set with
almost no recall loss. `_source` still returns the original vectors.

This recipe embeds the 40 real KB articles (demo/data/ai_kb.ndjson) into
128-dim vectors with a small deterministic feature-hasher (same idea as
XERJ's built-in lexical embedder), then indexes the SAME vectors into two
indices — one full-precision (`none`), one quantized (`scalar8`) — and
shows that:

  1. kNN returns the same top results from both,
  2. recall@10 of the quantized index vs the exact index stays >= 0.90,
  3. the quantized field's vector footprint is 4x smaller (1 vs 4 bytes/dim).

Usage:
    xerj --insecure --data-dir ./data &        # start XERJ
    python3 recipes/vector_quantization.py

    XERJ_URL   (default http://localhost:9200)
"""

import hashlib
import json
import math
import os
import pathlib
import urllib.error
import urllib.request

XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")
KB = pathlib.Path(__file__).resolve().parent.parent / "demo" / "data" / "ai_kb.ndjson"
DIM = 128
NONE_INDEX = "vq-none"
SQ8_INDEX = "vq-scalar8"


def call(method, path, body=None):
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


def _h(kind, token):
    """Stable (seed-independent) hash → 64-bit int, via md5."""
    return int.from_bytes(hashlib.md5(f"{kind}:{token}".encode()).digest()[:8], "big")


def embed(text, dim=DIM):
    """Deterministic feature-hashing embedder: word unigrams + char trigrams
    hashed into `dim` buckets with signed contributions, then L2-normalised.
    Real, reproducible, zero-dependency — the same family XERJ uses built-in."""
    vec = [0.0] * dim
    toks = "".join(c.lower() if c.isalnum() else " " for c in text).split()
    for w in toks:
        h = _h("w", w)
        vec[h % dim] += 1.0 if (h >> 63) & 1 else -1.0
        padded = f"#{w}#"
        for i in range(len(padded) - 2):
            t = padded[i : i + 3]
            ht = _h("t", t)
            vec[ht % dim] += 0.35 if (ht >> 63) & 1 else -0.35
    norm = math.sqrt(sum(x * x for x in vec)) or 1.0
    return [x / norm for x in vec]


def make_index(name, quantized):
    try:
        call("DELETE", f"/{name}")
    except urllib.error.HTTPError:
        pass
    field = {"type": "dense_vector", "dims": DIM, "similarity": "cosine"}
    if quantized:
        field["index_options"] = {"type": "int8_hnsw"}  # opt in to scalar8
    call("PUT", f"/{name}", {
        "mappings": {"properties": {"title": {"type": "text"}, "v": field}}
    })


def bulk_load(name, docs):
    lines = []
    for i, d in enumerate(docs):
        lines.append(json.dumps({"index": {"_index": name, "_id": str(i)}}))
        lines.append(json.dumps({"title": d["title"], "v": d["v"]}))
    call("POST", "/_bulk?refresh=true", "\n".join(lines) + "\n")


def knn(name, qv, k=10):
    r = call("POST", f"/{name}/_search", {"knn": {"field": "v", "query_vector": qv, "k": k}})
    return [(h["_id"], h["_score"]) for h in r["hits"]["hits"]]


def main():
    # ── 1. Embed the real KB into 128-dim vectors. ───────────────────────
    docs = []
    for raw in KB.read_text().splitlines():
        a = json.loads(raw)
        docs.append({"title": a["title"], "v": embed(a["title"] + " " + a["content"])})
    print(f"embedded {len(docs)} real KB articles into {DIM}-dim vectors\n")

    # ── 2. Index the SAME vectors two ways. ──────────────────────────────
    make_index(NONE_INDEX, quantized=False)
    make_index(SQ8_INDEX, quantized=True)
    bulk_load(NONE_INDEX, docs)
    bulk_load(SQ8_INDEX, docs)
    print(f"indexed into `{NONE_INDEX}` (float32) and `{SQ8_INDEX}` (int8_hnsw / scalar8)\n")

    # ── 3. Same query, both indices — exact vs quantized scoring. ────────
    q = embed("how do I stop an agent's context window from overflowing?")
    print("query: 'how do I stop an agent's context window from overflowing?'\n")
    for name in (NONE_INDEX, SQ8_INDEX):
        top = knn(name, q, k=3)
        tag = "float32 (exact)" if name == NONE_INDEX else "scalar8 (quantized)"
        print(f"── {tag}")
        for _id, score in top:
            print(f"    {score:.5f}  {docs[int(_id)]['title']}")
        print()

    # ── 4. recall@10 of quantized vs exact over the whole corpus. ────────
    hits = 0
    total = 0
    for d in docs:
        exact = {i for i, _ in knn(NONE_INDEX, d["v"], k=10)}
        approx = {i for i, _ in knn(SQ8_INDEX, d["v"], k=10)}
        hits += len(exact & approx)
        total += len(exact)
    recall = hits / total if total else 0.0
    print(f"recall@10 (scalar8 vs float32 ground truth): {recall:.3f}")
    print(f"vector footprint: float32 = {DIM * 4} B/vec  →  scalar8 = {DIM} B/vec  (4x smaller)")
    if recall < 0.90:
        raise SystemExit(f"FAIL: recall {recall:.3f} < 0.90")
    print("\nOK — 4x smaller vectors, recall preserved. `_source` still holds the originals.")


if __name__ == "__main__":
    main()
