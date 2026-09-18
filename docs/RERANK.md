# Reranking search results (`rerank`)

> **This feature sends your data off the machine.** A search that carries a
> `rerank` block POSTs the question and the text of up to `window` hits
> (default 30, maximum 300) to a third-party API. It is the **one** XERJ
> feature that does this. Every other request — indexing, search, embedding,
> agent memory — runs entirely on the node. Reranking is off until an operator
> supplies a provider key, a request only triggers it by asking for it, and an
> operator can forbid it outright with `[rerank] enabled = false`. Read
> [What leaves the machine](#what-leaves-the-machine) before you turn it on.

`rerank` is an optional second stage on `POST /{index}/_search`. The engine
retrieves and ranks as usual; the stage then hands the top hits to an external
relevance judge, which returns a probability per document that it answers the
question. Hits are reordered by that probability, and the probability replaces
`_score`.

It exists for one reason above ordering. A BM25 or fused score orders results
but has no absolute meaning: a `_score` of 7.2 is not comparable across queries
and cannot be used as a cut-off. A calibrated probability can, so
`rerank.min_score` is a threshold that means the same thing on every query.

> **Not verified: ranking quality with the real model.** No provider key was
> available to this project, so XERJ has **not** measured how well the Jev model
> ranks. Every test of this stage runs against a test double that scores by a
> table the test supplies. What is verified is the mechanism — the wire format,
> the reorder, paging, pruning, the failure policy, and what is and is not sent.
> The quality figures published by the provider's ecosystem are quoted in
> [Quality numbers](#quality-numbers-and-what-we-did-not-measure) with that
> caveat attached.

The stage is in `engine/crates/xerj-api/src/rerank_stage.rs`; the provider
client, the request parser and the failure policy are the leaf crate
`engine/crates/xerj-rerank`. The end-to-end tests are
`engine/crates/xerj-api/tests/rerank_stage_http.rs`.

## Set it up

A node needs a provider key. There are two places to put it, and **a value in
the config file wins over the environment** — the same rule as
`cluster.auth_secret`, so a stray variable in a service's environment cannot
silently re-point where document text is sent.

```toml
# xerj.toml
[rerank]
enabled  = true                                    # false = refuse every rerank request (403)
api_key  = "ts-..."                                # or the TYPESAFE_API_KEY environment variable
endpoint = "https://api.typesafe.ai/v1/systemone"  # or TYPESAFE_ENDPOINT; this is the default
```

| Setting | Environment fallback | Default | Meaning |
|---|---|---|---|
| `rerank.enabled` | none | `true` | `false` refuses every `rerank` request with HTTP 403, whatever the environment holds. Nothing is sent. |
| `rerank.api_key` | `TYPESAFE_API_KEY` | empty | The provider key. Empty in both places means reranking is not configured: requests get HTTP 503. |
| `rerank.endpoint` | `TYPESAFE_ENDPOINT` | `https://api.typesafe.ai/v1/systemone` | Where documents are POSTed. Must be an absolute `http(s)://` URL with no `user:password@` in it; the node refuses to start otherwise. |

The settings are resolved once, at startup. Changing the environment of a
running node does nothing; restart it.

There is deliberately **no per-request key**. A key in a search body would land
in slow-query logs, audit trails and client-side request dumps.

### Check whether it is configured

```sh
curl -s -H "Authorization: ApiKey $ADMIN_KEY" http://localhost:9200/_xerj/rerank
```

```json
{
  "enabled": true,
  "configured": true,
  "providers": ["jev"],
  "api_key":  { "set": true, "source": "config" },
  "endpoint": { "url": "https://api.typesafe.ai/v1/systemone", "source": "default" },
  "defaults": { "provider": "jev", "model": "jev-latest", "window": 30, "batch": 30,
                "max_concurrency": 8, "max_doc_chars": 1200, "timeout_ms": 10000 },
  "limits":   { "max_docs_per_call": 30, "max_window": 300, "max_concurrency": 16,
                "max_doc_chars": 8000, "max_timeout_ms": 60000 },
  "data_egress": "A search that carries a `rerank` block sends the text of up to `window` hits, and the query, to the endpoint above. No other request sends document text anywhere."
}
```

The endpoint reports **that** a key is set and **where it came from**
(`"config"` or `"env"`). It never returns the key, and the URL is printed with
any userinfo, query string and fragment removed. `configured` is `false` when a
key exists but `enabled = false`: a key on a node that forbids reranking is not
a working configuration.

`/_xerj/*` is the node's operator namespace and is superuser-only. A caller
without superuser rights finds out from the search itself: HTTP 503 means no
key, HTTP 403 means the operator disabled it.

## Request

```json
POST /kb/_search
{
  "query": { "match": { "body": "vitamin d supplementation bone density" } },
  "size": 5,
  "rerank": { "min_score": 0.5 }
}
```

`"rerank": {}` is valid and uses every default.

| Field | Type | Default | Limit | Meaning |
|---|---|---|---|---|
| `provider` | string | `"jev"` | `jev`, `typesafe` (alias), `none`, `disabled` | `none` / `disabled` skips the call and reports `applied: false`. Anything else is a 400. |
| `model` | string | `"jev-latest"` | | Sent to the provider verbatim. |
| `window` | integer | 30 | 1–300 | How many of the engine's top hits are judged. **Every document in the window is a paid judgement.** |
| `min_score` | number | none | 0–1 | Drop hits whose probability is below this. It is a probability; 7 is a 400. |
| `query` | string | inferred | non-empty | The question documents are judged against. See [The question](#the-question). |
| `fields` | string[] | every returned string field | non-empty | Which returned fields are sent. **Exhaustive**: `["body"]` sends the body and nothing else, not even the title. See [What leaves the machine](#what-leaves-the-machine). |
| `instructions` | string | a generic relevance question | | Overrides what "relevant" means. A support corpus and a code corpus do not mean the same thing by it. |
| `timeout_ms` | integer | 10000 | 1–60000 | Wall-clock budget for the **whole stage**, not per call. |
| `max_doc_chars` | integer | 1200 | 1–8000 | Title and text are each cut to this many characters, on a character boundary. |
| `batch` | integer | 30 | clamped to 30 | Documents per provider call. Values above 30 are lowered to 30, not refused: the ceiling is the provider's, not a caller mistake. |
| `max_concurrency` | integer | 8 | 1–16 | Provider calls in flight at once. |

Unknown keys are **refused by name**, not ignored. A caller who misspells
`min_score` as `treshold` gets a 400 naming `treshold`, instead of unpruned
results that look pruned.

The ceilings are the server's because the caller picks the window and the
operator pays for it.

### The question

The judge needs one natural-language question. XERJ reads it from the search
query only when the query is a shape that carries exactly one:

| Query shape | Question read from |
|---|---|
| `match`, `match_phrase` | the field's query string |
| `multi_match`, `semantic`, `simple_query_string` | `query` |
| anything else — `bool`, `hybrid`, `term`, `match_all`, a top-level `knn` with no text query, no `query` at all | **refused (400)**: pass `rerank.query` |

This is deliberately shallow. A `bool` tree has no single question in it, and
picking one would rank against a guess without anyone noticing. `rerank.query`
always wins when it is present, and the `_rerank` block reports the question
that was actually used.

## Response

The stage adds one top-level block, `_rerank`, placed **before** `hits` on the
wire so that a reader which truncates long output from the bottom still sees
which ranking it is holding.

Applied:

```json
{
  "_rerank": {
    "applied": true,
    "provider": "jev",
    "model": "jev-latest",
    "score_kind": "probability",
    "window": 4,
    "judged": 4,
    "skipped_no_text": 0,
    "pruned_below_min_score": 2,
    "dropped_unjudged": 0,
    "partial_failures": 0,
    "usage": { "input_tokens": 100, "output_tokens": 10 },
    "query": "vitamin d supplementation bone density",
    "took_ms": 212
  },
  "took": 215,
  "hits": { "total": { "value": 4, "relation": "eq" }, "max_score": 0.9, "hits": [ ... ] }
}
```

Degraded (the provider missed the deadline — see [Failure
policy](#failure-policy-degrade-on-deadline-surface-on-contract)):

```json
{
  "_rerank": {
    "applied": false,
    "reason": "rerank deadline exceeded after 301ms",
    "provider": "jev",
    "score_kind": "engine",
    "query": "vitamin d supplementation bone density",
    "took_ms": 301
  },
  "hits": { ... the engine's order, the engine's scores ... }
}
```

| `_rerank` field | Meaning |
|---|---|
| `applied` | `true`: the order is the judge's. `false`: the order and scores are the engine's; `reason` says why. **Always read this.** |
| `score_kind` | `"probability"` when `_score` is a 0–1 relevance probability, `"engine"` when it is still BM25 / fusion. |
| `window` | Hits considered: `min(rerank.window, hits the engine returned)`. |
| `judged` | Documents the provider returned a verdict for. |
| `skipped_no_text` | Hits in the window with no string content in the response. They are not sent: a blank document costs a judgement and a verdict on nothing means nothing. |
| `pruned_below_min_score` | Judged hits removed by `rerank.min_score`. |
| `dropped_unjudged` | With `min_score` set, hits that were never judged cannot be shown to clear the bar, so they are dropped and counted here. |
| `fields_without_text` | Present only when there is something to report: the names in `rerank.fields` that **no hit in the window** returned any text for — a typo, a field these hits lack, or a field the projection removed. Those documents were judged without it. |
| `partial_failures` | Provider calls that failed while others succeeded. The scored hits are still correctly ordered — each probability is absolute — but the window was not fully covered. |
| `usage` | Tokens the provider reported, summed over every call that answered. Zero when the provider omits it. |
| `query` | The question that was judged, inferred or given. |
| `took_ms` | Time spent in the stage. The response's own `took` includes it. |

What changes on a hit: `_score` becomes the probability, and `hits.max_score`
becomes the top hit's probability. Hits that were not judged keep the engine's
score and sort **after** every judged hit, in the engine's order — an unjudged
document is not evidence of irrelevance, but it cannot be ranked against
documents that were scored. Equal probabilities keep the engine's order.

## How it interacts with the rest of `_search`

The stage owns exactly one thing: the order, and with `min_score` the
membership, of `hits.hits`. Everything else in the response stays the engine's
and describes the **full match set**, not the window. Each row below is pinned
by a test in `rerank_stage_http.rs`.

| Combined with | Behaviour |
|---|---|
| `aggs` | **Unchanged.** Aggregations are computed over every matching document, and are identical with and without `rerank` — including when `min_score` prunes the page to nothing. |
| `hits.total`, `track_total_hits` | **The engine's total.** `true`, an integer cap and `false` each report exactly what they report without `rerank`. Four documents matched and two were judged irrelevant: `total` is 4, and `pruned_below_min_score` is 2. Both facts are true and the response says both. |
| `size`, `from` | Paging happens **inside the reranked window**. XERJ widens the engine's page to `window`, judges all of it, then cuts `from`/`size` out of the judge's order, so page two continues page one. `from + size` must be ≤ `window`, or the request is a 400: a page cut across the window boundary would mix two rankings. |
| `highlight`, `fields`, `inner_hits`, `matched_queries` | **Travel with their hit.** The stage moves rendered hits, so nothing is recomputed and nothing is swapped between hits. |
| `"fields": ["_passage"]` | The matching passage is **judgeable**, and leads the text the judge reads. Name it in `rerank.fields` to send only the passage. |
| the large-response hint (`_xerj.hints`) | Its ready-to-send corrected request **keeps the `rerank` block**. A suggestion without it would be a different search, returning the engine's order. Its narrower projection also narrows what the judge is sent. |
| `_source` filtering | Decides what the judge sees — see the next section. `_source: false` with no `fields` clause is a 400 (nothing can be judged); beside a `fields` clause that returns the text, it works. |
| `explain` | The engine's explanation is kept, unaltered, under a new top node whose `value` equals the new `_score` and whose description says the score is a rerank probability. An `_explanation.value` that disagreed with `_score` would mislead. |
| `profile` | Unchanged; it profiles the engine. The stage reports its own time in `_rerank.took_ms`. |
| top-level `min_score` | The **engine's** threshold, applied to **engine** scores before the window is cut. It shapes the match set and `hits.total` exactly as it does without `rerank`, and it is never compared against a probability. `rerank.min_score` is the probability threshold. The two compose: engine bar first, probability bar second. |
| `knn` (top level) | Works. A vector is not a question, so a `knn` with no text `query` needs `rerank.query`. Beside a text query the question is read from the text query, and vector-only hits are inside the window the judge sees. Vectors are never sent. |
| `rescore` | Runs first, inside the engine. `rerank` has the last word. |
| multi-index and wildcard searches (`/a,b/_search`, `/kb*/_search`) | Work. The window is cut from the merged result, so hits from different indices are ranked against each other; `_index` stays with its `_id`. |
| `sort` (body or `?sort=`) | **400.** An explicit sort already fixes the order. |
| `search_after`, `collapse`, `scroll` | **400.** Each of them depends on the engine's order. |
| `size: 0` | **400.** The provider would be paid to judge documents the response does not return. |

### Endpoints that refuse `rerank`

Only `POST /{index}/_search` (and its `GET` and no-index forms) runs the stage.
These endpoints answer a body that carries `rerank` with a 400 that says so,
rather than dropping the key and returning the engine's order under a 200:

| Endpoint | Refusal |
|---|---|
| `_msearch` | **Per item.** The item that carries `rerank` gets a `status: 400` entry; the rest of the batch still runs. |
| `_search/template`, `_msearch/template` | 400 when the *rendered* template contains `rerank` (per item for `_msearch/template`). |
| `_async_search` | 400. A stored search would also be a stored third-party call that nobody is waiting on. |
| `_search_scroll`, `?scroll=` | 400. A scroll streams the engine's order. |

## What leaves the machine

**The judge sees exactly what the response returns, and nothing else.**
Candidate text is read from each hit as the response will carry it: the
projected `_source`, then the hit's `fields`. That makes the rule auditable —
whatever you were about to receive is what was sent.

Sent to the provider, per search:

- the question (`rerank.query`, or the one read from the query);
- `rerank.instructions`, if given, and the model name;
- for each hit in the window, cut to `max_doc_chars` characters each:
  - with no `rerank.fields`: the `title` field if the hit returns one; then the
    matching passage, if the request asked for `"fields": ["_passage"]`; then
    every other top-level string field (and array of strings) in the returned
    `_source`. The passage goes **first** because text is cut at
    `max_doc_chars`: on a long document an appended passage would be the part
    that gets cut, and it is the part that says why the hit matched;
  - with `rerank.fields`: **those fields and nothing else.** The list is
    exhaustive. `["body"]` does not send the title; `["title", "body"]` does.
    A name is looked up in the returned `_source` first, then in the hit's
    `fields`. `"_passage"` is accepted as a name and sends the passage text;
- the provider key, as a bearer token in the `Authorization` header and nowhere
  else. If the provider echoes it back in an error body, XERJ redacts it before
  the body reaches the search caller or the log: whoever runs a search is not
  necessarily the operator who owns the key.

Never sent: document `_id`s and index names (documents are keyed `d0`, `d1`, … by
position), numbers, booleans, vectors, nested objects, and any field the
response does not return.

So `_source` filtering is also an egress control:

```json
{ "query": { "match": { "body": "refund" } },
  "_source": ["title"],
  "rerank": {} }
```

sends titles only. The `body` text is not in the response, so it is not in the
request to the provider either.

To send the **matching passage and nothing else** — usually the best trade on
long documents, for both the judge and the amount of text that leaves:

```json
{ "query": { "match": { "body": "refund" } },
  "_source": ["title"],
  "fields": ["_passage"],
  "rerank": { "fields": ["_passage"] } }
```

Whether judging the passage ranks better than judging the head of the document
has not been measured; what is certain is that less text is sent.

What was returned is checked on the **rendered hits**, not predicted from the
request, so a document is never judged blind:

- `rerank.fields` names a field that `_source` filtering removes and `fields`
  does not request: **400** before the search runs, naming the field;
- no hit in the window returns any text to judge: **400** `"nothing to judge"`,
  and nothing is sent;
- some named fields return text and others return none anywhere in the window:
  the search proceeds, and `_rerank.fields_without_text` lists the ones that
  contributed nothing.

Known limitation: today a `fields` entry is dropped when an explicit `_source`
includes list omits that field ([#932](https://github.com/xerj-org/xerj/issues/932)),
so `"_source": ["title"], "fields": ["body"], "rerank": {"fields": ["body"]}`
is refused as nothing to judge. `"_source": false, "fields": ["body"]` works.

To rule reranking out entirely on a node, set `[rerank] enabled = false`.

## Failure policy: degrade on deadline, surface on contract

A blanket "fall back to the engine's order on any error" would be the wrong
default. A caller who asked for calibrated relevance and silently got lexical
order has been misled. So the policy is split:

| What happened | Result |
|---|---|
| The provider did not answer inside `timeout_ms` | **Degrade.** HTTP 200, the engine's order and scores, `_rerank.applied: false` with the reason. A slow third party is not a reason to deny the caller results it already has. |
| `rerank.provider` is `none` / `disabled` | Degrade, same shape. |
| Malformed `rerank` block, unknown provider, a refused combination | **400** `illegal_argument_exception`. Nothing is sent. |
| The operator set `enabled = false` | **403** `rerank_exception`. Nothing is sent. |
| No key configured | **503** `rerank_exception`, naming `[rerank] api_key` and `TYPESAFE_API_KEY`. Nothing is sent. A search **without** `rerank` on the same node is unaffected. |
| Provider answered 401 / 403 / another 4xx | **502** `rerank_exception` with the provider's status. Not retried. |
| Provider answered 200 with a body that is not the documented shape | **502** `rerank_exception`. |
| Provider unreachable | **502** `rerank_exception`. |
| Provider answered 429, 529 or 5xx | Retried, up to 3 attempts per call, with a short exponential back-off and an extra pause after a 429 — but never past the deadline. If the deadline runs out first, degrade; if the retries run out first, 502. |

Contract and configuration faults do not fix themselves, and hiding them costs
the operator more than it saves. A surfaced fault returns **no hits**: handing
back the engine's order next to an error would invite a caller to ignore the
error.

If some provider calls succeed and others fail, the stage keeps what it has:
the judged hits are ordered by probability, the rest follow in the engine's
order, and `partial_failures` says how many calls were lost.

The split follows the one Meilisearch uses for its reranking call in its
personalization module (return the original hits when the deadline is exceeded,
return the error otherwise). The approach was adapted; no code was copied.

## Cost and concurrency facts

- **30 documents per provider call.** The `hev/jev-rerank` README reports a
  ~32k-token request budget, "~30–50 typical passages per call", and uses 30;
  XERJ uses the same figure and has not measured the provider's limit itself. A
  larger window is split: `window: 35` is two calls (30 + 5), and `window: 300`,
  the maximum, is ten.
- Scores from different calls are comparable, because each answer is an absolute
  probability rather than a rank within its batch. That is what makes splitting
  safe.
- **8 calls in flight by default, 16 at most**, sent in waves. The provider's
  rate limits are undocumented; the same README reports sustained 429s at about
  24 requests in flight (their observation, not ours), and both ceilings sit
  under that. A search that provokes a 429 is slower than one that paced itself.
- One question is asked **per document** rather than one multiple-choice
  question across them. Real corpora have more than one relevant document, and a
  multiple-choice question forces a single winner. This one-`noul`-per-document
  approach is the one used by [`hev/jev-rerank`](https://github.com/hev/jev-rerank)
  (Apache-2.0).
- The node keeps one HTTP connection pool for the provider, so consecutive
  searches do not pay a TLS handshake each.
- `_rerank.usage` is the meter reading for the search. XERJ does not price it.

## From an agent (MCP)

`xerj_search` and `xerj_hybrid_search` take an optional `rerank` argument with
the same fields. `true` or `{}` means defaults.

- With a **plain-string** `query`, the MCP server passes your string as
  `rerank.query`, because the plain-string path expands into a `bool` query that
  the node will not read a question from.
- `xerj_hybrid_search` **requires** `rerank.query`: a hybrid search has several
  sub-queries and no single question.
- The MCP server cannot supply a provider key. Without one on the node the tool
  call returns the node's 503; repeat the search without `rerank`.

## Quality numbers, and what we did not measure

XERJ has not run the Jev model. The figures below for Jev, Voyage and Cohere are
**published in the [`hev/jev-rerank`](https://github.com/hev/jev-rerank) README,
were not run by this project, and rerank that project's own first-stage
shortlist, which is not XERJ's.** Same datasets, same metric, not a controlled
comparison.

nDCG@10 on the BEIR `test` split:

| | SciFact | NFCorpus | Run by |
|---|---:|---:|---|
| Jev (TypeSafe AI), 30 Nouls in one request — the shape this stage sends | 0.768 | 0.358 | hev/jev-rerank README — not by us |
| Voyage rerank-3 | 0.755 | 0.357 | hev/jev-rerank README — not by us |
| Cohere rerank-v3.5 | 0.745 | 0.340 | hev/jev-rerank README — not by us |
| XERJ BM25 | 0.6572 | 0.3016 | us |
| XERJ MiniLM vectors only | 0.6764 | 0.3291 | us, `--embed-mode neural` |
| XERJ BM25 top-30, reordered by MiniLM | 0.6855 | 0.3323 | us, `--embed-mode neural` |
| XERJ hybrid RRF | 0.6993 | 0.3448 | us, `--embed-mode neural` |

**Our rows were measured with `--embed-mode neural` and the
`all-MiniLM-L6-v2` model, not with the default embedder.** XERJ's default
embedder is lexical feature hashing, which has no model in it; do not read the
vector or hybrid rows as what a default node scores. The BM25 row uses no
embedder at all. Method, raw output and the reproduce commands are in
[`benchmarks/beir-hybrid`](../benchmarks/beir-hybrid).

Two things those runs do say:

- Reordering a BM25 shortlist with the same bi-encoder scored **below** fusing
  with it, on both datasets. That is why there is no "local MiniLM" rerank
  provider: it would be a regression with a good name.
- Where a team already has labelled history, some yes/no and pick-one decisions
  need no judge model at all —
  [`benchmarks/decisions-as-retrieval`](../benchmarks/decisions-as-retrieval)
  measures that, also under `--embed-mode neural`.

## Credits

- The one-`noul`-per-document request shape:
  [`hev/jev-rerank`](https://github.com/hev/jev-rerank), Apache-2.0.
- The degrade-on-deadline, surface-on-contract failure policy: Meilisearch's
  personalization module (MIT). Approach adapted, no code copied.
- The provider API is TypeSafe AI's System One
  (`POST /v1/systemone`, <https://docs.typesafe.ai/api>).
