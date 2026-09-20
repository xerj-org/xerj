#!/usr/bin/env python3
"""Live security check of share links, against a real node with auth ON.

    XERJ_URL=http://127.0.0.1:9510 XERJ_DATA_DIR=/path/to/data \
        python3 share_security_live.py

The node must be a THROWAWAY: this test creates indices, aliases and a brain,
and burns the claim rate-limit windows. Never point it at a node you use.
`run_share_live.sh` beside this file boots one and runs this against it.

What it proves, in order:
  1. management is superuser-only; secrets are shown once and never stored;
  2. a guest key reads the shared index — and nothing else on the node, by any
     route: cluster surface, other indices, aliases, body-named indices,
     cross-index lookups inside a query, server-side contexts, the console
     API, the native router, another brain, writes of any kind;
  3. an alias is resolved when the share is made, not when it is used;
  4. nothing under /_share — and nothing a guest reads — is cacheable; the
     guest page ships a strict CSP;
  5. the claim limiter: per-share lockout, junk traffic cannot lock a real
     guest out (the tunnel case), and a spoofed X-Forwarded-For buys nothing;
  6. the share id is never part of a request path: a claim is
     `POST /_share/claim` with `{id, passcode}`, and with XERJ_SERVER_LOG set
     (a node started with `logging.access_log = true`) no share id this test
     created is anywhere in the node's log;
  7. a guest's refused attempts to list, create or revoke a share, or to mint
     a key, are in the audit log.

Exit status is the number of failed checks (0 = all passed).
"""
import json
import os
import sys
import urllib.error
import urllib.request

URL = os.environ.get("XERJ_URL", "http://127.0.0.1:9510").rstrip("/")
DATA = os.environ.get("XERJ_DATA_DIR") or os.path.join(
    os.environ.get("XERJ_DATA_PARENT", os.path.dirname(os.path.abspath(__file__))), "data"
)
ADMIN = open(os.path.join(DATA, "admin.key")).read().strip()
# The node's native REST listener (`server.rest_port`), when the caller knows it.
NATIVE = os.environ.get("XERJ_NATIVE_URL", "").rstrip("/")
# Set when the node was started with server.trusted_proxies = ["127.0.0.1"].
TRUSTS_LOOPBACK = os.environ.get("XERJ_TRUSTS_LOOPBACK", "") == "1"
# The node's own log, when the caller started it with logging.access_log = true.
SERVER_LOG = os.environ.get("XERJ_SERVER_LOG", "")

PASSED = FAILED = 0


def call(method, path, body=None, key=None, headers=None, base=None):
    """-> (status, parsed-json-or-{'raw':…}, response headers)"""
    h = {"content-type": "application/json"}
    if key:
        h["authorization"] = "ApiKey " + key
    data = None
    if isinstance(body, bytes):
        data, h["content-type"] = body, "application/x-ndjson"
    elif body is not None:
        data = json.dumps(body).encode()
    h.update(headers or {})
    req = urllib.request.Request((base or URL) + path, data=data, method=method, headers=h)
    try:
        resp = urllib.request.urlopen(req, timeout=30)
        status, raw, rh = resp.status, resp.read(), resp.headers
    except urllib.error.HTTPError as e:
        status, raw, rh = e.code, e.read(), e.headers
    try:
        parsed = json.loads(raw or b"{}")
    except Exception:
        parsed = {"raw": raw[:300].decode(errors="replace")}
    return status, parsed, rh


def ok(name, cond, detail=""):
    global PASSED, FAILED
    if cond:
        PASSED += 1
        print("  ok  ", name)
    else:
        FAILED += 1
        print("  FAIL", name, "->", str(detail)[:400])


def section(title):
    print(f"\n── {title}")


def claim(share_id, passcode, **kw):
    """The guest's one call. The id is in the BODY — never in the path."""
    return call("POST", "/_share/claim", {"id": share_id, "passcode": passcode}, **kw)


def audit_lines():
    return [json.loads(l) for l in open(os.path.join(DATA, "audit.jsonl")) if l.strip()]


def hits_outside(resp, allowed):
    """Index names in a search response that are not in `allowed`."""
    out = []
    for h in (resp.get("hits") or {}).get("hits") or []:
        if h.get("_index") not in allowed:
            out.append(h.get("_index"))
    return out


SECRET = "nobody else should ever read this"


def nd(*lines):
    """NDJSON body."""
    return ("\n".join(json.dumps(l) for l in lines) + "\n").encode()


def leaks(resp):
    """Does any part of a response carry the private document's text?"""
    return SECRET in json.dumps(resp)


