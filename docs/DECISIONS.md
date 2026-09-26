# Typed decisions from your own history (`/v1/systemone`, `/_decide`)

> **This feature sends nothing off the machine.** The vote is an ordinary
> search of an ordinary index on this node. The module adds no outbound
> client; no document or query text leaves, no provider key is involved, and
> no tokens are spent. That is the point of it — see
> [docs/RERANK.md](./RERANK.md) for the feature that *does* send text out,
> and the contrast between the two.

TypeSafe AI's System One API is a documented interface for typed judgement:
`POST /v1/systemone` with a state, a model, and a map of questions, each
answered `noul` (a yes/no with a 0–1 probability) or `choice` (one of N named
options with probabilities). Clients exist for it — `jev-reranker` on PyPI,
the official SDKs — and they all speak that one shape.

XERJ already had the retrieval answer for the same shape of question: index
the labelled examples you already have, answer a new question by a weighted
vote over its *k* nearest neighbours, and report the winning label's vote
share as the probability
([benchmarks/decisions-as-retrieval](../benchmarks/decisions-as-retrieval):
Banking77 0.933 accuracy / ECE 0.012, SMS 0.983, sub-millisecond, no model).
So the node speaks the wire:

- **`POST /v1/systemone`** (native REST, `--port + 1`) — the documented
  request/response shape, answered by the vote. A client written for
  TypeSafe's API works unmodified with
  `TYPESAFE_ENDPOINT=http://localhost:<port+1>/v1/systemone`.
- **`POST /_decide`** (ES-compat port) — the same vote without the wire
  costume: it names its index per request and returns the evidence.

Both surfaces are one mechanism, in
`engine/crates/xerj-api/src/systemone_api.rs`; the end-to-end tests are
`engine/crates/xerj-api/tests/systemone_http.rs`. The acceptance gate for the
wire — the pip-installed `jev-reranker`, unmodified, ranking off a XERJ node —
is [benchmarks/systemone-gate](../benchmarks/systemone-gate).

## Set it up

```toml
# xerj.toml
[decisions]
index          = "judgements"   # an index of labelled examples; empty (the default) = both endpoints 503
k              = 10
label_field    = "label"
text_field     = "text"
positive_label = "true"         # the label whose share a noul answers
min_confidence = 0.0            # /_decide abstains below this
```

The history index is ordinary documents: one per example, with the text the
example was decided on (`text_field`) and its label (`label_field`). Index it
with the usual `PUT /{index}/_doc` or `_bulk`. For a two-class history the
labels are the two class names and `positive_label` names the one a `noul`'s
probability is the share of (`"spam"`, not `"true"`, for a spam history).

## What the vote answers — and what it does not

The vote is **not** zero-shot and claims no judgement. It answers "what does
history similar to this question say?", which is exactly the right question
when you HAVE history (support routing, moderation, triage, spam) and the
wrong one when you do not. Where there is no history — a new policy, a new
category — use a judge model; that is what
[docs/RERANK.md](./RERANK.md) is for.

**What the vote retrieves on — the payload, never the prose (#1000, #1001).**
A question's `instructions` are a question ABOUT the payload; their vocabulary
is not retrieval text. The vote text is exactly what the question points at:

- every `` `path` `` reference the instructions name — resolved against the
  instructions' own string fields first (the data-field pattern:
  `{"question": "Judge `document` …", "document": text}`, which
  `xerj-rerank`'s stage and the provider's docs use), then against the state
  by dotted path (`` `documents.doc_0` ``, the shape `jev-reranker` sends).
  A reference that resolves to structure rather than text — the rubric
  object `jev-reranker` ≥ 0.1.2 ships with its relevance preset — is
  acknowledged and skipped: judge rules are not retrieval vocabulary;
- a question that references nothing votes on the state alone — a string
  state whole, an object state by ALL its string leaves, whatever the keys
  are named;
- references that resolve to nothing, and a payload with no text at all, are
  422s naming the question — never a confident vote on text the question
  never saw.

