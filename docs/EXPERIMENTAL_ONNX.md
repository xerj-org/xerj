# Experimental ONNX embedding backend

Status: embedding-library prototype, **not a server or CLI mode**.

XERJ's supported built-in neural backend remains Candle and
`--embed-mode neural` continues to mean Candle. Default and musl builds do not
compile ONNX Runtime.

The opt-in `xerj-ai` feature provides the exact FP32 MiniLM inference path and
a bounded length-aware microbatch API:

```bash
cd engine
cargo run --release -p xerj-ai --features onnx-experimental \
  --example onnx_experimental -- /path/model.onnx /path/tokenizer.json
```

The caller supplies an ONNX model with `input_ids`, `attention_mask`, and
`token_type_ids` inputs and a `[batch, sequence, 384]` `last_hidden_state`
output. The current verified artifact is the official FP32
`sentence-transformers/all-MiniLM-L6-v2` ONNX export.

Safety properties:

- safe `ort` API only;
- one session behind a mutex;
- bounded pending inputs, documents per batch, and padded token slots;
- length-aware grouping;
- exact restoration to caller input order;
- clear backpressure and invalid-budget errors.

## Measured prototype results

On the controlled 128-document mixed-length benchmark, exact FP32 ONNX plus
the retained scheduler processed 116.671 documents/s versus 9.045 documents/s
for Candle using the identical precomputed length-aware batch plan: **12.90x
at the `xerj-ai` embedding layer**. The median minimum same-document
Candle/ONNX cosine was `0.9999991655`.

This is not a claim of 12.90x end-to-end XERJ indexing. The benchmark excludes
document extraction, HTTP handling, persistence, and HNSW construction. ONNX
also used 16 intra-op threads; the measured CPU-efficiency improvement was
2.92x, smaller than the wall-time result. A server-level comparison is required
before publishing an indexing-throughput claim.

Measured stripped full-server binary sizes were:

- current Candle build: 36.06 MiB;
- Candle plus ONNX: 54.81 MiB (**+18.75 MiB / +52.0%**);
- ONNX-only experimental build: 52.49 MiB (**+16.43 MiB / +45.6%**).

The roughly 90 MiB ONNX model is a separate runtime asset comparable to the
existing Candle safetensors asset. The much larger static ONNX Runtime archive
downloaded while compiling is not added wholesale to the final binary.

The bundled `ort` binaries do not cover XERJ's musl release targets. Default
and musl builds therefore remain Candle-only; production adoption requires a
reproducible musl packaging strategy or an explicitly narrower target matrix.

This is deliberately not exposed in `xerj --help`. Server integration still
needs model acquisition, backend fingerprinting/reindex compatibility,
platform packaging, lifecycle metrics, cancellation, and end-to-end retrieval
quality gates. Do not describe XERJ as supporting `--embed-mode onnx`.
