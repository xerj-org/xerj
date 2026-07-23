# Token-Usage Profile — how an AI agent spends context on XERJ

> **The one-sentence answer to "how does XERJ use 100× more tokens than grep?"**
> It doesn't — a *query pattern* does. Every measurement below is reproducible
> against a running XERJ; every number is bytes on the wire, the thing that
> actually becomes tokens in an agent's context.

## Why this document exists

`grep -c` prints `2`. XERJ answers the same question with
`{"hits":{"total":{"value":2}}}`. One is 1 byte, the other 30. Scale that to a
question XERJ answers badly — a relational join — and the same 30-byte floor
becomes 15,914 bytes. That looks like the engine is 100× more expensive.

It is not. **The token cost of a XERJ answer is a function of three things, and
the agent controls all three:**

```
tokens ≈ envelope_overhead  +  answer  +  materialized_intermediate_data
```

- **`answer`** — the information you actually asked for. Almost always tiny: a
  count is 1–6 bytes, a five-bucket breakdown ~40 bytes.
- **`envelope_overhead`** — JSON scaffolding (`took`, `_shards`, `max_score`,
  key names). A fixed ~30-byte floor when trimmed, ~185 bytes when not.
- **`materialized_intermediate_data`** — data the agent pulls into context only
  to feed the *next* request. Zero for native patterns; unbounded for the wrong
  ones. **This is the entire "100×".**

grep has no envelope and never materializes intermediates, so on a
one-integer question it is unbeatable. The moment the question needs ranking,
typed aggregation, or orientation over unknown data, grep's cost explodes and
XERJ's stays flat — see [the corpus-composition law](#the-corpus-composition-law).

## The measured profile

All figures from the reference corpus (`demo/agent-gate/make_corpus.py`),
bytes on the wire, `filter_path` applied unless noted:

| pattern | answer | response B | envelope B | answer B |
|---|---|--:|--:|--:|
| scalar count, trimmed | `2` | **30** | 29 | 1 |
| scalar count, full envelope | `2` | 185 | 184 | 1 |
| terms agg, 5 buckets | 5 services | 211 | 207 | 4 |
| retrieval, 1 hit + fragment | a file path | 279 | 269 | 10 |
| **join, materialized** | 966 ids | **15,914** | 15,908 | 6 |
| **join, denormalized** | `122` | **36** | 33 | 3 |

Read the last two rows together. **The same join question costs 15,914 bytes
one way and 36 bytes the other — a 442× difference — for an identical answer.**
The engine did not change. The pattern did.

## The three rules that turn 100×-worse into competitive

### Rule 1 — always trim the envelope (`filter_path`)

Every `_search` accepts `?filter_path=`. Without it you pay ~185 bytes of
`took`/`_shards`/`max_score` on every call to deliver a one-byte answer.

```
# 185 bytes
GET /idx/_search            {"size":0,"query":{"term":{"uid":"u4242"}}}
# 30 bytes — same answer
GET /idx/_search?filter_path=hits.total.value   {...}
```

For aggregations, name the exact path:
`filter_path=aggregations.svc.buckets.key,aggregations.svc.buckets.doc_count`.
**6× on this one habit.**

### Rule 2 — never materialize a join side; denormalize at ingest

XERJ has no server-side JOIN. Answering a relational question the relational
way forces the agent to pull one whole side of the relation into context:

```
# 15,914 bytes: 966 enterprise ids into context, then echoed back in query 2
q1: enterprise user_ids  -> [966 ids]           # materialized intermediate
q2: open tickets WHERE user_id IN [966 ids]     # ids echoed again
```

The native answer is to put the joined attribute where it will be queried,
**once, at ingest** — then it is a single filtered aggregation:

```
# 36 bytes: plan lives on the ticket already
GET /tickets/_search?filter_path=aggregations.u.value
  {"size":0,"query":{"bool":{"filter":[
     {"term":{"status":"open"}},{"term":{"plan":"enterprise"}}]}},
   "aggs":{"u":{"cardinality":{"field":"user_id"}}}}
```

`autoindex` already **detects** the foreign-key correlation (it prints
`user_id ↔ user_id, live-confirmed 20/20` in the data map). Denormalizing on
that correlation at ingest is the search-engine-native shape. Until it does so
automatically, an agent should enrich the smaller table onto the larger one
once, rather than join per query. **442× on this one decision.**

