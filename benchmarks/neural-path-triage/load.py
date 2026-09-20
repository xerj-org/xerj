#!/usr/bin/env python3
"""Index one BEIR corpus into a XERJ node started with `--embed-mode neural`.

    python3 load.py http://localhost:9560 scifact /path/to/beir/scifact

Mapping: `title` and `text` are plain `text` (standard analyzer), `body` is
`semantic_text` holding "<title>. <text>", which the node embeds at ingest.
NEVER point this at a node whose data you care about: it DELETEs the index first.
"""
import json
import sys

from common import http

url, index, data_dir = sys.argv[1], sys.argv[2], sys.argv[3]
print(http(url, "DELETE", "/" + index))
print(http(url, "PUT", "/" + index, {"mappings": {"properties": {
    "title": {"type": "text"}, "text": {"type": "text"}, "body": {"type": "semantic_text"}}}}))
docs = [json.loads(l) for l in open(data_dir + "/corpus.jsonl")]
BATCH = 200
for i in range(0, len(docs), BATCH):
    lines = []
    for d in docs[i:i + BATCH]:
        lines.append(json.dumps({"index": {"_index": index, "_id": d["_id"]}}))
        lines.append(json.dumps({"title": d["title"], "text": d["text"],
                                 "body": d["title"] + ". " + d["text"]}))
    r = http(url, "POST", "/_bulk", ("\n".join(lines) + "\n").encode(), "application/x-ndjson")
    if r.get("errors") or "_err" in r:
        print("bulk problem", str(r)[:300])
        break
    if i % 1000 == 0:
        print("indexed", i, flush=True)
print(http(url, "POST", "/" + index + "/_refresh"))
print(http(url, "GET", "/" + index + "/_count"))
