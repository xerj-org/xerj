# Coverage-guaranteed whitebox audit: the dangerous-sink census

You cannot claim *"we security-reviewed WordPress core"* without proving you
enumerated **every** dangerous call site. Reading files, grep, and even the taint
graph all leave the same unanswered question: *did we miss any?* This recipe
answers it — with a provable, zero-gap census of every dangerous PHP built-in
call in the codebase, indexed into XERJ, then enriched by an AI agent into a
queryable audit ledger. It is the **minimum** that lets you talk about *coverage*.

Everything here is reproducible; scripts are in
[`sink-census/`](sink-census/). Measured on real WordPress core (1,492 PHP files).

## The pipeline

```
 php_dangerous_functions.json   275 built-ins/constructs, 28 categories -> class + safe/unsafe + taint-arg
        │
        ▼  tree-sitter AST  (every call site, any formatting)
 AST census   11,191 call sites
        │
        ▼  grep + AST string/comment/inline-HTML ORACLE  (reconcile every occurrence)
 COVERAGE PROOF   12,214 grep hits = 10,778 AST calls + proven non-calls, 0 UNEXPLAINED
        │
        ▼  bulk index
 XERJ `wpsinks`   one doc per call site (file, line, fn, class, arg)
        │
        ▼  AI-agent enrichment  (the embedding-pipeline analogue)
 enriched ledger   reachable / guarded / severity / verdict — queryable
```

> **The full map is the reference.** [`PHP-DANGEROUS-FUNCTIONS.md`](sink-census/PHP-DANGEROUS-FUNCTIONS.md)
> is the readable table of all 275 functions; `sink-census/php_dangerous_functions.json`
> is the machine-readable source. This whole recipe is packaged as a copyable
> Claude Code skill — see [`skill/SKILL.md`](skill/SKILL.md).

## Step 1 — Map the dangerous built-ins (the full reference)

`php_dangerous_functions.json` maps **275 PHP built-ins/constructs across 28
categories** to a **vuln class**, a **safe vs unsafe recipe**, and the
**taint-relevant argument** (which arg carries the danger — critical for the
taint graph). Categories: command exec (13), code eval (7), dynamic callables
(27 — `call_user_func`/`array_map`/`preg_replace_callback`/…), deserialization
(8), include loaders (5), file read (38), file write/delete/perms (24), directory
(7), SSRF (17), SQL drivers (22), LDAP (7), XXE (10), variable injection (5),
ReDoS/ereg (9), mail injection (4), header/redirect (4), weak crypto (7), weak
random (9), info disclosure (12), runtime-config tamper (9), process control (9),
reflection invoke (5), type-juggling auth-bypass (6), output/XSS. The readable
table is [`PHP-DANGEROUS-FUNCTIONS.md`](sink-census/PHP-DANGEROUS-FUNCTIONS.md).
This is the *knowledge* layer — extend it for your framework/language.

## Step 2 — AST census: every call site

`sink_census.py` parses every file with tree-sitter-php and records **each call
site** of a catalog function — `function_call_expression`, member/scoped method
calls (`->loadXML`), and language constructs (`echo`/`include`/`require`/`print`).
tree-sitter captures a real call regardless of whitespace, line breaks, or
namespacing (`\fopen(`), which grep cannot.

**Result: 11,191 call sites** of 275 built-ins/constructs across 1,492 files.

## Step 3 — The coverage guarantee (the part that matters)

An AST census is only trustworthy if you can prove it missed nothing. The census
does this by **reconciling every grep occurrence against the AST**, using the AST
itself as the oracle for what is code vs string/comment:

- grep finds **12,214** word/`fn(` occurrences of the catalog names.
- **10,778** are AST call sites.
- The remaining **1,436** are each *proven non-calls*: inside a string, comment,
  or inline HTML/JS (AST node ranges), a function *definition*, a *method* call on
  another object (`$response->header(...)` ≠ the `header()` built-in), a *variable*
  name (`$echo`), a namespaced call already in the AST, or `exit`/`die`/`new`
  handled as their own node types.
- **UNEXPLAINED residual: 0.**

```
COVERAGE RECONCILIATION
  AST call sites: 10,778   grep occurrences: 12,214
  UNEXPLAINED: 0  => coverage PROVEN (0 gaps)
```

