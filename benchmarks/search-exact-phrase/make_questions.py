import os,re,random,json,collections
root='corpus'  # engine/crates/**/*.rs at the measured commit (README)
files={}
for d,dirs,fs in os.walk(root):
    dirs.sort()
    for f in sorted(fs):
        p=os.path.join(d,f); files[os.path.relpath(p,root)]=open(p,errors='replace').read()
lit=re.compile(r'"([A-Za-z][A-Za-z ,.\'-]{15,90})"')
occ=collections.defaultdict(set)
for p,s in files.items():
    for m in lit.finditer(s):
        t=m.group(1).strip()
        if len(t.split())>=4: occ[t].add(p)
def count_files(t): return sum(1 for s in files.values() if t in s)
A=sorted((t,next(iter(ps))) for t,ps in occ.items() if len(ps)==1)
random.seed(7); random.shuffle(A)
out=[]
for t,p in A:
    if count_files(t)==1: out.append((t,p))
    if len(out)==100: break
A=out
fn=re.compile(r'\bfn ([a-z][a-z0-9]*(?:_[a-z0-9]+){2,})\s*[<(]')
defs=collections.defaultdict(set)
for p,s in files.items():
    for m in fn.finditer(s): defs[m.group(1)].add(p)
B=[(n,next(iter(ps))) for n,ps in defs.items() if len(ps)==1 and not n.startswith('test')]
B.sort(); random.seed(11); random.shuffle(B); B=B[:100]
qs=[{"set":"A_exact_string","q":t,"gt":p} for t,p in A]
qs+=[{"set":"B_words_of_symbol","q":n.replace('_',' '),"gt":p} for n,p in B]
qs+=[{"set":"C_identifier","q":n,"gt":p} for n,p in B]
json.dump(qs,open('questions.json','w'),indent=0)
print(collections.Counter(q['set'] for q in qs)); print(qs[0],qs[150],qs[250],sep='\n')
