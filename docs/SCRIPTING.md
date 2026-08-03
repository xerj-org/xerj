# Painless scripting in XERJ

XERJ ships a small Painless interpreter. It is not a port of Elasticsearch's
Painless and it is not a general scripting runtime. It is a purpose-built
interpreter that covers the script shapes the ES-compat suite and real
dashboards send: predicates, scoring expressions, runtime-field `emit()`
scripts, and `ctx._source` update statements.

Everything below is read out of the implementation:

- interpreter: `engine/crates/xerj-engine/src/painless.rs`
- HTTP entry points: `engine/crates/xerj-api/src/es_compat.rs`
- query parsing: `engine/crates/xerj-query/src/parser.rs`

If a construct is not listed here, assume it is not implemented. The
interpreter rejects what it does not understand rather than guessing.

## Where scripts run

Each row was traced from the request body to the call into the interpreter.

| Surface | Request shape | Evaluated at |
|---|---|---|
| `script` query | `{"query": {"script": {"script": {"source": "...", "params": {}}}}}` | `xerj-engine/src/index.rs:26502` |
| `script_score` query | `{"query": {"script_score": {"query": ..., "script": {"source": "..."}}}}` | `xerj-engine/src/index.rs:33162` |
| `function_score` with a `script_score` function | `functions: [{"script_score": {"script": {...}}}]` | `xerj-engine/src/index.rs:33162` |
| `script_fields` | `{"script_fields": {"name": {"script": {"source": "..."}}}}` | `xerj-api/src/es_compat.rs:11051` |
| Runtime fields | `{"runtime_mappings": {"name": {"type": "...", "script": {"source": "emit(...)"}}}, "fields": ["name"]}` | `xerj-api/src/es_compat.rs:10263`, and inside `top_hits` at `xerj-engine/src/aggs.rs:9395` |
| Script rescore | `{"rescore": {"script": {"script": {"source": "..."}}}}` | `xerj-engine/src/index.rs:28362` |
| Script-bucketed `terms` agg | `{"aggs": {"a": {"terms": {"script": {"source": "..."}}}}}` | `xerj-engine/src/aggs.rs:3292` |
| `terms_set.minimum_should_match_script` | `{"terms_set": {"f": {"terms": [...], "minimum_should_match_script": {"source": "params.num_terms"}}}}` | `xerj-engine/src/index.rs:26575` |
| Scripted update | `POST /{index}/_update/{id}` with a `script` | `xerj-api/src/es_compat.rs:15160` |
| Scripted update by query | `POST /{index}/_update_by_query` with a `script` | `xerj-api/src/es_compat.rs:15160` |
| Standalone execute | `POST /_scripts/painless/_execute` | `xerj-api/src/es_compat.rs:24974` |

Two things that look like script surfaces and are not:

- **Sorting by script is not implemented.** `parse_sort_field_spec`
  (`xerj-query/src/parser.rs:2833`) reads field names, order, mode, missing and
  format. There is no `_script` sort branch, so a `_script` sort entry is
  treated as a field named `_script` and no script runs.
- **Stored Painless scripts are not supported.** `PUT /_scripts/{id}`
  (`xerj-api/src/es_compat.rs:24804`) stores mustache **search templates**, and
  `GET` returns them with `"lang": "mustache"`. A query that references a script
  by `id` instead of `source` is rejected by the parser, which requires
  `script.script.source` (`xerj-query/src/parser.rs:1146`).

## The implemented subset

### Context values

| Expression | Meaning |
|---|---|
| `doc['field'].value`, `doc.field.value` | first value of the field. Errors when the field is missing, with the same message ES 7.0+ uses |
| `doc['field'].size()`, `.length`, `.empty` | defined on a missing field, so `doc['x'].size() == 0 ? 0 : doc['x'].value` is the way to guard |
| `params.name`, `params['name']` | value from the script's `params` object |
| `params['_source']` | the document as an object |
| `_score` | the document's current score |
| `emit(value)` | records a runtime-field value. Call it more than once for a multi-valued field |

`doc[...]` returns null in `POST /_scripts/painless/_execute`, because there is
no document in standalone execution.

### Literals, operators, statements

- Literals: integer, float, string, `true`, `false`, `null`. There are no array
  or map literals; a list has to arrive through `params`.
