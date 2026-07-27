---
name: xerj-security-audit
description: Coverage-guaranteed whitebox security audit of a codebase using XERJ + tree-sitter AST. Use when the user wants to security-review PHP (or other-language) code with a provable "we enumerated every dangerous call" guarantee, or asks to run the WordPress-style sink census / audit. Drives an index-once, query-read-reason loop; proves zero gaps against grep; enriches findings into a queryable ledger.
---

# XERJ whitebox security audit

Run a security review where **XERJ is the agent's second brain**: index the code
once, then interrogate it — never load whole files. The deliverable is a
*defensible coverage claim*, not a vibe. Scripts referenced below live in
`docs/case-studies/wordpress-security-audit/sink-census/` (copy them alongside
this skill to reuse in another repo).

## When to use
- "Security-review this codebase / WordPress plugin / PHP app with coverage."
- "Find the dangerous calls / sinks and prove we didn't miss any."
- "Run the sink census / audit ledger."

## Prerequisites (state them, then set up)
- XERJ running, ES-compatible, on `http://127.0.0.1:9200` (`xerj --insecure --data-dir ./data`).
- `pip install tree-sitter tree-sitter-php` (+ the grammar for the target language).
- The target source tree on disk.

## The loop (do these in order; report honestly at each step)

### 1. Map the dangerous functions (knowledge)
Use `php_dangerous_functions.json` — 275 PHP built-ins/constructs across 28
categories (command/code exec, `unserialize`, include loaders, file r/w/delete,
SSRF, SQL drivers, XXE, callables, variable-injection, reflection, weak crypto/
random, type-juggling, …), each with vuln class + the **taint-relevant argument**.
For another framework/language, extend this map — it is data, not code.

### 2. Census every call site (AST)
`python3 sink_census.py <src>` parses every file with tree-sitter and records

### 2b. Census the dangerous PATTERNS (the class a sink list misses)
`python3 pattern_census.py <src>` AST-detects the non-function vulns — loose `==`
/ magic-hash, non-strict `in_array`, `strcmp`-with-array (`?login[]=`), `@`
suppression, variable-variables, unsafe `setcookie`, `switch`-on-request,
Host-header trust, ORDER-BY injection — into `wppatterns`. Semantic classes a
census can't decide (SQL truncation, charset SQLi, second-order, phar, TOCTOU,
upload-exec, is_numeric bypass, regex-anchor, session-fixation, wrong-context
XSS) are catalogued in `php_dangerous_patterns.json` with detection guidance and
swept by reasoning. FULL REFERENCE: `PHP-SECURITY-GUIDE.md` — every function AND
pattern mapped to attack + detection + safe recipe.
**every** call site of a catalogued function (calls, methods, constructs like
`echo`/`include`/backtick, `new ReflectionFunction`). tree-sitter captures real
calls regardless of formatting/namespacing — grep cannot.

### 3. PROVE coverage (the guarantee — do not skip)
The census reconciles **every grep occurrence against the AST**, using AST
string/comment/inline-HTML node ranges as the oracle. A clean run prints:
```
UNEXPLAINED: 0  => coverage PROVEN (0 gaps)
```
If UNEXPLAINED > 0, EVERY residual must be resolved (it is either a real call the
extractor missed — fix the catalog/node-handling — or a proven non-call). Never
report coverage while residuals are unexplained.

### 4. Index + triage (XERJ `wpsinks`)
The census bulk-indexes one doc per call site (`file,line,fn,class,arg`). Query
the risk profile; the rare high-value classes are the must-review set (in WP core:
RCE-command 11, deserialization 13, SQL 200, SSRF 107). You can now *guarantee*
you've seen every one.

### 5. AI-enrich into a ledger (`enrich_pipeline.py`)
For each high-risk site: read the arg + enclosing code, decide `reachable`
(request/cache/trusted/literal), `guarded` (which defense), `severity`, and a
`verdict`; write it back to `wpsinks`. This is the embedding-pipeline analogue,
but the enrichment is reasoned security judgement, and it accretes across runs.
Enrich 100% of the rare classes; queue the long tail.

### 5b. Trace any sink through the API (`trace_sink.py <file> <line>`)
Joins `wpsinks -> wpaudit -> wpauthz -> reverse callers` with NO local file
access: the sink + verdict, the enclosing function's sources/sinks/sanitizer, its
authz guards, and who calls it + whether they read a request source. The verdict
flags a SOURCE->SINK candidate when a source and an unsanitized sink coexist in a
function. (Reverse callers scan `_source` `calls` arrays because of the
array-`term` engine bug, and restrict common method names like `load`/`query` to
the same file to avoid class collision.)

### 6. Reason about the real bugs (the parts a census can't see)
Sink coverage ≠ total coverage. Also sweep, using the other detectors in the case
study: object-scoped authorization / IDOR (`wp_authz_graph.py`,
`wp_checkuse_idor.py`), validator *completeness* (`wp_ssrf_ranges.py` — an
incomplete allow/deny list, e.g. SSRF missing `169.254`), and sanitizer
*composition* order (`wp_compose_index.py`). Verify each candidate by reading,
then by executing the flow. Trace reachability to real input.

### 6b. Interprocedural taint (`taint_analysis.py`)
Once the AST is exported, run source->sink taint over the XERJ call graph: every
function that reads a request source, walked to every dangerous sink it can reach
with no sanitizer on the path (ranked by severity, full path per flow). This is
high-recall REACHABILITY triage, not precise per-argument data-flow — the SQL
flows include WP_Query (safe) and intval-guarded sinks, so read each candidate.
See `TAINT-ANALYSIS.md`.

### 6c. POP-gadget hunt (`gadget_hunt.py`)
Trace every magic method (__wakeup/__destruct/__toString/__call/...) through the
call graph to a dangerous call — the deserialization gadget surface. Flag the
AUTO-triggered ones (unserialize/free/print) as the live gadgets. WP core: 0.
See `GADGET-CHAINS.md`.

## Honesty rules (these make the result trustworthy)
1. Precision from the graph/facts; full-text only to navigate. FTS over raw code
   is noisy for security.
2. Verify by reading, then by executing (port the flow to a runnable language if
   no runtime). No claim from memory of the framework — cite pulled `file:line`.
3. Trace reachability to attacker-controlled input before calling something a bug.
4. No manufactured findings — a clean negative is a valid result.
5. Report substrate/engine bugs: an impossible query result means the tool is
   wrong, not reality. (Known XERJ caveats today: `term` on a keyword *array*
   matches only the first element — traverse `_source` in code for reverse
   call-graphs; boolean `term` matches as a string — use `"true"`.)
6. State the coverage claim precisely: *"every call to a catalogued dangerous
   function is enumerated with a proven-zero-gap census"* — not "we read the code."

## The coverage statement this earns
> Every call to a known-dangerous built-in in the target (N sites, M functions,
> K files) is enumerated with a proven-zero-gap AST census, indexed, and triaged
> by vuln class; 100% of the command-exec / deserialization / SQL sinks carry an
> AI verdict.

## Reuse in another repo
Copy `docs/case-studies/wordpress-security-audit/sink-census/*` and this skill.
Point `sink_census.py` at your tree; extend `php_dangerous_functions.json` (or add
a language grammar + a parallel catalog) for your stack. The proof step and the
enrichment loop are language-agnostic.
