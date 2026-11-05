# The `/_facts` API: asking a question and reading a verified answer

What this file is: the request/response reference for the `/_facts` endpoints —
the facts store, the exact executor, and the learned compiler the engine serves.
Last updated: 2026-11-05 (rc.101). If this document and the engine
disagree, the engine wins (`engine/crates/xerj-api/src/facts_api.rs` and
`engine/crates/xerj-serve/`); the [CHANGELOG](../CHANGELOG.md) carries the
release summary. Companion docs: [MODEL_BUNDLE.md](./MODEL_BUNDLE.md) (what the
model directory is and how roles are served) and
[MODEL_QUICKSTART.md](./MODEL_QUICKSTART.md) (get the model and serve it).

`/_facts` is the surface of **XERJ-Query**, the model that answers over a facts
namespace. It is compiled into the default build from rc.101 and is **off until
the server is started with `--facts-model-dir <DIR>`** (`XERJ_FACTS_MODEL_DIR` is
the environment equivalent; the flag wins when both are given). On a node started
without it, every path and method under `/_facts` answers
`404 xerj_query_not_enabled`, with a reason that names the flag; no model loads
and no facts thread starts, so the node is the same node rc.100 was.

```sh
xerj --facts-model-dir ~/xerj-model --insecure --data-dir ~/xerj-data
```

Verify: `curl -s localhost:9200/_facts | jq '.model.state'` prints `"loaded"`
(while the model is still loading it is not yet `loaded`; a node without the
flag answers 404 `xerj_query_not_enabled`).

## The surface

```text
GET    /_facts                          readable namespaces, residency, compiler status, _ask limits
GET    /_facts/{ns}[?fingerprint=true]  namespace status (revision, published, epochs, records, cleanup_pending)
DELETE /_facts/{ns}                     drop the namespace (index + projection)
POST   /_facts/{ns}/_world              one memory-world-v1 batch (documents, facts, edits)
POST   /_facts/{ns}/_bulk               {documents?, facts?, edits?}: the same as one batch
GET    /_facts/{ns}/{fact_id}[?revision] one fact and its visibility at a committed revision (no model)
DELETE /_facts/{ns}/{fact_id}           withdraw a fact (history stays readable)
POST   /_facts/{ns}/_lookup             {subject, relation, scope?, revision?} -> VALUE | UNKNOWN | CONFLICT (no model)
POST   /_facts/{ns}/_ask                {question, revision?, ...} -> a verified answer + evidence + program
```

`_lookup`, `_world`, `_bulk`, and `GET .../{fact_id}` use **no model** — they are
the deterministic store. Only `_ask` runs the model.

## Writing facts: `POST /_facts/{ns}/_bulk`

A fact must reference an available source version (`doc_id` + `version` present
in the same batch or already committed), or the whole batch is refused
`400 facts_contract_error` and nothing commits.

```json
{
  "documents": [{"doc_id": "d1", "version": "v1", "text": "Ana Bel is 170 cm tall."}],
  "facts": [{"fact_id": "f1", "subject": "Ana Bel", "relation": "height",
             "kind": "number", "value": "170", "unit": "cm",
             "doc_id": "d1", "version": "v1", "start": 11, "end": 14, "quote": "170"}]
}
```

`_world` additionally takes `base_documents` / `base_facts` and an `edits` array
(`{"op": "correct", "doc": {...}, "fact": {...}, "supersedes": "<fact_id>"}` or
`{"op": "withdraw_document", "doc_id, "version"}`). A commit returns the new
revision (`committed.to_rev`); `GET /_facts/{ns}` reports `revision` and
`published_rev`. A batch over `limits.max_actions_per_bulk` is refused before any
write (`413 facts_batch_too_large`).

## Asking: `POST /_facts/{ns}/_ask`

Request body:

| field | required | notes |
|---|---|---|
| `question` | yes | at most 4096 bytes |
| `revision` | no | ask at a committed revision — see "Revision and phase" below |
| `item_id` | no | at most 256 bytes; **salts manifest order only, it does not select a point in history** |
| `linker` | no | one of: `casefold_v3` |
| `manifest` | no | one of: `relation_aware`, `reference` |
| `verify` | no | one of: `served` (default), `bindings`, `bindings_path`, `bindings_all` |
| `explain` | no | include extra per-step detail |
| `wait_for_ms` | no | how long to wait for a projection that is rebuilding |
| `timeout_ms` | no | this request's deadline; it can only shorten the node's |

### The response, and the raw -> verified split

The answer carries two layers. **Read the `final_*` fields.** The plain
`status` / `answer` / `evidence_ids` are the *unchecked* program's output and are
not to be relied on.

```jsonc
{
  "model": "...", "linker": "casefold_v3", "question": "...", "item_id": null,

  // RAW — the program the model wrote and what it computed, unchecked:
  "status": "ANSWER",                 // ANSWER | UNKNOWN | CONFLICT | UNSUPPORTED
  "answer": "...",
  "evidence_ids": ["f1"],
  "program": [["LOOKUP", 0]],         // stack-v5: [op, pointer, "in"|null]
  "trace": [{"step": 0, "op": "LOOKUP", "pointer": 0, "fact_id": "f1",
             "result": "...", "illegal": false, "inverse": false}],

  // VERIFIED — what the node will stand behind:
  "final_status": "ANSWER",           // ANSWER | CONFLICT | UNKNOWN | UNSUPPORTED
  "final_answer": "...",
  "final_evidence_ids": ["f1"],

  "verify": {"applies": true, "veto": false, "reason": "", "rule": "..."},
  "path": "facts",                    // which side answered: "facts" or "counting"

  // present only under the default verify rule (decline_v2):
  "reason_code": null,
  "decline": {"policy": "decline_v2", "stage": "P3+D2+D2b", "accepted": true,
              "reason_code": null, "policy_code": null, "detail": null,
              "final_status": "ANSWER", "final_answer": "...",
              "final_evidence_ids": ["f1"]},

  "timings_us": {"link": 0.0, "manifest": 0.0, "model": 0.0, "executor": 0.0,
                 "verify": 0.0, "total": 0.0},
  "latency_ms": 0.0, "namespace": "...", "handler_ms": 0.0
}
```

- **`status` / `answer` / `evidence_ids`** — the raw program, before any check.
- **`final_status` / `final_answer` / `final_evidence_ids`** — after the verify
  rule and, on a counting question, after partner agreement. This is the answer.
- **`verify`** — `served` (default; the `verify_v5` rule) runs *after* the
  episode, so it changes no decision, program, or raw answer: only `verify`,
  `final_status`, and `final_answer`. A veto turns a raw `ANSWER` into a
  `final_status` of `UNKNOWN`.
- **`decline`** (default rule `decline_v2`) — the calibrated accept/decline
  decision. `accepted: false` sets `final_status` to a decline (`UNSUPPORTED`,
  or `UNKNOWN`/`CONFLICT` passed through with its reason; a *counting* question's
  decline is always `UNSUPPORTED`). A repair, when one happened, is logged under
  `decline.repair` (`D2R_REPAIRED`) with the program and answer it replaced.
- A raw `UNKNOWN` / `CONFLICT` is a **factual** outcome, not an error. Every
  operational problem is an HTTP error instead (below).

### The counting cascade

When the question routes to counting, the answer is served only if **all** of
these hold; otherwise the fact path answers under `decline_v2` and `path` is
`"facts"`:

1. the question **routed** to counting (`router_v3`);
2. its **hard checks** passed (`guard_v3`);
3. the served counting model reported `status: "ANSWER"` with a non-empty
   `program_sha256`;
4. **only then** the partner model runs, and the answer is served iff it produced
   the **same program hash**.

The response carries a counting block recording `routed`, `hard_ok`, `status`,
`program_sha256`, `partner_agreement` (true / false / null when the partner did
not run), and `notes`. A failed partner counts as no agreement — nothing is
served from a single model. `partner_agreement` tells you whether two
independently-seeded models wrote the same program for this count.