#### When one side already fits in a single document: `terms` lookup

XERJ *does* support ES `terms` lookup, which pushes the value substitution
server-side — the id set never enters the agent's context at all:

```
GET /events/_search?filter_path=hits.total.value
  {"size":0,"track_total_hits":true,
   "query":{"terms":{"uid":{"index":"allowset","id":"set1","path":"ids"}}}}
```

The engine fetches `allowset/set1`, reads the array at `ids`, and filters
`events` by it — verified end-to-end. The constraint is that **the lookup
values must live in one document's array field** (`allowset/set1.ids`). That
fits allow-lists, cohort sets, and feature groups; it does *not* fit a
customer/ticket join where each id is its own document, so denormalize-at-ingest
remains the answer for that shape.

> Both `_search` and `_count` resolve terms lookups. (`_count` did not until
> the change that ships with this document — it silently returned 0 for a
> filter `_search` counted correctly. An unresolved lookup object now produces
> a loud parse error instead of matching nothing.)

### Rule 3 — ask for `size: 0` whenever you only need the aggregate

A `terms` or `cardinality` question does not need hits. `size:0` drops the
`hits.hits` array entirely. Combined with Rule 1 this is the difference between
a 40-byte answer and a full page of source documents.

## The corpus-composition law

The single property that decides whether XERJ saves or costs tokens is **how
much of the corpus is searchable prose/code versus structured records** — not
any engine setting.

| corpus | prose fraction | result at equal correctness |
|---|---|---|
| 234 files / 170k LOC of source + docs | ~100% | XERJ **5.3× FEWER** tokens |
| 36 MB, 458k log/CSV records, 11 prose files | 0.01% | XERJ **more** tokens, but higher correctness |

- **Retrieval savings scale with the prose fraction.** grep over a large text
  corpus is unranked and must return many candidate lines; ranked search
  returns the one fragment. This is where XERJ wins on tokens.
- **Analytics savings scale with record count**, but analytics answers are
  *small* either way (both grep+awk and XERJ return a handful of numbers), so
  the token win there is modest — the real win is correctness and latency, not
  tokens.

**A single headline token number across both regimes is meaningless.** Any
honest report states the corpus composition next to the ratio. The gate in
`demo/agent-gate/` refuses to print a ratio without it.

## What to measure, and how (for this and future playbooks)

The rules below are enforced in code by `demo/agent-gate/gate.py`. Reuse them
for any new benchmark so results stay comparable and honest.

1. **Count bytes on the wire, not tool calls.** Tokens come from response and
   request payloads. Number of HTTP calls barely matters — two 30-byte calls
   beat one 15 KB call. Measuring "API calls" instead of bytes is how the join
   cost gets misattributed to "loops."
2. **Savings only over jointly-correct tasks.** A path that returns fewer bytes
   because it returned nothing useful has not saved anything. Exclude any task
   either path got wrong.
3. **Baselines use only the question's vocabulary.** A grep written by someone
   who already knows the answer's location is not a baseline.
4. **Both paths equally tuned.** If the baseline is competent, the XERJ side
   must use `filter_path`, `size:0`, and native patterns. Comparing a tuned
   baseline to an untuned engine is its own dishonesty.
5. **Vary each query to defeat the result cache**; report the first, uncached
   timing.
6. **Treat `timed_out:true` and bucket-vs-`hits.total` mismatch as WRONG**, not
   fast. A partial answer presented as complete is the worst failure mode.
7. **Report corpus composition beside every ratio.**

## Quick reference — the token-frugal request shapes

| you want | shape |
|---|---|
| a count | `size:0`, `track_total_hits:true`, `filter_path=hits.total.value` |
| a breakdown | `size:0`, `terms` agg, `filter_path=aggregations.NAME.buckets.key,aggregations.NAME.buckets.doc_count` |
| distinct count | `cardinality` agg, `filter_path=aggregations.NAME.value` |
| one relevant passage | `size:1`, `highlight` with `number_of_fragments:1`, `_source` narrowed, `filter_path=hits.hits.highlight` |
| a relational answer | denormalize the attribute at ingest, then a single filtered agg — never materialize an id set |
| orientation | read the `autoindex-catalog` dataset docs (~890 tokens for the whole corpus) — do **not** sample files |
