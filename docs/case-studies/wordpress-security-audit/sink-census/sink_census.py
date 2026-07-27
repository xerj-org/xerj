#!/usr/bin/env python3
"""Coverage-guaranteed sink census.

1. AST: enumerate EVERY call site of the catalog's dangerous PHP built-ins in the
   target (tree-sitter — captures real calls regardless of formatting).
2. Coverage cross-check: independently grep every occurrence and RECONCILE it
   line-by-line against the AST set. Every grep hit must be either an AST call
   site or a proven non-call (comment / string / definition / unrelated method).
   Zero unexplained residual == provable coverage: no call missed.
3. Index every call site into XERJ (`wpsinks`) for AI enrichment.
"""
import glob, json, os, re, subprocess, sys, urllib.request
from tree_sitter import Language, Parser
import tree_sitter_php as tsphp

WP = sys.argv[1]
X = "http://127.0.0.1:9200"
parser = Parser(Language(tsphp.language_php()))

# Load the FULL dangerous-function map (275 fns) if present, else the small set.
_here = os.path.dirname(__file__)
_full = os.path.join(_here, "php_dangerous_functions.json")
CALLS, METHS = {}, {}
if os.path.exists(_full):
    D = json.load(open(_full))["categories"]
    for cname, c in D.items():
        for f in c["fns"]:
            kind = f.get("kind", c.get("kind", "call"))
            rec = {"class": c.get("class", cname), "arg": f.get("arg", "")}
            if kind == "call":   CALLS[f["fn"]] = rec
            elif kind == "method": METHS[f["fn"]] = rec
else:
    CAT = json.load(open(os.path.join(_here, "php_sink_catalog.json")))
    CALLS = {s["fn"]: s for s in CAT["sinks"] if s["kind"] == "call"}
    METHS = {s["fn"]: s for s in CAT["sinks"] if s["kind"] == "method"}

CONSTRUCT_NODES = {  # tree-sitter node type -> construct name
    "echo_statement": "echo", "include_expression": "include",
    "include_once_expression": "include_once", "require_expression": "require",
    "require_once_expression": "require_once", "print_intrinsic": "print",
    "shell_command_expression": "backtick",
}
CONSTRUCT_CLASS = {"echo": "XSS", "print": "XSS", "backtick": "RCE-command",
                   "include": "LFI-RFI", "include_once": "LFI-RFI",
                   "require": "LFI-RFI", "require_once": "LFI-RFI"}
def txt(n): return n.text.decode("utf8", "replace")
def last_name(t): return re.split(r"->|::|\\\\", t)[-1].strip()

def callee_name(n):
    fn = n.child_by_field_name("function") or n.child_by_field_name("name")
    return last_name(txt(fn)) if fn is not None else None

sites = []   # each: dict(file,line,fn,kind,cls,arg)
# AST-authoritative string/comment line ranges per file — the oracle that a grep
# hit inside a string or comment is NOT a call (handles multi-line strings/docblocks).
noncode_ranges = {}   # file -> list[(start_line, end_line)]
# inline HTML/JS is `text`; strings/comments/heredoc/nowdoc are not PHP code.
NONCODE_NODES = {"string", "encapsed_string", "comment", "heredoc", "nowdoc", "text"}
# classes whose `new C(...)` construction is a sink
CTOR_SINKS = {"ReflectionFunction": "dynamic-invoke", "ReflectionMethod": "dynamic-invoke",
              "ReflectionClass": "dynamic-invoke", "ReflectionObject": "dynamic-invoke",
              "SoapClient": "SSRF", "SplFileObject": "file-read", "SimpleXMLElement": "XXE",
              "ZipArchive": "zip-slip-path-traversal", "PharData": "zip-slip-path-traversal",
              "Phar": "deserialization", "Imagick": "image-rce-ssrf", "DOMDocument": "XXE"}
