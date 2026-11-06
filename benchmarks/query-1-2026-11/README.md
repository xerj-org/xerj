# Benchmarks and methodology

*Measured 2026-11-06. Machine: one laptop, AMD Ryzen AI Max+ 395 (16 cores, 32 threads, 119 GiB RAM, no GPU), unless a
row says otherwise. XERJ: the v1.0.0-rc.101 x86_64 release binary (sha256 `999647a5…`, checked against the release
`.sha256`). Every number below comes from a run listed under "How to reproduce" or is marked ARITHMETIC.*

Our XERJ Query-1 announcement reported no retrieval metrics, set a 120 ms answer time against a 2.7–3.3 s RAG time
without saying what each one measured, and did not name its data. A practitioner called this out, rightly, on all three
points. This section reports standard retrieval metrics (R@k and nDCG@k) on public data, on the practitioner's own benchmark
and on our 160-question test; splits the latency into search and answer writing on one machine; and names every data set.

## In short

- **Search takes milliseconds.** XERJ BM25 over our 5,000-record test store: 0.81 ms at the median. Hybrid search on BEIR,
  including turning the query into a vector: 14.8–32.7 ms on SciFact and NFCorpus (SQLite 11.6–31.4 ms), 25.2 ms on FiQA
  (SQLite 92.2 ms).
- **RAG takes seconds because a language model writes the answer.** In our test, search took under 1 ms to 106 ms and
  Opus 5.5 writing the answer took 1.5–2.3 s (Messages API time per call). The "2.7–3.3 s" we published was that chain
  plus about 1 s of start-up of the command-line client we used, which no application would pay; it was never search.
- **On the same vectors, XERJ and a SQLite FTS5 + sqlite-vec stack give the same results.** BEIR hybrid nDCG@10 differs
  by at most 0.0091, never significantly; on our 160-question test by 0.002.
- **On SearchTome**, the benchmark the practitioner published for the Knowl tool, XERJ's hybrid search scored nDCG@3 82.68
  against Knowl's 81.38 on the same machine with the same embedding server (difference +1.29, 95% CI +0.65 to +1.95).
  XERJ's default BM25 scored slightly lower than Knowl's FTS5 (−1.02, CI −1.62 to −0.43).
- **XERJ Query-1 answers in about 220 ms** on 4 laptop cores, not 120 ms. The 120 ms came from an earlier model setting
  that we never shipped, on a different workload.

## 1. Retrieval quality on public data: BEIR

Three BEIR test sets (Thakur et al. 2021, https://arxiv.org/abs/2104.08663): SciFact (5,183 documents, 300 test
queries), NFCorpus (3,633 documents, 323 queries) and FiQA-2018 (57,638 documents, 648 queries). Every test query is
scored with `pytrec_eval`. R@k is the share of a query's relevant documents found in the top k; nDCG@k rewards finding
them near the top.

Two stacks, given the same vectors:
- **XERJ rc.101**, one node: BM25 (`multi_match` on title and text, default analyzer), kNN (`dense_vector`, cosine), and
  hybrid (the `hybrid` query with RRF, k = 60, 100 hits per leg).
