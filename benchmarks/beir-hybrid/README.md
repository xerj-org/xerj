# BEIR: what XERJ's shipped retrieval scores, with a free local model

**Run:** 2026-09-18, `xerj v1.0.0-rc.74`, `--embed-mode neural` (built-in Candle BERT,
`sentence-transformers/all-MiniLM-L6-v2`, 384-dim, ~90 MB, CPU). One node, one shard,
default settings. Metric: nDCG@10 on each dataset's BEIR `test` split, computed by
`eval.py` from live queries — nothing is assumed or carried over.

| Arm | SciFact (300 q, 5,183 docs) | NFCorpus (323 q, 3,633 docs) |
|---|---:|---:|
| BM25 (`multi_match` on title, text) | 0.6572 | 0.3016 |
| MiniLM vectors only (`semantic`) | 0.6764 | 0.3291 |
| BM25 top-30 → reorder by MiniLM | 0.6855 | 0.3323 |
| **Hybrid RRF (`hybrid`, server-side)** | **0.6993** | **0.3448** |

Raw output: [`results/`](./results).

## What this says

1. **Hybrid is the best arm XERJ has, on both datasets, and it already ships.** No new
   code is needed to get it.
2. **Reranking a BM25 shortlist with the same bi-encoder is worse than fusing with it.**
   That is why `xerj-rerank` has no "local MiniLM" provider: it would be a regression
   with a good name. A local second stage is only worth building with a cross-encoder,
   and only if it beats 0.699 / 0.345 here.

## Against hosted rerankers — read the caveat

Numbers published in [`hev/jev-rerank`](https://github.com/hev/jev-rerank)'s README for
the same datasets and metric:

| | SciFact | NFCorpus |
|---|---:|---:|
| Jev (TypeSafe AI) | 0.768 | 0.358 |
| Voyage rerank-3 | 0.755 | 0.357 |
| Cohere rerank-v3.5 | 0.745 | 0.340 |
| **XERJ hybrid, local MiniLM, no API** | **0.699** | **0.345** |

**We did not run those systems.** They rerank that project's own first-stage shortlist,
which is not ours, so this is the same dataset and metric but not a controlled
comparison. What can be said: on NFCorpus XERJ's free local hybrid is inside the band of
the hosted rerankers; on SciFact it is 4.6–6.9 points behind them. A controlled
comparison needs a `TYPESAFE_API_KEY` and the `rerank` stage, which now exists.

## Defects this run surfaced

- **Neural indexing was ~3–7 documents/second on CPU in this run.** SciFact (5,183
  short abstracts) took about 25 minutes. That is consistent with the ~3 docs/s figure
  recorded in `xerj-autoindex/src/infer/mod.rs`, and it is the binding constraint on any
  "index your mail on a laptop" story.
- **Hybrid and filtered-semantic queries cost ~220–370 ms p50** on a 5k-document index,
  against ~18 ms for BM25. Most of that is the per-query BERT forward pass on CPU; the
  machine was also compiling during the run, so treat the absolute values as upper
  bounds. The first SciFact vector-arm p50 (0.4 ms) is a **cache artefact** — an earlier
  aborted run had already embedded those queries — and must not be quoted.
- **BM25 returned zero hits for 25 of 323 NFCorpus queries** under default-OR
  `multi_match`. **Explained, and not a defect:** exactly 25 of the test queries
  share no token with any document's title or text — they are single words such
  as `deafness`, `eggnog`, `Fosamax` and `Zoloft` that occur nowhere in the
  corpus. [`lexical_gap.py`](./lexical_gap.py) counts them without a running
  node ([output](./results/nfcorpus-lexical-gap.txt)); the same check reports 0
  for SciFact, where BM25 also had `empty=0`. This is the dataset's lexical gap,
  and it is the clearest reason the vector arm matters on NFCorpus: no reorder
  of a BM25 shortlist can repair an empty list.

## Reproduce

```sh
# a throwaway node on private ports, neural embedder
xerj -c xerj.toml -d ./data --insecure --embed-mode neural &
curl -LO https://public.ukp.informatik.tu-darmstadt.de/thakur/BEIR/datasets/scifact.zip && unzip scifact.zip
XERJ_URL=http://localhost:9410 python3 load.py scifact/corpus.jsonl scifact
XERJ_URL=http://localhost:9410 python3 eval.py 30 scifact
```