### Revision and phase — a serving-contract note

The engine **does not resolve a point in history from `item_id`.** `item_id`
only salts manifest ordering. To read a namespace as it was *before* an edit, you
must pass the committed `revision` on `_ask` (and on `_lookup` and
`GET .../{fact_id}`). Asking without a `revision` answers at the head.

Worked shape: write a fact `f1` (`170 cm`), then a `correct` edit superseding it
with `f2` (`171 cm`) at revision `R`. At the head, `f1` reads `visible: false`
and an `_ask` returns `171`. `GET /_facts/{ns}/f1?revision=<R-1>` still reads
`170`, and `_ask` with `"revision": <R-1>` answers `170`. A historical ("before")
question that is asked at the head will be answered from post-edit state — that
is correct behaviour, not a defect: name the revision.

## `GET /_facts` and `GET /_facts/{ns}`

`GET /_facts` lists the namespaces the caller may read (one a caller cannot read
is simply not listed, so the list reveals nothing a 403 would not), each `ready`
(projection in memory) or `cold` (rebuilt on first access), plus the compiler
status and the `_ask` limits, and the node's counting state. A plain facts export
reports counting off (`COUNTING_OFF`); a bundle with a healthy counting role
reports it on.

`GET /_facts/{ns}` reports `revision`, `published_rev`, epochs, records, and
`cleanup_pending`; `?fingerprint=true` adds a content fingerprint.

## Errors — every operational problem is an HTTP error, never a factual UNKNOWN

| status | code | meaning |
|---|---|---|
| 404 | `xerj_query_not_enabled` | the node was started without `--facts-model-dir` (every `/_facts` path; start the server with the flag) |
| 503 | `MODEL_UNAVAILABLE` | no usable model directory (`retryable: false` until the node restarts) |
| 503 | `ENRICHMENT_PENDING` | the namespace's projection is rebuilding; `retryable: true` + `Retry-After`, the retry finds it |
| 503 | `NAMESPACE_RECOVERING` | a cleanup the engine refused, or residue past the committed head; look at the namespace |
| 503 | `COMMIT_OUTCOME_UNKNOWN` | an epoch write failed mid-commit |
| 503 | `FACTS_CONFIG_INVALID` | the node's `_ask` limits could not be read |
| 504 | `ASK_DEADLINE` | no answer within the deadline (`stage` is `queue` or the compiler); nothing was answered |
| 429 | `ASK_QUEUE_FULL` | every ask slot busy and the wait line is full; nothing ran, retry later |
| 409 | `REVISION_NOT_COMMITTED` | asked at a revision that is not committed |
| 403 | — | this key cannot read the namespace |
| 501 | `FACTS_CLUSTER_UNSUPPORTED` | `/_facts` is single-node only; `cluster.enabled = true` refuses every route |
| 400 | `facts_bad_request` | question too long, unknown `linker`/`manifest`/`verify`, malformed body |
| 400 | `facts_contract_error` | a write batch the store refused (with the store's own message); nothing committed |

The admission bounds (`XERJ_FACTS_MAX_CONCURRENT_ASKS`,
`XERJ_FACTS_MAX_QUEUED_ASKS`, `XERJ_FACTS_ASK_DEADLINE_MS`) are all off unless
configured.

## Over MCP

`xerj_facts_ask` is a thin, read-only MCP proxy of `POST /_facts/{ns}/_ask`. It
carries the question and the optional fields and hands back the engine's response
verbatim — same contract as the HTTP call, so anything you can do here you can do
over MCP, and vice versa. `xerj mcp` lists the tool only when the node it proxies
answers `GET /_facts` (that is, a node started with `--facts-model-dir`); on a
default node the tool list is the published schema, without it.
`XERJ_MCP_FACTS=on|off` on the `xerj mcp` process decides instead of probing. A
client that calls the tool by name on a default node gets the node's
`404 xerj_query_not_enabled` back as the tool error.
