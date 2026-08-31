# Experimental ONNX embedding backend

Status: end-to-end experimental server mode for GNU/Linux glibc builds. It is
off by default. XERJ's default remains the non-neural lexical feature hash;
`--embed-mode neural` remains the built-in Candle backend.

## Exact model contract

This is not a generic ONNX-model interface. The supplied graph must be an FP32
`sentence-transformers/all-MiniLM-L6-v2`-compatible feature-extraction export:

- int64 inputs named `input_ids`, `attention_mask`, and `token_type_ids`;
- rank-3 token output named `last_hidden_state` or `token_embeddings`;
- output width exactly 384;
- a `tokenizer.json` from the same model/export.

XERJ applies attention-mask mean pooling and L2 normalization. It truncates at
512 tokens. A mapping that declares another vector width is rejected.

## Copy-paste workflow

Export a compatible model and inspect its interface:

```bash
python3 -m venv /tmp/xerj-onnx-export
/tmp/xerj-onnx-export/bin/pip install 'optimum[onnxruntime]' onnx
/tmp/xerj-onnx-export/bin/optimum-cli export onnx \
  --model sentence-transformers/all-MiniLM-L6-v2 \
  --task feature-extraction \
  /tmp/xerj-minilm-onnx

/tmp/xerj-onnx-export/bin/python - <<'PY'
import onnx
p = "/tmp/xerj-minilm-onnx/model.onnx"
m = onnx.load(p)
print("inputs:", [(v.name, [d.dim_value for d in v.type.tensor_type.shape.dim])
                  for v in m.graph.input])
print("outputs:", [(v.name, [d.dim_value for d in v.type.tensor_type.shape.dim])
                   for v in m.graph.output])
PY

sha256sum \
  /tmp/xerj-minilm-onnx/model.onnx \
  /tmp/xerj-minilm-onnx/tokenizer.json
```

Confirm the three required input names, a supported output name, and width 384.
Export tools can change their file layout or output names; XERJ deliberately
fails instead of guessing.

For the checksum-pinned transformer-fused FP32 graph created during an
internal optimizer screen whose evidence is not published here, see the
repository's
[offline transformation recipe](../demo/playbooks/onnx-model-optimization/README.md).
The recipe does not download, bundle, or recreate its source model. It works
only when the operator already has the exact recorded source model and
tokenizer bytes, pins the optimizer environment, and publishes only the exact
checked graph plus its manifest. The generic export above is not an acquisition
path for those checksum-pinned inputs.

Build the opt-in server:

```bash
cd engine
cargo build --release -j 32 -p xerj-server --features onnx-experimental
```

Start it with explicit local assets:

```bash
target/release/xerj \
  --insecure \
  --data-dir /tmp/xerj-onnx-data \
  --embed-mode onnx-experimental \
  --onnx-model /tmp/xerj-minilm-onnx/model.onnx \
  --onnx-tokenizer /tmp/xerj-minilm-onnx/tokenizer.json
```

No model is downloaded at server startup. Incorrect paths, an incompatible
build, and invalid admission limits fail startup with a corrective error. The
first real semantic inference prints one concise activation message containing
the verified model and tokenizer hashes. ONNX Runtime messages below warning
are hidden by default; use `XERJ_ONNX_LOG=info` or `verbose` for diagnosis.

In another terminal, index a corpus:

```bash
target/release/xerj autoindex /path/to/corpus \
  --url http://localhost:9200 \
  --prefix finance-onnx \
  --state-dir /path/to/new-finance-onnx-state

target/release/xerj autoindex map \
  --url http://localhost:9200 \
  --prefix finance-onnx
```

ONNX runs only for fields inferred as `semantic_text`, normally a sufficiently
long body field. Short or structured datasets may infer none. Check
`autoindex --dry-run`, the data map's `semantic_field`, and the server's
activation log before claiming an ONNX result.

Query the mapped semantic field:

```bash
curl -s http://localhost:9200/finance-onnx-*/_search \
  -H 'content-type: application/json' \
  -d '{"query":{"semantic":{"field":"body","query":"Which quarter had the largest operating-margin decline?","k":10}},"size":5}'
```

### Validate cold start, cancellation, and sharing

ONNX loads lazily on the first semantic request. The log must show exactly one
`ONNX lazy initialization scheduled` line followed by one completion line.
The first request includes model-load latency; later requests reuse that
session.

To verify cancellation safety, issue a deliberately short first request and
then retry immediately:

```bash
curl --max-time 0.05 -sS http://localhost:9200/finance-onnx-*/_search \
  -H 'content-type: application/json' \
  -d '{"query":{"semantic":{"field":"body","query":"cold load","k":5}}}'

curl --max-time 10 -sS http://localhost:9200/finance-onnx-*/_search \
  -H 'content-type: application/json' \
  -d '{"query":{"semantic":{"field":"body","query":"cold load","k":5}}}'
```

The first client may time out, but initialization remains process-owned. The
retry waits for the same load rather than starting another session. Likewise,
concurrent first requests share one initialization and then run through the
existing bounded admission and microbatch policy. A warning is emitted if
model initialization is still running after 30 seconds; the eventual
completion or full error chain is always logged.

Initialization failures, including a caught loader panic, are retained for the
life of that shared model configuration. Every waiting or later request
receives the same terminal error; XERJ does not silently retry a possibly
corrupt runtime initialization or risk loading two sessions. Correct the
assets/runtime issue and restart the process.

## Restart and vector-space safety

