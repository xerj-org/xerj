"""Jev's choice/noul use cases as retrieval: classify by voting over the k nearest
labelled examples; the winning label's vote share is reported as its probability."""
import csv, json, os, sys, time, urllib.request, collections
U=os.environ.get("XERJ_URL","http://localhost:9410"); K=int(os.environ.get("K","10"))
def post(p,b):
    r=urllib.request.Request(U+p,data=json.dumps(b).encode(),method="POST",headers={"content-type":"application/json"})
    try: return json.loads(urllib.request.urlopen(r,timeout=120).read())
    except urllib.error.HTTPError as e: return {"_err":e.code,"body":e.read().decode()[:200]}
ARMS={"bm25 (no model)":lambda t:{"match":{"text":t}},
      "minilm":lambda t:{"semantic":{"field":"body","query":t}},
      "hybrid rrf":lambda t:{"hybrid":{"queries":[{"query":{"match":{"text":t}}},{"query":{"semantic":{"field":"body","query":t}}}],"fusion":"rrf"}}}
def classify(idx,qb):
    r=post(f"/{idx}/_search",{"size":K,"_source":["label"],"query":qb})
    hs=r.get("hits",{}).get("hits",[])
    if not hs: return None,0.0
    # rank-weighted vote so the ordering matters, not just membership
    v=collections.Counter()
    for i,h in enumerate(hs): v[h["_source"]["label"]]+=1.0/(i+1)
    lab,w=v.most_common(1)[0]; return lab,w/sum(v.values())
def ece(conf_correct,bins=10):
    n=len(conf_correct); tot=0.0
    for b in range(bins):
        xs=[(c,k) for c,k in conf_correct if b/bins<c<=(b+1)/bins or (b==0 and c==0)]
        if xs: tot+=len(xs)/n*abs(sum(k for _,k in xs)/len(xs)-sum(c for c,_ in xs)/len(xs))
    return tot
def run(name,idx,test,positive=None):
    print(f"\n== {name}: {len(test)} test items, k={K}")
    for arm,qf in ARMS.items():
        cc=[]; tp=fp=fn=0; none=0; t0=time.time()
        for text,gold in test:
            pred,conf=classify(idx,qf(text))
            if pred is None: none+=1
            cc.append((conf,1 if pred==gold else 0))
            if positive:
                tp+=pred==positive and gold==positive; fp+=pred==positive and gold!=positive; fn+=pred!=positive and gold==positive
        acc=sum(k for _,k in cc)/len(cc); ms=(time.time()-t0)/len(test)*1000
        hi=[k for c,k in cc if c>=0.8]
        line=f"  {arm:16s} acc={acc:.4f}  ECE={ece(cc):.3f}  conf>=0.8: {len(hi)/len(cc):5.1%} of items at {sum(hi)/max(len(hi),1):.4f} acc  no-hit={none}  {ms:6.1f}ms/item"
        if positive:
            p=tp/max(tp+fp,1); r=tp/max(tp+fn,1); line+=f"  | {positive}: P={p:.3f} R={r:.3f} F1={2*p*r/max(p+r,1e-9):.3f}"
        print(line,flush=True)
b=[(r["text"],r["category"]) for r in csv.DictReader(open("b77_test.csv"))]
N=int(os.environ.get("N","1000"))
import random; random.Random(11).shuffle(b)
run("Banking77 — 77-way intent routing (Jev `choice`)","b77",b[:N])
s=[tuple(x) for x in json.load(open("sms_test.json"))]
run("SMS spam — yes/no detection (Jev `noul`)","sms",s[:N],positive="spam")
