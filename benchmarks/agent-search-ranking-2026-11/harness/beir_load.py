import json, sys, time, urllib.request
URL, CORPUS, IDX = sys.argv[1:4]


def req(m, p, b=None, ct="application/json"):
    d = b if isinstance(b, (bytes, type(None))) else json.dumps(b).encode()
    r = urllib.request.Request(URL + p, data=d, method=m, headers={"content-type": ct})
    try:
        return json.loads(urllib.request.urlopen(r, timeout=600).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:400]}


print(req("PUT", "/" + IDX, {"mappings": {"properties": {"title": {"type": "text"}, "text": {"type": "text"}}}}))
docs = [json.loads(l) for l in open(CORPUS)]
for i in range(0, len(docs), 500):
    lines = []
    for d in docs[i:i + 500]:
        lines.append(json.dumps({"index": {"_index": IDX, "_id": d["_id"]}}))
        lines.append(json.dumps({"title": d["title"], "text": d["text"]}))
    body = ("\n".join(lines) + "\n").encode()
    for _ in range(60):
        r = req("POST", "/_bulk", body, "application/x-ndjson")
        if r.get("_err") != 429 and not (r.get("errors") and any(
                next(iter(it.values())).get("status") == 429 for it in r.get("items", []))):
            break
        time.sleep(5)
    if r.get("errors") or "_err" in r:
        print("bulk problem", str(r)[:300]); sys.exit(1)
print(req("POST", "/" + IDX + "/_refresh"), req("GET", "/" + IDX + "/_count"))
