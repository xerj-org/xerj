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
  --fresh

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

## Restart and vector-space safety

Every ONNX semantic index stores `embedding_identity.json` with SHA-256 model
and tokenizer fingerprints plus dimensions, pooling, and token limit. Restart
with the same assets. XERJ refuses:

- another model or tokenizer at restart;
- switching an ONNX-pinned index to another backend;
- enabling ONNX in place on a populated marker-less semantic index;
- caller-supplied derived vectors whose identity cannot be verified.

The error tells the operator to restore the original assets or re-run
autoindex with `--fresh` and a new prefix. XERJ never silently mixes vector
spaces.

## Admission and errors

One safe ONNX Runtime session is shared per complete model configuration.
Length-aware microbatches are serialized through it. Before model loading or
tokenization, shared admission enforces:

- `onnx_max_inflight_calls` (default 8);
- `onnx_max_input_bytes_per_call` (default 8 MiB);
- `onnx_max_inflight_input_bytes` (default 32 MiB);
- `onnx_max_pending`, microbatch size, and padded-token limits.

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

Measured stripped server binaries:

- Candle: 36.06 MiB;
- Candle plus ONNX: 54.81 MiB (+18.75 MiB / +52.0%);
- ONNX-only experimental build: 52.49 MiB (+16.43 MiB / +45.6%).

The approximately 90 MiB model is a separate runtime asset. Bundled `ort`
binaries do not cover XERJ's musl release targets, so standard musl releases
remain Candle-only. ONNX production adoption still needs a supported target
matrix, end-to-end quality gates, and full-corpus throughput/resource results.
