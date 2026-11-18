import subprocess, time, json, urllib.request, statistics
def q(id):
    r=urllib.request.urlopen(f"http://127.0.0.1:19821/src-claims-public-claims/_doc/{id}", timeout=5)
    return json.load(r)
lat=[]
for i in range(1,61):
    cid=f"CLM-{i:07d}"; new=f"appeal-{i}"
    t0=time.perf_counter()
    subprocess.run(["docker","exec","uc-pg","psql","-U","postgres","-d","claims","-qc",f"UPDATE claims SET status='appealed', reviewer='{new}', updated_at=now() WHERE claim_id='{cid}'"],check=True,capture_output=True)
    t1=time.perf_counter()
    while True:
        try:
            d=q(cid)
            if d.get("_source",{}).get("reviewer")==new: break
        except Exception: pass
        if time.perf_counter()-t1>10: print("timeout",cid); break
        time.sleep(0.002)
    lat.append((time.perf_counter()-t1)*1000)
lat.sort()
p=lambda x: lat[min(len(lat)-1,int(len(lat)*x))]
print(json.dumps({"n":len(lat),"p50_ms":round(p(.5),1),"p95_ms":round(p(.95),1),"max_ms":round(lat[-1],1)}))
# delete + insert
subprocess.run(["docker","exec","uc-pg","psql","-U","postgres","-d","claims","-qc","DELETE FROM claims WHERE claim_id='CLM-0200000'; INSERT INTO claims VALUES ('CLM-0200001','West','telehealth','received',412.50,'2026-11-05','A. Rivera',now())"],check=True)
time.sleep(1.5)
for cid in ["CLM-0200000","CLM-0200001"]:
    try: print(cid, q(cid).get("found"))
    except urllib.error.HTTPError as e: print(cid, "HTTP", e.code)
