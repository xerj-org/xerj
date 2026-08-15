# Vector quantization: int8 scoring at recall@10 ≈ 0.998

> **Read this first.** `scalar8` on XERJ today changes **precision, not
> memory**. Scores are computed from 1-byte-per-dimension codes, so the field
> has the recall profile of int8 — but the serving path still reads the
> full-precision vector out of `_source` and quantizes it per query, so
> nothing gets smaller in RAM. The ingest-time code array that would make it
> a memory win is tracked in
> [#392](https://github.com/xerj-org/xerj/issues/392). If you came here for a
> smaller working set, that issue is the one to watch, not this recipe.

## The problem

Dense vectors are heavy. A 768-dim `float32` embedding is ~3 KB; a million
of them is ~3 GB of vector data that has to be resident to serve low-latency
kNN. Scale to tens of millions and the vector working set — not the text,
not the postings — becomes the thing that decides how much RAM you rent.

The standard fix is **scalar quantization**: store each dimension in one
byte instead of four. The catch everyone worries about is recall — does
compressing the vectors quietly wreck ranking quality? That is the question
this recipe answers, and the answer is no.

## Why XERJ

XERJ lets you opt a `dense_vector` field into **scalar8** (int8)
quantization per field. When you do, the kNN *serving* path scores against
1-byte-per-dimension codes instead of 4-byte floats, while `_source` still
returns the **original** vectors for retrieval. It's off by default (full
float32), so you choose the precision model per field, spelled exactly like
Elasticsearch's `int8_hnsw`.

On a real 128-dim corpus the recall cost is negligible: **recall@10 = 0.998**
against the exact float32 index. That number is computed by the run below,
not stipulated.

What that costs in bytes *as an encoding* is 128 rather than 512 per vector,
and the run measures that too — but see the note at the top: XERJ does not
hold those codes resident today, so treat it as the size of the encoding, not
as a saving you get.

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
encoding size over 40 vecs: float32 = 20480 B (512 B/vec)  →  scalar8 = 5120 B (128 B/vec)  (4.00x smaller)

OK — recall preserved through 1-byte-per-dim codes. `_source` still holds
the originals. scalar8 changes precision, not resident memory (issue #392).
```

The encoding-size line is a real measurement: the run encodes every corpus
vector as float32 bytes (`struct`) and as int8 codes and compares the actual
byte totals — 20480 B vs 5120 B, exactly 4.00×. It is the cost of the two
*encodings*, measured in the client. It is **not** a measurement of XERJ's
resident footprint, and XERJ does not currently realise it as one
([#392](https://github.com/xerj-org/xerj/issues/392)).

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
- `encoding size over 40 vecs: float32 = 20480 B (512 B/vec)  →  scalar8 = 5120 B (128 B/vec)  (4.00x smaller)`

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
- **No memory saving yet.** The serving path reads the full-precision vector
  from `_source` and quantizes it per query, so `scalar8` costs the same RAM
  as float32 and a little more CPU. Tracked in
  [#392](https://github.com/xerj-org/xerj/issues/392).
- **`scalar8` disables ANN.** A quantized field is excluded from HNSW-served
  kNN and always takes the exact brute-force scan, so it is slower, not
  faster, than leaving the field full-precision. Also [#392](https://github.com/xerj-org/xerj/issues/392).
- **A `scalar8` `_score` — and the order it produces — depends on the
  candidate set.** The codebook is fitted per query over the candidates being
  scored, which is what keeps an updated document from being scored on a stale
  codebook ([#371](https://github.com/xerj-org/xerj/issues/371)). Three things
  follow, all of them measured, none of them true of a full-precision field:

  - **A `filter` reorders the documents it does not remove.** On a 60-document
    4-dim cosine corpus (30 tightly clustered `grp:a` documents, 30 spread
    `grp:b` ones), adding `filter: {"term": {"grp": "a"}}` — which removes only
    `grp:b` — returned the same 30 documents with a maximum `_score` difference
    of **1.976e-05**, but in a **different order at 19 of the 30 positions**.
    The score delta is tiny; the reordering is what a caller actually sees.
  - **The trigger is the candidate set, not the `filter` keyword.** Indexing
    one more unrelated document moves existing documents too. Same corpus, one
    document added far from the query: maximum `_score` difference **7.100e-06**
    over the 30 pre-existing documents, and a reordered top 10.
  - **Unfiltered corpora over 1000 documents also score differently from
    v1.0.0-rc.17**, which fitted the codebook from the first ≤1000 candidates
    and then cached it for the life of the process. On 1500 documents, **all 40
    of the top-40 `_score` values changed** (maximum difference **4.880e-05**)
    and two adjacent ranks swapped. Below 1000 documents, unfiltered and never
    updated, scores are byte-identical to rc.17.

  Every individual difference is bounded by SQ8's own quantization step (1/255
  of the fitted per-dimension range), i.e. inside the approximation error
  `scalar8` already carries — but documents whose true scores are close will
  swap places, so treat `scalar8` ordering as stable only for a fixed corpus
  and a fixed filter. Elasticsearch fits its codebook per segment at index time
  and does not have this property;
  [#392](https://github.com/xerj-org/xerj/issues/392) is what would bring XERJ
  in line.