- **SQLite 3.53.4 with FTS5 and sqlite-vec 0.1.9**: FTS5 BM25 (default `unicode61` tokenizer, the query's words OR-ed),
  `vec0` exact cosine search, and RRF with k = 60 over the two top-100 lists, ties broken the same way XERJ breaks them.

Embedding models: `BAAI/bge-small-en-v1.5` (384 dimensions) and EmbeddingGemma-300m (768 dimensions, from the
`onnx-community/embeddinggemma-300m-ONNX` export, because the original repository is gated; not EmbeddingGemma 2).
EmbeddingGemma was not run on FiQA.

**SciFact**

| Stack | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 0.5148 | 0.6701 | 0.7843 | 0.6143 | 0.6572 |
| SQLite FTS5 BM25 | 0.5282 | 0.6757 | 0.7989 | 0.6231 | 0.6695 |
| XERJ kNN, bge-small | 0.5759 | 0.7064 | 0.8362 | 0.6652 | 0.7127 |
| sqlite-vec kNN, bge-small | 0.5759 | 0.7064 | 0.8362 | 0.6652 | 0.7127 |
| XERJ hybrid (RRF), bge-small | 0.5698 | 0.7089 | 0.8164 | 0.6632 | 0.7054 |
| SQLite hybrid (RRF), bge-small | 0.5716 | 0.7092 | 0.8221 | 0.6639 | 0.7088 |
| XERJ kNN, EmbeddingGemma | 0.6259 | 0.8124 | 0.9186 | 0.7484 | 0.7900 |
| sqlite-vec kNN, EmbeddingGemma | 0.6259 | 0.8124 | 0.9186 | 0.7484 | 0.7900 |
| XERJ hybrid (RRF), EmbeddingGemma | 0.5832 | 0.7416 | 0.8604 | 0.6880 | 0.7331 |
| SQLite hybrid (RRF), EmbeddingGemma | 0.6007 | 0.7514 | 0.8636 | 0.6995 | 0.7422 |
| XERJ BM25, stemming analyzer | 0.5282 | 0.6871 | 0.8073 | 0.6297 | 0.6732 |
| SQLite FTS5 BM25, porter tokenizer | 0.5382 | 0.7026 | 0.8245 | 0.6428 | 0.6874 |

**NFCorpus**

| Stack | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 0.0572 | 0.0976 | 0.1494 | 0.3586 | 0.3016 |
| SQLite FTS5 BM25 | 0.0567 | 0.1017 | 0.1494 | 0.3628 | 0.3079 |
| XERJ kNN, bge-small | 0.0530 | 0.1032 | 0.1620 | 0.4006 | 0.3434 |
| sqlite-vec kNN, bge-small | 0.0535 | 0.1031 | 0.1620 | 0.4004 | 0.3438 |
| XERJ hybrid (RRF), bge-small | 0.0643 | 0.1150 | 0.1723 | 0.4157 | 0.3551 |
| SQLite hybrid (RRF), bge-small | 0.0640 | 0.1148 | 0.1714 | 0.4183 | 0.3551 |
| XERJ kNN, EmbeddingGemma | 0.0626 | 0.1174 | 0.2010 | 0.4402 | 0.3900 |
| sqlite-vec kNN, EmbeddingGemma | 0.0626 | 0.1174 | 0.2011 | 0.4402 | 0.3902 |
| XERJ hybrid (RRF), EmbeddingGemma | 0.0633 | 0.1212 | 0.1946 | 0.4368 | 0.3763 |
| SQLite hybrid (RRF), EmbeddingGemma | 0.0642 | 0.1208 | 0.1913 | 0.4390 | 0.3780 |
| XERJ BM25, stemming analyzer | 0.0580 | 0.1039 | 0.1512 | 0.3845 | 0.3195 |
| SQLite FTS5 BM25, porter tokenizer | 0.0570 | 0.1047 | 0.1542 | 0.3881 | 0.3223 |

NFCorpus queries have many relevant documents each, so R@1 and R@3 are small for every stack by construction.

**FiQA-2018**

| Stack | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 0.1103 | 0.1917 | 0.3021 | 0.2054 | 0.2382 |
| SQLite FTS5 BM25 | 0.1078 | 0.1879 | 0.2946 | 0.2009 | 0.2348 |
| XERJ kNN, bge-small | 0.2050 | 0.3362 | 0.4639 | 0.3732 | 0.4035 |
| sqlite-vec kNN, bge-small | 0.2050 | 0.3362 | 0.4639 | 0.3732 | 0.4035 |
| XERJ hybrid (RRF), bge-small | 0.1642 | 0.2887 | 0.4156 | 0.3106 | 0.3462 |
| SQLite hybrid (RRF), bge-small | 0.1644 | 0.2824 | 0.4228 | 0.3091 | 0.3496 |

How to read it:
- **The harness is right.** bge-small on both stacks matches its published model-card scores within 0.0008 nDCG@10
  (SciFact 0.71275, NFCorpus 0.34305, FiQA 0.40343 on https://huggingface.co/BAAI/bge-small-en-v1.5). Both vector
  searches return the exact cosine top 10 for 99.8–100% of positions.
- **Same vectors, same quality.** XERJ and SQLite hybrid differ by at most 0.0091 nDCG@10 (paired randomization test,
  p = 0.15 to 0.99). The engines differ only in BM25 tokenization: FTS5 is ahead by 0.012 on SciFact (p = 0.09) and
  0.006 on NFCorpus (p = 0.04), XERJ by 0.003 on FiQA (p = 0.30).
- **Our BM25 is below the BEIR paper on NFCorpus** (0.3016 against 0.325, Table 2 of the paper) mainly because XERJ's
  default analyzer does not stem. With a stemming analyzer it scores 0.3195.
- **Hybrid is not always better.** With equal weights and bge-small, RRF beats BM25 alone on all three sets (+0.048 to
  +0.108 nDCG@10, p = 0.0001), but it loses to the vectors alone when the vectors are much stronger: −0.048 to −0.057 on
  SciFact with EmbeddingGemma and on FiQA with bge-small, on both engines (p ≤ 0.001).

**Search latency on BEIR** (top 10, one query at a time, turning the query into a vector included, milliseconds,
p50 / p95; three passes over all test queries, XERJ's result cache switched off):

| Data set, embedding model | Query embedding p50 | XERJ BM25 | SQLite FTS5 | XERJ kNN | sqlite-vec | XERJ hybrid | SQLite hybrid |
|---|---:|---:|---:|---:|---:|---:|---:|
| SciFact, bge-small | 9.3 | 3.9 / 6.2 | 6.6 / 11.2 | 11.8 / 14.7 | 11.6 / 14.7 | 17.3 / 22.0 | 17.7 / 24.0 |
| NFCorpus, bge-small | 9.1 | 1.7 / 3.6 | 0.7 / 4.7 | 11.4 / 13.0 | 11.0 / 12.6 | 14.8 / 18.4 | 11.6 / 16.0 |
| FiQA (57,638 docs), bge-small | 9.0 | 9.2 / 14.1 | 59.4 / 98.3 | 13.1 / 15.1 | 32.0 / 34.4 | 25.2 / 31.7 | 92.2 / 133.4 |
| SciFact, EmbeddingGemma | 18.4 | 4.3 / 6.2 | 7.1 / 13.1 | 22.2 / 35.9 | 23.7 / 37.6 | 32.7 / 46.2 | 31.4 / 46.1 |
| NFCorpus, EmbeddingGemma | 15.1 | 2.3 / 3.6 | 0.7 / 4.5 | 18.3 / 28.9 | 18.8 / 29.2 | 24.6 / 36.2 | 20.1 / 31.8 |

SQLite runs inside the benchmark process; XERJ is a server called over local HTTP, so its numbers include a round
trip SQLite does not pay. On the two small sets the stacks are about equal; on FiQA XERJ is faster.

## 2. Retrieval quality on SearchTome, with Knowl

SearchTome (`meghanadh/SearchTome` on Hugging Face, revision `48d3b354…`, CC BY-SA 4.0) has six domains, one book per
domain, 413 pages and 6,005 test queries with one correct page each. The task is to put that page in the top 3, scored by
R@1, R@3 and nDCG@3. Knowl (https://github.com/baldaworks/knowl, MIT) is the practitioner's knowledge-base tool; its
SearchTome runner and its published results are in its `docs/testing/` folder. Knowl's hybrid search is SQLite FTS5 +
sqlite-vec + RRF.

We ran Knowl's own runner (commit `6097d85`) and XERJ side by side on one machine, with one embedding server for both
(`intfloat/multilingual-e5-base` in Hugging Face Text Embeddings Inference 1.9.0, 2 pinned cores) and the same page
chunks (exported from Knowl's own chunker). Knowl's runner refuses 25 of the 6,005 queries before searching
(`invalid_query`); its published results score the other 5,980, and so does the table below. XERJ answers all 6,005;
counting Knowl's 25 refusals as misses moves its scores down by 0.2–0.5 points.

| System (5,980 queries) | R@1 | R@3 | nDCG@3 | Macro R@1 | Macro R@3 | Macro nDCG@3 |
|---|---:|---:|---:|---:|---:|---:|
| Knowl lexical (SQLite FTS5) | 58.51 | 78.95 | 70.60 | 58.17 | 79.04 | 70.49 |
| XERJ BM25, default query | 57.29 | 78.14 | 69.57 | 58.53 | 79.59 | 70.92 |
| XERJ BM25, title boosted ×10 (`multi_match`, Knowl's field weights) | 23.76 | 40.75 | 33.63 | 23.72 | 44.84 | 36.05 |
| Knowl hybrid, E5 (FTS5 + sqlite-vec, RRF) | 70.92 | 88.49 | 81.38 | 70.76 | 88.84 | 81.47 |
| XERJ kNN only, E5 | 73.01 | 89.57 | 82.86 | 73.24 | 90.04 | 83.21 |
| XERJ hybrid, E5 (RRF inside XERJ, over chunks) | 72.83 | 89.43 | 82.68 | 73.15 | 90.08 | 83.20 |
| XERJ BM25 + kNN, fused with Knowl's page-level RRF | 69.48 | 87.88 | 80.45 | 70.39 | 88.77 | 81.30 |

Percentages. Micro averages every query equally; macro averages the six domains equally.

Paired differences on the 5,980 queries (points, XERJ minus Knowl, 95% bootstrap CI, 20,000 resamples):

| Comparison | ΔR@1 | ΔR@3 | ΔnDCG@3 |
|---|---|---|---|
| XERJ BM25 (default) vs Knowl lexical | −1.22 [−2.11, −0.32] | −0.80 [−1.52, −0.08] | −1.02 [−1.62, −0.43] |
| XERJ hybrid vs Knowl hybrid | +1.91 [+0.89, +2.93] | +0.94 [+0.23, +1.64] | +1.29 [+0.65, +1.95] |
| XERJ legs with Knowl's page-level RRF vs Knowl hybrid | −1.44 [−2.26, −0.62] | −0.62 [−1.14, −0.12] | −0.93 [−1.38, −0.47] |
| XERJ kNN only vs Knowl hybrid | +2.09 [+1.05, +3.13] | +1.07 [+0.30, +1.86] | +1.48 [+0.78, +2.18] |

Latency per query (cold pass of the second, quiet session; milliseconds, p50 / p95; the hybrid rows include about
17–18 ms of query embedding):
Knowl lexical 3.73 / 5.13 · XERJ BM25 0.27 / 0.39 · Knowl hybrid 37.92 / 52.74 · XERJ kNN only 28.84 / 35.85 ·
XERJ hybrid 42.67 / 51.92 · XERJ legs with Knowl's RRF 33.68 / 43.00.

How to read it:
- Our run of Knowl's lexical search reproduces its published 58.51 / 78.95 / 70.60 exactly. Our Knowl E5 hybrid run
  gives 70.92 / 88.49 / 81.38 against the published 71.37 / 88.34 / 81.47.
- Knowl's published hybrid p50 of 108.51 ms (EmbeddingGemma 2 on LiteRT-LM, on the author's laptop) is a search number,
  the same kind as the 17–43 ms here. We did not run the EmbeddingGemma 2 or LiteRT-LM profiles.
- The pools are small: the education book has 8 pages, so picking 3 at random already gives R@3 = 37.5% there. The
  queries are synthetic and grouped by book; the confidence intervals describe these six books, not other collections.
- The title-boosted XERJ query is a second configuration we declared before the run, copying Knowl's field weights. It
  does much worse, and we keep it to show that.
- XERJ's default BM25 is close to Knowl's lexical search overall but clearly behind it on the medicine book (R@1 56.3%
  against 64.1%): Knowl runs an all-terms-must-match pass first and weights titles, while XERJ's default `match` is a
  plain any-term query (https://github.com/xerj-org/xerj/issues/3007).
- On these small indexes XERJ's server-side hybrid request takes about 23 ms, slower than Knowl's whole SQLite search
  step (about 21 ms by subtraction) (https://github.com/xerj-org/xerj/issues/3006). Returning each page's full text
  instead of its id moves XERJ BM25 from 0.27 to 0.37 ms. Building the indexes took about the same time for both (324
  and 327 s), 98% of it in embedding calls.

## 3. Retrieval quality on our 160-question test

The test behind the announcement: 160 questions over one store of 5,000 records. The records are lines of the form
`subject | relation | type: value | source sentence`, for example `us7000ee59 | magnitude | number: 4.5 | 4.5`. The
store holds 4,305 distinct records in 82 collections: 13 collections were stored twice by mistake. 3,732 of the 4,305
distinct records (87%) come from public data: the MoMA and Tate collection metadata, OurAirports, the USGS earthquake
catalogue, the NASA Exoplanet Archive, the UCI Online Retail data set and metadata of three Python packages; 573 come
from invented worlds. All 160 questions are taken from the 1,600-question evaluation we develop XERJ Query-1 against
(section 7).

A retrieved record counts as relevant if it is one of the question's evidence records in our gold answers. 90 of the
160 questions have such records (54 lookups, 36 counts); the other 70 have none (the data cannot answer them, the
correct count is 0, or the gold answer comes from a database query). The gold stays on our scoring host. Record level
scores the records themselves. The RAG pipelines retrieve chunks of 8 records, which cannot rank records inside a chunk,
so we also score every arm at chunk level. Every arm used the same query text and, where vectors are used, the same
bge-small vectors.

**Record level** (90 questions)

| Arm | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| RAG BM25 (`rank_bm25`), 8-record chunks | 0.084 | 0.281 | 0.662 | 0.264 | 0.421 |
| RAG bge-small, 8-record chunks | 0.073 | 0.256 | 0.566 | 0.243 | 0.363 |
| GraphRAG local search (its source text units) | 0.028 | 0.128 | 0.278 | 0.118 | 0.182 |
| XERJ BM25 | 0.365 | 0.533 | 0.788 | 0.619 | 0.707 |
| XERJ kNN, bge-small | 0.275 | 0.525 | 0.735 | 0.542 | 0.620 |
| XERJ hybrid (RRF) | 0.388 | 0.561 | 0.824 | 0.644 | 0.736 |
| SQLite FTS5 BM25 | 0.327 | 0.541 | 0.816 | 0.635 | 0.718 |
| sqlite-vec kNN, bge-small | 0.275 | 0.525 | 0.735 | 0.542 | 0.620 |
| SQLite hybrid (FTS5 + sqlite-vec, RRF) | 0.373 | 0.566 | 0.834 | 0.645 | 0.738 |
| XERJ hybrid, filtered to the question's collection | 0.393 | 0.571 | 0.831 | 0.648 | 0.744 |

**Chunk level** (8-record chunks, 90 questions)

| Arm | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| RAG BM25 (`rank_bm25`), 8-record chunks | 0.597 | 0.840 | 0.963 | 0.881 | 0.913 |
| RAG bge-small, 8-record chunks | 0.495 | 0.768 | 0.896 | 0.791 | 0.823 |
| GraphRAG local search (its source text units) | 0.232 | 0.553 | 0.882 | 0.531 | 0.647 |
| XERJ BM25 | 0.528 | 0.807 | 0.933 | 0.826 | 0.863 |
| XERJ kNN, bge-small | 0.480 | 0.772 | 0.946 | 0.770 | 0.833 |
| XERJ hybrid (RRF) | 0.529 | 0.823 | 0.956 | 0.837 | 0.876 |
| SQLite FTS5 BM25 | 0.512 | 0.810 | 0.954 | 0.831 | 0.872 |
| sqlite-vec kNN, bge-small | 0.480 | 0.772 | 0.946 | 0.770 | 0.833 |
| SQLite hybrid (FTS5 + sqlite-vec, RRF) | 0.529 | 0.834 | 0.956 | 0.845 | 0.879 |
| XERJ hybrid, filtered to the question's collection | 0.524 | 0.847 | 0.980 | 0.859 | 0.896 |

How to read it:
- Ranking single records puts the exact evidence first (record nDCG@3 0.644 for XERJ hybrid against 0.264 for chunk
  BM25). At chunk level, plain BM25 over chunks finds the right neighbourhood as well as anything here.
- XERJ and the SQLite stack are again the same: XERJ hybrid minus SQLite hybrid, record nDCG@10, −0.002 (95% CI −0.032
  to +0.023, bootstrap over collections). The two kNN arms return the same lists.
- The 90-question sample is small: the 95% interval on XERJ hybrid's record nDCG@10 runs from 0.666 to 0.803.

**What the answering model did with what search found.** For each RAG arm we checked whether the records it handed to
Opus 5.5 held all the evidence, then looked at Opus's answer (right / wrong / declined):

| Arm | All evidence in context | Opus answers when it was | Opus answers when some was missing |
|---|---:|---|---|
| RAG BM25 top-10 chunks (80 records) | 79 of 90 | 76 / 2 / 1 | 3 / 4 / 4 of 11 |
| RAG BM25 top-5 chunks (40 records) | 71 of 90 | 68 / 2 / 1 | 3 / 7 / 9 of 19 |
| RAG bge-small top-10 chunks | 70 of 90 | 66 / 3 / 1 | 3 / 6 / 11 of 20 |
| RAG bge-small top-5 chunks | 62 of 90 | 58 / 2 / 2 | 3 / 10 / 15 of 28 |
| GraphRAG local search | 86 of 90 | 81 / 3 / 2 | 1 / 0 / 3 of 4 |

Most wrong RAG answers came from evidence that search did not find.

**XERJ Query-1 does not rank, so R@k does not apply to it.** Instead we checked the records it cites. On the 34 lookup
questions it answered, the cited records are exactly the gold evidence every time. Its 31 counting answers cite no
records: the response carries only a hash of the program that produced the count.

## 4. Answer quality on the same 160 questions

This is the comparison the announcement reported. R / W / N = right, wrong, declined.

| Arm | What it read | R / W / N | R − 2W | Score [95% CI] | Wrong answers minus XERJ Query-1 [95% CI] | List price per 1,000 questions |
|---|---|---|---:|---|---|---:|
| XERJ Query-1 | each question's own collection | 124 / 1 / 35 | 122 | 96.1 [82.6, 114.9] | n/a | $0.042 of CPU |
| Opus 5.5, "oracle" | each question's own collection | 137 / 5 / 18 | 127 | 100 (reference) | [−1, 10] | $28.84 |
| Opus 5.5, whole store, 40 questions per call | the merged store | 130 / 6 / 24 | 118 | 92.9 [81.0, 104.0] | [0, 11] | $19.41 |
| GraphRAG local search + Opus 5.5 | retrieved from the merged store | 127 / 9 / 24 | 109 | 85.8 [73.8, 98.5] | [1, 16] | $65.39 |
| RAG BM25 top-10 + Opus 5.5 | retrieved | 124 / 9 / 27 | 106 | 83.5 [70.7, 94.7] | [2, 15] | $20.95 |
| RAG BM25 top-5 + Opus 5.5 | retrieved | 117 / 12 / 31 | 93 | 73.2 [58.5, 86.3] | [4, 18] | $13.83 |
| RAG bge-small top-10 + Opus 5.5 | retrieved | 109 / 14 / 37 | 81 | 63.8 [46.5, 78.4] | [5, 21] | $19.85 |
| RAG bge-small top-5 + Opus 5.5 | retrieved | 106 / 15 / 39 | 76 | 59.8 [41.6, 77.3] | [6, 24] | $13.41 |

Score is 100 × the arm's family-weighted Net over the oracle's, where Net = (R − 2W) / n per question family.
Confidence intervals come from a paired bootstrap over collections, 4,000 draws. Costs are list prices: Opus 5.5 at $4 /
$20 per million input / output tokens (https://platform.claude.com/docs/en/about-claude/pricing), CPU at $0.03625 per
vCPU-hour (AWS c7g.xlarge on-demand, us-east-1). The oracle's cost is averaged from its 1,600-question run, not re-run.

What this does and does not show:
- XERJ Query-1 was given each question's own collection, like the oracle. The RAG and GraphRAG arms had to find the
  records in the merged store first. The fair comparison for XERJ Query-1 is the oracle: 1 against 5 wrong answers, a
  difference that is not statistically clear at this sample size, and a Score of 96.1 against 100, also not separable.
- Against the RAG arms the wrong-answer gap is clear, but section 3 shows most RAG errors came from search misses.
- XERJ Query-1 declined 35 questions, more than the oracle (18) and fewer than embedding RAG (37, 39).
- All 160 questions come from our own development set (section 7), so XERJ Query-1 is on home ground; the language
  models were not trained for it.

## 5. Latency: where the time goes

Same laptop; the language model ran on Anthropic's servers. For the language-model arms the table shows the Messages API
time recorded for each call; the start-up of the command-line client we used to make the calls is not counted, because
an application calling the API would not pay it. Medians per question.

| What | Search / retrieval p50 | Writing the answer p50 |
|---|---|---|
| XERJ BM25 `_search` over the 5,000 records (returns records, no answer) | 0.81 ms (p95 0.99 ms) | none |
| SQLite FTS5 / sqlite-vec / hybrid over the same records (in process, query vector ready) | 0.78 / 1.39 / 2.19 ms | none |
| **XERJ Query-1**, release binary and shipped model, 4 model threads | finding the facts: under 0.1 ms | the whole ask, model writing the program and the engine running it: 219–226 ms (p95 361–412 ms) |
| RAG, BM25 top-10 + Opus 5.5 | 2.0 ms (0.75 ms in the original run) | 1,648 ms |
| RAG, BM25 top-5 + Opus 5.5 | 2.1 ms (0.83 ms) | 1,561 ms |
| RAG, bge-small top-10 + Opus 5.5 | 16 ms (4.15 ms batched) | 1,564 ms |
| RAG, bge-small top-5 + Opus 5.5 | 18 ms (4.15 ms batched) | 1,532 ms |
| GraphRAG local search + Opus 5.5 | 106 ms (96 ms) | 1,552 ms |
| Opus 5.5 reading the whole store, one question per call (20 questions) | none | 2,274 ms |

The retrieval re-run happened while other jobs loaded the laptop; the figure in brackets is the original run on the same
laptop.

**Why RAG takes seconds.** Search returns records, not answers. A RAG pipeline turns retrieved records into an answer by
handing them to a language model, and the model writing that answer is where both its answer quality and its time go.
In our test that model was Opus 5.5, the reference reader of our evaluation. Writing a one-line answer took it 1.5–1.6 s
at the median in the RAG arms (2.3 s when it read the whole store). Search itself took under 1 ms to 106 ms. When search
had found all the evidence, Opus got 76 of 79 answers right (BM25 top-10 arm). We did not test smaller answer models.

XERJ Query-1 works differently: it does not write the answer. It writes a short program (about 220 ms on 4 laptop cores,
including running it) and the engine computes the answer from stored facts. So the comparison for XERJ Query-1 is answer
time against answer time, never against search time.

**Our earlier numbers, labelled.**

| Published | What it actually measured |
|---|---|
| "120 ms" (p50) | An earlier model bundle in an int8 serving mode we did not ship, on a development build, with one synthetic namespace of 8,000 airport documents × 5 facts = 40,000 facts (the pages said "up to 8,000 facts") and 100 copies of one lookup question. The shipped model on our 160 questions takes 219–226 ms on the same laptop. |
| "1,039 ms" (p50) | The 160 questions with the shipped model, on four 2.8 GHz efficiency cores of a shared ARM box that was training a model at the time (4.1 CPU-seconds per question, against 0.9 on the laptop). |
| "2.7–3.3 s" | One `claude -p` call per question for the RAG, GraphRAG and whole-store arms: about 1 s of command-line start-up plus 1.5–2.3 s of Opus writing the answer. Search was not included and took under 1 ms to 106 ms. |

The first question after a restart waits for the model to load: 3.2–3.3 s.

## 6. Data sets and references

Every URL was fetched on 2026-11-06.

| Data | What we used | Source | Licence | Public? |
|---|---|---|---|---|
| BEIR SciFact, NFCorpus, FiQA-2018 | test splits, all queries | https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/ (sha256 of each zip in our run files); paper https://arxiv.org/abs/2104.08663 | cc-by-sa-4.0 on the BeIR cards (https://huggingface.co/datasets/BeIR/scifact and the NFCorpus and FiQA cards) | yes |
| SearchTome | 6 domains, 413 pages, 6,005 test queries | https://huggingface.co/datasets/meghanadh/SearchTome, revision `48d3b3545950b3857dc3657fb22e4b79b8ed8df5` | CC BY-SA 4.0 | yes |
| Our 160-question test store | 5,000 records (4,305 distinct, 82 collections), 160 questions | built from our development evaluation (below) | sources below; invented worlds are ours | no |
| … MoMA collection | artworks and artists | https://github.com/MuseumofModernArt/collection, commit `51a140da` | CC0 | yes |
| … Tate collection | artworks | https://github.com/tategallery/collection, commit `a51d8afc` | CC0 | yes |
| … OurAirports | airports | https://ourairports.com/data/, commit `51db9e04` of the data | public domain | yes |
| … USGS earthquakes | events of magnitude 4.5 or more, 2020–2024 | https://earthquake.usgs.gov/fdsnws/event/1/ | US public domain | yes |
| … NASA Exoplanet Archive | `pscomppars` table | https://exoplanetarchive.ipac.caltech.edu (acknowledgement: https://exoplanetarchive.ipac.caltech.edu/docs/acknowledge.html) | acknowledgement requested | yes |
| … UCI Online Retail | dataset 352 | https://archive.ics.uci.edu/dataset/352/online+retail, DOI 10.24432/C5BW33 | CC BY 4.0 | yes |
| … Python package metadata | fsspec, tokenizers, safetensors | PyPI | BSD-3 (fsspec), Apache-2.0 | yes |
| Our development evaluation | 1,600 scored questions in four families, the model's development and promotion set | built from the sources above plus invented worlds; one family's questions were written by an Opus 5.5 test-author agent | n/a | no |
| English Wikipedia, 10 GB (articles) | the first 22 of 41 files of the 20231101.en dump (file 22 in part): 3,299,578 articles, 10,039,621,739 bytes of JSONL | https://huggingface.co/datasets/wikimedia/wikipedia, revision `b04c8d1ceb2f5cd4588862100d08de323dccfbaa` | CC BY-SA 3.0 and GFDL | yes |
| English Wikipedia passages + HotpotQA | the same files cut into passages of up to 1,200 characters; the first 1,000,000 (and the first 1,117,217 for the index-size chart), with 1,337 HotpotQA questions | as above; https://huggingface.co/datasets/hotpotqa/hotpot_qa (distractor, validation) | CC BY-SA 3.0 + GFDL; HotpotQA CC BY-SA 4.0 | yes |
| 95,000-fact namespace | the first sentence of 95,000 articles from file `train-00002-of-00041` of the same dump | as above | CC BY-SA 3.0 and GFDL | derived, not published |
| The "120 ms" workload | 8,000 synthetic airport documents × 5 facts | our generator script | ours | no |
| Encoder speed passages | 2,335 passages cut from XERJ's own Markdown docs | this repository | Apache-2.0 | yes |

Models and tools: `BAAI/bge-small-en-v1.5` (revision `5c38ec7c`, MIT), `onnx-community/embeddinggemma-300m-ONNX`
(revision `5090578d`, Gemma Terms of Use), `intfloat/multilingual-e5-base` (revision `d1287505`, MIT), Opus 5.5 through
Anthropic's API, Knowl (commit `6097d85`, MIT), SQLite 3.53.4, sqlite-vec 0.1.9, Text Embeddings Inference 1.9.0, the
XERJ Query-1 model bundle `candidate_a_s47_2026-11-04` from the rc.101 release. GraphRAG in our test is our own minimal implementation of Edge et al. 2024
(https://arxiv.org/abs/2404.16130), not Microsoft's `graphrag` package.

## 7. What is private, and why

- **The 1,600-question development evaluation** (its worlds, gold answers, reference answers and scorer). It is the set
  XERJ Query-1 is developed and promoted against. Once its answers are public, any model, ours included, can be trained
  on them, and the set stops measuring anything.
- **The 160-question test** is drawn from that evaluation (every question in it is an item of it), so its gold stays
  private too. The script that built the merged store was not saved, so we cannot rebuild it exactly.
- **XERJ Query-1's code.** The engine is Apache-2.0. The model's crates (`xerj-infer`, `xerj-serve`, `xerj-exec`) are
  proprietary for now; the model bundle is a public release asset, so anyone can run it on their own facts.
- **The "120 ms" workload and its model bundle** were a development experiment; the number is withdrawn.

What this means for you: the BEIR and SearchTome results use public data, so anyone can re-run them once the scripts are added to this folder (Knowl's SearchTome runner is public today). The results on our
160-question test can be checked only by us today; we publish the scripts and aggregate numbers, not the gold.

## 8. How to reproduce

The aggregate results are in [`results/2026-11-06/`](./results/2026-11-06/). The scripts named below are being cleaned of local paths and will be added to this folder in a follow-up change; until then this section lists what each step runs.

- **BEIR** (public data, any machine): download the three BEIR zips; start the release binary with
  `XERJ_DISABLE_QUERY_CACHE=1 xerj --insecure --port <port> --data-dir <dir> --disable-feedback`; embed with
  `embed_st.py` (bge-small, with the model card's query instruction) or `embed_onnx.py` (EmbeddingGemma); run
  `xerj_bench.py` and `sqlite_bench.py` per data set; `latency_pass.py` for the latency table; `score.py`, `sig.py`,
  `build_report.py`, `render_md.py` for the tables. Set the work directory in `common.py`.
- **SearchTome** (public data): Knowl's runner as described in its `docs/testing/searchtome.md`, and our
  `xerj_searchtome.py` against the same Text Embeddings Inference server, with the dataset pinned to the revision above.
- **Our 160-question test**: `xerj_retrieval.py`, `rag_rankings.py` and `sqlite_h2h_rankings.py` produce the ranked lists;
  `spark_score_retrieval*.py` scores them. Scoring needs the private gold.
- **Latency**: `lat_extract.py` reads each `claude -p --output-format json` record (wall time, `duration_api_ms`,
  `duration_ms`); `lat_xq.py` times XERJ Query-1 over its HTTP API; `lat_xbm25.py` times XERJ BM25.
- **XERJ Query-1 on your own facts**: install with `curl -fsSL https://xerj.org/get | sh -s -- --with-model`, then
  `docs/MODEL_QUICKSTART.md`.

## 9. Limits

- Our 160-question test is small (82 distinct collections, 13 of them stored twice), in-distribution for XERJ Query-1,
  and its gold is private. XERJ Query-1 read each question's own collection of 6 to 148 facts.
- On one namespace of 95,000 facts (first sentences of Wikipedia articles) XERJ Query-1 took 0.49–1.55 s per question
  (median 0.82 s) and declined all 20 test questions. Keep namespaces small.
- XERJ Query-1 answers over facts you write; it does not extract facts from documents. Its counting answers cite no
  records, and some counts outside its operation set come back wrong as verified.
- Our RAG baselines are simple: `rank_bm25` and bge-small over 8-record chunks, top 5 or 10, no reranker. The only answer
  model was Opus 5.5 at default effort, called through `claude -p`.
- Records were pre-extracted for every arm. Extraction from raw documents was not measured.
- The laptop was shared with other jobs during some runs; each latency pass records the CPU other processes used, and
  the headline figures come from quiet passes.
- XERJ caches whole responses by default. Benchmarks must switch it off (`XERJ_DISABLE_QUERY_CACHE=1`) or use unique
  queries; all latency numbers here do one or the other.
- XERJ's hybrid ranking breaks ties differently after a restart (106 of 160 rankings identical across three server
  processes); no metric here moved by more than 0.011 because of it.
