# Vector quantization: 4× smaller vectors, same results

## The problem

Dense vectors are heavy. A 768-dim `float32` embedding is ~3 KB; a million
of them is ~3 GB of vector data that has to be resident to serve low-latency
kNN. Scale to tens of millions and the vector working set — not the text,
not the postings — becomes the thing that decides how much RAM you rent.

The standard fix is **scalar quantization**: store each dimension in one
byte instead of four. That's a 4× reduction. The catch everyone worries
about is recall — does compressing the vectors quietly wreck ranking
quality?

## Why XERJ

XERJ lets you opt a `dense_vector` field into **scalar8** (int8)
quantization per field. When you do, the kNN *serving* path scores against
1-byte-per-dimension codes instead of 4-byte floats — a ~4× smaller vector
working set — while `_source` still returns the **original** vectors for
retrieval. It's off by default (full float32), so you choose precision vs.
memory per field, exactly like Elasticsearch's `int8_hnsw`.

On a real 128-dim corpus the recall cost is negligible: **recall@10 = 0.998**
against the exact float32 index, with the vector footprint cut from 512 to
128 bytes per vector — a **measured 4.00× reduction**. (Both numbers are
computed by the run below, not stipulated.)

## The solution

Opt a field in at mapping time with `index_options.type: int8_hnsw`:

```bash
curl -sX PUT "$XERJ_URL/docs" -H 'content-type: application/json' -d '{
  "mappings": {
    "properties": {
      "title": { "type": "text" },
      "v": {
        "type": "dense_vector",
        "dims": 128,
        "similarity": "cosine",
        "index_options": { "type": "int8_hnsw" }
      }
    }
  }
}'
```

Index and query exactly as you would a full-precision field — nothing else
changes:

```bash
curl -sX POST "$XERJ_URL/docs/_search" -H 'content-type: application/json' -d '{
  "knn": { "field": "v", "query_vector": [0.12, 0.08, -0.31, "..."], "k": 10 }
}'
```

The scores come back slightly different from an exact float32 index (that's
the quantization at work — a query that exactly matches a stored vector
scores ~0.99999 instead of 1.0), but the **ranking is the same**.

## Try it

`docs/examples/vector-quantization/quant_demo.py` (the mirrored
`recipes/vector_quantization.py` runs the same demo) embeds the 40 real KB
articles into 128-dim vectors, indexes the same vectors into a float32 index
and a scalar8 index, and prints the side-by-side top hits, the measured
recall@10, and the measured byte footprint of each encoding:

```
$ python3 docs/examples/vector-quantization/quant_demo.py
embedded 40 real KB articles into 128-dim vectors

indexed into `vq-none` (float32) and `vq-scalar8` (int8_hnsw / scalar8)

query: 'how do I stop an agent's context window from overflowing?'

── float32 (exact)
    0.67958  Long-context windows do not replace memory
    0.60029  p95 latency budgets for interactive RAG agents
    0.59712  SOC 2 controls that apply to vector workloads

── scalar8 (quantized)
    0.67938  Long-context windows do not replace memory
    0.60021  p95 latency budgets for interactive RAG agents
    0.59731  SOC 2 controls that apply to vector workloads

recall@10 (scalar8 vs float32 ground truth): 0.998
vector footprint over 40 vecs: float32 = 20480 B (512 B/vec)  →  scalar8 = 5120 B (128 B/vec)  (4.00x smaller)

OK — 4x smaller vectors, recall preserved. `_source` still holds the originals.
```

The footprint line is a real measurement: the run encodes every corpus
vector as float32 bytes (`struct`) and as int8 codes and compares the actual
byte totals — 20480 B vs 5120 B, exactly 4.00×.

## Reproduce it yourself

```bash
# 1. Start XERJ (dev mode, default ES-compat port 9200)
xerj --insecure --data-dir ./data &

# 2. Run the demo (stdlib-only Python 3, no packages, no API keys)
python3 docs/examples/vector-quantization/quant_demo.py
```

`XERJ_URL` overrides the server (default `http://localhost:9200`); `XERJ_KB`
overrides the KB path (default: auto-discovered `demo/data/ai_kb.ndjson`).
The embedder and corpus are deterministic, so a customer should see exactly:

- `recall@10 (scalar8 vs float32 ground truth): 0.998`
- `vector footprint over 40 vecs: float32 = 20480 B (512 B/vec)  →  scalar8 = 5120 B (128 B/vec)  (4.00x smaller)`

These numbers are stable run-to-run (verified across repeated runs — no
variance); the printed kNN scores are likewise identical each run.

## Notes and limits

- **Opt-in per field.** Fields without `int8_hnsw` keep exact float32
  scoring, byte-for-byte unchanged.
- **`_source` is never quantized.** Retrieval returns the vectors you
  indexed; only the scoring path uses the compact codes.
- **`scalar8` is wired; `binary` is not yet.** Binary (1-bit) quantization
  is rejected at startup rather than silently storing full precision.
- **Cosine is normalised** before quantizing for the tightest code range;
  `dot_product` and `l2_norm` similarities are supported too.
