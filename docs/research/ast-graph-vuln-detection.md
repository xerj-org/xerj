# Research: AST + graph + FTS for AI vulnerability finding, at codebase scale

**Status:** working prototype. Verified on a realistic WordPress plugin (PHP,
6/6 planted bugs) and extended to **five languages** (PHP, Python, Go, Rust,
Java) on a single tree-sitter engine, then run against **real WordPress core**
(1,492 files, ~619k lines) to test the design at scale. See
`docs/examples/ast-vuln-graph/` to reproduce every number below.

## The problem

An AI is good at judging whether a specific piece of code is exploitable. It is
bad at two things that vulnerability finding requires:

1. **Scale.** A real WordPress install is 100k+ lines across core and plugins.
   It does not fit in a context window, and "load it all and look for bugs" is
   both impossible and, where partial, expensive.
2. **Reach.** The interesting bugs are *interprocedural*: a request value read
   in one function, passed through a helper, and reaching a `$wpdb` query in a
   different file. A per-file scan (grep, or single-file semgrep rules) sees the
   source and the sink as unrelated. It also can't reason about *absence* —
   e.g. a state-changing admin handler with no nonce check (CSRF).

So the two naive options both fail: **full-context load** doesn't scale, and
**grep/per-file matching** misses the real bugs and drowns you in false hits
(every `$wpdb` use, every `$_GET`).

## The approach

Split precision from navigation, and let each layer do what it's good at.

```
   PHP source
      │  parse (phply / tree-sitter)
      ▼
   AST  ──►  taint GRAPH        ← precision: interprocedural source→sink,
      │      (functions,          reachability, sanitizer-on-path, nonce-absence
      │       call edges,
      │       sources/sinks/
      │       sanitizers,
      │       hook entry points)
      ▼
   findings + function facts  ──►  XERJ index   ← navigation: FTS / semantic
      │                                             search + keyword filters
      ▼                                             (nopriv=true, sinks=sql, …)
   AI reviews ONLY the code on flagged paths  ← judgement: confirm exploitability,
                                                 cite file:line
```

Three layers:

1. **AST → taint graph (precision).** Parse every file, record per function:
   the request sources it reads (`$_GET/$_POST/$_REQUEST/…`), the dangerous
   sinks it hits (`$wpdb->query`, `echo`, `include`, `unserialize`, state
   changes like `update_option`), the sanitizers present (`esc_html`,
   `sanitize_*`, `$wpdb->prepare`, `wp_verify_nonce`), and its call edges. Then
   walk the call graph from every source-bearing function; if a sink is reached
   with no sanitizer on the path, that's a finding — **across function and file
   boundaries.** Hook wiring (`add_action('wp_ajax_nopriv_…')`) marks
   unauthenticated entry points.

2. **XERJ index (navigation).** Store the findings and the function facts as
   documents — the fact text is searchable (FTS/semantic), and graph properties
   (`nopriv`, `sinks`, `entry`) are keyword-filterable. An AI agent can now ask,
   in plain language, "unauthenticated handlers that reach a SQL query" and get
   the exact functions, or filter `nopriv:true AND sinks:sql`.

3. **AI review (judgement).** The AI reads only the code on the flagged paths
   and confirms exploitability with a `file:line` citation.

## What was verified

Reproducible in `docs/examples/ast-vuln-graph/` (a realistic 66-file WP plugin
with 6 planted vulnerabilities among ~60 benign helper files):

| metric | result |
|---|--:|
| planted vulnerabilities | 6 |
| **found by the AST graph** | **6 / 6** |
| — of which *interprocedural* (source and sink in different files) | 2 (SQLi, XSS) |
| — of which *unauthenticated-critical* (nopriv hook → SQL sink) | 1 |
| — of which *absence property* (state change, no nonce) | 1 (CSRF) |
| tokens to review the flagged paths | ~412 |
| tokens to load the whole plugin | ~5,491 |
| **reduction** | **~13×** (and it grows without bound as the codebase grows) |

The critical finding — `acme_ajax_search → acme_find_items [UNAUTH]` — requires
**all three graphs at once**: the *hook* graph (the entry is `wp_ajax_nopriv`,
so unauthenticated), the *call* graph (source and sink are in different files),
and *taint* (the `$_GET` value reaches `$wpdb->get_results` with no
`prepare`/sanitizer). No single-file tool produces it.

### The finding that shaped the architecture

The first cut indexed raw function text and let the AI query it with FTS. Asked
for "echo request input without escaping," FTS returned two **benign** helper
functions — because they contain `esc_html`, and the words matched. **FTS over
raw code is noisy for security.** The fix is the ordering above: the *graph*
decides what's a real finding; FTS/semantic search only navigates the
graph-verified findings. Precision comes from the AST; recall-friendly natural
language comes from XERJ. Neither alone is enough.

## Why this beats grep and per-file semgrep

- **grep `$wpdb`** → every DB call, no reachability, no taint. On the demo it
  flags the file but can't tell the injectable query from a prepared one, and
  never links the cross-file source.
