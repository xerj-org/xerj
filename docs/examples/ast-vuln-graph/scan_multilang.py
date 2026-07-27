#!/usr/bin/env python3
"""Multi-language AST taint scanner (tree-sitter). One engine; per-language
taint config. Finds interprocedural source->sink flows the same way the PHP
extractor does, for Python / Go / Rust / Java.
"""
import glob, importlib, json, os, re, sys
from tree_sitter import Language, Parser

# per-language: parser module, file globs, AST node types, and the taint lists
# (matched as substrings against each function's own source text — the AST gives
# the function SCOPING and call edges; the patterns give the vuln signatures).
LANGS = {
 "python": dict(mod="tree_sitter_python", ext=[".py"],
   func=["function_definition"], call=["call"],
   sources=["request.args","request.form","request.values","request.GET","request.POST",
            "os.environ","sys.argv","input("],
   sinks={"sql":[".execute(",".executemany(",".raw("],
          "cmd":["os.system(","os.popen(","subprocess"],
          "code":["eval(","exec("],
          "deser":["pickle.load","yaml.load(","marshal.load"]},
   sanit=["shlex.quote","html.escape","escape(","parameterize"]),
 "go": dict(mod="tree_sitter_go", ext=[".go"],
   func=["function_declaration","method_declaration"], call=["call_expression"],
   sources=["r.URL.Query()","r.FormValue","r.PostFormValue","r.Header.Get","os.Getenv","os.Args"],
   sinks={"sql":[".Query(",".Exec(",".QueryRow("],
          "cmd":["exec.Command(","os/exec"],
          "deser":["gob.NewDecoder"]},
   sanit=["html/template","QueryContext",".Prepare("]),
 "rust": dict(mod="tree_sitter_rust", ext=[".rs"],
   func=["function_item"], call=["call_expression","macro_invocation"],
   sources=["env::var","env::args","std::env"],
   sinks={"cmd":['Command::new','process::Command'],
          "sql":["sqlx::query(","format!"],
          "unsafe":["unsafe {","unsafe{"]},
   sanit=["query!","shell_escape"]),
 "java": dict(mod="tree_sitter_java", ext=[".java"],
   func=["method_declaration"], call=["method_invocation"],
   sources=["request.getParameter","request.getHeader","getQueryString","System.getenv"],
   sinks={"sql":[".executeQuery(",".executeUpdate(",".execute("],
          "cmd":["Runtime.getRuntime().exec(","ProcessBuilder("],
          "deser":[".readObject(","ObjectInputStream"]},
   sanit=["PreparedStatement",".setString(","ESAPI"]),
 "php": dict(mod="tree_sitter_php", ext=[".php"], lang_fn="language_php",
   func=["function_definition","method_declaration"],
   call=["function_call_expression","member_call_expression","scoped_call_expression"],
   sources=["$_GET","$_POST","$_REQUEST","$_COOKIE","$_SERVER","$_FILES"],
   sinks={"sql":["->query(","->get_results(","->get_var(","->get_row(","->get_col("],
          "cmd":["exec(","system(","shell_exec(","passthru(","popen("],
          "code":["eval("],
          "deser":["unserialize("]},
   stmt_sinks={"echo_statement":"xss","include_expression":"lfi","require_expression":"lfi","include_once_expression":"lfi","require_once_expression":"lfi"},
   sanit=["esc_html","esc_attr","esc_url","sanitize_","wp_verify_nonce",
          "check_admin_referer","->prepare(","absint","intval","wp_kses"]),
}

def node_name(n):
    f=n.child_by_field_name("name")
    if f: return f.text.decode("utf8","replace")
    for c in n.children:
        if c.type in ("identifier","name","field_identifier"): return c.text.decode()
    return "<anon>"

