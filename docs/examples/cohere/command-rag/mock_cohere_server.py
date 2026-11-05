#!/usr/bin/env python3
"""A key-free stand-in for the parts of the Cohere API this example calls.

It answers the request shapes documented by Cohere so the cookbook and the
XERJ proxy embedder can be tested without a key or network:

  POST /v2/embed                         {model, texts, input_type, embedding_types}
  POST /v2/rerank                        {model, query, documents, top_n}
  POST /v2/chat                          {model, messages, documents}
  POST /compatibility/v1/embeddings      OpenAI-shaped, WITHOUT the per-item `index`
  GET  /                                 call counters (for the test's assertions)

The vectors and scores are deterministic and meaningless beyond "similar text
gets similar vectors". Nothing here measures Cohere quality.

CITATION_FORM=id   (default) citations echo the document ids the client sent.
CITATION_FORM=doc  citations name documents by position ("doc:0", "doc:1", ...),
                   the other form the cookbook has to handle.
"""
import hashlib
import http.server
import json
import os
import re
import sys

DIM = 256
PORT = int(os.environ.get("MOCK_COHERE_PORT", "8901"))
CITATION_FORM = os.environ.get("CITATION_FORM", "id")
CALLS = {"embed": 0, "rerank": 0, "chat": 0, "compat_embed": 0, "texts": 0}


def vec(text):
    v = [0.0] * DIM
    t = text.lower()
    for i in range(len(t) - 2):
        h = int(hashlib.md5(t[i:i + 3].encode()).hexdigest(), 16)
        v[h % DIM] += 1.0
    n = sum(x * x for x in v) ** 0.5 or 1.0
    return [x / n for x in v]


def words(text):
    return set(re.findall(r"[a-z0-9]+", text.lower()))


def overlap(query, doc):
    q = words(query)
    return len(q & words(doc)) / (len(q) or 1)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.reply(200, CALLS)

    def do_POST(self):
        if not self.headers.get("Authorization", "").startswith("Bearer "):
            return self.reply(401, {"message": "no api key"})
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        path = self.path.rstrip("/")
        if path == "/v2/embed":
            texts = body.get("texts", [])
            if body.get("input_type") not in ("search_document", "search_query", "classification", "clustering"):
                return self.reply(400, {"message": "input_type is required"})
            CALLS["embed"] += 1
            CALLS["texts"] += len(texts)
            return self.reply(200, {"id": "mock", "embeddings": {"float": [vec(t) for t in texts]},
                                    "texts": texts, "meta": {"billed_units": {"input_tokens": len(texts)}}})
        if path == "/compatibility/v1/embeddings":
            texts = body.get("input", [])
            if isinstance(texts, str):
                texts = [texts]
            CALLS["compat_embed"] += 1
            CALLS["texts"] += len(texts)
            # Deliberately no "index" per item: the XERJ proxy must keep request order.
            return self.reply(200, {"object": "list", "model": body.get("model"),
                                    "data": [{"object": "embedding", "embedding": vec(t)} for t in texts]})
        if path == "/v2/rerank":
            docs = body.get("documents", [])
            CALLS["rerank"] += 1
            scored = sorted(((overlap(body.get("query", ""), d if isinstance(d, str) else json.dumps(d)), i)
                             for i, d in enumerate(docs)), reverse=True)
            top = scored[: body.get("top_n") or len(docs)]
            return self.reply(200, {"id": "mock", "results": [{"index": i, "relevance_score": round(s, 4)} for s, i in top]})
        if path == "/v2/chat":
            docs = body.get("documents", [])
            CALLS["chat"] += 1
            cited = docs[:2]
            text = "Mock answer built from the documents provided. " + " ".join(
                f"[{(d.get('data') or {}).get('title', 'document')}]" for d in cited)
            citations = []
            for n, d in enumerate(cited):
                ident = d.get("id") if CITATION_FORM == "id" else f"doc:{n}"
                citations.append({"start": 0, "end": 4, "text": "Mock", "type": "TEXT_CONTENT",
                                  "sources": [{"type": "document", "id": ident, "document": d.get("data", {})}]})
            return self.reply(200, {"id": "mock", "finish_reason": "COMPLETE",
                                    "message": {"role": "assistant", "content": [{"type": "text", "text": text}],
                                                "citations": citations}})
        return self.reply(404, {"message": f"unknown path {self.path}"})


if __name__ == "__main__":
    sys.stderr.write(f"[mock-cohere] listening on 127.0.0.1:{PORT} (citations: {CITATION_FORM})\n")
    http.server.HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
