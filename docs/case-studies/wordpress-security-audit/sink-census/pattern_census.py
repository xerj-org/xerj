#!/usr/bin/env python3
"""Census the AST-detectable dangerous PATTERNS (the non-function vuln classes:
loose ==, non-strict in_array, strcmp/array injection, @ suppression, variable
variables, unsafe setcookie, switch-on-request, Host-header trust, ORDER BY
injection). Complements sink_census.py so coverage spans patterns, not just
sinks. Indexes hits into XERJ `wppatterns`."""
import glob, json, os, re, sys, urllib.request
from tree_sitter import Language, Parser
import tree_sitter_php as tsphp

WP = sys.argv[1]; X = "http://127.0.0.1:9200"
parser = Parser(Language(tsphp.language_php()))
def txt(n): return n.text.decode("utf8", "replace")

SECRET_RE = re.compile(r"(pass|pwd|hash|hmac|mac|sig|sign|token|nonce|secret|key|digest|auth)", re.I)
REQ_RE = re.compile(r"\$_(GET|POST|REQUEST|COOKIE)\b")
hits = []
def add(pid, cls, f, line, snippet):
    hits.append({"pattern": pid, "class": cls, "file": f, "line": line, "snippet": snippet[:160].replace("\n", " ")})

def scan(path, rel):
    src = open(path, "rb").read()
    root = parser.parse(src).root_node
    def w(n):
        t = n.type
        if t == "binary_expression":
            op = n.child_by_field_name("operator")
            ops = txt(op) if op is not None else ""
            if ops in ("==", "!=", "<>"):
                s = txt(n)
                if SECRET_RE.search(s):
                    add("timing-compare", "crypto-logic", rel, n.start_point[0]+1, s)
                else:
                    add("loose-eq", "type-juggling", rel, n.start_point[0]+1, s)
        elif t == "function_call_expression":
            fn = n.child_by_field_name("function"); name = txt(fn) if fn is not None else ""
            base = name.split("\\")[-1]
            args = n.child_by_field_name("arguments")
            argn = [a for a in args.children if a.type == "argument"] if args else []
            if base in ("in_array", "array_search"):
                strict = len(argn) >= 3 and txt(argn[-1]).strip().lower() in ("true", "1")
                if not strict:
                    add("in-array-loose", "type-juggling", rel, n.start_point[0]+1, txt(n))
            elif base in ("strcmp", "strcasecmp", "strncmp", "strncasecmp"):
                if any(REQ_RE.search(txt(a)) for a in argn):
                    add("strcmp-array", "type-juggling", rel, n.start_point[0]+1, txt(n))
            elif base in ("extract", "import_request_variables") or (base == "parse_str" and len(argn) < 2):
                add("mass-assignment", "input-shape", rel, n.start_point[0]+1, txt(n))
            elif base in ("setcookie", "setrawcookie"):
                s = txt(n).lower()
                if "httponly" not in s or "secure" not in s or "samesite" not in s:
                    add("cookie-flags", "auth-session", rel, n.start_point[0]+1, txt(n)[:120])
        elif t == "error_suppression_expression":
            add("error-suppression", "footgun", rel, n.start_point[0]+1, txt(n)[:120])
        elif t == "dynamic_variable_name" or (t == "variable_name" and n.child_count and n.children[0].type == "variable_name"):
            add("var-variable", "input-shape", rel, n.start_point[0]+1, txt(n))
        elif t == "switch_statement":
            cond = n.child_by_field_name("condition")
            if cond is not None and REQ_RE.search(txt(cond)):
                add("switch-loose", "type-juggling", rel, n.start_point[0]+1, txt(cond))
        for c in n.children: w(c)
    w(root)
    # regex-only patterns over raw source
    for m in re.finditer(r"\$_SERVER\[\s*['\"]HTTP_(HOST|X_FORWARDED_[A-Z]+)['\"]", src.decode("utf8", "replace")):
        pass  # counted below by grep for speed
    for i, ln in enumerate(src.decode("utf8", "replace").splitlines(), 1):
        if re.search(r"ORDER\s+BY[^'\"]*\$", ln, re.I) or re.search(r"\bLIMIT\b[^'\"]*\$_", ln, re.I):
            add("order-by-inj", "sql-semantic", rel, i, ln.strip())
        if re.search(r"\$_SERVER\[\s*['\"]HTTP_(HOST|X_FORWARDED_)", ln):
            add("host-header", "trust-boundary", rel, i, ln.strip())

for p in glob.glob(f"{WP}/**/*.php", recursive=True):
    scan(p, os.path.relpath(p, WP))

from collections import Counter
c = Counter(h["pattern"] for h in hits)
print(f"pattern census: {len(hits)} hits across {len(c)} detectable patterns")
for pid, n in c.most_common():
    print(f"  {pid:20} {n}")

def post(path, body, ct="application/json"):
    r = urllib.request.Request(f"{X}/{path}", data=body.encode(), headers={"Content-Type": ct}, method="POST")
    return json.loads(urllib.request.urlopen(r).read())
try: urllib.request.urlopen(urllib.request.Request(f"{X}/wppatterns", method="DELETE")).read()
except Exception: pass
urllib.request.urlopen(urllib.request.Request(f"{X}/wppatterns", method="PUT",
    headers={"Content-Type": "application/json"},
    data=json.dumps({"mappings": {"properties": {"pattern": {"type": "keyword"}, "class": {"type": "keyword"},
        "file": {"type": "keyword"}, "line": {"type": "integer"}, "snippet": {"type": "text"}}}}).encode())).read()
lines = []
for h in hits:
    lines.append(json.dumps({"index": {}})); lines.append(json.dumps(h))
resp = post("wppatterns/_bulk", "\n".join(lines) + "\n", "application/x-ndjson")
print(f"indexed {len(hits)} pattern hits into XERJ `wppatterns` (errors={resp.get('errors')})")
