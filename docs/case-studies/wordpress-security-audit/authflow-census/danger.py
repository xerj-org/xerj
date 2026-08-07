import json, urllib.request

BASE = "http://127.0.0.1:9200/wp-wc-authflow/_search"
def search(body, size=None):
    url = BASE + ("?size=%d" % size if size is not None else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))

# 1. danger-sink distribution among METHODS that read attacker input and have NO in-body auth check
base_filter = [{"term": {"kind": "method"}},
               {"term": {"reads_input": 1}},
               {"term": {"has_auth": 0}},
               {"term": {"has_danger": 1}}]
d = search({"query": {"bool": {"filter": base_filter}},
            "aggs": {"s": {"terms": {"field": "danger_sinks", "size": 40}}}}, size=0)
print("=== METHODS reads_input=1, has_auth=0, has_danger=1 : %d ===" % d["hits"]["total"]["value"])
for b in d["aggregations"]["s"]["buckets"]:
    print("  %-24s %d" % (b["key"], b["doc_count"]))

# 2. the scariest sinks, listed (privesc/rce/file), no auth, reads input
SCARY = ["exec", "eval", "include_var", "sql_interp_candidate", "file_write",
         "file_delete", "file_upload", "user_create_update", "auth_state",
         "role_write", "blog_role", "user_meta_write"]
for sink in SCARY:
    d = search({"query": {"bool": {"filter": base_filter + [{"term": {"danger_sinks": sink}}]}},
                "_source": ["class", "method", "file", "line", "danger_sinks", "reads_super"]},
               size=60)
    hits = d["hits"]["hits"]
    if not hits:
        continue
    print("\n### sink=%s : %d methods (reads_input, no in-body auth) ###" % (sink, d["hits"]["total"]["value"]))
    for h in hits[:40]:
        s = h["_source"]
        cls = (s.get("class") or "") + "::" if s.get("class") else ""
        print("  %s%s  %s:%s  super=%s" % (cls, s["method"], s["file"], s["line"], ",".join(s.get("reads_super", []))))
