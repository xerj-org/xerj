#!/usr/bin/env python3
"""Enrich the substrate with an INTERPROCEDURAL AUTHZ graph so the agent can ask
the right question: is a handler's capability check BOUND TO THE OBJECT it acts
on? Per function we record the arg-shape of each cap/nonce check and whether it
performs a state change; then we propagate along call edges from each handler."""
import glob, json, os, re, sys, urllib.request
from tree_sitter import Language, Parser
import tree_sitter_php as tsphp

WP = sys.argv[1]; X = "http://127.0.0.1:9200"
parser = Parser(Language(tsphp.language_php()))

CAP_FIRSTARG = {"current_user_can", "current_user_can_for_blog", "current_user_can_for_site"}
CAP_OTHER    = {"user_can", "author_can"}          # object is a later arg; treat presence only
NONCE_FIRST  = {"check_ajax_referer", "check_admin_referer"}   # action = arg 0
NONCE_SECOND = {"wp_verify_nonce"}                              # action = arg 1
STATE = {"wp_insert_post","wp_update_post","wp_delete_post","wp_trash_post","wp_untrash_post",
         "update_option","delete_option","add_option","update_site_option","delete_site_option",
         "update_user_meta","delete_user_meta","add_user_meta","update_post_meta","delete_post_meta",
         "wp_delete_user","wp_update_user","wp_insert_user","wp_create_user","wp_set_object_terms",
         "switch_theme","activate_plugin","deactivate_plugins","wp_delete_attachment",
         "wp_update_comment","wp_delete_comment","wp_set_comment_status","wp_insert_term",
         "wp_update_term","wp_delete_term","update_metadata","delete_metadata"}

def txt(n): return n.text.decode("utf8","replace")
def callee(call):
    fn = call.child_by_field_name("function")
    if fn is None: return ""
    t = txt(fn)
    return t.split("->")[-1].split("::")[-1]

def arg_nodes(call):
    a = call.child_by_field_name("arguments")
    return [c for c in a.children if c.type == "argument"] if a else []

def is_static_string(argnode):
    """True if the argument is a single plain string literal (no interpolation)."""
    inner = argnode.children[0] if argnode.children else None
    if inner is None: return False
    if inner.type == "string": return True
    if inner.type == "encapsed_string":
        # dynamic if it contains a variable/interpolation child
        return not any(c.type in ("variable_name","dynamic_variable_name","member_access_expression")
                       for c in inner.children)
    return False  # variable, concat (binary_expression), etc. -> dynamic

# refinement 2: authorization expressed through a polymorphic/abstract method
# (e.g. $wp_list_table->ajax_user_can()) is invisible to name-based cap detection.
POLY_CAP_METHODS = {"ajax_user_can","check_permission","check_permissions",
                    "current_user_can","user_can","can_edit","check_capabilities"}
# refinement 1: STATE funcs whose FIRST arg is an entity id we can check for self-scope
ID_FIRST_STATE = {"update_user_meta","delete_user_meta","add_user_meta","update_post_meta",
                  "delete_post_meta","add_post_meta","wp_update_user","wp_delete_user",
                  "update_metadata","delete_metadata"}
def is_self_scoped(argtext, body):
    """True if a state-change target is the current session user (no cap needed)."""
    a = argtext.strip()
    if "get_current_user_id(" in a or "wp_get_current_user(" in a:
        return True
    if a in ("$user->ID","$current_user->ID","$user_id","$current_user_id","$user->id"):
        return ("get_current_user_id()" in body
                or re.search(r"\$(user|current_user)\s*=\s*wp_get_current_user\(", body) is not None)
    return False

