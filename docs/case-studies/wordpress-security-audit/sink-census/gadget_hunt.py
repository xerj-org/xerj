#!/usr/bin/env python3
"""POP-gadget hunt over XERJ data: find magic methods (__wakeup/__destruct/
__toString/__call/...) that reach a dangerous call — the deserialization
gadget-chain surface. A class is a usable gadget only if a magic method (which
unserialize/free/print auto-invokes) leads to a sink.

Interprocedural DFS over the wpaudit call graph (unambiguous edges), payoff =
a reached function whose wpaudit `sinks` is non-empty OR whose `calls` include a
high-impact dangerous function.
"""
import json, sys, urllib.request
X="http://127.0.0.1:9200"
MAGIC={"__wakeup","__destruct","__toString","__call","__callStatic","__get","__set",
       "__invoke","__unserialize","__sleep","__serialize","__set_state","__clone","__isset","__unset"}
# auto-triggered on unserialize/free/print/use (the ones that make a gadget "live")
AUTO={"__wakeup","__unserialize","__destruct","__toString"}
DANGER={"exec","shell_exec","system","passthru","popen","proc_open","pcntl_exec","eval","assert",
        "create_function","call_user_func","call_user_func_array","unserialize","file_put_contents",
        "fwrite","fputs","unlink","rename","copy","move_uploaded_file","extractTo","readImage",
        "mysqli_query","mysql_query","fopen","file_get_contents","include","require"}

def scroll():
    docs=[]; r=urllib.request.Request(f"{X}/wpaudit/_search?scroll=2m",
      data=json.dumps({"size":2000,"query":{"match_all":{}},"_source":["func","file","line","sinks","calls"]}).encode(),
      headers={"Content-Type":"application/json"})
    res=json.loads(urllib.request.urlopen(r).read()); sid=res["_scroll_id"]; hits=res["hits"]["hits"]
    while hits:
        docs+=[h["_source"] for h in hits]
        r=urllib.request.Request(f"{X}/_search/scroll",data=json.dumps({"scroll":"2m","scroll_id":sid}).encode(),headers={"Content-Type":"application/json"})
        res=json.loads(urllib.request.urlopen(r).read()); sid=res["_scroll_id"]; hits=res["hits"]["hits"]
    return docs

docs=scroll()
byname={}; name_count={}
for d in docs:
    name_count[d["func"]]=name_count.get(d["func"],0)+1
    byname.setdefault(d["func"],d)

def reaches_danger(start, depth=6):
    """DFS from a magic method; return (danger_fn, path) if a dangerous call is reached."""
    stack=[(start["func"],[start["func"]])]; seen=set()
    while stack:
        fn,path=stack.pop()
        if fn in seen or len(path)>depth: continue
        seen.add(fn); d=byname.get(fn)
        if not d: continue
        if fn!=start["func"] and d["sinks"]:
            return ("sink:"+"/".join(d["sinks"]), path)
        for c in d.get("calls",[]):
            if c in DANGER: return (c, path+[c])
            if c in byname and name_count.get(c,0)==1: stack.append((c,path+[c]))
    return None

magic=[d for d in docs if d["func"] in MAGIC]
gadgets=[]
for m in magic:
    r=reaches_danger(m)
    if r:
        danger,path=r
        gadgets.append({"magic":m["func"],"file":m["file"],"line":m["line"],
                        "auto": m["func"] in AUTO,"danger":danger,"path":path})
gadgets.sort(key=lambda g:(not g["auto"], g["magic"]))
print(f"magic methods: {len(magic)}   reaching a dangerous call: {len(gadgets)}")
print(f"  of which AUTO-triggered (__wakeup/__unserialize/__destruct/__toString -> the live gadgets): {sum(1 for g in gadgets if g['auto'])}")
print("\nGADGET CANDIDATES (magic -> ... -> danger):")
for g in gadgets:
    tag="LIVE" if g["auto"] else "call-only"
    print(f"  [{tag}] {g['magic']:14} {g['file'].split('/')[-1]}:{g['line']}")
    print(f"         -> {' -> '.join(g['path'])}  ==> {g['danger']}")
# token measure: facts pulled (func/calls/sinks for the whole graph loaded once)
import io
facts_bytes=sum(len(json.dumps({k:d[k] for k in ('func','calls','sinks')})) for d in docs)
print(f"\nXERJ-facts token cost (whole call graph, loaded once): ~{facts_bytes//4:,} tokens")
print(f"  (but the ANSWER — {len(gadgets)} candidates — is ~{len(json.dumps(gadgets))//4} tokens)")
json.dump(gadgets, open("gadgets.json","w"))
