# Batch retrieval on rust-vulns

Reproduce the retrieval-evaluation part of [#1801](https://github.com/xerj-org/xerj/issues/1801).
The CLI and MCP adapter shipped in [#2333](https://github.com/xerj-org/xerj/pull/2333).
This harness measures that implementation; it changes no engine or MCP behavior.

## Run

From the repository root, with Python 3.9+ and GitHub CLI installed:

```sh
set -eu
(cd engine && cargo +1.97.1 build --locked --offline --release -j 32 -p xerj-server)
XERJ_BENCH_ROOT=$(pwd)
XERJ_BENCH_TMP=$(mktemp -d)
python3 -m venv "$XERJ_BENCH_TMP/venv"
"$XERJ_BENCH_TMP/venv/bin/pip" install -r tools/benchmarks/batch-retrieval/requirements.txt
gh release download pack-rust-vulns-2026-10-24 --repo xerj-org/xerj \
  --pattern rust-vulns-pack.zip --dir "$XERJ_BENCH_TMP"
"$XERJ_BENCH_TMP/venv/bin/python" tools/benchmarks/batch-retrieval/run.py \
  --pack "$XERJ_BENCH_TMP/rust-vulns-pack.zip" \
  --binary "$XERJ_BENCH_ROOT/engine/target/release/xerj" \
  --engine-commit "$(git rev-parse HEAD)" \
  --output "$XERJ_BENCH_TMP/run" --repeats 5
```

The offline Cargo build needs cached dependencies; omit `--offline` on a first
build. Cargo runs from `engine/` to load its target-specific configuration.
Use a release binary built from the
declared engine commit; the harness records its SHA-256 and version but cannot
prove its source ancestry. Indexing and timing execute a private copy of the
exact byte snapshot that was hashed; changing the original binary mid-run
cannot change the measured executable.

The archive is pinned by its GitHub release asset SHA-256:
`54f0419e6d944635eeaf9199923064278c36fd578213db60ca77cc85d2a69e3d`.
It contains 2,051 identity-resolved records from RustSec and OSV (CC0-1.0).
The harness hashes the archive before opening it, checks manifest shard hashes
and counts, and reads ZIP members directly without extraction. It uses no Hub
pack installation, signature verification, or executable code from the archive.
The digest pins the bytes under evaluation; it is not a signature claim.

All index data is written under a fresh, private temporary directory on random
loopback ports. No existing node, index, corpus configuration or admin key is
used. The complete records are ingested with explicit keyword `id`/`aliases`
and text `title`/`body` mappings, `dynamic: false`, plus synthetic source
locators. This tests the adapter on the real pack, not autoindex schema inference
or Hub routing. Setup, bulk status, post-refresh count and final count are
checked. Children shut down normally; restart checks that the same data survives.
The temporary index is removed on exit. A failed run retains logs in the output
directory. Choose a new output path after a failed or interrupted run: existing
output directories are refused, including successful results.

The corpus is public advisory text. Only the operator can write the fresh
scratch and output directories in the intended usage. Output includes advisory
IDs/citations, public pack metadata, hardware metadata, counts and digests;
it excludes credentials and raw local source paths. Benchmark children remove
inherited proxy and `XERJ_*` configuration and disable automatic feedback so
the test only talks to its private node. No model, reranker or embedding
provider is called. The first tokenizer setup may download its fixed table.

## Workload and scoring

`cases.json` fixes the input order and labels before the run:

| Kind | Items | Success criterion |
|---|---:|---|
| Canonical ID | 15 | Exactly the labelled canonical advisory |
| CVE/GHSA alias | 15 | Exactly the labelled canonical advisory, through the explicit `aliases` field |
| Near-miss ID | 10 | `abstain: true`, `reason: no_hits` |
| Text | 8 | At least one labelled advisory in the first three hits |
| Duplicate | 1 | Same input/key as the first item; separate positional result |
| Invalid item | 1 | Independent query-validation error; never an abstention |

The eight text queries are hand-authored, title-informed known-item lookups.
Their `label_basis` is the advisory title, not a model-generated judgment.
They do not form an exhaustive relevance set: the text metric is labelled
hit@3, not answer accuracy, nDCG, full recall or calibrated confidence.
Canonical and alias targets are validated against the **full** pack before
starting the node. Canonical selection includes the eight text targets and a
SHA-256-ordered sample; aliases alternate CVE/GHSA from that ordering. Missing
IDs append `0` to a real ID and are checked absent. There are 49 valid retrieval
items and one expected item error per trial. Report error isolation separately
from retrieval accuracy; the valid-item abstention denominator is 49.

Every returned hit must cite its actual stored index and canonical ID. Exact
lookups reject extra wrong IDs. Errors, timeouts and transport failures are
never counted as successful misses. Quality is scored independently in every
trial. Ranked evidence, citations and outcomes must also agree across both
arms, both transports, warmups and repeats. A quality failure exits 1 and writes
the failed scores; malformed output, failed setup or parity failure exits 1
without publishing a successful report. Do not publish speed or cost gains
from a failed run.

## What is timed and counted

For each transport the two arms are:

- **Batch:** one `xerj_batch_search` call containing all 50 items.
- **Single:** 50 sequential one-item calls to the **same adapter**. This holds
  query semantics, mapping lookup, response schema and citation policy fixed.
  It is not a comparison with `xerj_search`, `xerj code`, grep or an LLM.

These are tool-call counts, not observed HTTP request counts: the intentionally
invalid item is rejected locally and mapping reads are part of the adapter.

Protocol version 2 requests `size: 3` and `max_tokens: 1000000` in every arm.
This explicit UAX #29 word allowance keeps retrieval evidence independent of
adapter defaults; a response carrying `_token_budget` fails the run. All
requests run at concurrency one. The earlier committed macOS/Linux receipts
used no explicit budget and predate the adapter's default. Their raw evidence
is retained unchanged; request payloads and token counts differ from version 2.
One full warmup per arm is discarded. Five measured repeats alternate arm
order. CLI runs before MCP on the same refreshed, unchanged index. Caches stay
warm; no cache-flush, cold-query or read-under-write claim is made.

CLI timing includes spawning a process per call. MCP uses one initialized,
persistent stdio session; initialization and process startup are excluded.
Task latency covers the complete group of calls and JSON handling, with
scoring, hashing and BPE counting outside the timed loop. Report median,
minimum, maximum and every per-trial/per-call wall time. These are local warm
end-to-end measurements, not engine-only latency.

Tokens use `tiktoken==0.12.0`, `cl100k_base`, on the exact UTF-8 JSON lines sent
and received. CLI counts stdin/stdout; MCP counts `tools/call` request/response
envelopes including the tool name and escaped content. Each line is tokenized
separately and counts are summed, not tokenized after concatenation. Byte counts
and SHA-256 digests accompany each call. MCP initialization, tool schemas,
system prompts, conversation history and reasoning are excluded. These are
payload BPE tokens, not billed model usage or complete agent-task token cost.
They are unrelated to the adapter's UAX #29 `max_tokens` word budget.

## Harness checks

```sh
python3 tools/benchmarks/batch-retrieval/test_run.py
```

The stdlib tests cover wrong/extra IDs, error-versus-miss scoring, citation
identity, labelled hit@3, positional correspondence, parity, archive pinning,
duplicate preservation and per-call wire accounting. CI runs these checks
without downloading the corpus, tokenizer or a model. Real-node evaluation is
the explicit command above, not a network-dependent merge gate.

Generated answers, automatic alias-field discovery, confidence calibration and
escalation remain separate work under #1801 and its related issues. This
evaluation alone does not close #1801.

## Measured run: 2026-10-25

[Raw per-call and per-item receipts](results/2026-10-25-macos-arm64.json),
from clean harness commit `f016e078d61bd3a2b94ea140ef3515ef1a266705` and a
Rust 1.97.1 release build of engine commit
`7e1554734c5b26f7c4f85ee7b5ba838148fe1089` (`xerj v1.0.0-rc.99`).
macOS arm64, Apple M4, 16 GiB RAM; `XERJ_NUM_CPUS=4`.
This was a shared developer laptop, with warm caches and concurrency one.
The command above ran with `--repeats 5`; the earlier two-repeat smoke is not
included in these receipts. The receipt records the binary, pack and query
SHA-256 values, full public pack manifest and source provenance.

| Transport / arm | Calls per 50 items | Task median ms (min–max) | Input BPE tokens | Output BPE tokens |
|---|---:|---:|---:|---:|
| CLI / batch | 1 | 26.487 (26.209–27.632) | 1,071 | 87,676 |
| CLI / single | 50 | 487.739 (480.636–491.249) | 1,757 | 87,872 |
| Persistent MCP / batch | 1 | 15.410 (15.340–16.186) | 1,101 | 100,591 |
| Persistent MCP / single | 50 | 29.515 (28.871–29.931) | 3,306 | 102,257 |

Token totals are per complete 50-item trial and were identical in all five
repeats for each arm. The CLI median ratio is 18.41x, including process startup;
the persistent MCP ratio is 1.92x. They do not measure server-only speedup.
Batching reduced input payload tokens by 39.04% (CLI) / 66.70% (MCP), but output
payload tokens by only 0.22% / 1.63%. Full stored advisory bodies dominate
output. This run does not support a large output-token-savings claim.

All four arms and five repeats scored 49/49 valid retrieval items correctly:
31/31 exact lookups including the duplicate, 8/8 labelled text hit@3, and
10/10 intended misses. Each trial abstained on 10/49 valid items (20.41%),
with zero false abstentions on the 39 labelled answerable items. The separate
invalid item returned its query-validation error without losing siblings.
Every returned hit had a checked index/ID citation; ranked evidence and
citations matched across modes, transports and repeats. The index retained
2,051 records before and after graceful restart. This is a small fixed
known-item workload, not an estimate of general advisory QA accuracy.

## Independent Linux verification: 2026-10-28

[Linux receipts](results/2026-10-28-linux-x86_64.json) repeat the same pinned
pack and workload with five measured repeats per arm, using a scoped release
build of `4af41af87f86e036142503d0e38fc84e2a41bd81` (rc.100 plus the reviewed
SQL/Markdown/empty-file fixes). All four arms passed 49/49 retrieval items in
every repeat, kept the expected per-item validation error separate, and
matched ranked evidence and citations. The node retained 2,051 records across
graceful restart. Payload token totals matched the earlier macOS receipt.

This was a shared Linux host; a separate conformance run also executed during
this review. The receipt retains the observed timings, but this run does not
isolate a version, platform, or throughput improvement. Wrong-archive and
existing-output CLI controls also failed before publication, with exit 1 and
exit 2 respectively; the existing-output sentinel remained unchanged.

## Default-budget compatibility verification: 2026-10-29

[Protocol v2 Linux receipts](results/2026-10-29-linux-x86_64-budget-v2.json)
use the explicit 1,000,000-word allowance in every arm, with the reviewed
MCP/default-budget build at `a89b55bb0`. All four arms passed 49/49 valid
retrieval items in each of five measured repeats and retained the separate
expected item error. Ranked evidence and citations matched across arms and
transports. No response budget bound, and the node retained 2,051 documents
through graceful restart.

The earlier receipts remain unchanged. Protocol v2 adds an explicit request
field, so its payload token counts are not interchangeable with those earlier
runs. This shared Linux host was also running validation; these observations
verify compatibility and full evidence, not an isolated performance gain.
