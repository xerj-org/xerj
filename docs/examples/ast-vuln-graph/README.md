# AST + graph vulnerability finding (prototype)

Finds interprocedural vulnerabilities that per-file tools miss, and hands an AI
only the code on each taint path — far fewer tokens. Full write-up:
[`docs/research/ast-graph-vuln-detection.md`](../../research/ast-graph-vuln-detection.md).

Two extractors live here:

- **`extract_ast.py`** — the original PHP/WordPress extractor (uses `phply`).
  Richest WP taint model: hook graph (`wp_ajax_nopriv` → unauthenticated),
  nonce-absence CSRF, sanitizer-on-path.
- **`scan_multilang.py`** — one tree-sitter engine, five languages
  (PHP, Python, Go, Rust, Java). Same interprocedural source→sink idea, driven
  by a per-language taint config. This is the path that scales to real code.

## Run the PHP/WordPress demo

```bash
pip install phply
python3 make_demo_plugin.py ./acme-plugin    # 6 planted vulns among ~60 benign files
python3 extract_ast.py       ./acme-plugin    # AST -> taint graph -> findings
```

Expected: **6/6** vulnerabilities, including the interprocedural SQLi
(`acme_ajax_search -> acme_find_items`, unauthenticated), the interprocedural
XSS, and the missing-nonce CSRF — none of which a single-file scan produces.
`ast_facts.json` holds the per-function facts (index these into XERJ for the
FTS/semantic navigation layer described in the research doc).

## Run the multi-language scanner

```bash
pip install -r requirements.txt
python3 scan_multilang.py python samples/py
python3 scan_multilang.py go     samples/go
python3 scan_multilang.py rust   samples/rs
python3 scan_multilang.py java   samples/java
python3 scan_multilang.py php    ./acme-plugin
```

Each `samples/<lang>/` file plants one interprocedural bug (a request/argv
**source** in a handler that reaches a dangerous **sink** in a *different*
function). All five are found:

| language | planted flow | found |
|---|---|:--:|
| Python | `handler → do_query` (SQLi via `.execute`), plus a sanitized negative case | ✅ |
| Go | `handler → run` (command injection via `exec.Command`) | ✅ |
| Rust | `handler → run` (command injection via `Command::new`) | ✅ |
| Java | `handler → query` (SQLi via `executeQuery`) | ✅ |
| PHP | 5 flows incl. interprocedural SQLi + XSS, LFI, unsafe deser | ✅ |

The taint model is **data, not code** — the `LANGS` dict in `scan_multilang.py`
lists each language's sources / sinks / sanitizers. Add a framework by adding a
row, not by touching the engine.

## Why tree-sitter, not phply, for real scale

`extract_ast.py` uses `phply` (pure-Python PHP). It is fine for the demo plugin,
but on **real WordPress core it fails to parse ~37% of files** (newer syntax) and
silently skips them — unacceptable for a security audit. `scan_multilang.py` uses
`tree-sitter-php`, which parses **100%** of the same tree. Measured on a full
WordPress checkout (1,492 PHP files, ~619k lines):

| | phply | tree-sitter-php |
|---|--:|--:|
| files parsed | ~63% | **100%** |
| full-tree scan time | — | **~3.6 s** |
| functions/methods indexed | — | 11,940 |
| whole-tree tokens (load-it-all) | — | ~5.2 M (unreviewable) |
| candidate findings to triage | — | **129** |

129 candidates from a 5.2M-token tree is the whole point: the graph decides
*what* to look at, the AI reviews only those paths. **These are candidates, not
confirmed bugs** — WP core is heavily audited, so most `echo`/`include` hits are
already escaped or path-validated in ways the name-based sanitizer list doesn't
yet recognize. Tightening the WP sanitizer model (context-aware escapers,
`plugins_url`/`get_template_part` path allow-lists) is what drops 129 toward the
true positives. That work is real, but it starts from a tractable 129, not 5.2M.

## What each file does

- `make_demo_plugin.py` — reproducible vulnerable WordPress plugin (ground truth).
- `extract_ast.py` — phply PHP/WP extractor: taint facts, call graph, hook graph,
  nonce-absence CSRF.
- `scan_multilang.py` — tree-sitter engine for PHP/Python/Go/Rust/Java; the
  `LANGS` config carries each language's taint lists.
- `samples/<lang>/` — minimal planted-vuln files, one per language.
- `wp_audit_index.py` — builds the audit substrate: indexes every WordPress
  function (code + taint facts) and every hook registration into XERJ, so an
  agent can *interrogate* the codebase instead of loading it. Powers the field
  test in [`docs/research/wordpress-audit-with-xerj.md`](../../research/wordpress-audit-with-xerj.md).

This is triage: it surfaces real, reachable patterns so an AI can confirm
exploitability with minimal context. See the research doc for the honest limits.

## Does this actually beat grep on a real codebase?

Measured, not asserted:
[`docs/research/wordpress-audit-with-xerj.md`](../../research/wordpress-audit-with-xerj.md)
is an honest audit journal of real WordPress core — the agent tracing unauth
input flow and SQLi reachability by querying XERJ. Result: the same audit cost
**~2,150 tokens via XERJ vs ~864,000 via grep-and-read** (~400×), while reading
*every* candidate instead of skimming. It's equally honest about the limits:
recall is bounded by the taint model, and this run's 4 SQLi candidates were all
false positives from one fixable gap (receiver-type-blind sink matching).
