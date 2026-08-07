import sys, json, urllib.request

BASE = "http://127.0.0.1:9200/wp-wc-authflow/_search"

def search(body, size=None):
    url = BASE + ("?size=%d" % size if size is not None else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=20))

def agg_entry_types():
    b = {"query": {"term": {"kind": "entry"}},
         "aggs": {"t": {"terms": {"field": "entry_type", "size": 20},
                        "aggs": {"u": {"terms": {"field": "unauth"}}}}}}
    d = search(b, size=0)
    print("=== entry_type x unauth ===")
    for x in d["aggregations"]["t"]["buckets"]:
        us = {y["key"]: y["doc_count"] for y in x["u"]["buckets"]}
        print("  %-20s total=%-4d unauth=%-4d auth=%-4d" %
              (x["key"], x["doc_count"], us.get(1, 0), us.get(0, 0)))

def list_entries(entry_type, unauth=None, n=200):
    filt = [{"term": {"entry_type": entry_type}}]
    if unauth is not None:
        filt.append({"term": {"unauth": unauth}})
    d = search({"query": {"bool": {"filter": filt}}}, size=n)
    print("=== %s unauth=%s : %d hits ===" % (entry_type, unauth, d["hits"]["total"]["value"]))
    for h in d["hits"]["hits"]:
        s = h["_source"]
        line = "  %s:%s" % (s["file"], s["line"])
        if s.get("action"): line += "  action=%s" % s["action"]
        if s.get("permission_callback") is not None: line += "  perm=%s" % s["permission_callback"]
        if s.get("callback_ref"): line += "  cb=%s" % s["callback_ref"]
        print(line)

if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "agg":
        agg_entry_types()
    elif cmd == "list":
        et = sys.argv[2]
        ua = int(sys.argv[3]) if len(sys.argv) > 3 else None
        list_entries(et, ua)