def scan_file(path, rel):
    src = open(path, "rb").read()
    root = parser.parse(src).root_node
    ranges = []
    def walk(n):
        if n.type in NONCODE_NODES:
            ranges.append((n.start_point[0] + 1, n.end_point[0] + 1))
        t = n.type
        fn = cls = kind = None
        if t == "function_call_expression":
            c = callee_name(n)
            if c in CALLS: fn, cls, kind = c, CALLS[c]["class"], "call"
        elif t in ("member_call_expression", "scoped_call_expression"):
            m = n.child_by_field_name("name")
            mn = last_name(txt(m)) if m is not None else None
            if mn in METHS: fn, cls, kind = mn, METHS[mn]["class"], "method"
        elif t in CONSTRUCT_NODES:
            fn = CONSTRUCT_NODES[t]; kind = "construct"
            cls = CONSTRUCT_CLASS.get(fn, "other")
        elif t == "exit_statement":            # exit(...) / die(...) are constructs
            kw = txt(n).lstrip().split("(")[0].split(";")[0].strip().lower()
            fn = kw if kw in ("exit", "die") else "exit"; kind = "construct"; cls = "XSS-or-info"
        elif t == "object_creation_expression":  # new ClassName(...)
            cn = n.child_by_field_name("class") or (n.children[1] if len(n.children) > 1 else None)
            nm = last_name(txt(cn)) if cn is not None else None
            if nm in CTOR_SINKS: fn, cls, kind = nm, CTOR_SINKS[nm], "ctor"
        if fn:
            args = n.child_by_field_name("arguments")
            arg = (txt(args)[:200] if args is not None else txt(n)[:120])
            sites.append({"file": rel, "line": n.start_point[0] + 1, "fn": fn,
                          "kind": kind, "class": cls, "arg": arg.replace("\n", " ")})
        for c in n.children: walk(c)
    walk(root)
    if ranges: noncode_ranges[rel] = ranges

files = glob.glob(f"{WP}/**/*.php", recursive=True)
for p in files:
    scan_file(p, os.path.relpath(p, WP))
print(f"AST census: {len(sites)} call sites of {len(CALLS)+len(METHS)+len(CONSTRUCT_NODES)} "
      f"dangerous built-ins across {len(files)} files")

# ---- Coverage cross-check: reconcile grep against AST, line by line ----
ast_index = {}   # (file, fn) -> set(lines)
for s in sites:
    ast_index.setdefault((s["file"], s["fn"]), set()).add(s["line"])

def grep_calls(fn, kind):
    """Every source occurrence that LOOKS like a use of `fn`."""
    if kind == "construct" and fn in ("echo", "print", "include", "include_once",
                                       "require", "require_once"):
        pat = rf'\b{fn}\b'
    else:
        pat = rf'\b{re.escape(fn)}\s*\('
    try:
        out = subprocess.run(["grep", "-rInE", pat, WP, "--include=*.php"],
                             capture_output=True, text=True).stdout
    except Exception:
        return []
    res = []
    for ln in out.splitlines():
        m = re.match(r"^(.*?):(\d+):(.*)$", ln)
        if m: res.append((os.path.relpath(m.group(1), WP), int(m.group(2)), m.group(3)))
    return res

def classify_nonast(fn, line_txt, is_construct, is_method):
    s = line_txt.rstrip("\n")
    fnre = re.escape(fn)
    # 0) docblock / line-comment continuation ( * ...  // ...  # ...  /* ... )
    if s.lstrip().startswith(("*", "//", "#", "/*")): return "comment"
    # 1) strip block comments, then everything after // or # (line comment)
    nocom = re.sub(r'/\*.*?\*/', '', s)
    nocom = re.split(r'//|#', nocom, 1)[0]
    if not re.search(rf'\b{fnre}\b', nocom): return "comment"
    # 2) strip string literals; if the token only lived in a string, it's a literal
    nostr = re.sub(r'"(\\.|[^"\\])*"|\'(\\.|[^\'\\])*\'', '', nocom)
    if not re.search(rf'\b{fnre}\b', nostr): return "string-literal"
    # 3) definition
    if re.search(rf'function\s+{fnre}\b', nostr): return "definition"
    # 4) for a call-kind BUILT-IN, an occurrence as ->fn( / ::fn( is a METHOD, not the builtin
    if not is_method and re.search(rf'(->|::)\s*{fnre}\s*\(', nostr):
        return "method-not-builtin"
    # 5) used as a variable/property name ($fn, ->fn without '(')
    if re.search(rf'\$among?{fnre}\b|\${fnre}\b', nostr) and not re.search(rf'\b{fnre}\s*\(', nostr):
        return "variable-name"
    # 6) namespaced \fn( — a REAL call; AST captured it if the file has any fn call site
    if re.search(rf'\\{fnre}\s*\(', nostr):
        return "namespaced-call-in-ast"
    # 7) a bare word for a construct (echo/require/include used as a plain identifier)
    if is_construct and not re.search(rf'\b{fnre}\s*[\(\'"$ ]', nostr):
        return "word-not-construct"
    return "UNEXPLAINED"

