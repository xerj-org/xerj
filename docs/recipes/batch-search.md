# Retrieve several IDs or questions in one call

`xerj batch-search` and the MCP tool `xerj_batch_search` share one adapter.
They retrieve evidence from a node that is already running; neither starts a
node, downloads a corpus, writes data, or generates an answer.

Save this as `queries.json`, replacing the index and field with those in your
data map. `id` must be an indexed **keyword** field for the identifier lookup.
There is no guessed ID field and no tokenized-text fallback for exact IDs.

```json
{
  "index": "advisories",
  "size": 3,
  "queries": [
    {"key": "known", "identifier": "CVE-2026-12345", "field": "id"},
    {"key": "question", "query": "when should we rotate keys"},
    {"key": "structured", "query": {"match": {"body": "rotation"}}}
  ]
}
```

```sh
xerj batch-search --input queries.json --url http://127.0.0.1:9200
# Or pipe the same JSON:
cat queries.json | xerj batch-search --input -
```

For MCP, call `xerj_batch_search` with that JSON object as `arguments`.
`XERJ_URL`, `XERJ_AUTH`, explicit `--url`/`--auth` and loopback admin-key discovery
work as for `xerj mcp`. The node must already be running. `prefix` can replace
`index`; combining both is an error. With neither, the target is `ax-*`.

The response has one `results` entry for each input, in the same order. Its
zero-based `position` is authoritative; optional `key` values may repeat.

| Status | Meaning |
|---|---|
| `hits`, `abstain: false` | Retrieved hits, each with `citation` and raw `hit` evidence. A hit is not a generated answer or a calibrated relevance guarantee. |
| `abstain`, `abstain: true`, `reason: no_hits` | The item completed with an empty hit list. This is retrieval abstention, not a confidence judgment. |
| `error` | Invalid item, unsupported exact-field mapping, failed/partial search, malformed response, or transport failure. Never relabeled as a miss. |

Citations always retain `_index` and `_id`. Available `ax_path`/`path`,
`ax_locator`, `ax_file`, stored line/page/cell and calculated `match_line` fields
are included too. No file line is inferred from a JSONL record ordinal.
There is no deduplication across questions, even for identical inputs.

Limits: 1–50 items, `size` 1–50 per item (default 3), a 1 MiB serialized input,
256 UTF-8 bytes per optional key, a 32 MiB HTTP response, and the MCP client's
30-second HTTP timeout. Locally invalid items do not reach the node. The other
items use one `_msearch` request and, when needed, one shared mapping read.
An unreadable mapping fails exact-ID items; plain text retains the established
search-query fallback. Exact ID fields must be flat and keyword-mapped wherever
present, with indexing enabled. Nested fields are outside this first version.

`max_tokens` defaults to 8,000 when omitted or null, for both MCP and
`xerj batch-search`. Pass a larger integer to request more. It caps the entire
successful response JSON, using the
same UAX #29 word counter as the other MCP search tools. This is **not BPE**.
If binding, the adapter removes raw `hit` bodies starting at later items,
then extra hits; it retains every item outcome and at least one citation per
matched item. `hits_omitted` counts omitted hits, and `_token_budget` names the
counter and limit. A nonbinding budget leaves the response byte-identical.
If the remaining statuses, errors and citations cannot fit, the whole call
returns an explicit error naming the minimum required budget. Split the batch
or increase the budget. Errors are outside the budget and are never truncated. This is a word budget,
not a byte cap or a guarantee that output fits a host's display limit.

CLI exit codes: `0` all items completed (including abstentions); `3` one or more
per-item errors (the JSON still contains all outcomes); `2` invalid CLI/input
envelope; `1` a batch-wide execution or budget error. Treat exit `3` as a partial
result to inspect, not a reason to discard the successful items.

The batch path uses `_msearch` rather than repeated `_search` calls. Plain-text
items reuse the mapping-aware search query and code-path demotion; the engine's
`_msearch` renderer returns stored evidence, not `_search`'s extracted `_passage`
field. Rerank/judge and per-item scopes/options are not exposed here. No latency,
model-token savings or confidence claim is made by this recipe.

For a pinned rust-vulns retrieval evaluation comparing 50-item batches with
one-item calls over both CLI and persistent MCP, see
[the batch retrieval harness](../../tools/benchmarks/batch-retrieval/README.md).
It measures labelled ID lookup, known-item text hit@3, retrieval abstentions,
warm task latency and payload BPE tokens; these are not generated-answer or
complete agent-task cost measurements.

This is the batch retrieval part of [#1801](https://github.com/xerj-org/xerj/issues/1801).
Full answer/abstain calibration and alias-aware ID discovery remain separate
work; the adapter and retrieval evaluation do not close that issue.
