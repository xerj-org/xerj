"""Live security check of share links on a node with auth ON."""
import json, urllib.request, os, sys, time
D=os.environ.get("XERJ_DATA_PARENT",os.path.dirname(os.path.abspath(__file__))); U=os.environ.get("XERJ_URL","http://localhost:9440")
ADMIN=open(D+"/data/admin.key").read().strip()
def call(m,p,b=None,key=None,raw=False):
    h={"content-type":"application/json"}
    if key: h["authorization"]="ApiKey "+key
    d=None if b is None else (b if isinstance(b,bytes) else json.dumps(b).encode())
    if isinstance(b,bytes): h["content-type"]="application/x-ndjson"
    r=urllib.request.Request(U+p,data=d,method=m,headers=h)
    try: x=urllib.request.urlopen(r,timeout=30); return x.status, json.loads(x.read() or b"{}")
    except urllib.error.HTTPError as e:
        body=e.read()
        try: return e.code, json.loads(body)
        except Exception: return e.code, {"raw":body[:200].decode(errors="replace")}
P=F=0
def ok(name,cond,detail=""):
    global P,F
    if cond: P+=1; print("  ok  ",name)
    else: F+=1; print("  FAIL",name,"->",str(detail)[:240])
# seed two indices: one shared, one that must stay private
call("POST","/_bulk",b'{"index":{"_index":"casefile","_id":"1"}}\n{"subject":"Lease dispute","body":"the landlord refused to return the deposit"}\n{"index":{"_index":"private-diary","_id":"1"}}\n{"body":"nobody else should ever read this"}\n',ADMIN)
call("POST","/casefile,private-diary/_refresh",None,ADMIN)
s,r=call("POST","/_share",{"index":"casefile","expires_in":"1h","max_claims":1,"label":"my lawyer"}); ok("create without a key is refused",s in(401,403),(s,r))
s,r=call("POST","/_share",{"index":"casefile","expires_in":"1h","max_claims":1,"label":"my lawyer"},ADMIN); ok("owner creates a share",s==200 and r.get("share_id") and r.get("passcode"),(s,r))
sid,pw=r.get("share_id"),r.get("passcode"); ok("link carries the id in the URL fragment, which browsers never send",("#"+str(sid)) in r.get("url_path",""),r.get("url_path"))
s,r=call("POST","/_share",{"index":"case*"},ADMIN); ok("wildcard index refused",s==400,(s,r))
s,r=call("POST","/_share",{"index":".xerj_audit"},ADMIN); ok("reserved-namespace index refused",s==400,(s,r))
s,r=call("POST",f"/_share/{sid}/claim",{"passcode":"wrong-code"}); ok("wrong passcode refused",s in(401,403),(s,r))
s,r=call("POST",f"/_share/{'0'*32}/claim",{"passcode":pw}); ok("unknown share id refused without detail",s in(401,403,404),(s,r))
s,g=call("POST",f"/_share/{sid}/claim",{"passcode":pw}); ok("right passcode yields a key",s==200 and g.get("api_key"),(s,g)); GK=g.get("api_key")
s,r=call("POST","/casefile/_search",{"query":{"match":{"body":"deposit"}}},GK); ok("guest can search the shared index",s==200 and r["hits"]["total"]["value"]==1,(s,r))
s,r=call("POST","/private-diary/_search",{"query":{"match_all":{}}},GK); ok("guest CANNOT search another index",s==403,(s,r))
s,r=call("POST","/_search",{"query":{"match_all":{}}},GK); leaked=[h["_index"] for h in r.get("hits",{}).get("hits",[]) if h["_index"]!="casefile"]; ok("guest _search over everything leaks nothing else",not leaked and s in(200,403),(s,leaked))
s,r=call("POST","/casefile,private-diary/_search",{"query":{"match_all":{}}},GK); leaked=[h["_index"] for h in r.get("hits",{}).get("hits",[]) if h["_index"]!="casefile"]; ok("guest multi-index search leaks nothing else",not leaked,(s,leaked))
s,r=call("PUT","/casefile/_doc/99",{"body":"tampered"},GK); ok("guest CANNOT write to the shared index",s==403,(s,r))
s,r=call("DELETE","/casefile",None,GK); ok("guest CANNOT delete the shared index",s==403,(s,r))
s,r=call("GET","/_share",None,GK); ok("guest CANNOT list shares",s==403,(s,r))
s,r=call("POST","/_share",{"index":"casefile"},GK); ok("guest CANNOT mint shares",s==403,(s,r))
s,r=call("GET","/_cat/indices?format=json",None,GK); names=[x.get("index") for x in r] if isinstance(r,list) else []; ok("guest _cat/indices does not reveal the private index","private-diary" not in names,(s,names))
s,r=call("POST",f"/_share/{sid}/claim",{"passcode":pw}); ok("max_claims=1: second claim refused",s in(403,410),(s,r))
disk=open(D+"/data/shares.json").read(); ok("shares.json holds no passcode, share id or key",pw not in disk and sid not in disk and (GK or "x") not in disk)
mode=oct(os.stat(D+"/data/shares.json").st_mode & 0o777); ok("shares.json is 0600",mode=="0o600",mode)
s,l=call("GET","/_share",None,ADMIN); handle=(l.get("shares") or l if isinstance(l,dict) else l); h=None
try: h=(l.get("shares") or [])[0]["handle"]
except Exception: pass
ok("owner lists shares, no secrets in the listing",s==200 and pw not in json.dumps(l) and sid not in json.dumps(l),(s,l))
s,r=call("DELETE",f"/_share/{h}",None,ADMIN); ok("owner revokes",s==200,(s,r))
s,r=call("POST","/casefile/_search",{"query":{"match_all":{}}},GK); ok("revoke kills the guest's key",s in(401,403),(s,r))
# throttle: a fresh share, hammered with wrong passcodes
s,r=call("POST","/_share",{"index":"casefile","max_claims":5},ADMIN); sid2=r["share_id"]; codes=[]
for _ in range(14): codes.append(call("POST",f"/_share/{sid2}/claim",{"passcode":"guess"})[0])
ok("wrong-passcode hammering is throttled with 429",429 in codes,codes)
s,r=call("POST",f"/_share/{sid2}/claim",{"passcode":r.get('passcode','')}); 
print(f"passed={P} failed={F}")
