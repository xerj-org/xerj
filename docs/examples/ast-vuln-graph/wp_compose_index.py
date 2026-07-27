#!/usr/bin/env python3
"""Improve XERJ for composition audits: compute each function's SANITIZER
SEQUENCE fingerprint ONCE at index time, so the audit is a tiny structured query
over fingerprints instead of pulling and reading hundreds of code bodies.

Token cost of an audit drops from O(total code size) to O(true candidates)."""
import glob, json, os, re, sys, urllib.request
X="http://127.0.0.1:9200"; WP=sys.argv[1] if len(sys.argv)>1 else None

ESC   = ["esc_html","esc_attr","esc_sql","esc_url","esc_js","esc_textarea","wp_specialchars",
         "addslashes","wp_slash","wp_kses","wp_kses_post","sanitize_text_field"]
DEESC = ["html_entity_decode","wp_specialchars_decode","htmlspecialchars_decode","urldecode",
         "rawurldecode","stripslashes","stripslashes_deep","wp_unslash","base64_decode"]
RAW_SINK  = r"->(query|get_results|get_var|get_row|get_col)\("     # attacker-dangerous
SAFE_SINK = r"->(update|insert|delete|replace|prepare)\("          # self-escaping wpdb methods
OUT_SINK  = r"\becho\b|printf\(|\bprint\b"

def fingerprint(code):
    ev=[]
    for name in ESC:
        for m in re.finditer(r"\b"+re.escape(name)+r"\s*\(",code): ev.append((m.start(),"ESC:"+name))
    for name in DEESC:
        for m in re.finditer(r"\b"+re.escape(name)+r"\s*\(",code): ev.append((m.start(),"DEE:"+name))
    for m in re.finditer(RAW_SINK,code):  ev.append((m.start(),"SNKraw"))
    for m in re.finditer(SAFE_SINK,code): ev.append((m.start(),"SNKsafe"))
    for m in re.finditer(OUT_SINK,code):  ev.append((m.start(),"SNKout"))
    seq=[t for _,t in sorted(ev)]
    # danger-order = a de-escape after some escape, then a sink after that de-escape
    danger=False
    esc_i=[i for i,t in enumerate(seq) if t.startswith("ESC")]
    if esc_i:
        first_esc=esc_i[0]
        for i,t in enumerate(seq):
            if t.startswith("DEE") and i>first_esc and any(seq[j].startswith("SNK") for j in range(i+1,len(seq))):
                danger=True; break
    has_raw = any(t=="SNKraw" for t in seq)
    return seq, danger, has_raw

def process(code):
    seq,danger,has_raw=fingerprint(code)
    # audit-grade flag: dangerous order AND a RAW sink (the only order that can inject/XSS)
    return {"sanitizer_seq":seq, "compose_danger":danger, "sink_raw":has_raw,
            "compose_audit":bool(danger and (has_raw or "SNKout" in seq))}

if WP:  # (re)build a compose index from source
    from tree_sitter import Language, Parser
    import tree_sitter_php as tsphp
    parser=Parser(Language(tsphp.language_php()))
    def txt(n): return n.text.decode("utf8","replace")
    docs=[]
    for path in glob.glob(f"{WP}/**/*.php",recursive=True):
        root=parser.parse(open(path,"rb").read()).root_node
        rel=os.path.relpath(path,WP)
        def walk(n):
            if n.type in ("function_definition","method_declaration"):
                nm=n.child_by_field_name("name"); nm=txt(nm) if nm else "<anon>"
                d={"file":rel,"func":nm,"line":n.start_point[0]+1}; d.update(process(txt(n)))
                if d["sanitizer_seq"]: docs.append(d)
            for c in n.children: walk(c)
        walk(root)
    try: urllib.request.urlopen(urllib.request.Request(f"{X}/wpcompose",method="DELETE")).read()
    except Exception: pass
    m={"mappings":{"properties":{"file":{"type":"keyword"},"func":{"type":"keyword"},
       "line":{"type":"integer"},"sanitizer_seq":{"type":"keyword"},
       "compose_danger":{"type":"boolean"},"sink_raw":{"type":"boolean"},
       "compose_audit":{"type":"boolean"}}}}
    urllib.request.urlopen(urllib.request.Request(f"{X}/wpcompose",data=json.dumps(m).encode(),
        headers={"Content-Type":"application/json"},method="PUT")).read()
    lines=[]
    for d in docs: lines.append(json.dumps({"index":{}})); lines.append(json.dumps(d))
    r=urllib.request.Request(f"{X}/wpcompose/_bulk",data=("\n".join(lines)+"\n").encode(),
        headers={"Content-Type":"application/x-ndjson"},method="POST")
    resp=json.loads(urllib.request.urlopen(r).read())
    print(f"wpcompose: indexed {len(docs)} functions with sanitizer fingerprints, errors={resp.get('errors')}")