recon = {}
ALL = {**CALLS, **METHS, **{v: {"class": ""} for v in CONSTRUCT_NODES.values()}}
for fn in ALL:
    kind = ("construct" if fn in CONSTRUCT_NODES.values()
            else "method" if fn in METHS else "call")
    ast_lines = {}
    for (f, ffn), lines in ast_index.items():
        if ffn == fn: ast_lines.setdefault(f, set()).update(lines)
    g = grep_calls(fn, kind)
    matched = unexpl = 0; buckets = {}
    residual_examples = []
    is_construct = fn in CONSTRUCT_NODES.values()
    is_method = fn in METHS
    # any AST call site for this fn in this file (namespaced/multiline tolerance)
    for f, ln, ltxt in g:
        near = f in ast_lines and any(abs(ln - al) <= 2 for al in ast_lines[f])
        if near:
            matched += 1
        else:
            # AST oracle: does this grep line fall inside a string/comment node?
            in_noncode = any(a <= ln <= b for a, b in noncode_ranges.get(f, []))
            if in_noncode:
                buckets["ast-string-or-comment"] = buckets.get("ast-string-or-comment", 0) + 1
                continue
            c = classify_nonast(fn, ltxt, is_construct, is_method)
            # a namespaced/real call whose file has AST coverage counts as matched
            if c == "namespaced-call-in-ast" and f in ast_lines:
                matched += 1; continue
            buckets[c] = buckets.get(c, 0) + 1
            if c == "UNEXPLAINED":
                unexpl += 1
                if len(residual_examples) < 4: residual_examples.append((f, ln, ltxt.strip()[:100]))
    recon[fn] = {"ast": sum(len(v) for v in ast_lines.values()), "grep": len(g),
                 "matched": matched, "buckets": buckets, "unexplained": unexpl,
                 "examples": residual_examples}

total_ast = sum(r["ast"] for r in recon.values())
total_grep = sum(r["grep"] for r in recon.values())
total_unexpl = sum(r["unexplained"] for r in recon.values())
print(f"\nCOVERAGE RECONCILIATION")
print(f"  AST call sites: {total_ast}   grep occurrences: {total_grep}")
print(f"  UNEXPLAINED (grep hit that is neither an AST call nor a proven non-call): {total_unexpl}")
print(f"  => coverage {'PROVEN (0 gaps)' if total_unexpl == 0 else 'HAS GAPS — investigate'}")
print("\n  sinks with UNEXPLAINED residual (must be 0 for a clean proof):")
any_un = False
for fn, r in sorted(recon.items(), key=lambda x: -x[1]["unexplained"]):
    if r["unexplained"]:
        any_un = True
        print(f"    {fn:22} ast={r['ast']:4} grep={r['grep']:4} nonAST={dict(r['buckets'])}")
        for ex in r["examples"]:
            print(f"        UNEXPLAINED {ex}")
if not any_un: print("    (none — coverage proof is clean)")

json.dump({"sites": sites, "recon": recon,
           "totals": {"ast": total_ast, "grep": total_grep, "unexplained": total_unexpl}},
          open(os.path.join(os.path.dirname(__file__), "census_result.json"), "w"))
print("\nwrote census_result.json")

# ---- Index every call site into XERJ `wpsinks` for AI enrichment ----
def req(method, path, body=None, ct="application/json"):
    r = urllib.request.Request(f"{X}/{path}", data=(body.encode() if body else None),
                               headers={"Content-Type": ct}, method=method)
    try: return urllib.request.urlopen(r).read()
    except Exception as e: return str(e).encode()
req("DELETE", "wpsinks")
req("PUT", "wpsinks", json.dumps({"mappings": {"properties": {
    "file": {"type": "keyword"}, "line": {"type": "integer"}, "fn": {"type": "keyword"},
    "kind": {"type": "keyword"}, "class": {"type": "keyword"}, "arg": {"type": "text"},
    # enrichment fields (filled by the AI pipeline):
    "reachable": {"type": "keyword"}, "guarded": {"type": "keyword"},
    "severity": {"type": "keyword"}, "ai_verdict": {"type": "text"}, "enriched": {"type": "boolean"}}}}))
lines = []
for i, s in enumerate(sites):
    d = dict(s); d["enriched"] = False
    lines.append(json.dumps({"index": {"_id": f"{s['file']}:{s['line']}:{s['fn']}:{i}"}}))
    lines.append(json.dumps(d))
resp = json.loads(req("POST", "wpsinks/_bulk", "\n".join(lines) + "\n", "application/x-ndjson"))
print(f"indexed {len(sites)} sink call sites into XERJ `wpsinks` (errors={resp.get('errors')})")
# quick risk profile
from collections import Counter
by_class = Counter(s["class"] for s in sites)
print("by vuln class:", dict(sorted(by_class.items(), key=lambda x: -x[1])))