def analyze(fnode):
    caps=[]; nonces=[]; state_sites=[]; reads_req_id=False
    body = txt(fnode)
    if any(s in body for s in ("$_POST['id']","$_GET['id']","$_REQUEST['id']","$_POST[ 'id' ]")):
        reads_req_id=True
    def w(n):
        if n.type == "function_call_expression":
            c = callee(n); args = arg_nodes(n)
            if c in CAP_FIRSTARG:
                cap = txt(args[0].children[0]).strip("'\"") if args else "?"
                caps.append({"cap":cap, "object_scoped": len(args) >= 2})
            elif c in CAP_OTHER:
                caps.append({"cap":c, "object_scoped": len(args) >= 2})
            elif c in NONCE_FIRST:
                nonces.append({"dynamic": not (args and is_static_string(args[0]))})
            elif c in NONCE_SECOND:
                nonces.append({"dynamic": not (len(args) >= 2 and is_static_string(args[1]))})
            elif c in STATE:
                # refinement 1: self-scope is decided HERE, at the call site
                selfscoped = (c in ID_FIRST_STATE and args
                              and is_self_scoped(txt(args[0].children[0]) if args[0].children else "", body))
                state_sites.append({"fn":c, "self":bool(selfscoped)})
        elif n.type == "member_call_expression":
            # refinement 2: polymorphic authorization gate
            m = n.child_by_field_name("name")
            if m is not None and txt(m) in POLY_CAP_METHODS:
                caps.append({"cap":"->"+txt(m), "object_scoped": True})
        for ch in n.children: w(ch)
    w(fnode)
    return caps, nonces, state_sites, reads_req_id

FUNC_TYPES = {"function_definition","method_declaration"}
CALL_TYPES = ("function_call_expression","member_call_expression","scoped_call_expression")
def name_of(n):
    f=n.child_by_field_name("name")
    return txt(f) if f else "<anon>"
def callee_last(call):
    fn=call.child_by_field_name("function") or call.child_by_field_name("name")
    if fn is None and call.children: fn=call.children[0]
    if fn is None: return None
    return txt(fn).split("->")[-1].split("::")[-1].rstrip("(")

docs=[]
for path in glob.glob(f"{WP}/**/*.php", recursive=True):
    try: src=open(path,"rb").read()
    except Exception: continue
    root=parser.parse(src).root_node
    rel=os.path.relpath(path, WP)
    def walk(n):
        if n.type in FUNC_TYPES:
            caps,nonces,state_sites,rid=analyze(n)
            calls=[]
            def cw(x):
                if x.type in CALL_TYPES:
                    c=callee_last(x)
                    if c: calls.append(c)
                for ch in x.children: cw(ch)
            cw(n)
            docs.append({"file":rel,"func":name_of(n),"line":n.start_point[0]+1,
                         "caps":caps,"nonces":nonces,"state_sites":state_sites,"reads_req_id":rid,
                         "has_state_site":bool(state_sites),
                         "has_nonself_site":any(not s["self"] for s in state_sites),
                         "has_obj_cap":any(c["object_scoped"] for c in caps),
                         "has_generic_cap":any(not c["object_scoped"] for c in caps),
                         "has_nonce":bool(nonces),"calls":calls})
        for ch in n.children: walk(ch)
    walk(root)

# index
try: urllib.request.urlopen(urllib.request.Request(f"{X}/wpauthz",method="DELETE")).read()
except Exception: pass
m={"mappings":{"properties":{"file":{"type":"keyword"},"func":{"type":"keyword"},
   "line":{"type":"integer"},"state_change":{"type":"boolean"},"reads_req_id":{"type":"boolean"},
   "has_state_site":{"type":"boolean"},"has_nonself_site":{"type":"boolean"},
   "reads_req_id":{"type":"boolean"},
   "has_obj_cap":{"type":"boolean"},"has_generic_cap":{"type":"boolean"},
   "has_nonce":{"type":"boolean"},"calls":{"type":"keyword"}}}}
urllib.request.urlopen(urllib.request.Request(f"{X}/wpauthz",data=json.dumps(m).encode(),
    headers={"Content-Type":"application/json"},method="PUT")).read()
