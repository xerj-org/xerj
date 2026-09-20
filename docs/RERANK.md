# Reranking search results (`rerank`)

> **There are two providers, and only one of them sends your data off the
> machine.**
>
> `"provider": "jev"` (the default) POSTs the question and the text of up to
> `window` hits (default 30, maximum 300) to a third-party API. That is the only
> **search-time** feature that sends document text off the node. Two other
> features send text off the node, both operator configuration and off by
> default: `[embedding] default_endpoint` (`--embed-mode proxy`) sends document
> text at write time and query text at search time to an external embeddings
> API, and the WAL tap (`PUT /_xerj/wal_tap`) replays every write on tapped
> indices to an external `_bulk` endpoint. The node's other outbound
> connections carry no document or query text: the one-time HuggingFace model
> downloads for `--embed-mode neural` and for the local judge, and Raft messages
> to your own peers in cluster mode. Reranking through the hosted provider is
> off until an operator supplies a key, a request only triggers it by asking for
> it, and an operator can forbid it outright with `[rerank] enabled = false`.
> Read [Every way data leaves a XERJ node](#every-way-data-leaves-a-xerj-node)
> before you turn it on.
>
> [`"provider": "local"`](#the-local-provider-a-cross-encoder-in-this-process)
> runs a cross-encoder inside the node. No key, no network call, no bill, and no
> document or query text leaves the host. It is **opt-in per request and not the
> default**, because on our own BEIR harness it does not beat the hybrid RRF
> ranking XERJ already ships — see
> [What we measured](#what-we-measured-no-tier-beats-hybrid-rrf-on-every-dataset).

`rerank` is an optional second stage on `POST /{index}/_search`. The engine
retrieves and ranks as usual; the stage then hands the top hits to a relevance
judge, which returns one score per document. Hits are reordered by that score,
and the score replaces `_score`. The judge is either the hosted provider (a
third-party API that returns a calibrated probability) or the `local` provider
(a cross-encoder in this process, whose number is a ranking score and **not** a
calibrated probability — that distinction is
[below](#the-score-is-a-ranking-score-not-a-calibrated-probability)).

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
| `rerank.endpoint` | `TYPESAFE_ENDPOINT` | `https://api.typesafe.ai/v1/systemone` | Where documents are POSTed. Must be an absolute `http(s)://` URL with no `user:password@` in it. The node refuses to start otherwise, **whichever place the value came from** — the config file at config load, the environment variable at boot (`TYPESAFE_ENDPOINT: rerank.endpoint must not carry credentials…`). |

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
  "providers": ["jev", "local"],
  "local": {
    "compiled_in": true, "enabled": true, "download": true,
    "default_model": "small", "threads": 16, "max_inflight": 2, "max_pair_tokens": 512,
    "models": [
      { "model": "small", "repository": "cross-encoder/ms-marco-MiniLM-L6-v2",
        "download_mb": 91, "resident_mb": 349, "licence": "Apache-2.0",
        "training_data": "trained on MS MARCO passage ranking (model card). …",
        "calibration": { "method": "none", "scale": 1.0, "bias": 0.0,
                         "fitted_on": "none (raw sigmoid)" },
        "state": "loaded" }
    ],
    "data_egress": "none: documents are scored in this process. …"
  },
  "api_key":  { "set": true, "source": "config" },
  "endpoint": { "url": "https://api.typesafe.ai/v1/systemone", "source": "default" },
  "defaults": { "provider": "jev", "model": "jev-latest", "window": 30, "batch": 30,
                "max_concurrency": 8, "max_doc_chars": 1200, "timeout_ms": 10000 },
  "limits":   { "max_docs_per_call": 30, "max_window": 300, "max_concurrency": 16,
                "max_doc_chars": 8000, "max_timeout_ms": 60000,
                "max_instructions_chars": 2000, "max_query_chars": 4000,
                "max_model_chars": 128, "max_fields": 64, "max_field_name_chars": 256 },
  "data_egress": "A search that carries a `rerank` block for a hosted provider sends the text of up to `window` hits, and the query, to the endpoint above; provider `local` scores in this process and sends nothing. A hosted rerank provider is the only search-time feature that sends document text off the node. Two other features send text off the node, both operator configuration and off by default: `[embedding] default_endpoint` (`--embed-mode proxy`) sends document text at write time and query text at search time to an external embeddings API, and the WAL tap (`PUT /_xerj/wal_tap`) replays every write on tapped indices to an external `_bulk` endpoint. The node's other outbound connections carry no document or query text: the one-time HuggingFace model download for `--embed-mode neural` and for the `local` rerank provider's cross-encoder, Raft messages (index names, mappings, shard assignments) to the configured peers in cluster mode, and object storage — the `S3Backend` client, which nothing on the segment path constructs today, and `xerj autoindex s3://`, which names buckets and keys to the endpoint you configure and reads objects IN."
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

## The `local` provider: a cross-encoder in this process

```json
POST /kb/_search
{ "query": { "match": { "body": "vitamin d supplementation bone density" } },
  "rerank": { "provider": "local", "window": 30 } }
```

No API key. No network call. No token bill. Nothing about the query or the
documents leaves the host — the model is a file on this machine and scoring is a
function call. That is the whole reason this provider exists: a second-stage
judge on an air-gapped node, on a laptop with no account anywhere, and on a
corpus whose text is not allowed to travel.

It also inverts one operator switch, so read this before you rely on either:
**`[rerank] enabled = false` does not switch the local provider off.** That
setting exists to forbid egress, and this provider has none. `[judge] enabled =
false` is the one that refuses it (403).

A cross-encoder reads the query and one document *together* and emits a single
relevance logit. That is the opposite trade from the engine's embedder, which
reads each text alone so the vectors can be indexed: a cross-encoder cannot be
precomputed and costs one forward pass per candidate, and in exchange every
attention layer sees both sides. So it is a second stage over a short window,
never a first stage — and the window is what you pay for, linearly.

### Turn it on

Nothing to configure for the default path: the provider is compiled into the
stock release binaries and loads its model the first time a request asks for it.
The `[judge]` block exists for the cases where the defaults are wrong.

```toml
# xerj.toml
[judge]
enabled      = true      # false → the local provider answers 403
download     = true      # false → never open a connection; the model must be on disk
cache_dir    = ""        # "" = HF_HOME, else ~/.cache/huggingface/hub
model_dir    = ""        # "" = use the cache. Air-gapped: <model_dir>/rerank-<tier>/
threads      = 0         # 0 = auto (cores, capped at 16 — see the latency sweep)
max_inflight = 2         # scoring calls admitted at once
rerank_model = "small"   # tier used when a request names none
```

| Setting | Default | Meaning |
|---|---|---|
| `judge.enabled` | `true` | `false` refuses `"provider": "local"` with HTTP 403. Independent of `[rerank] enabled`, which governs egress and so does not apply here. |
| `judge.download` | `true` | `false` never contacts huggingface.co. A model that is not already on disk is then HTTP 503, naming the file it wanted, rather than a silent fetch. |
| `judge.cache_dir` | HF default | Where the Hugging Face cache lives. |
| `judge.model_dir` | unset | Air-gapped root. Stage `config.json`, `tokenizer.json` and `model.safetensors` under `<model_dir>/rerank-<tier>/`. |
| `judge.threads` | `0` (auto) | Width of the judge's own rayon pool — how many of one window's forward passes run at once (#964). Auto is every core the resource policy grants latency work, capped at 16 — and the cap is measured, not guessed: per-window latency keeps improving to 16 concurrent passes and turns back above it (see the sweep below). |
| `judge.max_inflight` | `2` | Scoring calls admitted at once. Both share the one pool, so this bounds queueing, not CPU. |
| `judge.rerank_model` | `"small"` | The tier a request that names no `rerank.model` gets. |

Three request options are **refused by name** for this provider rather than
quietly ignored, because a caller who set them and got something else has been
misled: `rerank.instructions` (a cross-encoder reads no instructions — put what
matters in `rerank.query`), `rerank.batch` (forward passes are sized by a
padded-token budget, not a document count) and `rerank.max_concurrency` (the
thread budget is the server's). `rerank.max_doc_chars` defaults to **4,000**
here rather than the hosted provider's 1,200: the binding limit is the model's
512-token window, so the character clip only has to stop a megabyte field from
being tokenised for nothing.

Verify, without loading a model or touching the network:

```sh
curl -s -H "Authorization: ApiKey $ADMIN_KEY" localhost:9200/_xerj/rerank | jq .local
```

Expect `"compiled_in": true`, `"enabled": true`, a `models` array with three
entries, and each model's `state` — `not_downloaded`, `on_disk`, `loading`,
`loaded` or `failed`. A first search with `"provider": "local"` on a node that
has never downloaded the model answers **HTTP 200 with the engine's own order
and `_rerank.applied: false`**, while the download runs in the background; the
next search gets the reranked order. That is the deliberate behaviour, not a
bug: a 90 MB download is not something to hold a search request open for.

### The three models, their licences, and which is the default

The table of models is **closed** — a request picks a tier by name and can never
make the server fetch an arbitrary repository. Each entry pins a commit and the
sha256 of `model.safetensors`, checked on load, so a moved `main` cannot change
what your node scores with.

| Tier | Model | Download | Resident (F32) | Licence | Training-data note |
|---|---|---:|---:|---|---|
| `small` **(default)** | [`cross-encoder/ms-marco-MiniLM-L6-v2`](https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2) | 91 MB | ~349 MB | Apache-2.0 | Trained on MS MARCO passage ranking. Microsoft's MS MARCO terms say the datasets are "intended for non-commercial research purposes only". The **weights** are Apache-2.0; whether dataset terms reach a model trained on the data is not settled. Take advice before commercial use. |
| `base` | [`BAAI/bge-reranker-base`](https://huggingface.co/BAAI/bge-reranker-base) | 1.1 GB | ~1.1 GB | MIT | The card says the released models "can be used for commercial purposes free of charge" and that it was trained on "multilingual pair data", without listing the datasets. BAAI's published English fine-tuning data for its other models includes MS MARCO, so assume the same open question. |
| `large` | [`BAAI/bge-reranker-v2-m3`](https://huggingface.co/BAAI/bge-reranker-v2-m3) | 2.3 GB | ~4.6 GB | Apache-2.0 | Trained on bge-m3-data, Quora and FEVER per the card; bge-m3-data lists MS MARCO among its sources — the same unsettled question as `small`. |

**`small` is the default tier**, and the reason is *not* "it is the most
accurate" — on SciFact it is the worst of the two tiers we measured. It is the
default because it is the only one that fits a laptop without thinking about it
(91 MB down, ~349 MB resident, against 4.6 GB resident for `large`) and because
it is **3.1× faster per pair** on a CPU than `base` (measured, next sections).
Accuracy would not settle the question anyway: `base` was the best SciFact arm
we measured and the *worse* of the two on NFCorpus. **Measure the tiers on your
own corpus** — `"rerank": {"provider": "local", "model": "base"}` is one field —
and do not assume a bigger cross-encoder is a better one.

**XERJ ships no weights.** Every tier is downloaded from the Hugging Face Hub on
first use, under its own licence, and XERJ pins the revision and a sha256 rather
than re-distributing the files. The licence that applies to what you do with the
output is the model's, not XERJ's Apache-2.0.

### The score is a ranking score, not a calibrated probability

With the hosted provider, `_score` is a probability and `rerank.min_score` is a
threshold that means the same thing on every query. **With the local provider
today, it is not.** `_score = sigmoid(scale × logit + bias)`, and every shipped
tier carries `scale = 1, bias = 0` — the raw sigmoid. The MiniLM model emits
logits around ±10, so almost every document comes out as 0.00 or 1.00 whatever
its real chance of being relevant. The ordering is right; the number is not a
probability, and `rerank.min_score` against it is a knob you have to tune per
corpus rather than a meaningful cut-off.

`_rerank.local.calibration` reports this per response, so a client can tell:

```json
"calibration": { "method": "none", "scale": 1.0, "bias": 0.0,
                 "fitted_on": "none (raw sigmoid)" }
```

We tried to fit one and are not shipping it. Platt scaling and temperature
scaling were both fitted by maximum likelihood on a held-out split (SciFact
`train`, NFCorpus `dev`, FiQA `dev`; the BM25 top-30 window, which is the
population a default local rerank actually judges) and **both fits diverged**:
the maximum-likelihood slope ran away to ~10^9–10^11 instead of converging,
which is what a logistic fit does when the two classes are almost separable in
the one feature it has. A map with a slope of 10^11 is a step function — it
would make every score exactly 0 or 1 and destroy the little resolution the raw
sigmoid has. Expected calibration error on the test split, `small` tier, pooled
over all three datasets (35,626 pairs, 5.8% positive): **0.144 raw, 0.138 under
the Platt fit, 0.167 under the temperature fit** — the "calibrated" map is not
meaningfully better than the raw one, and one of the two is worse.

The `base` tier's fit, by contrast, **converged** — Platt `scale = 0.4331,
bias = −0.8912`, fitted on 7,651 held-out pairs — and it works: pooled expected
calibration error on the two test splits falls from **0.0509 raw to 0.0094**,
a 5.4× improvement (temperature scaling: 0.0444). That is a real result and it
still does not ship, for one reason: it was fitted and evaluated on the **same
two corpora** (SciFact and NFCorpus), and there is no out-of-domain check. A map
that is well calibrated on the two datasets it was tuned across is not evidence
that it is calibrated on yours, and "probability" is a word this project only
uses when it has that evidence.

So every tier ships `Calibration::NONE`, every response says
`score_kind: "relevance"`, and **no XERJ surface calls the local score
calibrated.** The next step is precise rather than vague: score a third corpus
with `base`, fit on two and report expected calibration error on the third; if
it holds, `base` ships its Platt map and starts reporting `"probability"` with
no code change, because the provider derives `score_kind` from the tier's own
calibration.

### What we measured: no tier beats hybrid RRF on every dataset

Three public BEIR datasets, nDCG@10 on each one's `test` split, against a node
running `--embed-mode neural` (`all-MiniLM-L6-v2`, CPU) so that the hybrid arm is
a real hybrid and not the default lexical feature-hashing embedder. One node, one
shard, default settings. Method, raw output and the reproduce commands:
[`benchmarks/local-judge`](../benchmarks/local-judge).

| Arm | SciFact<br>300 q, 5,183 docs | NFCorpus<br>323 q, 3,633 docs | FiQA<br>648 q, 57,638 docs |
|---|---:|---:|---:|
| BM25 (`multi_match` on title, text) | 0.6572 | 0.3016 | 0.2382 |
| BM25 top-30 → local `small` | 0.6824 | 0.3370 | 0.3160 |
| BM25 top-100 → local `small` | 0.6792 | 0.3420 | 0.3337 |
| BM25 top-30 → local `base` | 0.7084 | 0.3141 | not run |
| **Hybrid RRF — the arm that already ships** | **0.7021** | **0.3445** | **0.3572** |
| Hybrid top-30 → local `small` | 0.6936 | **0.3597** | **0.3751** |
| Hybrid top-100 → local `small` | 0.6872 | 0.3575 | 0.3697 |
| Hybrid top-30 → local `base` | **0.7186** | 0.3328 | not run |

The column that decides it is the difference from hybrid RRF on the same
queries, with a 95% paired-bootstrap interval (2,000 resamples):

| Arm | SciFact | NFCorpus | FiQA |
|---|---|---|---|
| Hybrid top-30 → `small` | −0.0085 `[−0.0367, +0.0201]` | **+0.0152** `[+0.0047, +0.0268]` | **+0.0179** `[+0.0025, +0.0322]` |
| Hybrid top-100 → `small` | −0.0150 `[−0.0450, +0.0163]` | **+0.0129** `[+0.0019, +0.0243]` | +0.0125 `[−0.0031, +0.0276]` |
| Hybrid top-30 → `base` | +0.0165 `[−0.0093, +0.0425]` | −0.0117 `[−0.0246, +0.0013]` | not run |

**No configuration beats hybrid RRF everywhere, and the two tiers do not agree
about which corpus they help.** `small` gains on NFCorpus and FiQA (intervals
excluding zero) and is a wash on SciFact; `base` is the other way round — it is
the best SciFact arm we measured, and it is *worse* than the smaller model on
NFCorpus. A bigger cross-encoder is not uniformly a better one.

The BM25 rows say something different, and both readings matter.

**Against a BM25-only first stage the local judge is a large, unambiguous win**
— `small` adds +0.0253 (SciFact), +0.0354 (NFCorpus) and +0.0777 (FiQA) to BM25
alone, and `base` adds +0.0512 on SciFact. On FiQA it closes two thirds of the
distance between BM25 and hybrid without a single vector being indexed.
That is the case this provider is genuinely for: a node with no vectors indexed,
or a corpus you are not going to re-index.

**Against hybrid RRF it is not the clear win a default would need.** Two of the
five measured hybrid arms gain by more than the noise floor below, one gains by
less than it, and two lose. A default has to be right on a corpus nobody
measured, and this evidence does not support that.

So it ships **opt-in**. Three reasons, in order of weight: it does not beat what
already ships on every dataset; one call costs seconds of CPU (next section) where
hybrid costs none; and its score is not calibrated. Any one of those would be
enough to keep it off by default.

For context — and **not** as a controlled comparison, because we did not run
them and they rerank their own first-stage shortlist, not ours — the figures the
[`hev/jev-rerank`](https://github.com/hev/jev-rerank) README publishes are
0.768 / 0.358 for Jev, 0.755 / 0.357 for Voyage rerank-3 and 0.745 / 0.340 for
Cohere rerank-v3.5 on SciFact / NFCorpus.

#### Does the biggest model change the answer? No.

`large` (`bge-reranker-v2-m3`, 568M parameters) costs about twice `base` per
pair on a CPU, so it was scored on a **seeded 120-query subset** of each dataset
rather than the full sets. Every arm in the table below is recomputed on that
same subset, so the rows are paired with each other — but they are a different
query set from the tables above, and the two must not be read across.

<!--LARGETABLE-->

#### How much of this is noise

XERJ's hybrid fusion orders tied scores by a per-process seed
([#940](https://github.com/xerj-org/xerj/issues/940)), so the hybrid baseline
itself moves between node restarts. Measured rather than assumed, over three
fresh node processes on unchanged indices:

| | run 1 | run 2 | run 3 | spread |
|---|---:|---:|---:|---:|
| SciFact hybrid RRF | 0.7021 | 0.7034 | 0.7019 | 0.0015 |
| NFCorpus hybrid RRF | 0.3445 | 0.3436 | 0.3438 | 0.0009 |

Earlier runs of the same arm on the same data widen that band rather than
contradict it: the `benchmarks/neural-path-triage` triple scored 0.6993, 0.7023
and 0.7044 on SciFact, so **across all six recorded runs the SciFact hybrid arm
spans 0.6993–0.7044 — a band of 0.0051** — and NFCorpus spans 0.3436–0.3448
(0.0012). The conservative reading is the one this page uses: **0.0051 is the
SciFact noise floor and 0.0012 is NFCorpus's.** Both NFCorpus gains clear it;
the SciFact loss does not, which is why it is called a wash and not a loss.

Two further facts from the same three runs, which is why the reranked numbers
can be compared at all:

- the candidate **set** was identical on **100%** of queries — only the order of
  tied hits moved — while the top-10 *order* was identical on just 44% (SciFact)
  and 50% (NFCorpus) of them;
- consequently every reranked arm scored **identically across all three
  processes, to four decimal places** (spread 0.0000): a second stage that
  reorders an unchanged set by model score cannot inherit the tie-seed noise.
  `eval.py` reports this as `rerank_repeats` so it is checked, not asserted.

### Latency on a CPU: seconds, not milliseconds

**One 30-document call is the unit a search pays for**, so that is what is timed:
one `pair_score` process, the machine otherwise idle, the load average recorded
before and after each point. Hardware: one x86-64 host, 32 cores, 119 GB RAM,
**no GPU**, `target-cpu=x86-64-v3`. Documents are BEIR FiQA passages clipped at
the provider's 4,000-character default — real prose, not toy strings.

<!--LATTABLE-->

Three things to take from it.

<!--LATNOTES-->

### When it is worth turning on

Given the measurement above, the honest advice is narrow:

- **Do not switch it on for general relevance on a node that already runs
  hybrid search.** Hybrid RRF ships today and costs no model forward pass per
  hit; no local tier beat it on every dataset. Run the hybrid query instead.
- **Do switch it on when hybrid is not available to you** — no
  `--embed-mode neural`, no vectors indexed, or a corpus indexed lexically that
  you are not going to re-index. Against a BM25-only first stage the local judge
  was a large, unambiguous gain on all three datasets, and that is the case it
  is genuinely for.
- **Do switch it on when the hosted provider is the alternative and the data
  cannot leave.** A slightly-worse-than-hybrid local ranking is a different
  trade from a better ranking that sends your documents to a third party, and
  only you can make it.
- **If you do switch it on, measure both tiers on your own corpus.** They
  disagreed about which of ours they helped, by margins bigger than the
  difference between switching the feature on and leaving it off. Our datasets
  are English and short-document, and two of the three are ones every reranker
  is tuned on. Yours are probably neither.

And two things to plan around whichever way you go: the per-call latency above,
and the fact that **every request is judged from scratch** — there is no verdict
cache, so three pages over one 30-document window are three full rerank calls.
Fetch the window once and page client-side.

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
| `model` | string | `"jev-latest"` | ≤ 128 characters | Sent to the provider verbatim. |
| `window` | integer | 30 | 1–300 | How many of the engine's top hits are judged. **Every document in the window is a paid judgement.** |
| `min_score` | number | none | 0–1 | Drop hits whose probability is below this. It is a probability; 7 is a 400. |
| `query` | string | inferred | non-empty, ≤ 4,000 characters | The question documents are judged against. The limit also applies to a question read from the search query. See [The question](#the-question). |
| `fields` | string[] | every returned string field | 1–64 names, each ≤ 256 characters | Which returned fields are sent. **Exhaustive**: `["body"]` sends the body and nothing else, not even the title. See [What leaves the machine](#what-leaves-the-machine). |
| `instructions` | string | a generic relevance question | ≤ 2,000 characters | Overrides what "relevant" means. A support corpus and a code corpus do not mean the same thing by it. The provider's wire format carries the instructions inside **every** per-document question, so this string is sent once per judged document — which is why it has a ceiling. |
| `timeout_ms` | integer | 10000 | 1–60000 | Wall-clock budget for the **whole stage**, not per call. |
| `max_doc_chars` | integer | 1200 | 1–8000 | Title and text are each cut to this many characters, on a character boundary. |
| `batch` | integer | 30 | clamped to 30 | Documents per provider call. Values above 30 are lowered to 30, not refused: the ceiling is the provider's, not a caller mistake. |
| `max_concurrency` | integer | 8 | 1–16 | Provider calls in flight at once. |

Unknown keys are **refused by name**, not ignored. A caller who misspells
`min_score` as `treshold` gets a 400 naming `treshold`, instead of unpruned
results that look pruned.

The ceilings are the server's because the caller picks the window and the
operator pays for it. That includes the strings. `instructions` is repeated
once per judged document, so before it had a ceiling a single `size: 1` search
carrying a 1 MB `instructions` string at `window: 40` put 40 MB on the wire to
the provider (measured in review, against a test double). With the ceilings,
the most one search can send is window × (title + text, each cut at
`max_doc_chars`, + `instructions`) plus the question once per call. That is
arithmetic, not a measurement: at the default window and `max_doc_chars` with
instructions at their limit, 30 × (1,200 + 1,200 + 2,000) + 4,000 = 136,000
characters; at every maximum at once, 300 × (8,000 + 8,000 + 2,000) +
10 × 4,000 = 5.44 million characters, before JSON framing. An over-long value
is a 400 that names the field and the limit; the value itself is never echoed
back. The ceilings are also readable from `GET /_xerj/rerank` under `limits`.

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
    "unjudged": 0,
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
| `score_kind` | `"probability"` when `_score` is a calibrated 0–1 probability (the hosted provider); `"relevance"` when it is a monotone 0–1 ranking score that is **not** calibrated (every shipped local tier — see [The score is a ranking score](#the-score-is-a-ranking-score-not-a-calibrated-probability)); `"engine"` when it is still BM25 / fusion. A client that thresholds `_score` must branch on this. |
| `window` | Hits considered: `min(rerank.window, hits the engine returned)`. |
| `judged` | Documents the provider returned a verdict for. One verdict per document: an answer keyed to a document that was never sent, or sent in another call, is not counted. This is the figure `xerj_rerank_documents_judged_total` meters. |
| `unjudged` | Hits in the response that have no verdict — the provider did not answer for them, or they were skipped as blank. Their `_score` is `null` and they sort after every judged hit. Without `min_score`, `judged + unjudged` equals `window`. |
| `skipped_no_text` | Hits in the window with no string content in the response. They are not sent: a blank document costs a judgement and a verdict on nothing means nothing. |
| `pruned_below_min_score` | Judged hits removed by `rerank.min_score`. |
| `dropped_unjudged` | With `min_score` set, hits that were never judged cannot be shown to clear the bar, so they are dropped and counted here. |
| `fields_without_text` | Present only when there is something to report: the names in `rerank.fields` that **no hit in the window** returned any text for — a typo, a field these hits lack, or a field the projection removed. Those documents were judged without it. |
| `partial_failures` | Provider calls that did not deliver a verdict while others did: calls that failed, and calls the deadline stopped from ever being sent. The scored hits are still correctly ordered — each probability is absolute — but the window was not fully covered; `unjudged` says by how much. |
| `usage` | Tokens the provider reported, summed over every call that answered. Zero when the provider omits it. |
| `query` | The question that was judged, inferred or given. |
| `took_ms` | Time spent in the stage. The response's own `took` includes it. |

What changes on a hit: `_score` becomes the probability, and `hits.max_score`
becomes the top hit's probability. Hits that were not judged get `_score:
null` and sort **after** every judged hit, in the engine's order — an unjudged
document is not evidence of irrelevance, but it cannot be ranked against
documents that were scored. Equal probabilities keep the engine's order.

**`_score` is one kind per response.** When `applied` is `true`, every
`_score` is a probability or `null`; the engine's BM25 value is never left on
some hits beside probabilities on others. A client that sorts by `_score` or
scales by `hits.max_score` cannot tell a `1.63` BM25 from a `0.9` probability,
and `max_score` would be smaller than a later hit's score. `null` is what
Elasticsearch itself puts in `_score` when a hit has no comparable score, so
every client already handles it. `_score` and `hits.max_score` carry the
provider's probability at double precision: `0.9` from the provider is `0.9` on
the wire, so a threshold applied client-side agrees with `rerank.min_score`.

If the provider answered but not for a single document that was sent (every
answer keyed to something else), that is not a reranking: the response is the
degraded shape, `applied: false`, with the reason, and the operator's meters
count it as degraded, not applied.

## How it interacts with the rest of `_search`

The stage owns exactly one thing: the order, and with `min_score` the
membership, of `hits.hits`. Everything else in the response stays the engine's
and describes the **full match set**, not the window. Each row below is pinned
by a test in `rerank_stage_http.rs`.

| Combined with | Behaviour |
|---|---|
| `aggs` | **Unchanged.** Aggregations are computed over every matching document, and are identical with and without `rerank` — including when `min_score` prunes the page to nothing. |
| `hits.total`, `track_total_hits` | **The engine's total.** `true`, an integer cap and `false` each report exactly what they report without `rerank`. Four documents matched and two were judged irrelevant: `total` is 4, and `pruned_below_min_score` is 2. Both facts are true and the response says both. |
| `size`, `from` | Paging happens **inside the reranked window**. XERJ widens the engine's page to `window`, judges all of it, then cuts `from`/`size` out of the judge's order, so page two continues page one **as long as the provider gives the same probabilities on a repeat call** — there is no verdict cache, so every page request judges the whole window again (and pays for it again; see [Cost and concurrency facts](#cost-and-concurrency-facts)). Whether the real model is deterministic has not been verified by this project. To page without that dependency, ask for the window once (`size` = `window`) and page client-side. `from + size` must be ≤ `window`, or the request is a 400: a page cut across the window boundary would mix two rankings. |
| `highlight`, `fields`, `inner_hits`, `matched_queries` | **Travel with their hit.** The stage moves rendered hits, so nothing is recomputed and nothing is swapped between hits. |
| `"fields": ["_passage"]` | The matching passage is **judgeable**, and leads the text the judge reads. Name it in `rerank.fields` to send only the passage. |
| the large-response hint (`_xerj.hints`) | Its ready-to-send corrected request **keeps the `rerank` block**. A suggestion without it would be a different search, returning the engine's order. Its narrower projection also narrows what the judge is sent. |
| `_source` filtering | Decides what the judge sees — see the next section. `_source: false` with no `fields`, `docvalue_fields`, `stored_fields` or `script_fields` clause is a 400 (nothing can be judged); beside one that returns the text, it works, and the judge reads what came back — with or without `rerank.fields`: `"_source": false, "fields": ["body"], "rerank": {}` judges the `body` values returned under `fields`. |
| `explain` | The engine's explanation is kept, unaltered, under a new top node whose `value` equals the new `_score` and whose description says the score is a rerank probability. An `_explanation.value` that disagreed with `_score` would mislead. |
| `profile` | Unchanged; it profiles the engine. The stage reports its own time in `_rerank.took_ms`. |
| top-level `min_score` | The **engine's** threshold, applied to **engine** scores before the window is cut. It shapes the match set and `hits.total` exactly as it does without `rerank`, and it is never compared against a probability. `rerank.min_score` is the probability threshold. The two compose: engine bar first, probability bar second. |
| `knn` (top level) | Works. A vector is not a question, so a `knn` with no text `query` needs `rerank.query`. Beside a text query the question is read from the text query, and vector-only hits are inside the window the judge sees. Vectors are never sent. |
| `rescore` | Runs first, inside the engine. `rerank` has the last word. |
| `pit` (point in time) | Works: a PIT fixes the snapshot, not the order. Paging it with `search_after` is refused like any other `search_after`. |
| `index.max_result_window` smaller than `rerank.window` | **400**, in the caller's terms: `` `rerank.window` (30) exceeds `index.max_result_window` (20) on `kbsmall` ``. The stage asks the engine for `window` hits, so every participating index — including each one a wildcard expands to — must be able to return that many. Lower the window or raise the setting. |
| `"rerank": null` | The same as no `rerank` key, on every surface: `_search` runs without the stage, and an `_msearch` item or template carrying it is not refused. |
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
| `_search_scroll`, `?scroll=`, and the `_search/scroll` continuation | 400. A scroll streams the engine's order. |
| `_rank_eval` | **Per request.** A `requests[]` entry whose `request` carries `rerank` is reported under `failures` by its id and contributes nothing to `metric_score`; the other requests are still evaluated. `_rank_eval` runs the engine's ranking only, so scoring it while ignoring the block would publish "reranking changed nothing" about an order the judge never saw. To measure a reranked order, run the `_search` requests yourself and score the returned ids. |

`_count`, `_validate/query` and `_explain` take a query, not a search body, and
return no ordering; they ignore a `rerank` key as they ignore every other
search-body key. The native `/v1` search API and the gRPC Search RPC refuse the
block the same way the endpoints above do, and `"rerank": null` is an absent
block on all of them.

## What leaves the machine

**With `"provider": "local"`, nothing.** The cross-encoder runs in this process;
the list below is about a **hosted** provider. The rest of this section still
describes what the *local* judge is shown — the same text, built by the same
code — it just never leaves the host.

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
    `_source`; then every string value the hit returns under `fields` — which
    is where `fields`, `docvalue_fields`, `stored_fields` and `script_fields`
    put theirs — for a name `_source` did not already return, in name order.
    A value present in both is sent once. The passage goes **first** because
    text is cut at
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

### Every way data leaves a XERJ node

Reranking through a **hosted** provider is the only thing a **search request**
can do that sends document text off the node — the `local` provider scores
in-process and sends nothing. Reranking is not the only outbound connection a
node can make. This
is the complete list, taken from the engine source — every outbound HTTP or TCP
client under `engine/crates` — and kept complete by a test
(`engine/crates/xerj-rerank/tests/egress_inventory.rs`) that fails when a new
client appears in a source file this list does not account for:

| Path | What leaves the node | When | Default |
|---|---|---|---|
| **Rerank provider** (hosted) — `[rerank]`, `TYPESAFE_API_KEY` / `TYPESAFE_ENDPOINT` | The question, `rerank.instructions`, the model name, and the returned text of up to `window` hits (above) | A search that carries a `rerank` block naming a hosted provider | Off: no key is configured |
| **Local judge model download** — `[judge]`, provider `local` | HTTPS requests to the HuggingFace Hub naming the cross-encoder (`config.json`, `tokenizer.json`, `model.safetensors`); no document or query text | The first time the local judge loads without `judge.model_dir`; later starts read the local cache. Scoring itself sends nothing — ever | Off: nothing loads until a request asks for `"provider": "local"`; `[judge] download = false` forbids the fetch, and `--no-default-features` builds the provider out entirely |
| **Proxy embeddings** — `[embedding] default_endpoint`, `--embed-mode proxy` | Document text of the fields it embeds, and the text of queries it embeds | Document text at write time (index, `_bulk`, update, reindex); query text at search time, for every query it embeds (`semantic`, hybrid, semantic memory recall) | Off: `default_endpoint` is empty |
| **WAL tap** — `[wal_tap]`, `PUT /_xerj/wal_tap` | Every write on the tapped indices (indexed documents and deletes), as `_bulk` to `{target_url}/_bulk`; never system indices | Continuously, every `poll_interval_ms` | Off: `enabled = false` |
| **Neural model download** — `--embed-mode neural` | HTTPS requests to the HuggingFace Hub naming the model (`config.json`, `tokenizer.json`, `model.safetensors`); no document or query text | The first time the neural embedder loads without `embedding.local_model_dir`; later starts read the local cache | Off: the default embedder is lexical |
| **Cluster transport** — `[cluster] enabled = true` | Raft messages to the configured `peers`: index names, mappings, shard assignments, node addresses, cluster settings; no document text and no queries | While cluster mode runs | Off: single node |
| **Object storage backend** — `[storage]`, `S3Backend` | S3 requests to the configured endpoint: bucket and key names, ranged reads, and the credentials from the environment; no document text and no query text | Never today — nothing on the segment path constructs it and `storage.backend = "s3"` refuses to start (#965 would wire it) | Off, and refused |
| **`xerj autoindex s3://`** (client, not the node) — `--endpoint-url`, `AWS_*` | S3 requests to the configured endpoint: bucket and key names and the credentials; the objects travel INTO the machine, and their text then goes to the node URL you gave the client | While that command runs | Off: only when you pass an `s3://` root |

A node started with the defaults opens none of these connections, and makes no
telemetry, update-check or licence call. Two things outside the node complete
the picture:

- **The Console in a browser** asks Google Fonts for its typefaces. That is
  the browser's request, not the node's, and it carries no index data; the
  [air-gapped recipe](./recipes/air-gapped-deployment.md) names the three pages
  that link the fonts, and a blocked request falls back to system fonts.
- **The command-line clients in the same binary** (`xerj autoindex`,
  `xerj mcp`, `xerj search` and the others) send what they read to the node URL
  you give them, `http://localhost:9200` unless you say otherwise.
  `xerj feedback --open-pr` runs your `gh` to open a GitHub pull request with
  the report it drafted; without that flag it does nothing over the network.

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
| Provider answered 200 with a body over 2 MiB | **502** `rerank_exception` ("larger than 2097152 bytes"). A real answer for 30 documents is a few kilobytes; the node stops reading at the ceiling instead of letting the endpoint choose how much memory a search allocates. |
| Provider unreachable | **502** `rerank_exception`. |
| Provider answered 429, 529 or 5xx | Retried, up to 3 attempts per call, with a short exponential back-off and an extra pause after a 429 — but never past the deadline. If the deadline runs out first, degrade; if the retries run out first, 502. |

Contract and configuration faults do not fix themselves, and hiding them costs
the operator more than it saves. A surfaced fault returns **no hits**: handing
back the engine's order next to an error would invite a caller to ignore the
error.

If some provider calls succeed and others fail, or the deadline stops later
calls from being sent, the stage keeps what it has: the judged hits are ordered
by probability, the rest follow with `_score: null` in the engine's order, and
`partial_failures` says how many calls did not deliver.

The split follows the one Meilisearch uses for its reranking call in its
personalization module (return the original hits when the deadline is exceeded,
return the error otherwise). The approach, and the retry back-off constants,
were adapted from that module (MIT); the code comment in `xerj-rerank` cites
the lines.

## Cost and concurrency facts

These are the **hosted** provider's. Provider `local` differs on every line:
there are no provider calls (the whole window is one in-process batch loop sized
by a padded-token budget, not a document count), no rate limit and no connection
pool, `max_concurrency` is refused because the budget is the server's
`[judge] threads`, and `_rerank.usage` stays at zero because nothing is billed.
What it does share is the last item below — **no verdict cache**, so paging
re-judges the window, and there it costs CPU seconds rather than money.


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
- **Every request is judged from scratch.** There is no verdict cache: three
  page requests (`from` 0, 10, 20) over one 30-document window are three
  provider calls and 90 paid judgements, not 30. Fetch the window once and page
  client-side when cost or page-to-page consistency matters.
- `_rerank.usage` is the meter reading for the search. XERJ does not price it.
  It is advisory: a provider that reports a token count that is not a
  non-negative number meters as zero for that count, and never costs the
  caller a ranking whose verdicts were all valid.

## From an agent (MCP)

`xerj_search` and `xerj_hybrid_search` take an optional `rerank` argument with
the same fields. `true` or `{}` means defaults.

- With a **plain-string** `query`, the MCP server passes your string as
  `rerank.query`, because the plain-string path expands into a `bool` query that
  the node will not read a question from.
- `xerj_hybrid_search` **requires** `rerank.query`: a hybrid search has several
  sub-queries and no single question.
- The MCP server cannot supply a provider key. Without one on the node the tool
  call returns the node's 503; repeat the search without `rerank`, or pass
  `{"provider": "local"}`, which needs no key.
- The tool description tells an agent the three things it has to know about the
  local arm before choosing it: the score is **not** calibrated
  (`score_kind: "relevance"`), our own measurement puts it behind hybrid RRF so
  a hybrid search is usually the better call, and one 30-document call costs
  seconds of CPU. `engine/crates/xerj-mcp/src/lib.rs` has a test that fails if
  any of those three sentences is dropped.

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

The **local** provider was measured, by us, on the same harness and on a third
dataset as well: [What we measured](#what-we-measured-no-tier-beats-hybrid-rrf-on-every-dataset).
It is the only rerank quality figure in this document that XERJ actually ran.

Two things the bi-encoder runs above do say:

- Reordering a BM25 shortlist with the same **bi-encoder** scored **below**
  fusing with it, on both datasets. That is why the local provider is a
  *cross*-encoder: reusing the embedder as a reranker would be a regression with
  a good name. (A cross-encoder over the same BM25 shortlist does beat BM25 by a
  wide margin — see the section linked above — which is the distinction that
  note was pointing at.)
- Where a team already has labelled history, some yes/no and pick-one decisions
  need no judge model at all —
  [`benchmarks/decisions-as-retrieval`](../benchmarks/decisions-as-retrieval)
  measures that, also under `--embed-mode neural`.

## Credits

- The one-`noul`-per-document request shape:
  [`hev/jev-rerank`](https://github.com/hev/jev-rerank), Apache-2.0.
- The degrade-on-deadline, surface-on-contract failure policy, and the retry
  back-off shape (exponential in the attempt, a flat extra pause after a 429):
  Meilisearch's personalization module (MIT, not part of its Enterprise
  Edition). Approach and constants adapted; cited in `xerj-rerank/src/lib.rs`.
- The provider API is TypeSafe AI's System One
  (`POST /v1/systemone`, <https://docs.typesafe.ai/api>).
