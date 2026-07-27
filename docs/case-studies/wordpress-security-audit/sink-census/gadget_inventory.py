#!/usr/bin/env python3
"""Gadget inventory over XERJ data: classes with a dangerous magic method that
LACK a throwing __wakeup/__unserialize guard — the ones whose __destruct fires
without a guard blocking it. Empirically proven (GADGET-WAKEUP-TEST.md): a
throwing __wakeup suppresses __destruct; so the EXPOSED gadgets are the unguarded
ones. Also flags autoload/bootstrap loadability (can unserialize instantiate it?).
"""
import json, os, re, urllib.request
X="http://127.0.0.1:9200"; here=os.path.dirname(__file__)
cat=json.load(open(os.path.join(here,"php_dangerous_functions.json")))["categories"]
FN2CLASS={f["fn"]:c.get("class",cn) for cn,c in cat.items() for f in c["fns"]}
CONSTRUCTS={"eval":"RCE-code","include":"LFI-RFI","require":"LFI-RFI","echo":"XSS","print":"XSS"}
MAGIC={"__destruct","__toString","__call","__callStatic","__get","__set","__invoke",
       "__unserialize","__wakeup","__sleep","__serialize","__isset","__unset","__clone"}
AUTOFIRE={"__destruct","__toString","__wakeup","__unserialize"}  # fire without a method being called

def q(body):
    r=urllib.request.Request(f"{X}/wpaudit/_search",data=json.dumps(body).encode(),headers={"Content-Type":"application/json"})
    return [h["_source"] for h in json.loads(urllib.request.urlopen(r).read())["hits"]["hits"]]

magic=q({"query":{"terms":{"func":list(MAGIC)}},"size":800,"_source":["func","file","line","code"]})

def dangerous(code):
    out=[]
    for fn,cls in FN2CLASS.items():
        if re.search(rf"(?<![\w$])(->|::|\b){re.escape(fn)}\s*\(",code): out.append((fn,cls))
    for kw,cls in CONSTRUCTS.items():
        if re.search(rf"(?<![\w$>]){kw}[\s(\"'$]",code): out.append((kw,cls))
    return sorted(set(out))

# group magic methods by file (~class in WP: one class per file)
byfile={}
for m in magic:
    byfile.setdefault(m["file"],{})[m["func"]]=m

rows=[]
for f,methods in byfile.items():
    # does this class have a throwing __wakeup or __unserialize guard?
    guard=False
    for g in ("__wakeup","__unserialize"):
        if g in methods and re.search(r"\bthrow\b", methods[g].get("code","")): guard=True
    # dangerous magic methods in this class
    for name,m in methods.items():
        d=dangerous(m.get("code",""))
        if d and name in MAGIC and name not in ("__wakeup","__unserialize"):
            rows.append({"class_file":f,"magic":name,"line":m["line"],"guard":guard,
                         "autofire": name in AUTOFIRE,"danger":d})

exposed=[r for r in rows if not r["guard"] and r["autofire"]]
guarded=[r for r in rows if r["guard"]]
print(f"classes-with-dangerous-magic-method entries: {len(rows)}")
print(f"  GUARDED by throwing __wakeup/__unserialize: {len(guarded)}")
print(f"  EXPOSED (auto-fire magic, NO guard) -> gadget candidates: {len(exposed)}\n")
print("EXPOSED gadget candidates (auto-fire, unguarded):")
for r in sorted(exposed,key=lambda x:x['class_file']):
    dd=", ".join(f"{fn}[{cls}]" for fn,cls in r["danger"])
    print(f"  {r['class_file'].split('/')[-1]:42} {r['magic']:12} -> {dd}")
json.dump({"exposed":exposed,"guarded":[{k:r[k] for k in ('class_file','magic','danger')} for r in guarded]},
          open(os.path.join(here,"gadget_inventory.json"),"w"),indent=1)
print(f"\nwrote gadget_inventory.json  (facts-only token cost of this whole hunt: ~{len(json.dumps([m for m in magic]))//4:,} tokens)")
