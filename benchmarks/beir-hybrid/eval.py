"""nDCG@10 on BEIR SciFact (test split) for retrieval arms on a live XERJ node.
Every number printed comes from a query against the node; nothing is assumed."""
import json, math, sys, time, urllib.request, collections
import os
U=os.environ.get("XERJ_URL","http://localhost:9410"); IDX=sys.argv[2] if len(sys.argv)>2 else "scifact"; WINDOW=int(sys.argv[1]) if len(sys.argv)>1 else 30
def post(p,b):
    r=urllib.request.Request(U+p,data=json.dumps(b).encode(),method="POST",headers={"content-type":"application/json"})
    try: return json.loads(urllib.request.urlopen(r,timeout=120).read())
    except urllib.error.HTTPError as e: return {"_err":e.code,"body":e.read().decode()[:300]}
queries={}
for l in open(IDX+"/queries.jsonl"):
    d=json.loads(l); queries[d["_id"]]=d["text"]
qrels=collections.defaultdict(dict)
for i,l in enumerate(open(IDX+"/qrels/test.tsv")):
    if i==0: continue
    q,d,s=l.split("\t"); qrels[q][d]=int(s)
def ndcg10(ranked,rel):
    dcg=sum(rel.get(d,0)/math.log2(i+2) for i,d in enumerate(ranked[:10]))
    ideal=sorted(rel.values(),reverse=True)[:10]
    idcg=sum(g/math.log2(i+2) for i,g in enumerate(ideal))
    return dcg/idcg if idcg else 0.0
def ids(res): return [h["_id"] for h in res.get("hits",{}).get("hits",[])]
BM=lambda q:{"multi_match":{"query":q,"fields":["title","text"]}}
SEM=lambda q:{"semantic":{"field":"body","query":q}}
def bm25(q,n=100): return ids(post(f"/{IDX}/_search",{"size":n,"_source":False,"query":BM(q)}))
def sem(q,n=100):  return ids(post(f"/{IDX}/_search",{"size":n,"_source":False,"query":SEM(q)}))
def hybrid(q,n=100):
    return ids(post(f"/{IDX}/_search",{"size":n,"_source":False,"query":{"hybrid":{"queries":[{"query":BM(q)},{"query":SEM(q)}],"fusion":"rrf"}}}))
def rerank_window(q,w):
    """BM25 top-w, reordered by the embedding score; the tail keeps BM25 order."""
    base=bm25(q,100); win=base[:w]
    r=post(f"/{IDX}/_search",{"size":w,"_source":False,"query":{"bool":{"must":[SEM(q)],"filter":[{"ids":{"values":win}}]}}})
    order=ids(r)
    if "_err" in r: raise SystemExit(f"filtered semantic failed: {r}")
    seen=set(order); return order+[d for d in win if d not in seen]+base[w:]
arms={"bm25":bm25,"minilm (vector only)":sem,"hybrid rrf (server)":hybrid,f"bm25 top-{WINDOW} -> minilm rerank":lambda q:rerank_window(q,WINDOW)}
probe=post(f"/{IDX}/_search",{"size":1,"query":SEM("test")})
if "_err" in probe: raise SystemExit(f"semantic arm unavailable: {probe}")
print(f"queries={len(qrels)}  docs={post(f'/{IDX}/_count',{}).get('count')}")
for name,fn in arms.items():
    t=time.time(); scores=[]; empty=0; lat=[]
    for qid,rel in qrels.items():
        t0=time.time(); ranked=fn(queries[qid]); lat.append(time.time()-t0)
        if not ranked: empty+=1
        scores.append(ndcg10(ranked,rel))
    lat.sort()
    print(f"{name:34s} nDCG@10={sum(scores)/len(scores):.4f}  empty={empty:3d}  p50={lat[len(lat)//2]*1000:6.1f}ms  p95={lat[int(len(lat)*.95)]*1000:6.1f}ms")