lines=[]
for d in docs:
    lines.append(json.dumps({"index":{}})); lines.append(json.dumps(d))
r=urllib.request.Request(f"{X}/wpauthz/_bulk",data=("\n".join(lines)+"\n").encode(),
    headers={"Content-Type":"application/x-ndjson"},method="POST")
resp=json.loads(urllib.request.urlopen(r).read())
print(f"wpauthz: {len(docs)} funcs indexed, errors={resp.get('errors')}")

# ---- interprocedural propagation from each authenticated handler ----
byname={}
for d in docs: byname.setdefault(d["func"],d)

# Trusted plumbing: auth/verification, RNG, and caching primitives have benign
# write side-effects (wp_salt caches to update_site_option; wp_rand seeds a
# transient) that must NOT be attributed to a caller's authorization surface.
# They are analysis boundaries, exactly like the STATE primitives.
INFRA_LEAF = {
 "check_ajax_referer","check_admin_referer","wp_verify_nonce","wp_create_nonce",
 "wp_nonce_tick","wp_hash","wp_salt","wp_get_session_token","wp_get_current_user",
 "current_user_can","current_user_can_for_blog","current_user_can_for_site","user_can","map_meta_cap",
 "wp_rand","wp_generate_password","random_int","random_bytes",
 "set_transient","get_transient","set_site_transient","get_site_transient",
 "wp_cache_set","wp_cache_add","wp_cache_get",
}
def reach(handler, want, depth=6, leaves=None):
    """DFS over the call graph. `leaves` names are treated as boundaries (not
    expanded) so call-site facts aren't overridden by a primitive's internals."""
    leaves = leaves or set()
    seen=set(); stack=[(handler,depth)]
    while stack:
        fn,dd=stack.pop()
        if fn in seen or dd<0: continue
        seen.add(fn); d=byname.get(fn)
        if not d: continue
        if want(d): return True
        for c in d["calls"]:
            if c in byname and c not in leaves:
                stack.append((c,dd-1))
    return False
STATE_LEAVES = STATE | INFRA_LEAF

handlers=[d for d in docs if d["func"].startswith("wp_ajax_") and not d["func"].startswith("wp_ajax_nopriv_")]
def gate(h):   # any object-scoped / polymorphic cap anywhere on the graph
    return reach(h["func"], lambda d: d["has_obj_cap"])
def anycap(h):
    return reach(h["func"], lambda d: d["has_obj_cap"] or d["has_generic_cap"])

# refined sweep: reaches a NON-SELF state change (call-site scoped, primitive-leaf)
refined=[]
for h in handlers:
    if reach(h["func"], lambda d: d["has_nonself_site"], leaves=STATE_LEAVES) and not gate(h):
        refined.append((h["func"],h["file"],h["line"],anycap(h)))
print(f"\n[REFINED] handlers reaching a non-self state change with NO object-scoped/polymorphic cap: {len(refined)}")
for fn,fl,ln,ac in sorted(refined):
    print(f"   {fn:38} {fl}:{ln}   [{'generic-cap-only' if ac else 'NO-CAP-AT-ALL'}]")

# IDOR view: reads a request object id, acts on it, but the cap (if any) is GENERIC
idor=[]
for h in handlers:
    reads_id = reach(h["func"], lambda d: d["reads_req_id"], leaves=STATE_LEAVES)
    acts     = reach(h["func"], lambda d: d["has_nonself_site"], leaves=STATE_LEAVES)
    if reads_id and acts and not gate(h):
        idor.append((h["func"],h["file"],h["line"],anycap(h)))
print(f"\n[IDOR] handlers: read request object-id + act on it + NO object-scoped cap: {len(idor)}")
for fn,fl,ln,ac in sorted(idor):
    print(f"   {fn:38} {fl}:{ln}   [{'generic-cap-only(IDOR-shape)' if ac else 'no-cap'}]")
