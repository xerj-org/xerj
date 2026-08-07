import json, urllib.request
BASE = "http://127.0.0.1:9200/wp-wc-authflow/_search"
def search(body, size=None):
    url = BASE + ("?size=%d" % size if size is not None else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))

for sink in ["dynamic_call", "remote_fetch_var"]:
    q = {"query": {"bool": {"filter": [{"term": {"kind": "method"}},
                                       {"term": {"reads_input": 1}},
                                       {"term": {"danger_sinks": sink}}]}},
         "_source": ["class","method","file","line","auth_checks","danger_sinks","reads_super"]}
    d = search(q, size=100)
    print("\n### %s (reads_input) : %d ###" % (sink, d["hits"]["total"]["value"]))
    for h in d["hits"]["hits"]:
        s=h["_source"]; cls=(s.get("class")+"::") if s.get("class") else ""
        print("  %s%s  %s:%s auth=%s super=%s" % (cls,s["method"],s["file"],s["line"],
              ",".join(s.get("auth_checks",[])) or "-", ",".join(s.get("reads_super",[]))))
