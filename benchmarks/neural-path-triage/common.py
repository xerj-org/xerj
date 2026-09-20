"""Shared helpers for the BEIR neural-baseline harness. Python 3 stdlib only."""
import collections
import json
import math
import os
import urllib.error
import urllib.request


def http(url, method, path, body=None, content_type="application/json", timeout=1800):
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    req = urllib.request.Request(url + path, data=data, method=method,
                                 headers={"content-type": content_type})
    try:
        return json.loads(urllib.request.urlopen(req, timeout=timeout).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:400]}


def load_beir(data_dir):
    """Returns (queries, qrels) for the BEIR *test* split under data_dir."""
    queries = {}
    for line in open(os.path.join(data_dir, "queries.jsonl")):
        d = json.loads(line)
        queries[d["_id"]] = d["text"]
    qrels = collections.defaultdict(dict)
    for i, line in enumerate(open(os.path.join(data_dir, "qrels", "test.tsv"))):
        if i == 0:
            continue
        q, d, s = line.split("\t")
        qrels[q][d] = int(s)
    return queries, qrels


def ndcg10(ranked, rel):
    dcg = sum(rel.get(d, 0) / math.log2(i + 2) for i, d in enumerate(ranked[:10]))
    ideal = sorted(rel.values(), reverse=True)[:10]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg else 0.0


def bm25_query(q):
    return {"multi_match": {"query": q, "fields": ["title", "text"]}}


def semantic_query(q):
    return {"semantic": {"field": "body", "query": q}}


def hybrid_query(q):
    return {"hybrid": {"queries": [{"query": bm25_query(q)}, {"query": semantic_query(q)}],
                       "fusion": "rrf"}}


def process_cpu_seconds(pid):
    """utime+stime of a process, from /proc (Linux only)."""
    fields = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