# ─────────────────────────────────────────────────────────────────────────────
section("seed: one shared index, one private index, one brain, aliases")
bulk = (
    b'{"index":{"_index":"casefile","_id":"1"}}\n'
    b'{"subject":"Lease dispute","body":"the landlord refused to return the deposit"}\n'
    b'{"index":{"_index":"casefile","_id":"2"}}\n'
    b'{"subject":"Inspection report","body":"mould in the bathroom, photographed in March"}\n'
    b'{"index":{"_index":"private-diary","_id":"1"}}\n'
    b'{"subject":"diary","body":"' + SECRET.encode() + b'","tag":"deposit"}\n'
)
s, r, _ = call("POST", "/_bulk", bulk, ADMIN)
ok("seed documents indexed", s == 200 and not r.get("errors"), (s, r))
call("POST", "/casefile,private-diary/_refresh", None, ADMIN)
s, r, _ = call("PUT", "/casefile-pdfs/_doc/1?refresh=true", {"subject": "Scanned lease", "body": "twelve month tenancy"}, ADMIN)
ok("second shared index seeded", s in (200, 201), (s, r))
s, r, _ = call("POST", "/_graph/case/link", {"src": "casefile/1", "dst": "casefile-pdfs/1", "type": "mentions"}, ADMIN)
ok("brain 'case' has an edge", s in (200, 201), (s, r))
# What `xerj brain <folder>` records for a folder with more than one dataset:
# every index it built, comma-joined, in the brain's meta document.
s, r, _ = call("PUT", "/.xerj-memory-case-edges/_doc/__xerj-brain-meta?refresh=true",
               {"meta_version": 1, "brain": "case", "nodes_index": "casefile,casefile-pdfs", "created_at": 1}, ADMIN)