- Arithmetic `+ - * / %`, comparison `< <= > >= == !=`, logical `&& || !`,
  ternary `? :`, unary `-` and `!`. `+` concatenates when either side is a
  string.
- Declarations `double x = ...`, `int`, `long`, `float`, `boolean`, `String`,
  `def`, `var`. Types are parsed and discarded; the interpreter is dynamically
  typed.
- `if (...) { ... } else { ... }`, blocks, `;` separators, explicit
  `return x;`, and implicit return of the last statement's value.
- `Math.max`, `min`, `abs`, `log`, `log10`, `sqrt`, `pow`, `exp`, `floor`,
  `ceil`, `round`, `Math.PI`, `Math.E` (`painless.rs:2490`).
- `dotProduct(params.q, 'field')` over a numeric vector field, or over a
  literal array supplied in `params`.
- On strings: `.length`, `.toString()`, `.toLowerCase()`, `.toUpperCase()`, and
  the date accessors `getHour`, `getMinute`, `getSecond`, `getDayOfMonth`,
  `getMonthValue`, `getYear`, `getDayOfWeek`, `getDayOfWeekEnum`,
  `getDisplayName`. On objects: `.toString()`, `.size()`, `.isEmpty()`, and
  member lookup. On arrays: `.size()`, `.length`, `.isEmpty()`
  (`painless.rs:2122-2165`).

### Local functions and lambdas

Both shipped and are exercised by the limit tests. They are not described
anywhere else in the docs, so in detail:

- A top-level declaration `<type> name(<type> arg, ...) { ... }` becomes a
  callable value. Parameter and return types are parsed and discarded
  (`painless.rs:1379`).
- A lambda literal is `(a, b) -> expr` or `(a, b) -> { ... }`
  (`painless.rs:1414`). It evaluates to a closure value that can be stored in a
  variable or passed as an argument.
- A closure is invoked either by calling its name directly, `compare(a, b)`, or
  through **any** `.method(args)` call on the value: `s.get()`, `fn.apply(x)`,
  `pred.test(x)`. The method name is ignored and only the positional arguments
  matter (`painless.rs:2113`). That covers `Supplier`, `Function`,
  `BiFunction` and `Predicate` shapes without the interpreter knowing about any
  of them, which is what OpenSearch's UBI dashboard scripts need.
- A closure body runs in a fresh scope containing only its bound parameters. It
  cannot read the caller's locals; `doc`, `params` and `_score` still resolve,
  because they come from the evaluation context rather than the enclosing scope
  (`painless.rs:1764`).

### Not implemented

`for`, `while`, `break`, `continue`, `new` and `instanceof` are recognised by
the tokenizer (`painless.rs:469`) but no statement or expression parser accepts
them, so a script using any of them fails to parse. There are no loops of any
kind. There are no array or map literals, no field assignment through
`doc[...]`, no regular expressions, no `Debug`/`Logger`, and no Java standard
library beyond the methods listed above. Any unsupported construct is an
evaluation error, and what happens next depends on the surface (see
"Error handling" below).

## Resource limits

These are the constants in `painless.rs`. They are enforced, not advisory.

| Limit | Constant | Value | Scope |
|---|---|---|---|
| Script source length | `MAX_SCRIPT_LEN` (`:643`) | 65,536 bytes (64 KiB) | per script |
| Parser nesting depth | `MAX_PARSE_DEPTH` (`:631`) | 100 | per script |
| Expression evaluation depth | `MAX_EVAL_DEPTH` (`:638`) | 500 | per script |
| Closure call nesting depth | `MAX_CALL_DEPTH` (`:724`) | 32 | per evaluation |
| Closure invocations | `MAX_CALL_COUNT` (`:734`) | 10,000 | per evaluation |
| Work-unit budget | `MAX_SCRIPT_OPS` (`:824`) | 5,000,000 | per evaluation |
| Wall-clock slice, ceiling | `MAX_EVAL_SLICE` (`:857`) | 500 ms | per evaluation |
| Wall-clock slice, floor | `MIN_EVAL_SLICE` (`:873`) | 100 ms | per evaluation |
| Single string value | `MAX_PAINLESS_STRING_LEN` (`:653`) | 1,048,576 bytes (1 MiB) | per evaluation |
| Compiled-script cache | `MAX_SCRIPT_CACHE_ENTRIES` / `MAX_SCRIPT_CACHE_SRC_BYTES` (`:1571`, `:1577`) | 128 entries, 512 KiB of source | per thread |

