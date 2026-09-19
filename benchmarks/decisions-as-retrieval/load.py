import csv, json, sys, urllib.request, random, os
U=os.environ.get("XERJ_URL","http://localhost:9410")
def req(m,p,b=None,ct="application/json"):
    d=b if isinstance(b,(bytes,type(None))) else json.dumps(b).encode()
    r=urllib.request.Request(U+p,data=d,method=m,headers={"content-type":ct})
    try: return json.loads(urllib.request.urlopen(r,timeout=900).read())
    except urllib.error.HTTPError as e: return {"_err":e.code,"body":e.read().decode()[:300]}
def load(idx,rows):
    req("DELETE","/"+idx)
    print(req("PUT","/"+idx,{"mappings":{"properties":{"label":{"type":"keyword"},"text":{"type":"text"},"body":{"type":"semantic_text"}}}}))
    for i in range(0,len(rows),250):
        lines=[]
        for j,(t,l) in enumerate(rows[i:i+250]):
            lines.append(json.dumps({"index":{"_index":idx,"_id":str(i+j)}})); lines.append(json.dumps({"text":t,"body":t,"label":l}))
        r=req("POST","/_bulk",("\n".join(lines)+"\n").encode(),"application/x-ndjson")
        if r.get("errors") or "_err" in r: print("bulk problem",str(r)[:300]); sys.exit(1)
        if i%1000==0: print(idx,"indexed",i,flush=True)
    req("POST",f"/{idx}/_refresh"); print(idx,req("GET",f"/{idx}/_count").get("count"))
b=[(r["text"],r["category"]) for r in csv.DictReader(open("b77_train.csv"))]
load("b77",b)
s=[l.rstrip("\n").split("\t",1) for l in open("sms.tsv")]; s=[(t,l) for l,t in s]
random.Random(7).shuffle(s); json.dump(s[4000:],open("sms_test.json","w")); load("sms",s[:4000])