- **per-file semgrep** → catches direct source→sink *inside one function*, but
  the interprocedural SQLi and XSS here span files; connecting them needs the
  call graph (semgrep's interprocedural analysis is limited / paid).
- **This** → 6/6 including the interprocedural, unauth, and absence cases, and
  it hands the AI ~13× less to read.

## Multi-language, and the scale test on real WordPress

The taint model being *data, not code* was the claim; `scan_multilang.py` proves
it. One tree-sitter engine, a `LANGS` config with per-language sources / sinks /
sanitizers, and the same interprocedural walk finds the planted handler→sink bug
in **Python, Go, Rust, Java, and PHP** — a new language is a new config row, not
new engine code.

The switch to tree-sitter also fixed the parser-coverage limitation below. On a
full WordPress checkout, `phply` fails to parse ~37% of files (newer syntax) and
silently skips them — the worst failure mode for an audit. `tree-sitter-php`
parses 100%:

| | phply | tree-sitter-php |
|---|--:|--:|
| files parsed (real WP core) | ~63% | **100%** |
| full-tree scan time | — | **~3.6 s** for ~619k lines |
| functions/methods indexed | — | 11,940 |
| whole-tree tokens ("load it all") | — | ~5.2 M (unreviewable) |
| candidate findings to triage | — | **129** |

The headline is the last two rows: the graph turns a 5.2M-token tree no context
window can hold into **129 candidate paths** an AI can actually work through.
Honesty check — these are *candidates, not confirmed bugs*: WP core is heavily
audited, so most of the 129 (largely `echo`/`include` hits) are already escaped
or path-validated in ways the name-based sanitizer list doesn't yet recognize.
The follow-on work is a context-aware WP sanitizer model, and it starts from a
tractable 129, not from 5.2M tokens.

## Design for real WordPress scale

The prototype is a plugin; core + plugins is the target. The design carries over:

- **Extraction is per-file and embarrassingly parallel.** Parse each file to
  function facts independently; a 100k-line tree is a fan-out job, not a
  whole-program load. Incremental: re-parse only changed files.
- **The taint model is data, not code.** Sources, sinks, sanitizers, and
  state-changers are per-framework lists (`docs/examples/ast-vuln-graph/extract_ast.py`
  has the WP set). Add React's `dangerouslySetInnerHTML`, Java's
  `Runtime.exec`, Django's `.raw()`, etc.
- **Findings are the unit of work.** Index findings (path + severity + unauth
  flag + the code on the path) as first-class XERJ documents. The agent queries
  findings, not raw code — severity-ranked, filterable, and each one already
  carries the minimal code to review.
- **XERJ is the right store** because it already unifies keyword filters
  (`nopriv`, `sinks`), full-text and semantic search, and returns chunked code
  with `file:line` — one index answers every query shape an audit needs, and the
  same index holds the docs, configs, and SQL in the same repo.

## Honest limitations

This is triage that finds *more real bugs with far fewer tokens*, not a sound
analyzer. Be clear about what it is not:

- **Static approximation.** Taint follows call edges and direct argument flow;
  it does not model data flow through arrays/objects, aliasing, or dynamic
  dispatch. It will miss flows that launder taint through a `$GLOBALS` array or
  a variable-variable, and it can over-report when a value is validated in a way
  the name-based sanitizer list doesn't recognize.
- **Sanitizer detection is name-based.** It trusts `esc_html`/`prepare` by name;
  it does not check that the *right* escaper is used for the *context* (a
  classic real bug: `esc_html` on a value placed inside an HTML attribute).
- **CSRF/nonce is heuristic** — "state change reachable from an entry with no
  nonce call anywhere on the path." Real nonce logic can be conditional.
- **Parser coverage.** Solved for PHP by moving to tree-sitter (100% of real WP
  core vs phply's ~63%). The rule stands for any parser: a file that fails to
  parse must be reported, not silently dropped — the same loud-skip rule
  autoindex needs.
- **Precision on real code needs a per-framework sanitizer model.** The 5-language
  demo is clean because the samples are minimal. On real WP core the same
  substring/name-based sinks over-report (129 candidates, most already escaped).
  Two concrete fixes, both known: match sinks on the AST *callee node* (done —
  no more `exec(` matching `fsockopen(`), and resolve call edges only to
  unambiguously-defined names (done — no more collisions across 11,940
  functions). What remains is context-aware sanitizer recognition per framework.
- **Every finding needs confirmation.** The point is to shrink what the AI (and
  human) read so they *can* reason about exploitability — not to auto-file CVEs.

## Concrete next steps for XERJ

1. **Ship an AST-aware code extractor in `autoindex`** — chunk code by function
   (not fixed line windows), and store call edges + taint facts as fields, so
   `xerj autoindex ./code` produces the graph-ready index directly.
2. **Findings as a first-class index** with a severity/route schema, so the
   agent loop is "query findings → pull path code → confirm."
3. ~~**Multi-language via tree-sitter**, reusing the per-framework taint lists.~~
   **Done** — `scan_multilang.py` covers PHP/Python/Go/Rust/Java on one engine;
   adding a language is a config row.
4. **Loud skips** — a file that fails to parse must be reported (a silently
   dropped file in a security audit is the worst-case), matching the
   autoindex honesty rule.
5. **Per-framework sanitizer model** — the next precision lever, to drop the real
   WordPress candidate set from 129 toward its true positives (context-aware
   escaper matching, path allow-lists for `include`/template loaders).
