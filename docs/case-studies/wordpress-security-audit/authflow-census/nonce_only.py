import json, urllib.request
BASE = "http://127.0.0.1:9200/wp-wc-authflow/_search"
def search(body, size=None):
    url = BASE + ("?size=%d" % size if size is not None else "")
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))

# Methods that verify a NONCE (check_ajax_referer / wp_verify_nonce / check_admin_referer)
# but have NO capability check (current_user_can / user_can / cap_admin_level)
# and hit a dangerous sink. That is the classic WP privilege-escalation signature:
# "nonce != authorization".
q = {"query": {"bool": {
        "filter": [{"term": {"kind": "method"}},
                   {"term": {"has_danger": 1}}],
        "should": [{"term": {"auth_checks": "check_ajax_referer"}},
                   {"term": {"auth_checks": "wp_verify_nonce"}},
                   {"term": {"auth_checks": "check_admin_referer"}}],
        "minimum_should_match": 1,
        "must_not": [{"term": {"auth_checks": "current_user_can"}},
                     {"term": {"auth_checks": "user_can"}},
                     {"term": {"auth_checks": "cap_admin_level"}}]}},
     "_source": ["class", "method", "file", "line", "auth_checks", "danger_sinks", "reads_super"]}
d = search(q, size=200)
hits = d["hits"]["hits"]
print("=== nonce-gated but NO capability check, has danger sink : %d ===" % d["hits"]["total"]["value"])
# rank: privesc/rce sinks first
PRI = {"user_create_update":9,"role_write":9,"auth_state":9,"blog_role":9,"exec":9,"eval":9,
       "include_var":7,"file_write":7,"file_delete":7,"file_upload":6,"user_meta_write":6,
       "option_write":5,"sql_interp_candidate":5,"dynamic_call":5,"unserialize":6,"remote_fetch_var":4}
def score(s): return max([PRI.get(x,1) for x in s.get("danger_sinks",[])] or [0])
for h in sorted(hits, key=lambda h: -score(h["_source"])):
    s = h["_source"]
    cls = (s.get("class") + "::") if s.get("class") else ""
    print("  [%d] %s%s  %s:%s\n        nonce=%s danger=%s super=%s" % (
        score(s), cls, s["method"], s["file"], s["line"],
        ",".join(s.get("auth_checks",[])), ",".join(s.get("danger_sinks",[])),
        ",".join(s.get("reads_super",[]))))
