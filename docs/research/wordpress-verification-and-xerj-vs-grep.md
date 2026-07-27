# Verifying the findings by execution flow — and an honest XERJ-vs-grep verdict

This closes the loop the earlier docs opened: **stop reasoning and verify.** Run
the actual algorithm, trace the actual caller path, and — the part that matters
for the product — measure whether XERJ's pre-built AST/index is *real help*
versus reading files and `grep`. The answer is mixed and specific, and it exposed
real XERJ bugs.

## 1. The SSRF finding, verified by executing the flow

No PHP runtime was available, so `wp_http_validate_url` (`wp-includes/http.php`)
was ported **line-for-line** to Python — same scheme filter, same
`strpbrk(host, ':#?[]')`, same octet range test — and executed with real
`socket.gethostbyname`. Run on live payloads:

| payload | executed verdict |
|---|---|
| `http://127.0.0.1/`, `http://10.1.2.3/`, `http://192.168.0.1/` | REJECTED |
| `http://[::1]/` | REJECTED (`strpbrk` catches `:`) |
| **`http://169.254.169.254/latest/meta-data/…`** | **ALLOWED** |

The gap is not a reading artifact — the algorithm, executed, lets the AWS/GCP/Azure
metadata IP through. (A hostname like `metadata.google.internal` fails closed in a
non-cloud sandbox only because it doesn't resolve; in GCP it resolves to
`169.254.169.254`, which passes.)

## 2. Reachability, traced to user input

A gap in a validator only matters if user input reaches it. Tracing the callers
of the safe-remote path (`wp_safe_remote_get/post/request` → `WP_Http::request`
with `reject_unsafe_urls` → `wp_http_validate_url`):

- **`WP_XMLRPC_Server::pingback_ping`** — the XML-RPC pingback source URL is
  attacker-supplied and fetched. Classic SSRF vector; the 169.254 gap turns it
  into metadata access.
- **`WP_REST_URL_Details_Controller::get_remote_url`** — REST endpoint, user
  `url` parameter (author-level cap).
- oEmbed discovery, `download_url`, pingback discovery — all fetch caller-provided
  URLs.

So the finding is **real and reachable**: unauthenticated (pingback, where
XML-RPC is enabled) and low-privilege (REST url-details) paths both reach the
gap. Honest severity caveat unchanged: it is a *known-class* limitation core
punts to the `http_request_host_is_external` filter; exploitation needs the SSRF
path enabled in a cloud environment.

## 3. Is XERJ's pre-built index real help vs. reading + grep?

This was tested directly on the reachability question ("who calls
`wp_safe_remote_get`"), and the pre-built index **lost**:

| method | result | verdict |
|---|--:|---|
| `grep -rl wp_safe_remote_*` | 14 files | complete |
| XERJ full-text (`match` on `code`) | 9 functions | complete, function-scoped |
| **XERJ pre-built call graph (`term` on `calls`)** | **1** | **silent false negatives** |

A false "not reachable" is the worst error in a security audit, and the
structured call-graph produced exactly that. The cause is a **real XERJ
query-layer bug**, not my extraction (the data *is* in `_source`):

- XERJ **ignores the `keyword` mapping and analyzes array keyword fields** —
  `wp_safe_remote_get` is tokenized to `[wp, safe, remote, get]`, so
  `{term:{calls:"wp_safe_remote_get"}}` → **0**, while `{match:{…}}` → 8.
- **Boolean `term` is matched as a string** — `{term:{has_source:true}}` → **0**,
  but `{term:{has_source:"true"}}` → **440**.

`term` on a *scalar* keyword (`func`) and on a *single-token* array value
(`sinks:"sql"`) happen to work, which is why the bug is easy to miss — it fails
silently only on the multi-token and boolean cases.

### What this means for the earlier findings

The interprocedural conclusions (authz sweeps, REST IDOR, check-vs-use,
composition) were computed by **pulling `_source` and traversing/regexing in
Python**, not by XERJ `term` graph queries — `_source` is always the true stored
data, so those conclusions stand. Any conclusion that leaned on a raw XERJ
`term`/boolean match should be re-checked via `match` or a `_source` scan. In
effect, the sound audits used XERJ as a **document store + full-text layer**, not
as a structured graph engine — because the structured engine is unreliable here.

### The honest verdict

- **For raw reachability** ("who calls X", "where is Y used"): `grep` and XERJ
  full-text are complete and reliable; the **pre-built structured call graph is
  worse than grep** today (silent under-reporting). Grep needs no index and no
  server.
- **Where the pre-built index genuinely wins**: the **computed within-function
  fingerprints** grep *cannot express* — sanitizer-sequence order, SSRF range
  coverage, cap-object-scoping. Those delivered the measured ~2,700× token
  reduction and are the real value — *but only when queried via `match` or
  filtered in code*, never via the broken `term`/boolean path.
- **Net**: XERJ is real help as (a) a document store, (b) a full-text layer ≈
  grep-with-function-scope, and (c) a home for precomputed semantic fingerprints.
  Its "structured database" surface (exact keyword/boolean `term`) is **buggy and
  must not be trusted for security reachability** until fixed.

## 4. Concrete XERJ improvements this surfaced

1. **Honor `keyword` semantics** — store keyword fields (scalar *and* array)
   un-analyzed; `term` must match the exact stored value. This alone would make
   the pre-built call graph beat grep (structured *and* complete).
2. **Fix boolean `term`** — `{term:{field:true}}` must match JSON booleans, not
   only the string `"true"`.
3. **A code-aware text analyzer** (already noted): keep `_` and `->` as token
   characters so `esc_like`, `wp_unslash`, `$wpdb->query` are searchable as
   identifiers, not shredded into `esc`+`like`.

Until (1)–(2) land, the honest guidance is: **use XERJ for full-text navigation
and precomputed fingerprints; do interprocedural/exact-match logic over pulled
`_source` in code; and cross-check reachability with grep.** With them fixed, the
pre-AST index would move from "sometimes worse than grep" to "strictly better" —
structured, complete, and cheap.
