# Model bundles: serving facts and counting from one directory

What this file is: the serving contract for the model directory that XERJ's
`/_facts` endpoint loads. Last updated 2026-11-05 (rc.101). If this
document and the engine disagree, the engine's load checks and `bundle.json` win;
the [CHANGELOG](../CHANGELOG.md) carries the release summary. The request/response
contract for `/_facts` is in [FACTS_API.md](./FACTS_API.md); a step-by-step
"get it running" is in [MODEL_QUICKSTART.md](./MODEL_QUICKSTART.md).

The model is **XERJ-Query**. A node serves answers over a facts namespace from
the model directory it is started with, and it is **opt-in**: a node started
without a model directory serves no `/_facts` route at all (every `/_facts` path
answers `404 xerj_query_not_enabled`) and loads nothing. Point the node at the
model with the start-up flag (there is no `serve` subcommand — the node is the
bare `xerj` process):

```sh
xerj --facts-model-dir /path/to/model --insecure --data-dir ~/xerj-data
```

`XERJ_FACTS_MODEL_DIR=/path/to/model` is the environment equivalent; the flag
wins when both are given, and a blank value enables nothing. Use your production
start flags in place of `--insecure`; auth and TLS are unchanged by the model
directory.

Two shapes of `/path/to/model` are accepted:

- A **plain facts export** — a single role directory of weights and a
  tokenizer. This is the long-standing shape and still works unchanged.
- A **bundle** — a directory with a `bundle.json` that names a facts role and,
  optionally, a counting pair. A directory with no `bundle.json` is treated as
  a plain facts export.

Verify which one loaded:

```sh
curl -s localhost:9200/_facts | jq '{facts, counting}'
# plain export        -> counting is reported off (COUNTING_OFF)
# bundle with counting -> counting is reported on
```

## The rc.101 bundle

rc.101 ships one bundle as a release asset, `xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz`
(about 462 MiB unpacked; FP16 weights; facts served `w8`, counting served `f32`),
with a `.sha256` beside it. It is the `candidate_a_s47` bundle: on the honest
XERJ Score v2 (credit against Opus 5.5 = 100 on the facts + counting evaluation)
it scores **90.29** on the standard eval — not the fresh/sealed set; robustness
to new wordings and fresh-set counting are not separately measured for it. The
engine serves it bit-for-bit (2,387 of 2,387 evaluation items identical to the
offline run). Download and verify:

```sh
curl -fsSL -O https://github.com/xerj-org/xerj/releases/download/v1.0.0-rc.101/xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz
curl -fsSL -O https://github.com/xerj-org/xerj/releases/download/v1.0.0-rc.101/xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz.sha256
sha256sum -c xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz.sha256   # -> ...: OK
mkdir -p ~/xerj-model && tar -xzf xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz -C ~/xerj-model --strip-components=1
jq -r .schema ~/xerj-model/bundle.json                           # -> xerj-model-bundle-v1
```

The installer does the same with `curl -fsSL https://xerj.org/get | sh -s -- --with-model`
(see [the install page](https://xerj.org/docs/install)).

## Layout

```
<model dir>/
  bundle.json
  facts/              manifest.json  config.json  model.safetensors  tokenizer.json
  counting/served/    manifest.json  config.json  model.safetensors  tokenizer.json
  counting/partner/   manifest.json  config.json  model.safetensors  tokenizer.json
```

The `counting/served` and `counting/partner` roles are present only when the
bundle serves counting. Model weights are produced by the exporter and live
outside this repository.

## `bundle.json` (schema `xerj-model-bundle-v1`)

It names the roles and how to serve them. It never carries weights, hashes, or
a training precision.

```json
{"schema": "xerj-model-bundle-v1",
 "facts":    {"path": "facts", "serve": "w8", "rule": "decline_v2"},
 "counting": {"served": "counting/served", "partner": "counting/partner",
              "serve": "f32", "router": "router_v3", "guard": "guard_v3",
              "verifier": "rc4", "signal": "A", "repairs": true}}
```

## `manifest.json` (schema `xerj-model-role-v1`)

One inside each role directory, written by the exporter. Its hashes travel with
the weights, so a role is self-describing.

```json
{"schema": "xerj-model-role-v1", "role": "facts|counting", "stack": "v5|v6",
 "arm": "external_executor", "alphabet_sha256": "...", "tokenizer_sha256": "...",
 "weights_sha256": "...",
 "provenance": {"run": "...", "seed": 47, "source_precision": "fp32|fp16",
                "source_sha256": "..."}}
```

## Rules

1. **Precision-neutral.** `provenance` is for people; no code reads it. A test
   greps that no Rust path branches on `source_precision`. `model.safetensors`
   may hold F32 or F16 tensors; both widen to f32 at load, and every later step,
   `w8` included, is the same path. The tensor type is not a manifest field.
   (The shipped XERJ-Query model is FP16 throughout; `w8` and the int8-guard
   path are speed encodings of those same weights, not a different precision.)
2. **Directory swap.** Replace `facts/`, `counting/served/`, or
   `counting/partner/` as a whole — each with its own `manifest.json` — with no
   change to `bundle.json` or to code. Swap by renaming the directory while the
   node is stopped, then restart. There is no hot reload.
3. **Serving precision is per role**, set in `bundle.json`. `XERJ_FACTS_PRECISION`
   overrides the facts role only. The default counting precision is `f32`.
4. **Load checks** (each refused with a clear error): a role's weights and
   tokenizer hashes match its manifest; `served` and `partner` share one `stack`
   and one `alphabet_sha256`; every rule name is known. A broken **facts** role
   leaves the node answering 503 for `/_facts`. A broken or absent **counting**
   role does not stop the node: facts keeps serving and only counting questions
   decline with `COUNTING_OFF`.
5. **Serve rule (the counting cascade).** A counting answer is served only when
   the question routed to counting, its hard checks passed, the executor reported
   `ANSWER` and `COMPLETE`, and the partner model produced the same program hash.
   Otherwise the fact path answers under `decline_v2`. The partner model runs
   only when the first conditions hold. The response records the outcome under a
   counting block (`program_sha256`, `partner_agreement`); see
   [FACTS_API.md](./FACTS_API.md#the-counting-cascade).

## The counting interface (for developers)

The counting lane owns `xerj_serve::counting`; the facts serve rule consumes it:

```rust
pub struct CountingOutcome {
    pub routed: bool,
    pub hard_ok: bool,
    pub program_sha256: Option<String>,
    pub status: String,
    pub answer: Option<serde_json::Value>,
    pub notes: serde_json::Value,
}
```

## What an answer carries

Read `final_status` (`ANSWER`, `CONFLICT`, `UNKNOWN`, or `UNSUPPORTED`) and
`final_answer`. The plain `status` and `answer` are the unchecked program's and
are not to be relied on. `path` says which side answered: `counting` (with
`program_sha256` and `partner_agreement`) or `facts`. Operational problems are
HTTP errors, never an `UNKNOWN`: 404 `xerj_query_not_enabled` (node started
without `--facts-model-dir`), 503 `MODEL_UNAVAILABLE` (no usable model),
503 `ENRICHMENT_PENDING` (namespace rebuilding; retry), 504 `ASK_DEADLINE`,
429 `ASK_QUEUE_FULL`, 409 `REVISION_NOT_COMMITTED`, 403 (this key cannot read
the namespace). The full field-by-field contract, including the raw->verified
split and the revision/phase note, is in [FACTS_API.md](./FACTS_API.md).
