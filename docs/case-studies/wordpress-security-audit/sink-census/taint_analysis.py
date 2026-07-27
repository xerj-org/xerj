#!/usr/bin/env python3
"""Interprocedural taint analysis over XERJ data (no local files).

Uses the per-function facts already in `wpaudit` (sources it reads, sink classes
it hits, sanitizer-on-path flag, call edges) to find SOURCE -> SINK flows:

  a function that reads a request source ($_GET/$_POST/...) reaches a dangerous
  sink (sql/cmd/code/deser/lfi/xss) with NO sanitizer anywhere on the call path.

Intra-procedural: source + sink + no-sanitizer in the same function.
Inter-procedural: forward DFS over unambiguous call edges to a sink function.
Ranks flows by sink severity and joins `wpsinks` for the exact dangerous call.
"""
import json, urllib.request
X = "http://127.0.0.1:9200"
SEV = {"cmd": 5, "code": 5, "deser": 5, "sql": 4, "lfi": 3, "xss": 2}

def scroll(idx, src):
    docs = []
    r = urllib.request.Request(f"{X}/{idx}/_search?scroll=2m",
        data=json.dumps({"size": 2000, "query": {"match_all": {}}, "_source": src}).encode(),
        headers={"Content-Type": "application/json"})
    res = json.loads(urllib.request.urlopen(r).read()); sid = res["_scroll_id"]; hits = res["hits"]["hits"]
    while hits:
        docs += [h["_source"] for h in hits]
        r = urllib.request.Request(f"{X}/_search/scroll",
            data=json.dumps({"scroll": "2m", "scroll_id": sid}).encode(),
            headers={"Content-Type": "application/json"})
        res = json.loads(urllib.request.urlopen(r).read()); sid = res["_scroll_id"]; hits = res["hits"]["hits"]
    return docs

print("loading wpaudit call graph from XERJ ...")
docs = scroll("wpaudit", ["func", "file", "line", "sources", "sinks", "sanit", "calls"])
byname = {}
name_count = {}
for d in docs:
    name_count[d["func"]] = name_count.get(d["func"], 0) + 1
    byname.setdefault(d["func"], d)   # first def; edges resolved only if unambiguous
print(f"  {len(docs)} functions, {sum(1 for d in docs if d['sources'])} read a source, "
      f"{sum(1 for d in docs if d['sinks'])} hit a sink")

def taint_dfs(start, depth=8):
    """From a source function, forward-DFS unambiguous call edges to a sink,
    tracking whether a sanitizer was seen on the path. Returns (path, sink_classes)."""
    stack = [(start["func"], [start["func"]], start["sanit"])]
    seen = set()
    while stack:
        fn, path, san = stack.pop()
        if fn in seen or len(path) > depth: continue
        seen.add(fn)
        d = byname.get(fn)
        if not d: continue
        if d["sinks"] and not san:
            return path, d["sinks"], d
        for c in d.get("calls", []):
            if c in byname and name_count.get(c, 0) == 1:   # unambiguous edge only
                stack.append((c, path + [c], san or byname[c]["sanit"]))
    return None

flows = []
for d in docs:
    if not d["sources"]: continue
    # intra-procedural: source + sink + no sanitizer in the same function
    if d["sinks"] and not d["sanit"]:
        flows.append({"kind": "intra", "path": [d["func"]], "sink_fn": d,
                      "sinks": d["sinks"], "src": d["sources"]})
        continue
    if d["sanit"]: continue
    r = taint_dfs(d)
    if r:
        path, sinks, sink_d = r
        flows.append({"kind": "inter", "path": path, "sink_fn": sink_d,
                      "sinks": sinks, "src": d["sources"]})

def score(f): return max((SEV.get(s, 1) for s in f["sinks"]), default=0)
flows.sort(key=lambda f: (-score(f), -len(f["path"])))
print(f"\nTAINT FLOWS (source -> sink, no sanitizer on path): {len(flows)}")
from collections import Counter
byc = Counter(s for f in flows for s in f["sinks"])
print("  by terminal sink class:", dict(byc))
print("\n  top high-severity flows (read these):")
for f in flows[:20]:
    sd = f["sink_fn"]
    print(f"    [{'/'.join(sorted(f['sinks'], key=lambda s:-SEV.get(s,0)))}] "
          f"{f['src']}  {' -> '.join(f['path'])}")
    print(f"        sink in {sd['file']}:{sd['line']}")
json.dump(flows[:200], open("taint_flows.json", "w"), default=str)
print(f"\nwrote top-200 flows to taint_flows.json")
