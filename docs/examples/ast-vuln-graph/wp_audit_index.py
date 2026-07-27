#!/usr/bin/env python3
"""Build the audit substrate: index every WordPress function as one XERJ doc
(code text + taint facts) and every hook registration as a hook doc. Then the
agent AUDITS by querying this index, not by reading the whole tree."""
import glob, json, os, sys, urllib.request
from tree_sitter import Language, Parser
import tree_sitter_php as tsphp
from scan_multilang import LANGS, node_name, callee_text, calls_in

WP = sys.argv[1]
XERJ = "http://127.0.0.1:9200"
cfg = LANGS["php"]
parser = Parser(Language(tsphp.language_php()))

SRC = cfg["sources"]
SANIT = cfg["sanit"]
SINK_NAMES = {k: {p.rstrip("(").split("->")[-1].split("::")[-1].split(".")[-1]
                  for p in v} for k, v in cfg["sinks"].items()}
STMT = cfg["stmt_sinks"]
UNAUTH_HOOKS = ("wp_ajax_nopriv_", "admin_post_nopriv", "wp_ajax_nopriv")

def txt(n): return n.text.decode("utf8", "replace")

def stmt_sinks(n, acc):
    if n.type in STMT: acc.add(STMT[n.type])
    for c in n.children: stmt_sinks(c, acc)

def hooks_in(root, rel):
    """add_action/add_filter(hook, callback) -> registration edges."""
    out = []
    def w(n):
        if n.type == "function_call_expression":
            name = txt(n.child_by_field_name("function") or n)
            if name in ("add_action", "add_filter"):
                args = n.child_by_field_name("arguments")
                strs = [txt(a).strip("'\"") for a in args.children
                        if a.type in ("string", "encapsed_string", "string_content")] if args else []
                # tree-sitter wraps string literal; grab literal children
                lit = []
                if args:
                    for a in args.children:
                        if a.type == "argument":
                            s = a.children[0] if a.children else None
                            if s and s.type in ("string", "encapsed_string"):
                                lit.append(txt(s).strip("'\""))
                            else:
                                lit.append(None)
                if lit and lit[0]:
                    hook = lit[0]
                    cb = lit[1] if len(lit) > 1 and lit[1] else None
                    out.append((hook, cb, n.start_point[0] + 1))
        for c in n.children: w(c)
    w(root)
    return out

func_docs, hook_docs = [], []
files = glob.glob(f"{WP}/**/*.php", recursive=True)
for path in files:
    try: src = open(path, "rb").read()
    except Exception: continue
    root = parser.parse(src).root_node
    rel = os.path.relpath(path, WP)
    for hook, cb, ln in hooks_in(root, rel):
        hook_docs.append({"hook": hook, "callback": cb, "file": rel, "line": ln,
                          "unauth": any(hook.startswith(u) or hook == u for u in UNAUTH_HOOKS)})
    def walk(n):
        if n.type in cfg["func"]:
            code = txt(n); nm = node_name(n)
            cs = calls_in(n, cfg["call"])
            sinks = set()
            for full, last in cs:
                for k, ns in SINK_NAMES.items():
                    if last in ns: sinks.add(k)
            stmt_sinks(n, sinks)
            sanit = any(last in {s.rstrip("(").split("->")[-1] for s in SANIT}
                        or any(sp in full for sp in SANIT) for full, last in cs)
            srcs = [x for x in SRC if x in code]
            func_docs.append({"file": rel, "func": nm, "line": n.start_point[0] + 1,
                              "sources": srcs, "sinks": sorted(sinks), "sanit": sanit,
                              "calls": [last for _, last in cs],
                              "has_source": bool(srcs), "has_sink": bool(sinks),
                              "code": code[:6000]})
        for c in n.children: walk(c)
    walk(root)

def bulk(index, docs, mapping):
    urllib.request.urlopen(urllib.request.Request(f"{XERJ}/{index}", method="DELETE")).read() if _exists(index) else None
    req = urllib.request.Request(f"{XERJ}/{index}", data=json.dumps(mapping).encode(),
                                 headers={"Content-Type": "application/json"}, method="PUT")
    try: urllib.request.urlopen(req).read()
    except Exception as e: print("create", index, e)
    lines = []
    for d in docs:
        lines.append(json.dumps({"index": {}})); lines.append(json.dumps(d))
    body = ("\n".join(lines) + "\n").encode()
    r = urllib.request.Request(f"{XERJ}/{index}/_bulk", data=body,
                               headers={"Content-Type": "application/x-ndjson"}, method="POST")
    resp = json.loads(urllib.request.urlopen(r).read())
    print(f"{index}: indexed {len(docs)} docs, errors={resp.get('errors')}")

def _exists(index):
    try:
        urllib.request.urlopen(f"{XERJ}/{index}"); return True
    except Exception: return False

fmap = {"mappings": {"properties": {
    "file": {"type": "keyword"}, "func": {"type": "keyword"}, "line": {"type": "integer"},
    "sources": {"type": "keyword"}, "sinks": {"type": "keyword"}, "calls": {"type": "keyword"},
    "sanit": {"type": "boolean"}, "has_source": {"type": "boolean"}, "has_sink": {"type": "boolean"},
    "code": {"type": "text"}}}}
hmap = {"mappings": {"properties": {
    "hook": {"type": "keyword"}, "callback": {"type": "keyword"},
    "file": {"type": "keyword"}, "line": {"type": "integer"}, "unauth": {"type": "boolean"}}}}
bulk("wpaudit", func_docs, fmap)
bulk("wphooks", hook_docs, hmap)
print(f"functions={len(func_docs)} hooks={len(hook_docs)} "
      f"unauth_hooks={sum(1 for h in hook_docs if h['unauth'])}")
