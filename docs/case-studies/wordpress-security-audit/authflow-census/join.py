import json, re, urllib.request

BASE = "http://127.0.0.1:9200/wp-wc-authflow/_search"
def search(body, size=None):
    url = BASE + ("?size=%d" % size if size is not None else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))

def method_name_from_ref(ref):
    if not ref:
        return None
    # last quoted string, or ::method
    qs = re.findall(r"['\"]([A-Za-z_][A-Za-z0-9_]+)['\"]", ref)
    if qs:
        return qs[-1]
    m = re.search(r"::\s*([A-Za-z_]\w+)", ref)
    return m.group(1) if m else None

def lookup_method(name):
    if not name:
        return []
    d = search({"query": {"bool": {"filter": [{"term": {"kind": "method"}},
                                              {"term": {"method": name}}]}},
               "_source": ["class", "method", "file", "line", "auth_checks",
                           "danger_sinks", "reads_super", "sinks"]}, size=10)
    return [h["_source"] for h in d["hits"]["hits"]]

# pull all entries of interest
def entries(types):
    should = [{"term": {"entry_type": t}} for t in types]
    d = search({"query": {"bool": {"filter": [{"term": {"kind": "entry"}}],
                                   "should": should, "minimum_should_match": 1}}}, size=500)
    return [h["_source"] for h in d["hits"]["hits"]]

for group in (["ajax_auth"], ["ajax_nopriv"], ["adminpost_auth", "adminpost_nopriv"]):
    print("\n================= %s =================" % ",".join(group))
    for e in entries(group):
        mn = method_name_from_ref(e.get("callback_ref"))
        cands = lookup_method(mn)
        # prefer a candidate that has danger or reads input
        cand = None
        for c in cands:
            if c.get("danger_sinks") or c.get("reads_super"):
                cand = c; break
        if not cand and cands:
            cand = cands[0]
        tag = ""
        if cand:
            tag = "auth=%s danger=%s super=%s @ %s:%s" % (
                ",".join(cand.get("auth_checks", [])) or "-",
                ",".join(cand.get("danger_sinks", [])) or "-",
                ",".join(cand.get("reads_super", [])) or "-",
                cand.get("file"), cand.get("line"))
        print("  action=%-40s cb=%-30s -> %s | %s" %
              (e.get("action"), (e.get("callback_ref") or "")[:30], mn, tag))