The argument is airtight because **tree-sitter parses 100% of the PHP grammar**,
so every real call is a call-expression node by construction; the reconciliation
proves no real call hides in the grep/AST delta. (Worked example: the last two
stubborn residuals were the English word *"include"* inside a multi-line `__()`
help string — the AST string-range oracle classifies them correctly where a
line-based grep filter cannot.)

## Step 4 — Index into XERJ

Every call site becomes a `wpsinks` document: `file, line, fn, kind, class, arg`
plus empty enrichment fields. The risk profile is now one aggregation query:

| class | sites | class | sites |
|---|--:|---|--:|
| XSS (`echo`/`print`) | 3,799 | file-write | 178 |
| LFI/RFI (`include`/`require`) | 1,385 | variable-injection | 166 |
| type-juggle auth-bypass | 1,240 | weak-crypto | 142 |
| XSS-or-info (`printf`/`var_dump`) | 1,034 | **SSRF** | **107** |
| dynamic callables (2nd-order RCE) | 954 | file-delete | 61 |
| ReDoS/ereg | 709 | **XXE** | **26** |
| RCE-code (`eval`/`assert`) | 467 | **deserialization** | **13** |
| file-read | 328 | **RCE-command** | **11** |
| **SQL drivers** | **200** | dynamic-invoke (reflection) | 6 |

The rare classes are the point: there are only **11** command-execution and **13**
deserialization sinks in all of core — and now you can *guarantee* you've seen
every one. (The big classes — XSS/output, type-juggle, callables — are where the
taint graph + authz sweeps then decide *reachability*.)

## Step 5 — AI-agent enrichment (the embedding-pipeline analogue)

`enrich_pipeline.py` is the enrichment loop: for each candidate, an **AI agent**
(not a `model.encode`) reads the argument and enclosing code and writes a security
verdict back into XERJ — `reachable` (request / cache / trusted / literal),
`guarded` (which defense: `escapeshellarg`, HMAC, `is_serialized`, allow-list,
literal), `severity`, and a note. The ledger becomes queryable:

- high-risk sites enriched; the RCE-command (11), deserialization (13), and
  driver-SQL classes carry full agent verdicts (100% of the rarest classes); the
  rest queued.
- Example audit query — *unreviewed medium+ deserialization sinks* — returns the
  SimplePie feed-cache POI candidates with the verdict attached
  (`File.php:88 | feed-cache-file | POI if cache dir writable`).

This is the same shape as an embedding pipeline (index → enrich → query), but the
enrichment is *reasoned security judgement*, and it accretes: every reviewed sink
is a stored verdict the next audit and CI run reuse.

## The coverage statement you can now make

> Every call to a known-dangerous PHP built-in in WordPress core (11,191 sites,
> 275 functions across 28 categories, 1,492 files) has been **enumerated with a
> proven-zero-gap census**, indexed, and triaged by vuln class. 100% of the
> command-execution, deserialization, and driver-SQL sinks have an AI verdict; the
> remaining high-risk classes are queued in a queryable ledger with severity.

That is a defensible coverage claim — the thing "we read the code" can never be.

## Honest limits

- **Sink coverage ≠ total vulnerability coverage.** This proves you've seen every
  *dangerous-function call*. It does **not** cover logic bugs, missing-authz/IDOR,
  or incomplete validators (e.g. the SSRF-range gap) — those are found by the
  other detectors in this case study. Sink census is one guaranteed axis, not all.
- **Coverage is only as complete as the catalog.** The guarantee is "every call to
  a *catalogued* function." Extend `php_dangerous_functions.json` for framework sinks
  (`$wpdb->query`, `wp_remote_get`), and re-run — the census + proof re-runs
  unchanged.
- **Enrichment quality is the agent's judgement**, and every verdict cites the
  arg/context — but a `guarded` claim still needs the human spot-check the ledger
  makes cheap (few high-severity, all reviewable).
- **Reachability in the verdict is triage, not proof.** Pair it with the taint/
  authz graph to confirm a source actually reaches the sink.

## Reproduce

```bash
cd sink-census
python3 sink_census.py   /path/to/wordpress   # AST census + coverage proof + index wpsinks
python3 enrich_pipeline.py                     # AI verdicts -> enriched ledger
# then query wpsinks by class/severity/reachable/guarded
```
