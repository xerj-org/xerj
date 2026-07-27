# Auditing real WordPress with XERJ: an honest field test

**Question this answers:** when an AI agent audits a large codebase for security
bugs, does XERJ actually beat `grep` + reading the code — on tokens, on quality,
on hallucination? Not in theory. Measured, on **real WordPress core** (1,492 PHP
files, ~619k lines), with the agent (me) doing the reasoning and XERJ as the
retrieval substrate.

Short version: **XERJ cut the audit from ~864k tokens of grep-and-read to ~2,150
tokens — ~400× — and I read *every* candidate instead of skimming.** But the
narrowing is only as sound as the taint model behind it, and this run found
4/4 false positives from one fixable modeling gap. Both halves of that are the
finding.

## Setup: build the substrate, then interrogate it

Autoindex over raw files gives you FTS over code — and FTS over code is *noisy
for security* (a function is "relevant" to `esc_html` because it contains the
word, not because it's safe). So the substrate is **function-level facts**, one
XERJ document per function, carrying the code text *and* structured taint fields:

```
wpaudit   11,990 docs   { file, func, line, code, sources[], sinks[],
                           calls[], sanit, has_source, has_sink }
wphooks    1,343 docs   { hook, callback, file, line, unauth }
```

Built once by parsing every file with tree-sitter-php (100% parse rate; phply
skipped ~37%) and bulk-loading via XERJ's ES-compatible `_bulk`. ~3.6s. From here
I never load a file wholesale — I *query*.

## The audit journal (real queries, real reasoning)

### 1. Where does unauthenticated input even enter?

```
GET wphooks  { term: { unauth: true } }        →  2 hits (of 1,343)
```

All of WordPress core registers exactly **two** unauthenticated AJAX endpoints:
`generate-password` and `heartbeat`. Pulled both callbacks from `wpaudit`:

- `wp_ajax_nopriv_generate_password` — no sources, no sinks; returns
  `wp_generate_password()`. No attacker input. Dead end.
- `wp_ajax_nopriv_heartbeat` — reads `$_POST`, flagged "sanitized", **no direct
  sink** — but its `calls` list contains `apply_filters`/`do_action`. That flag
  would make a pattern scanner say "clean." Reading the body says otherwise:

```php
$data     = wp_unslash( (array) $_POST['data'] );          // NOT sanitized — unslash only
$response = apply_filters( 'heartbeat_nopriv_received', $response, $data, $screen_id );
```

Heartbeat is a **dispatcher**, not a sink. Raw unauthenticated `$_POST['data']`
flows *out through a filter* to whatever hooks `heartbeat_nopriv_received`. So
the real question isn't "is heartbeat safe" — it's "who listens":

```
GET wphooks  { terms: { hook: [heartbeat_nopriv_received, ...] } }   →  0 nopriv listeners in core
```

Core registers **zero** listeners on the nopriv filter (the `heartbeat_received`
listeners — `wp_refresh_post_lock`, `heartbeat_autosave` — are the *authenticated*
variant). **Conclusion: core's unauthenticated input surface terminates in
nothing. The risk is entirely "if a plugin hooks `heartbeat_nopriv_received`, it
owns raw unauth input" — and that is exactly where a plugin auditor should look.**

That conclusion came from tracing control flow across three files, not from
matching a pattern. A `grep` for `$wpdb` never goes near it.

### 2. The classic bug class: SQLi reachable from request input

```
GET wpaudit  { has_source:true  AND  sinks:sql  AND  sanit:false }   →  4 candidates (of 11,990)
```

Then I *read all four*. Every one is a **false positive**, all from the same
cause:

| candidate | flagged "SQL sink" | reality |
|---|---|---|
| `wp_ajax_query_attachments` | `$count_query->query()` | `WP_Query::query()` — structured args, auth-gated, key allow-list |
| `class-wp-users-list-table::prepare_items` | `$wp_user_search->get_results()` | `WP_User_Query::get_results()` — not `$wpdb` |
| `class-wp-ms-users-list-table::prepare_items` | `$wp_user_search->get_results()` | same |
| `SimplePie/Sanitize::sanitize` | `$xpath->query('//comment()')` | `DOMXPath::query()` on a **literal** — not even SQL |

**Single root cause: sink matching by method name is blind to the receiver's
type.** `->query()` is dangerous on `$wpdb` and harmless on `WP_Query`,
`WP_User_Query`, or `DOMXPath`. That is the next precision lever, and I only
learned it by *reading the code the tool pointed me at* — which was affordable
because there were four functions, not fifty-one files.

WordPress core is clean on this axis. Expected: it is the most-audited PHP on
earth. The value here is the *method and its cost*, not a CVE.

## The measured comparison

Same question — "request input reaching SQL" — three ways:

| path | what you read | tokens | recall | hallucination pressure |
|---|---|--:|---|---|
| load whole tree | everything | ~5,200,000 | — | can't read it → guesses |
| `grep` source+sink files | 51 files, in full | ~864,000 | high | forces skimming at scale |
| **XERJ agent** | 4 function bodies + a few fact rows | **~2,150** | model-bounded | low — every claim cites pulled code |

`grep` is file-scoped. It can tell you a file contains `$_GET` *and* a
`->query(`; it cannot tell you they're in the same function with no sanitizer
between them, so it hands you 51 whole files (864k tokens) and you read them all
or you skim. XERJ expresses `has_source AND sinks:sql AND sanit:false` as one
query and returns 4 functions. ~400× fewer tokens **and higher quality**, because
2,150 tokens is few enough to read every candidate exactly instead of
pattern-matching your way through 864k.

## Honest verdict: where XERJ really helps, and where it doesn't

**Where it genuinely wins**

- **Structured triage grep can't express.** "Source and sink in the same
  function, no sanitizer on the path" (`has_source AND sinks:sql AND sanit:false`)
  is a single query. This is the core win and it is real.
- **Cross-file resolution.** "Which of 1,343 hooks are unauthenticated" → 2;
  "who listens on this filter" → 0. Manually cross-referencing registration
  strings to callbacks across files is hours of error-prone work; here it's two
  queries.
- **Anti-hallucination by construction.** Every judgment above cites a snippet
  XERJ *returned* (`file:line` + code), not something recalled from training. At
  2,150 tokens I read the actual code; at 864k a model skims and starts guessing
  ("looks like a list table, probably fine") — which is where audit hallucinations
  come from.
- **The economics compound with scale.** The bigger the codebase, the more
  absurd whole-load and grep-and-read become, and the flatter XERJ's cost stays.

**Where it does NOT help — stated plainly**

- **XERJ does not find bugs.** The AST taint extractor produces the candidates;
  XERJ stores and queries them. Point XERJ at raw file chunks instead of
  function-level facts and its security value largely evaporates — FTS over code
  is noisy. The value is in the *substrate you build*, then XERJ navigating it.
- **Recall is bounded by the taint model.** XERJ can only surface what the facts
  captured. A source at file scope, taint laundered through `$GLOBALS`, or a
  wrongly-cleared sanitizer flag → XERJ never shows it and the agent never reads
  it. `grep` has perfect literal recall and terrible precision; XERJ trades recall
  for precision and tokens. Know which side of that trade your audit needs.
- **Precision today is not good enough to trust blindly** — 4/4 false positives
  this run, all from receiver-type-blind sinks. XERJ can't fix that; the extractor
  must (resolve the receiver: is it `$wpdb`?). The win is that reading 4 to clear
  them is cheap.
- **Small codebases don't need it.** For one plugin, grep + read-it-all is
  simpler and fine. XERJ's value turns on somewhere in the low thousands of
  functions — below that, the substrate-building cost isn't repaid.

## The one-line takeaway

For a large-codebase security audit, XERJ's real job is **turning an
unreadable haystack into a read-every-straw shortlist** — and that is exactly
what shrinks both the token bill and the hallucination surface. It is a
force-multiplier on a taint model, not a replacement for one. Ship the
receiver-typed sink model next; that's what turns this run's 4 false positives
into real signal.
