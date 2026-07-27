# Case study: auditing real WordPress core for security with XERJ

**What this is.** A reproducible security review of **real WordPress core**
(1,492 PHP files, ~619k lines) performed by an AI agent using XERJ as its
retrieval and reasoning substrate — and an honest, measured comparison against
the naive baseline of *"Claude reads all the files."* Every number here is
reproducible; see [`REPRODUCE.md`](REPRODUCE.md). The vulnerability details are in
[`FINDINGS.md`](FINDINGS.md).

The point of the case study is **not** "we found a WordPress 0-day" — core is one
of the most-audited PHP codebases alive, and it is verifiably hardened on every
flow tested. The point is the **method and its economics**: what an AI-assisted
audit costs, what it catches that reading-all misses, and where it is honest about
its own limits (including a real XERJ engine bug the audit surfaced).

## The headline: XERJ-assisted review vs. Claude reading all files

| | Claude reads all files | XERJ-assisted audit (this case study) |
|---|---|---|
| **Tokens to review core once** | ~5,200,000 just to *load* | **~26,000** for the *entire* audit |
| **Fits one context?** | No — ~26× a 200k window; must chunk | Yes — targeted queries + read only flagged code |
| **Interprocedural reach** | Lost when chunked (bugs are cross-file) | Call/hook graph preserved across all files |
| **Grounding / hallucination** | Skimming 5.2M tokens → guesses | Every claim cites pulled `file:line` code |
| **Reproducible?** | No — path-dependent on what it read | Yes — scripts + queries reproduce exactly |
| **Coverage** | Whatever fit in the windows read | AJAX 95 · REST 90 · file-scope 41 · engine |

**~199× fewer tokens, and higher quality** — because the graph decides *what* to
read, so the agent reads every candidate exactly instead of skimming a haystack it
cannot hold.

## What the audit actually did (and found)

The agent used XERJ as a **second brain**: index once, then *interrogate* the
codebase — locate the auth gatekeepers, read their real implementation, build a
model of correct-vs-buggy authorization, and hunt the buggy shape across the whole
tree. Surfaces swept, all with the finding stated honestly:

| surface | checked | result |
|---|--:|---|
| authenticated AJAX handlers | 95 | object-scoped, **0 IDOR** |
| REST mutating + read permission checks | 90 | object-scoped, **0 IDOR** |
| file-scope `wp-admin` action handlers | 41 | object-scoped (guards in shared scope) |
| `map_meta_cap` authz engine | — | **fails closed** |
| SQL double-`prepare` de-escape | 1 (core) | safe *only* via `placeholder_escape` |
| **SSRF: `wp_http_validate_url`** | — | **real gap → cloud-metadata SSRF** (see FINDINGS) |

The one real weakness — `wp_http_validate_url` allowing `169.254.169.254` (cloud
metadata) — was found by **reading** the IP-range coverage (a pattern scanner
can't see an *incomplete* allow-list), then **verified by executing** the
algorithm, then **traced** to unauthenticated reachability (`pingback_ping`). It
is a known-*class* limitation core punts to a filter, not a novel 0-day — stated
plainly.

## Why this beats reading-all *and* beats grep

- **vs reading-all:** 199× fewer tokens, interprocedural reach that chunking
  destroys, and grounded (non-hallucinated) findings.
- **vs grep:** grep is file-scoped — it can't express "request source and SQL
  sink in the *same function* with no sanitizer," or "unauthenticated hook →
  reaches a sink." XERJ can, in one query. (Measured: the SQLi triage narrowed
  11,990 functions → 4; grep hands you 51 whole files.)

## The honesty that makes it a real case study

An AI audit is only worth sharing if it reports its own failures. This one did:

1. **Substrate bugs caught mid-audit** — a dynamic-hook-registration miss and an
   OOP method-name collision, both caught because the agent noticed an *impossible*
   result and re-read the code. Fixed; the negatives became trustworthy.
2. **A real XERJ engine bug** — reverse call-graph `term` queries under-reported
   (`term` on a keyword *array* matched only the first element). The audit
   proved it, and it became an engine fix + PR: see
   [`../../research/xerj-keyword-array-term-fix.md`](../../research/xerj-keyword-array-term-fix.md).
   Until fixed, the sound interprocedural conclusions were computed over pulled
   `_source` in code, not the buggy `term` path — so they hold.
3. **No manufactured findings** — core came back clean on cap-presence,
   check-vs-use, and composition. That negative is the result.

## The reusable pattern

The method generalizes to any codebase and any invariant:

1. **Read once** to discover an invariant (escaper-last; block `169.254`;
   re-verify the object relationship).
2. **Compile it to an indexed fingerprint** (sanitizer sequence, IP-range
   coverage, cap-object-scoping).
3. **Query the fingerprint** → only violators, no code transfer.
4. **Read only the survivors** to confirm.

Steps 2–4 turned a million-token reading pass into a few-hundred-token query
(measured 2,700× on the sanitizer-composition audit). Precision comes from the
read; scale and durability come from XERJ.

