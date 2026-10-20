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
`engine/crates/xerj-api/tests/systemone_http.rs`, and the local tier's are
`engine/crates/xerj-api/tests/systemone_local_decide.rs`. The acceptance gate
for the wire — the pip-installed `jev-reranker`, unmodified, ranking off a
XERJ node — is [benchmarks/systemone-gate](../benchmarks/systemone-gate).

## The decide ladder

Both surfaces answer through one ordered ladder, per question (#1057):

1. **History vote** — where `[decisions] index` returns labelled support, the
   vote wins. It is the measured tier and it shows its evidence. One
   exception, only when tier 2 is armed (#1094): a vote whose winning label
   holds less than `decisions.escalate_below` of the vote weight (default
   0.8) is handed to the local head instead, so split votes keep feeding the
   flywheel below. Without tier 2 a split vote answers from history as
   before; `escalate_below = 0.0` turns escalation off. The local head is
   strongest on a binary `noul` over a trained label vocabulary (see below):
   on an index whose `choice` labels it was not trained on, lower
   `escalate_below` or set it to 0.0.
2. **Local zero-shot head** (off by default; feature `decide-local` +
   `--decide-mode local --decide-model-dir <path>`, or the
   `XERJ_DECIDE_MODE=local` + `XERJ_DECIDE_MODEL_DIR` env pair) — a ModernBERT-class
   candle classifier loaded from a local model directory holding
   `config.json`, `tokenizer.json` and `model.safetensors`. It answers every
   no-support outcome: no `[decisions] index` at all, a configured index that
   is missing, a question whose payload retrieves no labelled neighbour. No
   download path, no outbound client — the directory arrives the way any other
   air-gapped asset does.
3. **Hosted key** — reserved, not built. When tiers 1 and 2 cannot answer, the
   documented errors stand rather than a fabricated probability.

Tier 2 scores one NLI (premise, hypothesis) pair per candidate label — premise
= the payload, hypothesis = "This example is {label}." — reads the entailment
probability, and renormalises across the question's labels. A `noul` is scored
as two competing statements (the positive label and its negation); a `choice`
scores its own options. The trained model is the open `xerj-decide` artifact
([#1064](https://github.com/xerj-org/xerj/issues/1064)); the loader here is
validated end to end against a deterministic test fixture, and the artifact's
measured accuracy, calibration, and serving latency are published in
[DECIDE_MODEL.md](./DECIDE_MODEL.md) — the honest summary being that binary
`noul` on a trained label vocabulary is where tier 2 works, and transfer to an
untrained label vocabulary is ≈ chance.

The tier that answered is per-question evidence: `decisions.evidence.<id>.tier`
is `history` or `local`, and a local answer carries
`model: "xerj-decide-local-1"` — the same never-a-Jev-name discipline as the
vote's own id.

## The flywheel: every tier-2+ answer is cached (#1061)

The ladder's answers are not spent when served. Every tier-2 (local head)
answer — and every tier-3 answer, when that tier exists — is written back to
the `[decisions]` index as an ordinary document, so the answers a node
computes become the history that answers the next request:

- **What is written**: the configured `text_field` (the payload as decided),
  the configured `label_field` (the winning label), and the fixed `p`
  (probability served), `source` (the tier that produced it) and `ts` (RFC
  3339) fields. Because the text and label ride the configured fields, a
  cached answer is retrievable and votable exactly like a seeded example.
- **When**: after the response is complete. The write-back is spawned, never
  awaited by the request, and can neither delay nor fail it — an index that
  cannot be created or written is a log line, not an error the caller sees.
  A configured-but-missing index is created by the write-back itself, which
  is how a node armed with the local tier bootstraps its own history.
- **One cached answer per decided text**: the document id is a hash of the
  text (`decide-<xxh3-128 hex>`), so a question that escalates again
  overwrites its earlier cached answer instead of adding another vote for
  the local head's label.
- **What is not written**: history-tier answers (they are the index already —
  re-writing them would double their vote), and `/_decide` abstains (an
  abstain was not an answer; caching it would seed the history with a doubt).
  A node with no `[decisions] index` at all has nowhere to cache.
- **`source` on the wire**: every answer names its tier — `/_decide` at the
  top level, `/v1/systemone` per-question in `decisions.evidence.*.source`
  (the same string as `tier`, under the flywheel's name, and the value
  written into the cached document).

No replay claim is made yet: the Banking77 replay gate in the issue (after
2,000 cached hosted answers, ≥ 80 % of later traffic answered by history at
≥ 0.8 confidence and ≥ 0.97 accuracy) is a release-time measurement, and the
TypeSafe/Jev API-terms question on retaining provider outputs must be
answered before a tier-3 write-back ships.

## Human corrections: `human: true`, weighted `decisions.human_weight`×

A correction is an ordinary document with one extra field:

```sh
curl -X PUT localhost:9200/judgements/_doc/fix-42 -H 'content-type: application/json' \
  -d '{"text": "the message that was mislabelled", "label": "refund", "human": true}'
```

No new endpoint — the ordinary indexing path is the corrections path. In the
vote, a `human: true` neighbour weighs `decisions.human_weight`× (default
2.0, the issue's ≥ 2x floor; 1.0 makes corrections ordinary neighbours) its
reciprocal rank, so one correction outranks the cached answers it corrects.
Everything else keeps the 1/rank arithmetic the published measurements used.
Only a JSON boolean `true` earns the weight — `"true"`, `1`, and a missing
field are ordinary history — and `decisions.human_weight` must be a positive
number or the config refuses to boot (0 would erase a correction from the
vote, a negative value would invert it). The boosted weight is the weight the
vote used, so `/_decide` shows it in each neighbour's `weight`.

## Calibration: `p_cal` beside every `p_raw` (#1063)

A ladder probability ranks before it odds. On the FiQA rerank baseline a
hosted noul of 0.93 meant relevance 34 % of the time — ECE 0.3109
([benchmarks/decisions-calibration](../benchmarks/decisions-calibration)).
With `[decisions] calibration` set, the node fits a correction on its own
recorded outcomes and ships the calibrated probability BESIDE the raw one:

- **`isotonic`** — non-decreasing regression of the empirical positive rate
  on the raw probability (PAVA), interpolated between knots. The method that
  meets the FiQA gate: held-out ECE 0.0330 against raw 0.3831 on the same
  bins (gate ≤ 0.10).
- **`temperature`** — one scalar on the log-odds, `p' = σ(logit(p)/T)`,
  fitted on negative log-likelihood. Rank-preserving by construction, and
  the right tool on a thin history; on the FiQA-shaped curve it barely moves
  (held-out 0.3903) because one scalar cannot map 0.93 → 0.34 while keeping
  0.05 → 0.0002.

Both fit on a **deterministic held-out fifth** of the outcome pairs
(stable-sorted, every fifth held out — no RNG, no clock, so the same history
produces bit-identical `p_cal`), and the fit is cached per index and
refreshed at most once a minute; `GET /_decide/_calibration` always refits.

**Recording an outcome — the fit's data contract.** A history document is a
calibration pair when it carries the truth `label` (the configured
`label_field`), the `p` that was served **for that document's own label**,
and **no `source` field**:

```sh
curl -X PUT localhost:9200/judgements/_doc/outcome-42 -H 'content-type: application/json' \
  -d '{"text": "the message that was decided", "label": "false", "p": 0.9}'
```

That reads as "the system said 0.9 for its own label; the truth was `false`"
— the positive-label probability 1 − 0.9 against outcome 0. A document with
`source` is a flywheel answer, i.e. the model's own prediction, and is
excluded: a fit on predictions would learn the model is right by
construction. `human: true` corrections carrying the `p` they correct are the
best pairs there are — truth plus the number that was wrong. Ten labelled
pairs minimum, or the node says so instead of extrapolating.

**The honesty rules, all three on the wire.** No calibration configured → no
`p_cal` field at all (`calibration: {"configured": "none"}` on `/_decide`).
Configured but unfitted → `p_cal: null` with the reason published in the
`calibration` block beside it. Fitted → `p_cal` is the fit's value, and it is
never a copy of `p_raw` pretending to be calibrated. Whenever `p_cal`
exists, the `calibration` block rides with it — method, scope, the fit's ECE
raw and calibrated, and what it was fitted on — so **no probability ships
without an ECE beside it**. `p_raw` is always the honest raw share;
`confidence` keeps its raw meaning and the abstain gate keeps applying to it
(calibration changes what the number means, not the verdict).

**Scope.** The calibrated quantity is the positive label's probability — the
noul's answer, `/_decide`'s `p_raw`. Per-option `choice` shares are a
different quantity and are not calibrated by this fit (the block says
`"scope": "noul"`). The fit applies to every tier's noul through one seam —
history vote, local head, and (when that tier is built) a hosted passthrough
score — with no usable fit meaning "say so", never `p_cal = p_raw`.

## Set it up

```toml
# xerj.toml
[decisions]
index          = "judgements"   # an index of labelled examples; empty (the default) = both endpoints 503 (unless the local tier below is armed)
k              = 10
label_field    = "label"
text_field     = "text"
positive_label = "true"         # the label whose share a noul answers
min_confidence = 0.0            # /_decide abstains below this
escalate_below = 0.8            # with the local tier armed, a vote whose winner holds less than this share escalates to it (0.0 = off; #1094)
human_weight   = 2.0            # vote-weight multiplier for history docs carrying human: true
calibration    = "none"         # none | isotonic | temperature — p_cal beside every p_raw (#1063)
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
index, k, the requested model, and per-question evidence (the tier and source
that answered, winning label, its support, how many neighbours were found and
voted) — where every verified client ignores them.

Two deliberate breaks from the hosted API, both tested so they cannot regress:

1. **`model` is echoed as `xerj-history-vote-1`, never as a Jev model name.**
   Echoing `jev-1.13.0` would claim these probabilities are the hosted
   model's. The requested name is reported as `decisions.requested_model`.
2. **Zero support is a 422 naming the question ids, never a fabricated 0.5.**
   A question whose vote finds no labelled neighbour carries no information;
   inventing a probability is the silent-fake defect class this project
   treats as a bug. `/_decide` says the same thing as `abstain` + `reason`.
   The local tier is the documented exception: armed, it answers those same
   questions with the head's probabilities instead (200 where the vote 422'd,
   and no 503 on a node with no `[decisions] index` at all). Request-shape
   refusals — no payload to classify — are never rescued by it.

`score` questions (2–10 ordinal levels) have no vote analogue and no
benchmark: 422, naming the alternative.

## `POST /_decide`

The audit surface. Per request: `index` (required — optional when the local
tier is armed), `question` (required), `k` (default the configured 10,
clamped 1..100), `positive_label` (default the configured one). The response
returns the label, its confidence, the verdict (`abstain` below
`decisions.min_confidence`, or when no labelled neighbour exists), the **tier**
and **source** that answered (the same tier name under both fields), the
**`p_raw`** — the positive label's vote share, the quantity a noul answers,
not the winner's confidence — with **`p_cal`** beside it when calibration is
configured (see above), and the **neighbours** — each with `_id`, `label`,
`_score` (the engine's own BM25 score), `weight` (1/rank, ×
`decisions.human_weight` for a `human: true` document), and the text — so
every answer can be checked against the evidence that produced it. A local-tier answer has no neighbours (having none is what
tier 2 means) and carries `tier: "local"`, `source: "local"`,
`model: "xerj-decide-local-1"`; when it did not abstain and the request named
an index, it is cached into that index (the flywheel above).

```sh
curl -s localhost:9200/_decide -H 'content-type: application/json' \
  -d '{"index":"judgements","question":"refund my subscription","k":5}'
```

## `GET /_decide/_calibration`

The publication (#1063): the reliability curve of a labelled history — every
occupied bin's `[lo, hi)`, count, mean probability and empirical positive
rate — for the raw probabilities and (when fitted) the calibrated ones, the
ECE of each, the fit's parameters (an isotonic fit's knots, or the
temperature), the pair counts (fitted on / held out / total), and the date
range of the fitted documents. Always refits, so it doubles as the
operator's refresh; the fit it publishes becomes the cached fit every decide
request reads.

```sh
curl -s 'localhost:9200/_decide/_calibration?index=judgements'
```

`?index=` names the history (default: the configured `[decisions] index`; a
request naming nothing on a node that configures nothing is a 422, not an
empty curve). The raw curve and raw ECE publish even when calibration is not
configured — the reliability of the raw probabilities is audit information
in its own right — with a `reason` naming the setting that would fit them.

## Numbers, and only the ones we measured

Measured, ours, reproducible (raw JSON and runners in the results dirs):
Banking77 77-way intent 0.933 accuracy / ECE 0.012 (86.9% of traffic clears
confidence 0.8 at 0.979 accuracy); SMS spam 0.983 accuracy / 0.944 F1 under a
millisecond on BM25 alone; both in
[benchmarks/decisions-as-retrieval](../benchmarks/decisions-as-retrieval).
The calibration gate: isotonic held-out ECE 0.0330 against raw 0.3831 on the
FiQA rerank baseline's own curve (published raw 0.3109 reproduced exactly;
gate ≤ 0.10), measured at bin granularity from the retained aggregates —
the pair-level re-run needs the model's raw scores and is the rc.80 final
form — in
[benchmarks/decisions-calibration](../benchmarks/decisions-calibration).
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