ok("brain 'case' spans two indices", s in (200, 201), (s, r))
s, r, _ = call("POST", "/_graph/other/link", {"src": "private-diary/1", "dst": "x/9", "type": "mentions"}, ADMIN)
ok("brain 'other' has an edge", s in (200, 201), (s, r))
s, r, _ = call("POST", "/_aliases", {"actions": [
    {"add": {"index": "private-diary", "alias": "looks-harmless"}},
    {"add": {"index": "casefile", "alias": "both"}},
    {"add": {"index": "private-diary", "alias": "both"}},
    {"add": {"index": "casefile", "alias": "case-alias"}},
]}, ADMIN)
ok("aliases created", s == 200, (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("management is superuser-only")
want = {"index": ["casefile", "casefile-pdfs"], "brain": "case", "expires_in": "1h", "max_claims": 1, "label": "my lawyer"}
SHARED = {"casefile", "casefile-pdfs", ".xerj-memory-case-edges"}  # everything the guest holds
s, r, _ = call("POST", "/_share", want)
ok("create without a key is refused", s in (401, 403), (s, r))
s, r, h = call("POST", "/_share", want, ADMIN)
ok("owner creates a share", s == 200 and r.get("share_id") and r.get("passcode"), (s, r))
ok("create response is never cached", "no-store" in (h.get("cache-control") or ""), h.get("cache-control"))
SID, PW, HANDLE = r.get("share_id"), r.get("passcode"), r.get("handle")
ok("link carries the id in the URL fragment, which browsers never send",
   r.get("url_path") == f"/_xerj-console/share#{SID}", r.get("url_path"))
for bad, why in [("case*", "wildcard"), ("casefile,private-*", "pattern in a list"),
                 (".xerj_audit", "audit log"), (".xerj_sessions", "console sessions"),
                 (".xerj-memory-other-edges", "another brain's edges"), ("_all", "_all")]:
    s, r, _ = call("POST", "/_share", {"index": bad}, ADMIN)
    ok(f"cannot share {why} ({bad})", s == 400, (s, r))
s, r, _ = call("POST", "/_share", {"index": "no-such-index"}, ADMIN)
ok("cannot share an index that does not exist", s == 404, (s, r))
# The catalog lists every corpus on the node. The docs say it is never granted;
# it has to exist here for the refusal to be about WHAT it is.
s, r, _ = call("PUT", "/autoindex-catalog/_doc/x?refresh=true", {"index_name": "private-diary", "file_count": 1}, ADMIN)
ok("autoindex-catalog seeded", s in (200, 201), (s, r))
call("POST", "/_aliases", {"actions": [{"add": {"index": "autoindex-catalog", "alias": "whats-here"}}]}, ADMIN)
for bad, why in [("autoindex-catalog", "by name"), (["casefile", "autoindex-catalog"], "in a list"),
                 ("whats-here", "through an alias")]:
    s, r, _ = call("POST", "/_share", {"index": bad}, ADMIN)
    ok(f"cannot share the autoindex catalog {why}", s == 400 and "every corpus" in json.dumps(r), (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("claiming")
s, r, h = claim(SID, "wrong-code")
ok("wrong passcode refused", s == 401, (s, r))
ok("a refusal is never cached either", "no-store" in (h.get("cache-control") or ""), h.get("cache-control"))
s, r, _ = claim("0" * 32, PW)
ok("unknown share id refused without detail", s == 404 and PW not in json.dumps(r), (s, r))
s, r, _ = claim(HANDLE, PW)
ok("the public handle is not a claim id", s == 404, (s, r))
s, r, _ = call("POST", "/_share/claim", {"passcode": PW})
ok("a claim that names no id is an unknown link", s == 404, (s, r))
s, r, _ = call("GET", "/_share/claim")
ok("claim is POST-only (GET is not the open door)", s in (401, 403, 405), (s, r))
# The retired shape put the share id in the request path — and so in every
# access log on the way. It must not be a second way in. (A made-up id: the
# smoke script greps the node's log for real ones.)
s, r, _ = call("POST", f"/_share/{'f' * 32}/claim", {"passcode": PW})
# On the full server an unrouted path is a bare 404 from the router's fallback
# (before authentication); the ES router on its own answers 401. Either way no
# handler ran and no key was minted.
ok("POST /_share/{id}/claim — the id-in-path shape — is not an open door", s in (401, 404) and "api_key" not in r, (s, r))
s, r, _ = call("POST", "/_share/claim", {"id": SID, "passcode": PW, "pad": "x" * 16384})
ok("a claim body past the route's few-KiB cap is not read", s != 200 and "api_key" not in r, (s, r))
s, g, h = claim(SID, PW)
ok("right passcode yields a key", s == 200 and g.get("api_key"), (s, g))
ok("claim response is never cached",
   "no-store" in (h.get("cache-control") or "") and (h.get("pragma") or "") == "no-cache",
   dict(h))
ok("claim response is the guest-page contract",
   all(k in g for k in ("api_key", "index", "brain", "expires_at", "label"))
   and g["index"] == "casefile,casefile-pdfs" and g["brain"] == "case" and g["label"] == "my lawyer", g)
GK = g.get("api_key")
s, r, _ = claim(SID, PW)
ok("max_claims=1: second claim refused", s == 410, (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("what the guest CAN do")
s, r, _ = call("POST", "/casefile/_search", {"query": {"match": {"body": "deposit"}}}, GK)
ok("search the shared index", s == 200 and r["hits"]["total"]["value"] == 1, (s, r))
s, r, _ = call("POST", "/casefile/_search",
               {"query": {"multi_match": {"query": "deposit", "fields": ["subject^2", "body"]}},
                "highlight": {"pre_tags": ["\uE000"], "post_tags": ["\uE001"], "fields": {"subject": {}, "body": {}}}}, GK)
ok("highlighted snippets with the page's private-use delimiters",
   s == 200 and "\uE000" in json.dumps(r, ensure_ascii=False), (s, r))
s, r, h = call("GET", "/casefile/_doc/1", None, GK)
ok("read one document", s == 200 and r.get("found"), (s, r))
ok("…and what a guest reads is never cached", "no-store" in (h.get("cache-control") or ""), h.get("cache-control"))
s, r, _ = call("HEAD", "/casefile/_doc/1", None, GK)
ok("HEAD one document", s == 200, (s, r))
s, r, _ = call("POST", "/casefile/_field_caps?fields=*", None, GK)
ok("_field_caps on the shared index", s == 200, (s, r))
# `GET _source/{id}` used to be in the docs' "a guest can" table. The node has
# no such route — 404 for the owner — and a guest is not pre-approved for it.
s, r, _ = call("GET", "/casefile/_source/1", None, ADMIN)
ok("there is no /{index}/_source/{id} route (owner: 404)", s == 404, (s, r))
s, r, _ = call("GET", "/casefile/_source/1", None, GK)
# 404 from the full server's fallback, 403 where authz sees it first; the
# allow-list itself is pinned by the authz unit tests (`GUEST_DOC_OPS`).
ok("…and a guest gets nothing from it either", s in (403, 404) and not r.get("_source"), (s, r))
# The last "a guest can" row: who-am-I, the banner, the probes. None names an index.
for path in ("/", "/_security/_authenticate", "/health/ready", "/health/live"):
    s, r, _ = call("GET", path, None, GK)
    ok(f"GET {path} answers a guest", s == 200 and not leaks(r), (s, r))
s, r, _ = call("GET", "/_security/_authenticate", None, GK)
ok("…and who-am-I shows the guest role and nothing wider",
   s == 200 and r.get("roles") == [f"share:{HANDLE}"], (s, r))
s, r, _ = call("GET", "/casefile/_mapping", None, GK)
ok("read the shared index's mapping", s == 200 and "casefile" in r, (s, r))
s, r, _ = call("POST", "/casefile/_count", {"query": {"match_all": {}}}, GK)
ok("count", s == 200 and r.get("count") == 2, (s, r))
s, r, _ = call("POST", "/casefile/_mget", {"ids": ["1", "2"]}, GK)
ok("_mget inside the shared index", s == 200 and len(r.get("docs", [])) == 2, (s, r))
s, r, _ = call("GET", "/_graph/case/overview", None, GK)
ok("walk the shared brain (overview)", s == 200, (s, r))
ok("…and the overview counts notes across both shared indices", r.get("nodes", {}).get("total") == 3, r)
s, r, _ = call("GET", "/_graph/case/ego?node=casefile/1", None, GK)
ok("walk the shared brain (ego)", s == 200 and len(r.get("edges", [])) == 1, (s, r))
s, r, _ = call("GET", "/_graph/case/ego?node=casefile/1&include_nodes=true", None, GK)
ok("…with the linked notes hydrated from both shared indices", s == 200 and not leaks(r), (s, r))
s, r, _ = call("GET", "/_graph/case/ego?node=casefile/1&include_nodes=true&nodes_index=private-diary", None, GK)
ok("…but nodes_index cannot redirect hydration at a private index", s == 403 and not leaks(r), (s, r))
s, r, _ = call("GET", "/_graph/case/ego?node=casefile/1&include_nodes=true&nodes_index=casefile,private-diary", None, GK)
ok("…not even as one name in a list", s == 403 and not leaks(r), (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("what the guest CANNOT reach: the cluster surface")
for method, path in [
    ("GET", "/_cat/indices?format=json"), ("GET", "/_cat/aliases?format=json"),
    ("GET", "/_cat/shards"), ("GET", "/_cat/count"), ("GET", "/_cat/nodes"),
    ("GET", "/_cluster/health"), ("GET", "/_cluster/state"), ("GET", "/_cluster/stats"),
    ("GET", "/_cluster/settings"), ("GET", "/_nodes"), ("GET", "/_nodes/stats"),
    ("GET", "/_snapshot"), ("GET", "/_snapshot/_all"), ("GET", "/_tasks"),
    ("GET", "/_stats"), ("GET", "/_mapping"), ("GET", "/_aliases"), ("GET", "/_alias"),
    ("GET", "/_resolve/index/*"),
    ("GET", "/_index_template"), ("GET", "/_ingest/pipeline"),
    ("GET", "/_security/api_key"), ("GET", "/_audit/_search"), ("GET", "/_audit/_verify"),
    ("GET", "/v1/metrics"), ("GET", "/v1/indices"), ("GET", "/v1/indices/casefile"),
    ("GET", "/_xerj/wal_tap"),
]:
    s, r, _ = call(method, path, None, GK)
    ok(f"{method} {path} → refused", s in (403, 404, 405) and "private-diary" not in json.dumps(r), (s, r))
# A pattern is not refused: it is expanded over what the key HOLDS, so `_all`
# means "all of yours". What matters is that it never means more than that.
for path in ("/_all/_mapping", "/*/_mapping", "/case*,private*/_mapping"):
    s, r, _ = call("GET", path, None, GK)
    ok(f"GET {path} shows only what the guest holds", s == 200 and set(r) <= SHARED and "private-diary" not in r, (s, sorted(r)))
s, r, _ = call("PUT", "/_snapshot/exfil", {"type": "fs", "settings": {"location": "/tmp/exfil"}}, GK)
ok("cannot register a snapshot repository", s == 403, (s, r))
s, r, _ = call("POST", "/_security/api_key", {"name": "upgrade-myself"}, GK)
ok("cannot mint itself a wider key", s == 403 and "api_key" not in r, (s, r))

section("what the guest CANNOT reach: share management")
for method, path, body in [("GET", "/_share", None), ("POST", "/_share", {"index": "casefile"}),
                           ("DELETE", f"/_share/{HANDLE}", None)]:
    s, r, _ = call(method, path, body, GK)
    ok(f"{method} {path.split('/')[1]} → 403", s == 403, (s, r))

section("what the guest CANNOT reach: other indices")
for method, path, body in [
    ("POST", "/private-diary/_search", {"query": {"match_all": {}}}),
    ("GET", "/private-diary/_doc/1", None), ("GET", "/private-diary/_source/1", None),
    ("POST", "/private-diary/_count", None), ("GET", "/private-diary/_mapping", None),
    ("POST", "/private-diary/_field_caps?fields=*", None),
    ("POST", "/casefile,private-diary/_search", {"query": {"match_all": {}}}),
    ("POST", "/_search", {"query": {"match_all": {}}}),
    ("POST", "/_all/_search", {"query": {"match_all": {}}}),
    ("POST", "/*/_search", {"query": {"match_all": {}}}),
    ("POST", "/private-*/_search", {"query": {"match_all": {}}}),
    ("POST", "/.xerj-memory-other-edges/_search", {"query": {"match_all": {}}}),
    ("POST", "/.xerj_audit/_search", {"query": {"match_all": {}}}),
    ("POST", "/autoindex-catalog/_search", {"query": {"match_all": {}}}),
]:
    s, r, _ = call(method, path, body, GK)
    ok(f"{method} {path} leaks nothing", not leaks(r) and not hits_outside(r, SHARED), (s, r))
s, r, _ = call("POST", "/private-diary/_search", {"query": {"match_all": {}}}, GK)
ok("a named private index is a plain 403", s == 403, (s, r))

section("what the guest CANNOT reach: aliases pointing elsewhere")
for alias in ("looks-harmless", "both"):
    s, r, _ = call("POST", f"/{alias}/_search", {"query": {"match_all": {}}}, GK)
    ok(f"alias `{alias}` (→ private-diary) is refused", s == 403 and not leaks(r), (s, r))
    s, r, _ = call("GET", f"/{alias}/_doc/1", None, GK)
    ok(f"alias `{alias}` cannot fetch a document", s in (403, 404) and not leaks(r), (s, r))
s, r, _ = call("POST", "/case-alias/_search", {"query": {"match_all": {}}}, GK)
ok("an alias onto the shared index itself still works", s == 200 and not hits_outside(r, SHARED), (s, r))
s, r, _ = call("POST", "/_aliases", {"actions": [{"add": {"index": "private-diary", "alias": "casefile-2"}}]}, GK)
ok("cannot create an alias", s == 403, (s, r))
s, r, _ = call("PUT", "/private-diary/_alias/mine", None, GK)
ok("cannot create an alias (index route)", s == 403, (s, r))

section("what the guest CANNOT reach: indices named in a body")
s, r, _ = call("POST", "/casefile/_msearch", nd({"index": "private-diary"}, {"query": {"match_all": {}}}), GK)
ok("_msearch header naming another index", not leaks(r) and s in (200, 403), (s, r))
s, r, _ = call("POST", "/casefile/_msearch", nd({"index": ["casefile", "private-diary"]}, {"query": {"match_all": {}}}), GK)
ok("_msearch header naming a list", not leaks(r), (s, r))
s, r, _ = call("POST", "/casefile/_msearch", nd({"index": "looks-harmless"}, {"query": {"match_all": {}}}), GK)
ok("_msearch header naming an alias elsewhere", not leaks(r), (s, r))
s, r, _ = call("POST", "/_msearch", nd({"index": "private-diary"}, {"query": {"match_all": {}}}), GK)
ok("global _msearch", s == 403 and not leaks(r), (s, r))
s, r, _ = call("POST", "/casefile/_mget", {"docs": [{"_index": "private-diary", "_id": "1"}]}, GK)
ok("_mget naming another index", not leaks(r) and s in (200, 403), (s, r))
s, r, _ = call("POST", "/_mget", {"docs": [{"_index": "private-diary", "_id": "1"}]}, GK)
ok("global _mget", s == 403 and not leaks(r), (s, r))
s, r, _ = call("POST", "/casefile/_msearch", nd({}, {"query": {"match_all": {}}}), GK)
ok("_msearch inside the shared index still works", s == 200 and not leaks(r), (s, r))

section("what the guest CANNOT reach: another index from INSIDE a permitted query")
for name, query in [
    ("terms lookup", {"terms": {"body": {"index": "private-diary", "id": "1", "path": "tag"}}}),
    ("more_like_this like-doc", {"more_like_this": {"fields": ["body"], "like": [{"_index": "private-diary", "_id": "1"}],
                                                    "min_term_freq": 1, "min_doc_freq": 1}}),
    ("percolate stored doc", {"percolate": {"field": "q", "index": "private-diary", "id": "1"}}),
    ("geo_shape indexed_shape", {"geo_shape": {"loc": {"indexed_shape": {"index": "private-diary", "id": "1", "path": "body"}}}}),
    ("bool-wrapped terms lookup", {"bool": {"filter": [{"terms": {"body": {"index": "private-diary", "id": "1", "path": "body"}}}]}}),
]:
    s, r, _ = call("POST", "/casefile/_search", {"query": query}, GK)
    ok(f"{name} cannot read private-diary", not leaks(r) and not hits_outside(r, SHARED), (s, r))
    # The probe that matters: a terms lookup on `tag` = "deposit" would match
    # casefile/1 if — and only if — the node fetched the private document.
    if name == "terms lookup":
        ok("terms lookup did not silently resolve the private document",
           s == 403 or r.get("hits", {}).get("total", {}).get("value", 0) == 0, (s, r))
s, r, _ = call("POST", "/casefile/_search",
               {"query": {"match_all": {}}, "runtime_mappings": {"x": {"type": "lookup", "target_index": "private-diary",
                "input_field": "subject", "target_field": "subject", "fetch_fields": ["body"]}}, "fields": ["x"]}, GK)
ok("lookup runtime field cannot join private-diary", not leaks(r), (s, r))

section("what the guest CANNOT reach: server-side contexts")
for method, path, body in [
    ("POST", "/casefile/_search?scroll=1m", {"query": {"match_all": {}}}),
    ("POST", "/casefile/_search?%73croll=1m", {"query": {"match_all": {}}}),
    ("POST", "/casefile/_search?size=1&SCROLL=1m", {"query": {"match_all": {}}}),
    ("POST", "/_search/scroll", {"scroll": "1m", "scroll_id": "x"}),
    ("DELETE", "/_search/scroll", {"scroll_id": "x"}),
    ("POST", "/casefile/_pit?keep_alive=1m", None),
    ("DELETE", "/_pit", {"id": "x"}),
    ("POST", "/casefile/_async_search", {"query": {"match_all": {}}}),
    ("POST", "/casefile/_search/template", {"source": {"query": {"match_all": {}}}}),
    ("POST", "/_sql", {"query": "SELECT * FROM \"private-diary\""}),
    ("POST", "/casefile/_sql", {"query": "SELECT * FROM \"private-diary\""}),
    ("POST", "/_esql/query", {"query": "FROM private-diary"}),
]:
    s, r, _ = call(method, path, body, GK)
    # 404 = this build has no such route at all (`_esql`, index-scoped `_sql`).
    ok(f"{method} {path} → refused", s in (403, 404) and not leaks(r) and "_scroll_id" not in r and "id" not in r, (s, r))
# Someone else's server-side context. A PIT — not the path — decides which index
# a search runs against, so the owner's PIT on private-diary is the real probe.
s, r, _ = call("POST", "/private-diary/_pit?keep_alive=5m", None, ADMIN)
PIT = r.get("id")
ok("(owner opens a PIT on the private index)", s == 200 and PIT, (s, r))
s, r, _ = call("POST", "/casefile/_search", {"query": {"match_all": {}}, "pit": {"id": PIT}}, ADMIN)
ok("(and for the owner that PIT really does override the path index)", s == 200 and leaks(r), (s, r))
s, r, _ = call("POST", "/casefile/_search", {"query": {"match_all": {}}, "pit": {"id": PIT, "keep_alive": "1m"}}, GK)
ok("a guest cannot ride it via _search", s == 403 and not leaks(r) and "private-diary" not in json.dumps(r), (s, r))
s, r, _ = call("POST", "/casefile/_msearch", nd({}, {"query": {"match_all": {}}, "pit": {"id": PIT}}), GK)
ok("…nor via an _msearch body line", s == 403 and not leaks(r) and "private-diary" not in json.dumps(r), (s, r))
s, r, _ = call("POST", "/private-diary/_search?scroll=5m", {"size": 1, "query": {"match_all": {}}}, ADMIN)
SCROLL = r.get("_scroll_id")
s, r, _ = call("POST", "/_search/scroll", {"scroll": "1m", "scroll_id": SCROLL}, GK)
ok("a guest cannot continue the owner's scroll", s == 403 and not leaks(r), (s, r))

section("what the guest CANNOT reach: writes and index administration")
for method, path, body in [
    ("PUT", "/casefile/_doc/99", {"body": "tampered"}), ("POST", "/casefile/_doc", {"body": "tampered"}),
    ("POST", "/casefile/_update/1", {"doc": {"body": "tampered"}}), ("DELETE", "/casefile/_doc/1", None),
    ("POST", "/casefile/_bulk", nd({"index": {"_id": "77"}}, {"body": "tampered"})),
    ("POST", "/_bulk", nd({"index": {"_index": "casefile", "_id": "77"}}, {"body": "tampered"})),
    ("POST", "/casefile/_delete_by_query", {"query": {"match_all": {}}}),
    ("POST", "/casefile/_update_by_query", {"query": {"match_all": {}}}),
    ("POST", "/_reindex", {"source": {"index": "private-diary"}, "dest": {"index": "casefile"}}),
    ("POST", "/_reindex", {"source": {"index": "casefile"}, "dest": {"index": "mine"}}),
    ("PUT", "/casefile/_mapping", {"properties": {"x": {"type": "keyword"}}}),
    ("PUT", "/casefile/_settings", {"index": {"number_of_replicas": 0}}),
    ("POST", "/casefile/_refresh", None), ("POST", "/casefile/_flush", None),
    ("POST", "/casefile/_forcemerge", None), ("POST", "/casefile/_close", None),
    ("GET", "/casefile", None), ("GET", "/casefile/_settings", None), ("GET", "/casefile/_stats", None),
    ("GET", "/casefile/_segments", None), ("POST", "/casefile/_analyze", {"text": "x"}),
    ("PUT", "/brand-new", None), ("DELETE", "/casefile", None),
]:
    s, r, _ = call(method, path, body, GK)
    ok(f"{method} {path} → 403", s == 403, (s, r))
s, r, _ = call("GET", "/casefile/_doc/1", None, GK)
ok("the shared document is untouched", s == 200 and "tampered" not in json.dumps(r), (s, r))
s, r, _ = call("GET", "/casefile/_doc/99", None, ADMIN)
ok("no guest write landed", s == 404, (s, r))

section("what the guest CANNOT reach: another brain, memory, the console API")
for method, path, body in [
    ("GET", "/_graph/other/overview", None), ("GET", "/_graph/other/ego?node=private-diary/1", None),
    ("POST", "/_graph/case/link", {"src": "a", "dst": "b", "type": "t"}),
    ("POST", "/_graph/other/link", {"src": "a", "dst": "b", "type": "t"}),
    ("DELETE", "/_graph/case/link/e1", None),
    ("GET", "/_memory/case", None), ("POST", "/_memory/case/_recall", {"query": "x"}),
    ("POST", "/_memory/other/_recall", {"query": "x"}),
]:
    s, r, _ = call(method, path, body, GK)
    ok(f"{method} {path} → 403", s == 403 and not leaks(r), (s, r))
for method, path, body in [
    ("GET", "/_xerj-console/api/v1/me", None), ("GET", "/_xerj-console/api/v1/prefs", None),
    ("GET", "/_xerj-console/api/v1/dashboards", None), ("GET", "/_xerj-console/api/v1/auth/api-tokens", None),
    ("GET", "/_xerj-console/api/v1/data-sources/connections", None),
    ("POST", "/_xerj-console/api/v1/data-sources/connections/built-in/indices/private-diary/search",
     {"query": {"match_all": {}}}),
    ("POST", "/_xerj-console/api/v1/auth/magic/issue", {}),
]:
    s, r, _ = call(method, path, body, GK)
    ok(f"console API {path.rsplit('/v1/', 1)[1]} does not accept a guest key", s in (401, 403) and not leaks(r), (s, r))

if NATIVE:
    section("what the guest CANNOT reach: the native REST listener")
    # (`/v1/indices` is POST-only — create; the native listing is `GET /v1/indices/{name}`.)
    s, r, _ = call("GET", "/v1/indices/casefile", None, ADMIN, base=NATIVE)
    ok("(the owner can describe the shared index on the native port)", s == 200, (s, r))
    for method, path, body in [
        ("GET", "/v1/indices", None), ("GET", "/v1/indices/casefile", None),
        ("POST", "/v1/indices/casefile/search", {"query": {"match_all": {}}}),
        ("POST", "/v1/indices/private-diary/search", {"query": {"match_all": {}}}),
        ("POST", "/v1/indices/casefile/docs", {"body": "tampered"}),
        ("GET", "/v1/metrics", None), ("GET", "/_cat/indices", None),
        ("POST", "/private-diary/_search", {"query": {"match_all": {}}}),
    ]:
        s, r, _ = call(method, path, body, GK, base=NATIVE)
        ok(f"native port: {method} {path} → refused", s in (401, 403, 404, 405) and not leaks(r), (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("an alias is resolved when the share is made")
s, r, _ = call("POST", "/_share", {"index": "looks-harmless", "max_claims": 1}, ADMIN)
ok("sharing an alias records the concrete index it points at",
   s == 200 and r.get("indices") == ["private-diary"], (s, r))
asid, apw, ahandle = r.get("share_id"), r.get("passcode"), r.get("handle")
s, ag, _ = claim(asid, apw)
AK = ag.get("api_key")
s, r, _ = call("POST", "/private-diary/_search", {"query": {"match_all": {}}}, AK)
ok("that guest reads what the owner chose to share", s == 200 and r["hits"]["total"]["value"] == 1, (s, r))
call("POST", "/_aliases", {"actions": [{"remove": {"index": "private-diary", "alias": "looks-harmless"}},
                                        {"add": {"index": "casefile", "alias": "looks-harmless"}}]}, ADMIN)
s, r, _ = call("POST", "/casefile/_search", {"query": {"match_all": {}}}, AK)
ok("re-pointing the alias afterwards does not move the guest", s == 403, (s, r))
# By the handle, as the CLI does: a DELETE naming the full id would put that
# id in the request line, which the log check at the end would (rightly) find.
call("DELETE", f"/_share/{ahandle}", None, ADMIN)

# ─────────────────────────────────────────────────────────────────────────────
section("at rest, listing, revoke")
disk = open(os.path.join(DATA, "shares.json")).read()
ok("shares.json holds no passcode, share id or key", PW not in disk and SID not in disk and GK not in disk)
mode = oct(os.stat(os.path.join(DATA, "shares.json")).st_mode & 0o777)
ok("shares.json is 0600", mode == "0o600", mode)
s, l, h = call("GET", "/_share", None, ADMIN)
ok("owner lists shares, no secrets in the listing",
   s == 200 and PW not in json.dumps(l) and SID not in json.dumps(l) and "hash" not in json.dumps(l), (s, l))
ok("listing is never cached", "no-store" in (h.get("cache-control") or ""))
# "Every /_share response — success or error": the 401 and 403 come from the
# middleware, not from a share handler, and used to carry no Cache-Control.
for method, path, key, want in [("GET", "/_share", None, 401), ("POST", "/_share", None, 401),
                                ("DELETE", f"/_share/{HANDLE}", None, 401), ("GET", "/_share/claim", None, 401),
                                ("GET", "/_share", GK, 403), ("POST", "/_share", GK, 403)]:
    s, r, h = call(method, path, {} if method == "POST" else None, key)
    ok(f"{method} {path} as {'a guest' if key else 'nobody'} → {want}, no-store",
       s == want and "no-store" in (h.get("cache-control") or ""), (s, h.get("cache-control")))

# A guest reaching for the share and key management surface is the escalation
# attempt an owner most wants on record. These are refused in the
# authorization middleware, so no handler ever audited them.
before = len(audit_lines())
for method, path in [("POST", "/_share"), ("GET", "/_share"), ("DELETE", f"/_share/{HANDLE}"),
                     ("POST", "/_security/api_key"), ("GET", "/_security/api_key")]:
    s, r, _ = call(method, path, {} if method == "POST" else None, GK)
    ok(f"guest {method} {path} → 403", s == 403, (s, r))
new = audit_lines()[before:]
for op, resource in [("share.create", "_share"), ("share.list", "_share"), ("share.revoke", HANDLE),
                     ("security.api_key.create", "_security/api_key"), ("security.api_key.get", "_security/api_key")]:
    found = [e for e in new if e.get("op") == op and e.get("outcome") == "denied"]
    ok(f"…and the refused {op} is in the audit log, once, against {resource}",
       len(found) == 1 and found[0].get("resource") == resource
       and found[0].get("subject") not in (None, "", "superuser", "unauthenticated"), new)
s, r, _ = call("DELETE", f"/_share/{HANDLE}", None, ADMIN)
ok("owner revokes by handle", s == 200 and r.get("keys_invalidated") == 1, (s, r))
s, r, _ = call("POST", "/casefile/_search", {"query": {"match_all": {}}}, GK)
ok("revoke kills the guest's key", s in (401, 403), (s, r))
s, r, _ = call("GET", "/_graph/case/overview", None, GK)
ok("…on the graph API too", s in (401, 403), (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("the guest page is served without a session, with a strict policy")
for path in ("/_xerj-console/share", "/_xerj-console/share/", "/_xerj-console/share/share.js",
             "/_xerj-console/share/share.css"):
    s, r, h = call("GET", path)
    csp = h.get("content-security-policy") or ""
    ok(f"GET {path} → 200 + CSP + no-referrer + nosniff + no-store + unframeable",
       s == 200 and "default-src 'none'" in csp and "frame-ancestors 'none'" in csp
       and "script-src 'self'" in csp and "unsafe" not in csp
       and h.get("referrer-policy") == "no-referrer" and h.get("x-content-type-options") == "nosniff"
       and "no-store" in (h.get("cache-control") or "") and h.get("x-frame-options") == "DENY",
       (s, dict(h)))
for path in ("/_xerj-console/share/../api/v1/me", "/_xerj-console/share/%2e%2e/index.html",
             "/_xerj-console/share/nope.js"):
    s, r, _ = call("GET", path)
    ok(f"GET {path} opens nothing", s in (400, 401, 404), (s, r))
s, r, _ = call("GET", "/_xerj-console/api/v1/me")
ok("the console API still needs a session", s == 401, (s, r))

# ─────────────────────────────────────────────────────────────────────────────
section("the claim limiter")
s, r, _ = call("POST", "/_share", {"index": "casefile", "max_claims": 5}, ADMIN)
sid2, pw2 = r["share_id"], r["passcode"]
codes = [claim(sid2, "guess")[0] for _ in range(14)]
ok("wrong-passcode hammering is locked out with 429", codes[:10] == [401] * 10 and set(codes[10:]) == {429}, codes)
s, r, h = claim(sid2, pw2)
ok("…and while locked, even the right passcode waits (with Retry-After)",
   s == 429 and int(h.get("retry-after") or 0) >= 1, (s, dict(h)))

# The tunnel case. Behind `xerj share --tunnel` every client arrives from
# 127.0.0.1 — which is also where this test runs from. Flood the junk bucket…
s, r, _ = call("POST", "/_share", {"index": "casefile", "max_claims": 2}, ADMIN)
sid3, pw3 = r["share_id"], r["passcode"]
junk = [claim(f"{i:032x}", "x", headers={"x-forwarded-for": f"198.51.100.{i % 250}"})[0] for i in range(1, 16)]
if TRUSTS_LOOPBACK:
    ok("trusted loopback proxy: each forwarded address gets its own junk bucket", 429 not in junk, junk)
else:
    first_429 = junk.index(429) if 429 in junk else len(junk)
    ok("junk ids are throttled per source (404s, then nothing but 429)",
       0 < first_429 < len(junk) and set(junk[:first_429]) == {404} and set(junk[first_429:]) == {429}, junk)
    ok("a rotating X-Forwarded-For from a direct client buys no fresh bucket", junk[-1] == 429, junk)
# …and a real guest, from the same address, is not locked out by it.
s, r, _ = claim(sid3, pw3)
ok("a junk flood from the same address does NOT lock a real guest out", s == 200 and r.get("api_key"), (s, r))

audit = open(os.path.join(DATA, "audit.jsonl")).read()
ok("every claim outcome is in the audit log",
   all(w in audit for w in ("share.claim", "wrong passcode", "unknown share", "rate-limited", "share.revoke")))
if TRUSTS_LOOPBACK:
    ok("audit records the forwarded address when loopback is a declared proxy", "guest@198.51.100." in audit)
else:
    ok("audit records the TCP peer, never a header the client wrote",
       "guest@127.0.0.1" in audit and "198.51.100." not in audit)
ok("no passcode, share id or guest key is written to the audit log",
   all(x not in audit for x in (PW, SID, GK, pw2, sid2, pw3, sid3, asid, apw)))

# The node's own log. The guest page keeps the id in the URL fragment so that
# no server sees it; the first cut then posted it as part of a request PATH,
# and with `logging.access_log = true` every claim wrote the id to server.log
# (twice). Only meaningful against a node that logs requests — so insist on it.
if SERVER_LOG:
    log = open(SERVER_LOG, errors="replace").read()
    ok("the node is logging requests (logging.access_log = true), so the next check tests something",
       "/_share/claim" in log, log[-300:])
    ok("no share id, passcode or guest key is anywhere in the node's log",
       all(x not in log for x in (SID, sid2, sid3, asid, PW, pw2, pw3, apw, GK)))

print(f"\npassed={PASSED} failed={FAILED}")
sys.exit(min(FAILED, 255))
