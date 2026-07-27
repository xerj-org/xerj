#!/usr/bin/env python3
"""Check-vs-use IDOR detector. The cap-presence sweep proves a capability is
CHECKED; this proves it is checked against the SAME object the operation ACTS on.
The bug: permission_check resolves object from request key X, but the operation
reads/acts on request key Y (X != Y) -> the check validated a different object
than the one touched (the WP 4.7.0 id-type-juggle class)."""
import json, urllib.request, re
X="http://127.0.0.1:9200"
def q(body):
    r=urllib.request.Request(f"{X}/wpaudit/_search",data=json.dumps(body).encode(),
        headers={"Content-Type":"application/json"})
    return [h["_source"] for h in json.loads(urllib.request.urlopen(r).read())["hits"]["hits"]]

# object-identifying request keys (things that name WHICH object to act on)
ID_KEYS = {"id","parent","user_id","comment","comment_id","post","post_id","uuid",
           "attachment_id","term","term_id","menu","sidebar","stylesheet","slug",
           "force","autosave"}  # force/autosave excluded below; kept minimal
ID_KEYS -= {"force","autosave","slug"}

REQKEY = re.compile(r"""\$request\s*\[\s*['"](\w+)['"]\s*\]|get_param\(\s*['"](\w+)['"]|\$_(?:GET|POST|REQUEST)\s*\[\s*['"](\w+)['"]""")
def req_keys(code):
    ks=set()
    for m in REQKEY.finditer(code):
        ks.add(next(g for g in m.groups() if g))
    return ks
def obj_keys(code): return req_keys(code) & ID_KEYS

# all REST endpoint methods
R=q({"query":{"prefix":{"file":"wp-includes/rest-api/endpoints"}},"size":2000,
     "_source":["func","file","code"]})
byfile={}
for d in R: byfile[(d["file"],d["func"])]=d
files=sorted({d["file"] for d in R})
print(f"REST controllers: {len(files)}")

PAIRS=[("get_item","get_item_permissions_check"),
       ("update_item","update_item_permissions_check"),
       ("delete_item","delete_item_permissions_check"),
       ("create_item","create_item_permissions_check")]
flags=[]
for f in files:
    for op,chk in PAIRS:
        do=byfile.get((f,op)); dc=byfile.get((f,chk))
        if not do or not dc: continue
        ck=obj_keys(dc["code"]); uk=obj_keys(do["code"])
        # object-id keys the OPERATION reads that the CHECK never bound to
        gap = uk - ck
        if gap:
            flags.append((f.split('/')[-1],op,sorted(ck),sorted(uk),sorted(gap)))

print(f"\ncheck-vs-use candidates (operation reads an object-id key the check didn't): {len(flags)}")
for fn,op,ck,uk,gap in flags:
    print(f"  {fn}::{op}")
    print(f"      check binds keys: {ck}   op uses keys: {uk}   GAP: {gap}")

# also: AJAX handlers that check_ajax_referer on one id but act on another
print("\n--- also scanning self-contained AJAX handlers for id mismatch ---")
A=q({"query":{"prefix":{"func":"wp_ajax_"}},"size":300,"_source":["func","file","code"]})
for d in A:
    c=d["code"]
    # nonce action bound to an id vs a state/get on a different id
    nonce_ids=set(re.findall(r"check_ajax_referer\(\s*[\"']?[^\"',]*\$(\w+)",c))
    used_ids=set(re.findall(r"\$_(?:POST|GET|REQUEST)\[\s*['\"](\w+)['\"]",c)) & ID_KEYS
    if nonce_ids and used_ids and not (nonce_ids & used_ids):
        print(f"  {d['func']}: nonce bound to {nonce_ids} but acts on {used_ids}")