An **evaluation** is one script run against one document. A search evaluates
its scripts once per document, per clause, per aggregation bucket, so read
"per evaluation" literally.

The **work-unit budget** is the one that bounds cost rather than shape. A unit
is one interpreter step, plus one unit per 64 bytes (`BYTES_PER_OP`,
`painless.rs:804`) of any value a step produces or copies. That pricing exists
because `params.x`, `params['_source']` and `doc['x'].value` each materialise a
copy behind an expression that looks like constant work. The counter is
deterministic, with no clock in it, so a trip is reproducible on any machine.

The **wall-clock slice** is the backstop for work the counter prices too
cheaply. Each evaluation's slice is the enclosing request's remaining time,
clamped into the 100 ms to 500 ms range above, and it is fixed before any work
is charged (`PainlessCtx::new`, `painless.rs:158`). The clock is read once per
1,024 work units (`OPS_PER_CLOCK_CHECK`, `painless.rs:849`), so an evaluation
can overshoot its deadline by one sampling window plus the step in flight.

Two figures recorded next to those constants and pinned by tests in
`engine/crates/xerj-engine/tests/painless_cpu_budget.rs`: the largest benign
script the 64 KiB source cap admits costs 34,407 work units, which leaves the
5,000,000 ceiling a 145x margin; and the four adversarial shapes that motivated
the budget cost 9.03 s, 37.35 s, 17.17 s and 1.13 s **per document** without it,
and 97 ms, 109 ms, 168 ms and 155 ms with it.

## What a caller sees when a limit trips

Source length, parse depth and evaluation depth are checked before anything
runs. `check_script_limits` (`painless.rs:1665`) compiles a script without
evaluating it and reports only those three, and every search entry point walks
the request body for scripts
under `script` and `*_script` keys before executing
(`es_compat.rs:5221`). The walked fields are exactly `query`, `rescore`,
`sort`, `script_fields`, `runtime_mappings` and `aggs`/`aggregations`
(`GuardedField::ALL`, `es_compat.rs:5326`), and the same set is applied on
`_search`, scroll and async search (through `build_search_request`,
`es_compat.rs:5451`) and on `_msearch`, `_search/template` and
`_msearch/template` (through the raw-body resolver, `es_compat.rs:18231`,
`:24476`, `:24668`).

A violation caught by that guard is a 400 before any document is touched. The
exception type is not the same on every entry point, so check the status rather
than the type:

- `_search`, scroll, async search and `_search/template` raise
  `XerjError::invalid_query`, which the error layer renders as HTTP 400 with
  `"type": "search_phase_execution_exception"` and the limit message as
  `reason` (`es_compat.rs:5452`, `:24477`; mapping table in
  `xerj-api/src/error.rs:269`).
- `_msearch` and `_msearch/template` fail only the offending sub-request, with
  `{"error": {"type": "illegal_argument_exception", "reason": "..."},
  "status": 400}` in that item's slot and the rest of the batch still running
  (`es_compat.rs:18231`, `:24668`).

The remaining limits depend on the data and can only trip mid-evaluation. Those
faults are recorded into a task-local sink installed for the whole request
(`with_script_fault_capture`, `painless.rs:977`), and the handler turns the
first one into an error response instead of serving a substituted value. That
response is HTTP 400 with this body (`script_limit_response`,
`es_compat.rs:5257`):

```json
{
  "error": {
    "root_cause": [{ "type": "script_exception", "reason": "<reason>" }],
    "type": "script_exception",
    "reason": "<reason>"
  },
  "status": 400
}
```

`_msearch` embeds that same object as one sub-response inside its 200 envelope
(`script_limit_error_value`, `es_compat.rs:5268`).

Endpoints that run a search internally do not carry the up-front guard, but
they do refuse rather than report a truncated result when a limit trips during
matching: `_count` (`es_compat.rs:15390`), `_reindex` (`:17738`),
`_delete_by_query` (`:20257`) and `_update_by_query` (`:20377`) all return the
`script_exception` body instead of a count, a copy or a delete count taken over
a fail-closed subset.

