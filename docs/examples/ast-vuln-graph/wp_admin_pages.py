#!/usr/bin/env python3
"""Close the biggest substrate gap: direct wp-admin page handlers live at FILE
SCOPE (a top-level switch($action){case ...}), not inside functions, so the
function-only index never saw them. Extract each case block as a handler unit and
run the same authz analysis: object-scoped cap? object-scoped nonce? state change
on a request-identified object?"""
import glob, os, re, sys
from tree_sitter import Language, Parser
import tree_sitter_php as tsphp
parser=Parser(Language(tsphp.language_php()))
WP=sys.argv[1]
def txt(n): return n.text.decode("utf8","replace")

CAP=("current_user_can","current_user_can_for_blog","user_can","author_can")
NONCE=("check_admin_referer","check_ajax_referer","wp_verify_nonce")
STATE=("wp_trash_post","wp_delete_post","wp_untrash_post","wp_update_post","wp_insert_post",
       "update_option","delete_option","wp_delete_user","wp_update_user","update_post_meta",
       "delete_post_meta","update_user_meta","wp_set_object_terms","wp_delete_attachment",
       "wp_delete_comment","wp_set_comment_status","edit_post","bulk_edit_posts","wp_delete_term",
       "wp_update_term","update_metadata","delete_metadata","wp_trash_comment","wp_update_link")

def top_level_switch_cases(root):
    """Yield case blocks of switch statements that are NOT inside a function."""
    out=[]
    def walk(n, in_fn):
        infn = in_fn or n.type in ("function_definition","method_declaration")
        if n.type=="switch_statement" and not in_fn:
            body=n.child_by_field_name("body") or n
            for c in body.children:
                if c.type in ("case_statement","default_statement"):
                    out.append(c)
        for ch in n.children: walk(ch, infn)
    walk(root, False)
    return out

def calls_of(node, names):
    hits=[]
    def w(n):
        if n.type in ("function_call_expression","member_call_expression","scoped_call_expression"):
            fn=n.child_by_field_name("function") or n.child_by_field_name("name")
            if fn is not None:
                nm=txt(fn).split("->")[-1].split("::")[-1]
                if nm in names:
                    args=n.child_by_field_name("arguments")
                    ac=[a for a in (args.children if args else []) if a.type=="argument"]
                    hits.append((nm,ac))
        for ch in n.children: w(ch)
    w(node); return hits

rows=[]
for path in glob.glob(f"{WP}/wp-admin/**/*.php", recursive=True):
    root=parser.parse(open(path,"rb").read()).root_node
    rel=os.path.relpath(path,WP)
    for case in top_level_switch_cases(root):
        body=txt(case)
        label=re.match(r"\s*case\s+([^:]+):",body)
        name=label.group(1).strip() if label else "default"
        capcalls=calls_of(case,CAP); noncecalls=calls_of(case,NONCE); statecalls=calls_of(case,STATE)
        obj_cap=any(len(ac)>=2 for nm,ac in capcalls)
        dyn_nonce=any(any('$' in txt(a) or txt(a).count('.') for a in ac) for nm,ac in noncecalls)
        reads_id=bool(re.search(r"\$_(?:GET|POST|REQUEST)\[\s*['\"](?:id|post|post_id|user_id|comment)['\"]",body)) \
                 or "$post_id" in body or "$post_ID" in body
        if statecalls:  # only handlers that mutate state matter for authz
            rows.append(dict(file=rel,case=name,obj_cap=obj_cap,has_cap=bool(capcalls),
                             has_nonce=bool(noncecalls),dyn_nonce=dyn_nonce,reads_id=reads_id,
                             state=[nm for nm,_ in statecalls]))

print(f"file-scope wp-admin state-changing action handlers: {len(rows)}")
susp=[r for r in rows if not r["obj_cap"]]
print(f"\n[cap-presence] state-changing handlers with NO object-scoped cap: {len(susp)}")
for r in sorted(susp,key=lambda x:x['file']):
    tag="no-cap" if not r["has_cap"] else "generic-cap"
    print(f"   {r['file']:26} case {r['case']:22} [{tag}] nonce={r['has_nonce']}(dyn={r['dyn_nonce']}) state={r['state'][:3]}")