## Contents

- [`PLAYBOOK.md`](PLAYBOOK.md) — **step-by-step guide with the copy-paste prompts**
  to drive an AI + XERJ through this audit, and to point it at your own codebase.
- [`COVERAGE-AUDIT.md`](COVERAGE-AUDIT.md) — **the coverage-guaranteed sink census**:
  catalog every dangerous PHP built-in, enumerate every call site by AST, *prove*
  zero gaps against grep, then AI-enrich into a queryable ledger (`sink-census/`).
- [`VERIFY-AND-TRACE.md`](VERIFY-AND-TRACE.md) — **verify the data is live in
  XERJ and trace any sink end-to-end via the API** (sink → enclosing fn → sources
  → reverse callers → authz), with `sink-census/trace_sink.py`.
- [`GADGET-CHAINS.md`](GADGET-CHAINS.md) — **POP-gadget (deserialization) hunt**:
  every magic method traced to a sink; core has **0 auto-triggered gadgets**
  (`sink-census/gadget_hunt.py`); ~3,900× cheaper than grep+read.
- [`sink-census/TAINT-ANALYSIS.md`](sink-census/TAINT-ANALYSIS.md) — **interprocedural
  taint over the XERJ call graph** (source → sink, no sanitizer on path): 70
  candidate flows on WP core, ranked, with honest real-vs-FP verification
  (`sink-census/taint_analysis.py`).
- [`sink-census/PHP-DANGEROUS-FUNCTIONS.md`](sink-census/PHP-DANGEROUS-FUNCTIONS.md) —
  the **full map of 275 dangerous PHP functions** (28 categories, taint-arg noted).
- [`sink-census/PHP-SECURITY-GUIDE.md`](sink-census/PHP-SECURITY-GUIDE.md) — the
  **complete guide**: 275 functions **+** the non-function patterns (type-juggle
  `==`, array injection `?login[]=`, SQL truncation, magic-hash, null-byte,
  second-order, TOCTOU, timing, …) with attack + detection + safe recipe.
- **Copyable Claude Code skill**: [`skill/SKILL.md`](skill/SKILL.md)
  — install it and run this whole workflow on your own codebase.
- [`ZERODAY-SWEEP.md`](ZERODAY-SWEEP.md) — **the multi-agent per-file zero-day
  sweep** (222 agents, adversarial verify): found a role-injection authz gap the
  structural graph missed. See FINDINGS.md #4.
- [`AUTHENTICATION.md`](AUTHENTICATION.md) — **the WordPress auth story across all
  4 entry surfaces** (AJAX/REST/XML-RPC/admin-post): missing / insufficient /
  broken-`==` / race checked; ~51× fewer tokens than reading the handlers.
- [`FINDINGS.md`](FINDINGS.md) — the SSRF vulnerability, with verification and
  reachability.
- [`REPRODUCE.md`](REPRODUCE.md) — exact commands to rebuild the index and rerun
  every result.
- [`IMPROVEMENTS.md`](IMPROVEMENTS.md) — the roadmap: engine, extractor, detector,
  and workflow improvements that close the case study.
- [`verify_ssrf.py`](verify_ssrf.py) — self-contained, executes the finding.
- Detectors and scripts: [`../../examples/ast-vuln-graph/`](../../examples/ast-vuln-graph/)
  (`wp_audit_index.py`, `wp_authz_graph.py`, `wp_checkuse_idor.py`,
  `wp_admin_pages.py`, `wp_ssrf_ranges.py`, `wp_compose_index.py`).
- Full journals: [`../../research/`](../../research/)
  (`wordpress-audit-with-xerj.md`, `wordpress-authz-agentic-audit.md`,
  `wordpress-read-first-findings.md`, `wordpress-verification-and-xerj-vs-grep.md`).

## Honest limitations

- XERJ **does not find bugs**; the AST taint/authz model produces candidates and
  XERJ stores/queries them. Point it at raw file chunks and its security value
  largely evaporates.
- **Recall is bounded by the model** — a source at file scope or laundered through
  `$GLOBALS` is invisible. grep has perfect literal recall and terrible precision;
  this trades recall for precision and tokens.
- The precision detectors have **known false-positive drivers** (receiver-typed
  sinks, self-scoped writes, polymorphic caps) — every candidate still needs a
  human/AI read to confirm. The value is that there are few enough to read them
  all.

## Run it yourself, then improve it

Start with [`PLAYBOOK.md`](PLAYBOOK.md) — it has the exact prompts to drive an AI
through this audit and to retarget it at your own stack (the taint model is data,
not code). Then see [`IMPROVEMENTS.md`](IMPROVEMENTS.md) for the roadmap: the
engine fixes that make the structured index strictly better than grep, the
extractor precision levers, the detector classes still to add, and the workflow
(AST-aware `autoindex`, findings-as-index, MCP-native auditing, CI mode) that
would make this a one-command capability. The invitation is to point it at your
code, add your framework's fact-lists, and compile every invariant you learn into
a fingerprint the next audit gets for free.