The `reason` is one of these exact strings (`is_resource_limit_error`,
`painless.rs:896`):

| Limit | `reason` |
|---|---|
| Source too long, up front | `compile error: script source is N bytes, exceeds the 65536-byte limit` |
| Source too long, at evaluation | `script source is N bytes, exceeds the 65536-byte limit` |
| Parse depth | `compile error: script exceeds maximum nesting depth` |
| Evaluation depth | `script evaluation exceeded maximum depth; split the expression into smaller statements` |
| Closure call depth | `script evaluation exceeded maximum closure call depth; reduce the recursion depth` |
| Closure invocation count | `script evaluation exceeded the maximum closure invocation count` |
| Work budget | `script evaluation exceeded its per-document work budget; simplify the script` |
| Time budget | `script evaluation exceeded its per-document time budget; simplify the script` |

Once one evaluation has tripped a limit, every later evaluation inside the same
fault-capture scope returns that error immediately without running
(`eval_painless`, `painless.rs:1701`). That is what makes one budget the bound
for the whole request rather than one budget multiplied by the document count.
The scope is installed by `_search` (`es_compat.rs:6438`), by the engine's
`Index::search` (`index.rs:12534`), and by `_update` and `_update_by_query`
(`es_compat.rs:14924`, `:20394`). `POST /_scripts/painless/_execute` installs
no scope; it runs one script and returns any error from it, including a limit
message, as a 400 `script_exception` whose reason is prefixed with
`cannot evaluate script: ` (`es_compat.rs:24993`).

`POST /_scripts/painless/_execute` is stricter and answers differently: its
source cap is 4,096 bytes and an oversize script gets HTTP 413 with
`action_request_validation_exception` (`es_compat.rs:24911`).

### Errors that are not limits

A script that XERJ cannot parse or evaluate, including one that uses real
Painless features outside this subset, is **not** a 400. It degrades, per
surface:

- `script` query: the document does not match. A script that returns anything
  other than boolean `true` also does not match (`index.rs:26502`).
- `script_score` and script rescore: the score contribution becomes `0.0`
  (`index.rs:33162`, `index.rs:28362`).
- `script_fields`: the field is absent from the hit (`es_compat.rs:11051`).
- Script-bucketed `terms` agg: no buckets from that document
  (`aggs.rs:3292`).
- `terms_set.minimum_should_match_script`: the document cannot match
  (`index.rs:26575`).

This split is deliberate. A script outside the subset has to keep degrading so
that an otherwise-working request does not start failing; a resource-limit trip
is a script that was understood and refused, where a substituted `0.0` or `[]`
would be a plausible wrong answer. The contract is covered by
`engine/crates/xerj-api/tests/script_limits_http.rs`, including the
"unsupported syntax still degrades quietly" case.

## Scripted updates

`POST /{index}/_update/{id}` and `POST /{index}/_update_by_query` accept a
`script`, but they do not run the general interpreter over the whole script.
`apply_painless_update` (`es_compat.rs:15167`) splits the source on top-level
`;`, and each statement must target `ctx._source`:

| Form | Example |
|---|---|
| Assignment | `ctx._source.status = 'done'` |
| Compound assignment | `ctx._source.count += params.n` |
| Increment and decrement | `ctx._source.views++` |
| Remove a field | `ctx._source.remove('tmp')` |

Right-hand sides go through the interpreter, with `ctx._source.x` rewritten to
`doc['x'].value` first (`es_compat.rs:15087`), so `params`, `Math.*` and the
operators above all work there.

Real limitations of this path, from the same function:

- There is no control flow. Because statements are split on `;`, an `if` block
  in an update script produces fragments that do not start with `ctx._source`
  and the request fails with
  `unsupported update script statement: <statement>` as a 400.
- Other `ctx.*` statements are accepted and ignored. `ctx.op = 'noop'` and
  `ctx.op = 'delete'` parse without error and do nothing, so the document is
  still rewritten (`es_compat.rs:15216`). This differs from Elasticsearch.
- An unrecognised statement is an error rather than a quiet no-op, which is the
  opposite of the search-path behaviour described above.

### The request-level budget, and what it does not cover

