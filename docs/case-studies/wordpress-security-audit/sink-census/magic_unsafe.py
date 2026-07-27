#!/usr/bin/env python3
"""For every magic method, list the dangerous calls INSIDE its body — scanned
against the full 287-function dangerous catalog (+ constructs). This is the
precise 'magic methods with unsafe calls inside' answer, each call tagged with
its vuln class. Uses the wpaudit `code` field via XERJ."""
import json, os, re, urllib.request
X="http://127.0.0.1:9200"; here=os.path.dirname(__file__)
MAGIC=["__wakeup","__destruct","__toString","__call","__callStatic","__get","__set",
       "__invoke","__unserialize","__sleep","__serialize","__set_state","__clone","__isset","__unset"]
AUTO={"__wakeup","__unserialize","__destruct","__toString"}
# fn -> vuln class, from the full catalog
cat=json.load(open(os.path.join(here,"php_dangerous_functions.json")))["categories"]
FN2CLASS={f["fn"]: c.get("class",cn) for cn,c in cat.items() for f in c["fns"]}
CONSTRUCTS={"eval":"RCE-code","include":"LFI-RFI","include_once":"LFI-RFI","require":"LFI-RFI",
            "require_once":"LFI-RFI","echo":"XSS","print":"XSS","exit":"XSS-or-info","die":"XSS-or-info"}

def q(body):
    r=urllib.request.Request(f"{X}/wpaudit/_search",data=json.dumps(body).encode(),headers={"Content-Type":"application/json"})
    return [h["_source"] for h in json.loads(urllib.request.urlopen(r).read())["hits"]["hits"]]

magic=q({"query":{"terms":{"func":MAGIC}},"size":500,"_source":["func","file","line","code"]})

def dangerous_in(code):
    found=[]
    for fn,cls in FN2CLASS.items():
        # call or ->method( / ::method(
        if re.search(rf"(?<![\w$])(->|::|\b){re.escape(fn)}\s*\(",code):
            found.append((fn,cls))
    for kw,cls in CONSTRUCTS.items():
        if re.search(rf"(?<![\w$>]){kw}\b",code):
            # avoid matching in strings crudely: require a following space/paren/quote/$
            if re.search(rf"(?<![\w$>]){kw}[\s(\"'$]",code): found.append((kw,cls))
    # de-dup, drop the magic-method's own name noise
    return sorted(set(found))

rows=[]
for m in magic:
    d=dangerous_in(m.get("code",""))
    if d: rows.append((m,d))
rows.sort(key=lambda r:(r[0]["func"] not in AUTO, r[0]["func"]))

print(f"magic methods scanned: {len(magic)}   with >=1 dangerous call inside: {len(rows)}\n")
print("MAGIC METHODS WITH UNSAFE CALLS INSIDE (■ = auto-triggered on unserialize/free/print):")
for m,d in rows:
    mark="■" if m["func"] in AUTO else " "
    calls=", ".join(f"{fn}[{cls}]" for fn,cls in d)
    print(f" {mark} {m['func']:14} {m['file'].split('/')[-1]}:{m['line']}")
    print(f"      {calls}")

# aggregate: the full list of dangerous calls found inside magic methods, by class
from collections import Counter, defaultdict
bycls=defaultdict(Counter)
for _,d in rows:
    for fn,cls in d: bycls[cls][fn]+=1
print("\n=== ALL dangerous calls found inside magic methods, grouped by class ===")
for cls in sorted(bycls, key=lambda c:-sum(bycls[c].values())):
    print(f"  {cls}: {dict(bycls[cls])}")
json.dump([{"magic":m["func"],"file":m["file"],"line":m["line"],"auto":m["func"] in AUTO,
            "dangerous":[{"fn":fn,"class":cls} for fn,cls in d]} for m,d in rows],
          open(os.path.join(here,"magic_unsafe.json"),"w"),indent=1)
print("\nwrote magic_unsafe.json")
