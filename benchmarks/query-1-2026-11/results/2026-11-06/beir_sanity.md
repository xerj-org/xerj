
#### BM25 vs published BM25 (nDCG@10)

| dataset | BEIR paper (Anserini multifield) | Pyserini flat | Pyserini multifield | XERJ default (1) | XERJ stemmer (s1) | FTS5 unicode61 (4a) | FTS5 porter (s2) |
|---|---:|---:|---:|---:|---:|---:|---:|
| scifact | 0.665 | 0.679 | 0.665 | 0.6572 | 0.6732 | 0.6695 | 0.6874 |
| nfcorpus | 0.325 | 0.322 | 0.325 | 0.3016 | 0.3195 | 0.3079 | 0.3223 |
| fiqa | 0.236 | 0.236 | 0.236 | 0.2382 | not run | 0.2348 | not run |

#### bge-small-en-v1.5 vs its model card (MTEB, test split)

| dataset | metric | model card | XERJ kNN (2) | sqlite-vec (4b) |
|---|---|---:|---:|---:|
| scifact | R@1 | 0.5759 | 0.5759 | 0.5759 |
| scifact | R@3 | 0.7064 | 0.7064 | 0.7064 |
| scifact | R@10 | 0.8362 | 0.8362 | 0.8362 |
| scifact | nDCG@3 | 0.6652 | 0.6652 | 0.6652 |
| scifact | nDCG@10 | 0.7127 | 0.7127 | 0.7127 |
| nfcorpus | R@1 | 0.0529 | 0.0530 | 0.0535 |
| nfcorpus | R@3 | 0.1031 | 0.1032 | 0.1031 |
| nfcorpus | R@10 | 0.1620 | 0.1620 | 0.1620 |
| nfcorpus | nDCG@3 | 0.3993 | 0.4006 | 0.4004 |
| nfcorpus | nDCG@10 | 0.3431 | 0.3434 | 0.3438 |
| fiqa | R@1 | 0.2050 | 0.2050 | 0.2050 |
| fiqa | R@3 | 0.3362 | 0.3362 | 0.3362 |
| fiqa | R@10 | 0.4639 | 0.4639 | 0.4639 |
| fiqa | nDCG@3 | 0.3732 | 0.3732 | 0.3732 |
| fiqa | nDCG@10 | 0.4034 | 0.4035 | 0.4035 |

#### Exact-search check of the vector arms (share of the exact cosine top-10, numpy over the same vectors and live query vectors)

| run | arm | mean overlap@10 | worst query |
|---|---|---:|---:|
| sqlite__fiqa__bge-small | vec | 1.000 | 1.0 |
| sqlite__nfcorpus__bge-small | vec | 0.999 | 0.9 |
| sqlite__nfcorpus__egemma | vec | 1.000 | 1.0 |
| sqlite__scifact__bge-small | vec | 1.000 | 1.0 |
| sqlite__scifact__egemma | vec | 1.000 | 1.0 |
| xerj__fiqa__bge-small | knn | 0.998 | 0.8 |
| xerj__nfcorpus__bge-small | knn | 0.999 | 0.9 |
| xerj__nfcorpus__egemma | knn | 0.999 | 0.9 |
| xerj__scifact__bge-small | knn | 1.000 | 1.0 |
| xerj__scifact__egemma | knn | 1.000 | 1.0 |