Before rc.10 these two endpoints reached the interpreter through
`transform_document_serialized`, which established neither a request deadline
nor a fault sink, so every statement of every document got its own full slice.
As of rc.10 (#153) both wrap the work in one deadline and one fault sink:

- `SCRIPTED_UPDATE_BUDGET_MS = 30_000` (`es_compat.rs:20362`), the same 30 s
  `_search` falls back to when a request names no timeout.
- `_update` sets that deadline around the transform (`es_compat.rs:14923`).
- `_update_by_query` sets it around the whole hit loop, and the loop itself
  checks it per hit and breaks (`es_compat.rs:20409`). When it breaks early the
  response reports `"timed_out": true` instead of the hardcoded `false` it used
  to return.
- A limit trip is reported once for the request rather than once per document
  (`es_compat.rs:20463`).

The honest residual, which is a property of the search path rather than of the
update path: **the work budget and the time slice are per evaluation, meaning
per document.** `PainlessCtx::new` builds a fresh counter and a fresh deadline
for every document (`painless.rs:158`), so a script tuned to stay just under
5,000,000 work units costs that much on every matched document and never trips.
What bounds a search as a whole is the request deadline, `timeout` in the
request or 30,000 ms by default (`index.rs:12508`), plus the fail-fast on the
first fault described above.

The per-evaluation slice also has a floor. Once the request deadline has
passed, each evaluation still gets `MIN_EVAL_SLICE` (100 ms) rather than being
cut off instantly, because the document scan only notices an expired deadline
at a document boundary and cutting evaluations off immediately would turn
ordinary slow searches into `script_exception` 400s
(`painless.rs:859-873`). So an expensive-but-legal script can continue to
consume up to 100 ms per document past the deadline until the scan stops.

## Examples

Against a server started with `xerj --data-dir ./data --insecure`, listening on
`localhost:9200`.

A `script` query as a filter:

```sh
curl localhost:9200/orders/_search -H 'content-type: application/json' -d '{
  "query": {
    "script": {
      "script": {
        "source": "doc[\"total\"].size() == 0 ? false : doc[\"total\"].value > params.floor",
        "params": { "floor": 100 }
      }
    }
  }
}'
```

`script_score`, combining the query score with a field:

```sh
curl localhost:9200/orders/_search -H 'content-type: application/json' -d '{
  "query": {
    "script_score": {
      "query": { "match": { "title": "laptop" } },
      "script": { "source": "_score * Math.log(2 + doc[\"views\"].value)" }
    }
  }
}'
```

A runtime field, using a lambda as the predicate:

```sh
curl localhost:9200/orders/_search -H 'content-type: application/json' -d '{
  "runtime_mappings": {
    "big_order": {
      "type": "boolean",
      "script": {
        "source": "def over = (n) -> n > 100; emit(over.test(doc[\"total\"].value))"
      }
    }
  },
  "fields": ["big_order"],
  "_source": false
}'
```

A scripted update:

```sh
curl -X POST localhost:9200/orders/_update/order-1 \
  -H 'content-type: application/json' -d '{
  "script": {
    "source": "ctx._source.views++; ctx._source.status = params.next",
    "params": { "next": "shipped" }
  }
}'
```

An update by query:

```sh
curl -X POST localhost:9200/orders/_update_by_query \
  -H 'content-type: application/json' -d '{
  "query": { "term": { "status": "new" } },
  "script": { "source": "ctx._source.priority = 1" }
}'
```

Standalone evaluation, which has params but no document:

```sh
curl -X POST localhost:9200/_scripts/painless/_execute \
  -H 'content-type: application/json' -d '{
  "script": { "source": "params.a * params.b", "params": { "a": 6, "b": 7 } }
}'
```

The response is `{"result": "42.0"}`. Results are stringified, and whole
numbers keep one decimal place, matching Elasticsearch
(`es_compat.rs:24976`).

## Performance notes

Compiled ASTs are cached per thread and keyed by source text, including parse
failures, because an invalid script is also re-evaluated once per document
(`painless.rs:1643`). The cache is bounded by both entry count and total source
bytes, since the key is caller-supplied text.

There are no published latency numbers for script execution in this repo, so
this document does not give any. The two measured figures quoted above are the
work-budget calibration numbers recorded in `painless.rs` and pinned by
`engine/crates/xerj-engine/tests/painless_cpu_budget.rs`.
