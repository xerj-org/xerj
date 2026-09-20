import json, urllib.request, sys
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
    r=req("POST","/_bulk",("\n".join(lines)+"\n").encode(),"application/x-ndjson")
    if r.get("errors") or "_err" in r: print("bulk problem",str(r)[:300]); break
    if i%1000==0: print("indexed",i,flush=True)
print(req("POST","/"+DS+"/_refresh")); print(req("GET","/"+DS+"/_count"))
