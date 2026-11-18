"""usage: run.py <xerj-binary> <port> <label>

Starts a throwaway `--insecure` loopback node on <port> (ES API; +1/+2 for
REST/gRPC) in a temp data dir and HOME, autoindexes ./corpus, runs every
question in questions.json through `xerj search --json -k 10`, and records
the rank of the expected file among distinct `ax_path`s. Writes
results/<label>.json."""
import json,os,subprocess,sys,tempfile,time,urllib.request,collections,shutil
X,P,label=sys.argv[1],sys.argv[2],sys.argv[3]
D=tempfile.mkdtemp(); H=tempfile.mkdtemp(); url=f"http://localhost:{P}"
env=dict(os.environ,HOME=H); env.pop('XERJ_API_KEY',None)
srv=subprocess.Popen([X,'--insecure','--data-dir',D,'--port',P],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
for _ in range(200):
    try: urllib.request.urlopen(url+'/_cluster/health',timeout=1); break
    except Exception: time.sleep(0.25)
def idx(path,prefix):
    r=subprocess.run([X,'autoindex','--url',url,'--prefix',prefix,'--progress','none',path],env=env,capture_output=True,text=True)
    return r.returncode
def search(q,prefix,k=10):
    r=subprocess.run([X,'search','--url',url,'--prefix',prefix,'--json','-k',str(k),q],env=env,capture_output=True,text=True)
    try: hits=json.loads(r.stdout)['hits']['hits']
    except Exception: return [],r.stdout+r.stderr
    paths=[]
    for h in hits:
        p=(h.get('_source') or {}).get('ax_path')
        if p and p not in paths: paths.append(p)
    return paths,None
def text_tokens(q,prefix):
    r=subprocess.run([X,'search','--url',url,'--prefix',prefix,'-k','5',q],env=env,capture_output=True,text=True)
    return r.stdout
res={'label':label}
try:
    t=time.time(); res['idx_x']=idx('corpus','bx'); res['idx_x_s']=round(time.time()-t,1)
    qs=json.load(open('questions.json'))
    per=collections.defaultdict(lambda: collections.Counter()); rows=[]
    for q in qs:
        paths,err=search(q['q'],'bx')
        rank=paths.index(q['gt'])+1 if q['gt'] in paths else None
        rows.append({**q,'rank':rank,'err':err})
        c=per[q['set']]; c['n']+=1; c['r1']+=rank==1; c['top5']+=bool(rank and rank<=5); c['top10']+=bool(rank); c['err']+=bool(err)
    res['sets']={k:dict(v) for k,v in per.items()}; res['rows']=rows
finally:
    srv.terminate(); srv.wait(); shutil.rmtree(D,True); shutil.rmtree(H,True)
json.dump(res,open(f'results/{label}.json','w'),indent=1)
print(label, 'index', res.get('idx_x'), res.get('idx_x_s'),'s')
for k,v in sorted(res.get('sets',{}).items()): print(' ',k,v)
