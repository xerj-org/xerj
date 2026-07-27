# Verify the data in XERJ, and trace a sink via the API

This closes the loop: proof that the census/enrichment data is **live and correct
in XERJ**, and that you can **trace any sink end-to-end through the API** — the
capability the workflow promises. Every command below runs against the live
indices; nothing reads local files.

## 1. The indices are live

```bash
curl -s 'http://127.0.0.1:9200/_cat/indices?h=index,docs.count' | grep '^wp'
```
| index | docs | what it holds |
|---|--:|---|
| `wpsinks` | 11,191 | every dangerous-call site + AI enrichment |
| `wpaudit` | 11,990 | every function: code + taint facts (sources/sinks/calls) |
| `wpauthz` | 11,990 | per-function cap/nonce arg-shapes + call edges |
| `wphooks` | 1,343 | hook registrations (unauth entry points) |
| `wpcompose` | 2,445 | sanitizer-sequence fingerprints |

## 2. The enrichment is correct

```bash
curl -s localhost:9200/wpsinks/_search -H 'Content-Type: application/json' -d '{
 "size":0,"aggs":{"by_sev":{"terms":{"field":"severity"}},
 "rare":{"filter":{"terms":{"class":["RCE-command","deserialization"]}},
   "aggs":{"verdict":{"filter":{"bool":{"must_not":{"term":{"severity":"review"}}}}}}}}}'
```
```
total sink sites: 11,191
by severity: {review: 1472, medium: 17, low: 9, none: 4}
rare classes (RCE-command + deserialization): 24/24 carry a full verdict (100%)
```

Every enriched record stores the reasoned explanation — proof it's queryable:
```
File.php:88   [medium]  feed-cache-file        | simplepie-filterediterator
    "reads cache file then unserialize; POI if cache dir writable"
Memcache.php:96 [medium] feed-cache-backend    | simplepie-filterediterator
    "feed cache POI if attacker controls cache backend/content"
```

**Honest scope of enrichment:** 100% of the *rare* classes (RCE-command 11/11,
deserialization 13/13) carry a full agent verdict; the large classes (SQL 200,
SSRF 107, callables 954, …) carry class + severity and are `severity:review`
(queued for per-site agent read). The count is exact and queryable — no
overclaim. (Re-running `sink_census.py` resets enrichment; re-run
`enrich_pipeline.py` after.)

## 3. Trace a sink end-to-end through the API

`trace_sink.py <file> <line>` joins the indices with no local file access:

```
wpsinks -> the sink + AI enrichment (class, arg, reachable, guarded, verdict)
wpaudit -> the ENCLOSING function (sources / sinks / sanitizer / calls)
wpauthz -> that function's cap/nonce guards
wpaudit -> REVERSE callers (who calls it) + whether they read a request source
```

### Worked example — the REST widget-instance `unserialize`

```
$ python3 trace_sink.py wp-includes/rest-api/endpoints/class-wp-rest-widgets-controller.php 589

■ SINK  ...class-wp-rest-widgets-controller.php:589
    fn=unserialize  class=deserialization  arg=( $serialized_instance )
    ENRICHMENT: severity=medium  reachable=REST-auth-optin  guarded=show_instance_in_rest-gate
■ ENCLOSING FUNCTION  save_widget()  (line 526)
    reads sources: ['$_POST','$_REQUEST']   sanitizer-in-fn: False   other sinks: ['deser','lfi']
    AUTHZ: object-scoped-cap=False  nonce=False
■ REVERSE CALLERS of save_widget(): 2
    ::create_item   ::update_item
■ TRACE VERDICT: enclosing fn reads ['$_POST','$_REQUEST'] with a sink and no in-fn
    sanitizer -> SOURCE->SINK candidate (confirm arg flow + guards)
```

The trace correctly surfaces that request input reaches a deserialization sink in
`save_widget` — then you read the actual guard (the `show_instance_in_rest`
opt-in + the widget hash) to confirm or clear it. That is the whole loop: the
graph points, you read.

## Honest limitations of the trace (documented, not hidden)

- **Reverse callers use `_source` scans, not `term` queries.** XERJ's `term` on a
  keyword *array* matches only the first element (the engine bug this project
  filed — `research/xerj-keyword-array-term-fix.md`), so `trace_sink.py` scans the
  `calls` arrays in `_source` in code. When that fix lands, the reverse-caller step
  becomes a single `{"term":{"calls":"<fn>"}}` query.
- **Bare method names collide across classes.** `load`/`query`/`get`/`save` are
  defined by many classes; the trace restricts reverse-caller resolution for those
  to the same file and says so in the output. Class-qualified resolution (the
  extractor improvement in `IMPROVEMENTS.md`) removes the restriction.
- **The verdict is triage, not proof.** "SOURCE→SINK candidate" means a source and
  a sink coexist unsanitized in the function — you still confirm the argument
  actually flows and check the real guard. This is the same discipline the whole
  case study uses: verify by reading, then by executing.

## Reproduce

```bash
cd sink-census
python3 sink_census.py   /path/to/wordpress   # census + coverage proof + index wpsinks
python3 enrich_pipeline.py                     # AI verdicts -> enriched ledger
python3 trace_sink.py <file> <line>            # trace any sink through the API
```