Two defects this design closes, both measured on the SMS history (4,000
train / 300 held-out, shuffle seed 7): instruction wording used to change the
answer — accuracy 0.9867 with an empty instruction against 0.6700 with a
criteria-rich one, a 32-point swing from wording alone — and an object state
used to contribute only its `query` field, so `{"message": …}` voted on the
instruction prose at the spam base rate (accuracy 0.1733 against 0.9667 for
the same message as a string). After the fix the same 300 held-out messages
score identically under every instruction wording and every state shape —
0.9767 across all eight arms (the absolute number sits ~1 point under the
issue runs' 0.9867 because the history was re-indexed fresh for the re-run;
the invariant is the identity, not the digit). One nuance the client itself
decides: since 0.1.2, `jev-reranker`'s own question references `` `query` ``
in backticks, so the query is named payload and joins the vote BY CLIENT
DESIGN — the acceptance gate still uses a neutral query for a classification
history, and survives a deliberately spammy one (gap 0.61 against 0.85,
threshold 0.3).

## The wire, and the two deliberate breaks

Request: `{state, model, questions: {id: {type, instructions, criteria}}}`.
`state` may be a string or an object; `instructions` may be a string, object
or array. `criteria` descriptions are never retrieval text; the options they
name are `choice` answers, not query vocabulary.

Response: `{model, answers, usage}` with `answers` keyed exactly by the
question ids sent, each answer carrying only its documented fields (clients
validate strictly). XERJ's extras ride at the top level in `decisions` — the
index, k, the requested model, and per-question evidence (winning label, its
support, how many neighbours were found and voted) — where every verified
client ignores them.

Two deliberate breaks from the hosted API, both tested so they cannot regress:

1. **`model` is echoed as `xerj-history-vote-1`, never as a Jev model name.**
   Echoing `jev-1.13.0` would claim these probabilities are the hosted
   model's. The requested name is reported as `decisions.requested_model`.
2. **Zero support is a 422 naming the question ids, never a fabricated 0.5.**
   A question whose vote finds no labelled neighbour carries no information;
   inventing a probability is the silent-fake defect class this project
   treats as a bug. `/_decide` says the same thing as `abstain` + `reason`.

`score` questions (2–10 ordinal levels) have no vote analogue and no
benchmark: 422, naming the alternative.

## `POST /_decide`

The audit surface. Per request: `index` (required), `question` (required),
`k` (default the configured 10, clamped 1..100), `positive_label` (default
the configured one). The response returns the label, its confidence, the
verdict (`abstain` below `decisions.min_confidence`, or when no labelled
neighbour exists), and the **neighbours** — each with `_id`, `label`,
`_score` (the engine's own BM25 score), `weight` (1/rank), and the text — so
every answer can be checked against the evidence that produced it.

```sh
curl -s localhost:9200/_decide -H 'content-type: application/json' \
  -d '{"index":"judgements","question":"refund my subscription","k":5}'
```

## Numbers, and only the ones we measured

Measured, ours, reproducible (raw JSON and runners in the results dirs):
Banking77 77-way intent 0.933 accuracy / ECE 0.012 (86.9% of traffic clears
confidence 0.8 at 0.979 accuracy); SMS spam 0.983 accuracy / 0.944 F1 under a
millisecond on BM25 alone; both in
[benchmarks/decisions-as-retrieval](../benchmarks/decisions-as-retrieval).
The wire gate transcript is in
[benchmarks/systemone-gate](../benchmarks/systemone-gate). A third, smaller
run measures `/_decide` itself on an email-labelling history — 4 labels,
200 train / 60 held-out plus 8 hard boundary cases, seed-deterministic
corpus — in
[benchmarks/systemone-classify-email](../benchmarks/systemone-classify-email)
([#1026](https://github.com/xerj-org/xerj/pull/1026), merged 2026-09-26,
answering [discussion
#1012](https://github.com/xerj-org/xerj/discussions/1012)); read its README
for what those numbers do and do not license before quoting them. Not
claimed: anything about how a judge model scores on those datasets (no such
numbers are published and we did not run one there), any zero-shot
capability, and any judgement quality for histories unlike the ones measured.