Every ONNX semantic index stores `embedding_identity.json` with SHA-256 model
and tokenizer fingerprints plus dimensions, pooling, and token limit. Restart
with the same assets. XERJ refuses:

- another model or tokenizer at restart;
- switching an ONNX-pinned index to another backend;
- enabling ONNX in place on a populated marker-less semantic index;
- caller-supplied derived vectors whose identity cannot be verified.

The error tells the operator to restore the original assets or perform an
isolated autoindex rebuild with a new state directory, new prefix, and new
brain when graph detection is enabled (or `--no-graph`). Validate the new
target before switching readers; the shared catalog and old target require
explicit, validated cleanup. XERJ never silently mixes vector spaces.

The transformer-fused graph produced by the offline recipe is a different
model identity from its source graph because its model hash differs. Do not
replace a source model in place. Start with the fused model and run `autoindex
--fresh` under a new prefix, or retain the source model for the existing index.
The identity check must not be bypassed.

## Throughput controls

The default remains one ONNX Runtime session and a 64-passage caller window:

```toml
[embedding]
mode = "onnx-experimental"
onnx_model_path = "/models/all-MiniLM-L6-v2/model.onnx"
onnx_tokenizer_path = "/models/all-MiniLM-L6-v2/tokenizer.json"
onnx_scheduling_window = 64
onnx_session_pool_size = 1
```

Larger windows give the existing length-aware scheduler more passages to group
without changing the model, vectors, output dimensions, or internal
`onnx_max_batch` cap. Valid values are 1 through 4096.

Two independent sessions allow two complete scheduling windows to run at once:

```toml
[embedding]
onnx_scheduling_window = 512
onnx_session_pool_size = 2
onnx_intra_threads = 8
```

This is opt-in because a second session retains another copy of ONNX Runtime's
session state and therefore changes the memory/concurrency tradeoff. XERJ
constructs the requested pool atomically and never publishes a partial pool.
It runs at most two windows concurrently, drains both native calls, and applies
their results in original input order. Cancelling an HTTP waiter does not
release admission permits or return a session to the pool before its native
call actually finishes.

These controls do not select a smaller or quantized model. Model choice remains
the operator's responsibility through the explicit ONNX assets (or another
embedding backend).

The built-in Candle backend (`--embed-mode neural`) shares the same
length-aware batching rule, but its limits are compiled-in constants — 64 rows
and a 4096 `rows × padded_sequence_length` budget — not settings.
`onnx_max_batch` and `onnx_padded_token_budget` apply to the ONNX backend only,
so there is no operator lever for the neural backend's batch size.
`onnx_scheduling_window` does bound the caller-side window for either encoder.

## Admission and errors

One bounded ONNX Runtime session pool is shared per complete model
configuration. Length-aware microbatches are serialized through each member.
Before model loading or tokenization, shared admission enforces:

- `onnx_max_inflight_calls` (default 8);
- `onnx_max_input_bytes_per_call` (default 8 MiB);
- `onnx_max_inflight_input_bytes` (default 32 MiB);
- `onnx_max_pending`, microbatch size, and padded-token limits.
- `onnx_scheduling_window` (default 64, range 1 through 4096);
- `onnx_session_pool_size` (default 1, range 1 through 2).

Overload returns retryable HTTP 429. Autoindex does not call that junk and does
not journal the affected source file complete; correct the pressure/config and
rerun the same command to resume.

These settings are available under `[embedding]` in the TOML configuration.
The server validates impossible or zero admission limits before serving.

## Measured performance and limits

On the controlled 128-document mixed-length benchmark, optimized Candle
processed 9.045 documents/s and FP32 ONNX with the retained scheduler processed
116.671 documents/s: **12.90x at the embedding layer**. The median minimum
same-document Candle/ONNX cosine was `0.9999991655`; output ordering was
preserved. CPU-efficiency improved 2.92x.

This is not a 12.90x end-to-end indexing claim. It excludes extraction, HTTP,
lexical indexing, persistence, HNSW construction, and contention. Full
FinanceBench autoindex time must be measured, not projected.

The transformer-fused recipe came from an internal optimizer screen whose
evidence is not published in this repository. It therefore makes no public
speed or quality claim. See the
[evidence boundary](../demo/playbooks/onnx-model-optimization/README.md#evidence-boundary)
and run the applicable repository quality and performance gates before
publishing one.

Measured stripped server binaries:

- Candle: 36.06 MiB;
- Candle plus ONNX: 54.81 MiB (+18.75 MiB / +52.0%);
- ONNX-only experimental build: 52.49 MiB (+16.43 MiB / +45.6%).

The approximately 90 MiB model is a separate runtime asset. Bundled `ort`
binaries do not cover XERJ's musl release targets, so standard musl releases
remain Candle-only. ONNX production adoption still needs a supported target
matrix, end-to-end quality gates, and full-corpus throughput/resource results.

The fused recipe uses ONNX Runtime transformer optimizer 1.22.1 for offline
generation. Consumption is proven with XERJ's `ort`/`ort-sys` 2.0.0-rc.12
API-24 CPU path on GNU/Linux glibc x86-64. Its `com.microsoft` contrib
operators make other runtime/API lines and providers explicit validation
tasks. It is not a hardware-specific optimized-session cache, but neither
portability nor bit-identical output across CPUs or runtimes should be assumed.
XERJ already requests ONNX Runtime Level3 optimization at session creation for
both source and fused graphs. No published evidence currently shows an
incremental cold-start, steady-state throughput, or retrieval-quality benefit
from supplying the offline-fused graph. These require separate measurements;
the fresh-index identity cost applies regardless.
