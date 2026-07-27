#!/usr/bin/env python3
"""Trace a dangerous sink end-to-end THROUGH THE XERJ API — no local file access.
Demonstrates the workflow's tracing capability and that the enriched data is live
and correct. Given a sink `file:line`, it joins across the XERJ indices:

  wpsinks   -> the sink call site + AI enrichment (class, arg, reachable, verdict)
  wpaudit   -> the ENCLOSING function (facts: sources, sinks, calls, code)
  wpauthz   -> the enclosing function's cap/nonce guards
  wpaudit   -> REVERSE callers (who calls the enclosing function) + do they read a source

Usage: python3 trace_sink.py <file> <line>
"""
import json, sys, urllib.request
X = "http://127.0.0.1:9200"
def q(idx, body):
    r = urllib.request.Request(f"{X}/{idx}/_search", data=json.dumps(body).encode(),
                               headers={"Content-Type": "application/json"})
    return json.loads(urllib.request.urlopen(r).read())["hits"]["hits"]

FILE, LINE = sys.argv[1], int(sys.argv[2])

# 1) the sink + its AI enrichment
sink = q("wpsinks", {"query": {"bool": {"filter": [
    {"term": {"file": FILE}}, {"term": {"line": LINE}}]}}, "size": 1})
if not sink:
    print(f"no sink at {FILE}:{LINE}"); sys.exit(1)
s = sink[0]["_source"]
print(f"■ SINK  {FILE}:{LINE}")
print(f"    fn={s['fn']}  class={s['class']}  arg={s['arg'][:70]}")
print(f"    ENRICHMENT: severity={s.get('severity','-')}  reachable={s.get('reachable','-')}")
print(f"                guarded={s.get('guarded','-')}")
print(f"                verdict: {s.get('ai_verdict','-')}")

# 2) the enclosing function: same file, the function whose line <= LINE and is nearest
funcs = q("wpaudit", {"query": {"bool": {"filter": [{"term": {"file": FILE}},
          {"range": {"line": {"lte": LINE}}}]}}, "size": 200, "sort": [{"line": "desc"}],
          "_source": ["func", "line", "sources", "sinks", "sanit", "calls"]})
if not funcs:
    print("\n(no indexed function encloses this line — file-scope code)"); sys.exit(0)
enc = funcs[0]["_source"]
print(f"\n■ ENCLOSING FUNCTION  {enc['func']}()  (defined line {enc['line']})")
print(f"    reads sources: {enc['sources'] or 'none'}   sanitizer-in-fn: {enc['sanit']}")
print(f"    other sinks in fn: {enc['sinks']}")

# 3) authz guards on the enclosing function (wpauthz)
az = q("wpauthz", {"query": {"bool": {"filter": [{"term": {"file": FILE}},
       {"term": {"func": enc["func"]}}]}}, "size": 1,
       "_source": ["has_obj_cap", "has_generic_cap", "has_nonce", "caps"]})
if az:
    a = az[0]["_source"]
    print(f"    AUTHZ: object-scoped-cap={a.get('has_obj_cap')}  nonce={a.get('has_nonce')}  "
          f"caps={[c.get('cap') for c in a.get('caps',[])][:3]}")

# 4) reverse callers — who calls the enclosing function (via _source calls arrays).
#    term-on-array is buggy in XERJ today, so we scan _source in code (documented).
#    NOTE: bare method names (load/query/get) collide across classes — for those,
#    reverse-caller resolution is best-effort; restrict to the same file for methods.
COMMON = {"load","query","get","save","update","render","open","read","write","run"}
same_file_only = enc["func"] in COMMON
callers = q("wpaudit", {"query": {"match": {"code": enc["func"]}}, "size": 200,
            "_source": ["func", "file", "sources", "calls"]})
real = [c["_source"] for c in callers
        if enc["func"] in c["_source"].get("calls", []) and c["_source"]["func"] != enc["func"]
        and (not same_file_only or c["_source"]["file"] == FILE)]
print(f"\n■ REVERSE CALLERS of {enc['func']}(): {len(real)}"
      + ("   [common method name — restricted to same file to avoid class collision]" if same_file_only else ""))
caller_tainted = [c for c in real if c["sources"]]
for c in real[:6]:
    tag = f"  <== reads {c['sources']}" if c["sources"] else ""
    print(f"    {c['file'].split('/')[-1]}::{c['func']}{tag}")

# 5) the trace verdict — consider BOTH the enclosing fn's own sources AND callers'
if enc["sources"] and not enc["sanit"]:
    verdict = f"enclosing fn reads {enc['sources']} with a sink and no in-fn sanitizer -> SOURCE->SINK candidate (confirm arg flow + guards)"
elif caller_tainted:
    verdict = f"a caller reads request input -> POSSIBLE interprocedural source->sink path"
else:
    verdict = "no request source in the enclosing fn or its callers here (triage: not request-reachable at this hop)"
print(f"\n■ TRACE VERDICT: {verdict}")