def callee_text(call):
    """Best-effort dotted callee of a call node: os.system, $wpdb->query,
    request.getParameter, pickle.loads, etc."""
    fn = call.child_by_field_name("function") or call.child_by_field_name("name") \
         or (call.child_by_field_name("macro") if call.type=="macro_invocation" else None)
    node = fn or (call.children[0] if call.children else None)
    if node is None: return "", ""
    txt = node.text.decode("utf8","replace")
    last = re.split(r"->|::|\.", txt)[-1].strip()
    return txt, last

def calls_in(fnode, call_types):
    out=[]
    def w(n):
        if n.type in call_types:
            out.append(callee_text(n))
        for c in n.children: w(c)
    w(fnode)
    return out

def scan_lang(root_dir, lang):
    cfg=LANGS[lang]; m=importlib.import_module(cfg["mod"])
    parser=Parser(Language(getattr(m, cfg.get("lang_fn","language"))()))
    # sink/sanitizer name sets = last-segment of each configured pattern (strip . -> :: ( )
    def names(pats): return {re.split(r"->|::|\.", p.rstrip("(").strip())[-1] for p in pats}
    sink_names={k:names(v) for k,v in cfg["sinks"].items()}
    sanit_names=names(cfg["sanit"])
    raw={}   # (file,name)->fact ; also collect name multiplicity
    name_count={}
    files=[p for e in cfg["ext"] for p in glob.glob(f"{root_dir}/**/*{e}", recursive=True)]
    for path in files:
        try: src=open(path,"rb").read()
        except Exception: continue
        root=parser.parse(src).root_node
        rel=os.path.relpath(path, root_dir)
        def walk(n):
            if n.type in cfg["func"]:
                nm=node_name(n); txt=n.text.decode("utf8","replace")
                cs=calls_in(n, cfg["call"])
                sinks=set()
                for full,last in cs:
                    for k,ns in sink_names.items():
                        if last in ns: sinks.add(k)
                stmt_map=cfg.get("stmt_sinks",{})
                if stmt_map:
                    def find_stmt(nn):
                        if nn.type in stmt_map: sinks.add(stmt_map[nn.type])
                        for cc in nn.children: find_stmt(cc)
                    find_stmt(n)
                sanit=any(last in sanit_names or any(sp in full for sp in cfg["sanit"]) for full,last in cs)
                srcs=[x for x in cfg["sources"] if x in txt]
                key=(rel,nm)
                raw[key]={"file":rel,"func":nm,"line":n.start_point[0]+1,
                          "sources":srcs,"sinks":sorted(sinks),"sanit":sanit,
                          "callee_last":[last for _,last in cs]}
                name_count[nm]=name_count.get(nm,0)+1
            for c in n.children: walk(c)
        walk(root)
    # resolve edges: only to names defined exactly once (unambiguous)
    byname={}
    for k,f in raw.items(): byname.setdefault(f["func"],[]).append(k)
    for k,f in raw.items():
        f["calls"]=[byname[c][0] for c in f["callee_last"]
                    if c in byname and name_count.get(c,0)==1]
    # interprocedural taint
    findings=[]
    for key,f in raw.items():
        if not f["sources"]: continue
        stack=[(key,[f["func"]],f["sanit"])]; seen=set()
        while stack:
            cur,path,san=stack.pop()
            if cur in seen: continue
            seen.add(cur); cf=raw[cur]
            if cf["sinks"] and not san:
                findings.append({"sink":cf["sinks"],"path":path,"file":cf["file"]})
            for cal in cf["calls"]:
                stack.append((cal,path+[raw[cal]["func"]],san or raw[cal]["sanit"]))
    uniq={(x["path"][0],x["path"][-1],tuple(x["sink"])):x for x in findings}
    return raw, list(uniq.values())

if __name__=="__main__":
    lang=sys.argv[1]; root=sys.argv[2]
    funcs,findings=scan_lang(root, lang)
    print(f"[{lang}] parsed {len(funcs)} functions, {len(findings)} findings")
    for x in findings:
        print(f"  {'/'.join(x['sink'])}: {' -> '.join(x['path'])}  (sink in {x['file']})")
