#!/usr/bin/env python3
"""XERJ retrieval for the xerj arm.

HONESTY CONTRACT — this script sees ONLY the question text.
It never sees answer_file, answer_line, answer_text, or the grader regex.
It is a fixed, question-agnostic query template. No per-question tuning.
"""
import json, sys, urllib.request

ES = "http://localhost:9200/refsym/_search"
TOPK = 8

def search(question, allowed_paths=None):
    must = [{"multi_match": {
        "query": question,
        "fields": ["code", "name_text^2", "sig^1.5", "doc"],
        "type": "best_fields"}}]
    # repo filter is MANDATORY: the index also contains `usearch`, which is NOT
    # on disk for the native arm. Without it the xerj arm sees a corpus its peer cannot.
    flt = [{"term": {"repo": "lucene"}}]
    if allowed_paths:
        flt.append({"terms": {"path": allowed_paths}})
    body = {"size": TOPK, "query": {"bool": {"filter": flt, "must": must}},
            "_source": ["path", "name", "kind", "line", "end_line", "sig", "code"]}
    r = urllib.request.Request(ES, data=json.dumps(body).encode(),
                               headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(r))["hits"]["hits"]

def render(hits):
    out = []
    for h in hits:
        s = h["_source"]
        out.append("// %s:%s-%s  (%s %s)\n%s" %
                   (s["path"], s["line"], s.get("end_line"), s["kind"], s["name"], s["code"]))
    return "\n\n".join(out)

if __name__ == "__main__":
    q = sys.argv[1]
    paths = json.load(open(sys.argv[2])) if len(sys.argv) > 2 else None
    hits = search(q, paths)
    print(render(hits))
