import json, urllib.request, sys, time
import os
U=os.environ.get("XERJ_URL","http://localhost:9410")
def req(m,p,b=None,ct="application/json"):
    d = b if isinstance(b,(bytes,type(None))) else json.dumps(b).encode()
    r=urllib.request.Request(U+p,data=d,method=m,headers={"content-type":ct})
    try: return json.loads(urllib.request.urlopen(r,timeout=600).read())
    except urllib.error.HTTPError as e: return {"_err":e.code,"body":e.read().decode()[:400]}
DS=sys.argv[2]
print(req("DELETE","/"+DS))
print(req("PUT","/"+DS,{"mappings":{"properties":{
  "title":{"type":"text"},"text":{"type":"text"},
  "body":{"type":"semantic_text"}}}}))
docs=[json.loads(l) for l in open(sys.argv[1])]
B=200
for i in range(0,len(docs),B):
    lines=[]
    for d in docs[i:i+B]:
        lines.append(json.dumps({"index":{"_index":DS,"_id":d["_id"]}}))
        lines.append(json.dumps({"title":d["title"],"text":d["text"],"body":(d["title"]+". "+d["text"])}))
    body=("\n".join(lines)+"\n").encode()
    # A 429 — whole request or per item — is the node's memory breaker asking
    # the writer to wait (#1091's A/B lost a third of FiQA to one). The batch
    # carries fixed _ids, so resending it is idempotent; anything else stops.
    for attempt in range(60):
        r=req("POST","/_bulk",body,"application/x-ndjson")
        throttled=r.get("_err")==429 or (r.get("errors") and any(
            next(iter(it.values())).get("status")==429 for it in r.get("items",[])))
        if not throttled: break
        time.sleep(5)
    if r.get("errors") or "_err" in r: print("bulk problem",str(r)[:300]); break
    if i%1000==0: print("indexed",i,flush=True)
print(req("POST","/"+DS+"/_refresh")); print(req("GET","/"+DS+"/_count"))
