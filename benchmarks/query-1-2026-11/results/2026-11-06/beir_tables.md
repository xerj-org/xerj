
#### Quality — SciFact (5,183 docs, 300 test queries)

| stack | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 | hit@1 | hit@10 | empty results |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| (1) XERJ BM25, default analyzer | 0.5148 | 0.6701 | 0.7843 | 0.6143 | **0.6572** | 0.5333 | 0.8033 | 0 of 300 |
| (2) XERJ kNN, bge-small vectors | 0.5759 | 0.7064 | 0.8362 | 0.6652 | **0.7127** | 0.6067 | 0.8467 | 0 of 300 |
| (3) XERJ hybrid RRF, bge-small | 0.5698 | 0.7089 | 0.8164 | 0.6632 | **0.7054** | 0.5900 | 0.8300 | 0 of 300 |
| (4a) SQLite FTS5 BM25 (unicode61) | 0.5282 | 0.6757 | 0.7989 | 0.6231 | **0.6695** | 0.5467 | 0.8200 | 0 of 300 |
| (4b) sqlite-vec kNN, bge-small | 0.5759 | 0.7064 | 0.8362 | 0.6652 | **0.7127** | 0.6067 | 0.8467 | 0 of 300 |
| (4c) SQLite hybrid RRF, bge-small | 0.5716 | 0.7092 | 0.8221 | 0.6639 | **0.7088** | 0.5967 | 0.8367 | 0 of 300 |
| (5a) XERJ kNN, EmbeddingGemma | 0.6259 | 0.8124 | 0.9186 | 0.7484 | **0.7900** | 0.6600 | 0.9233 | 0 of 300 |
| (5b) XERJ hybrid RRF, EmbeddingGemma | 0.5832 | 0.7416 | 0.8604 | 0.6880 | **0.7331** | 0.6100 | 0.8733 | 0 of 300 |
| (5c) sqlite-vec kNN, EmbeddingGemma | 0.6259 | 0.8124 | 0.9186 | 0.7484 | **0.7900** | 0.6600 | 0.9233 | 0 of 300 |
| (5d) SQLite hybrid RRF, EmbeddingGemma | 0.6007 | 0.7514 | 0.8636 | 0.6995 | **0.7422** | 0.6267 | 0.8767 | 0 of 300 |
| (s1) XERJ BM25, stemmer analyzer | 0.5282 | 0.6871 | 0.8073 | 0.6297 | **0.6732** | 0.5467 | 0.8233 | 0 of 300 |
| (s2) SQLite FTS5 BM25, porter | 0.5382 | 0.7026 | 0.8245 | 0.6428 | **0.6874** | 0.5567 | 0.8467 | 0 of 300 |

#### Quality — NFCorpus (3,633 docs, 323 test queries)

| stack | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 | hit@1 | hit@10 | empty results |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| (1) XERJ BM25, default analyzer | 0.0572 | 0.0976 | 0.1494 | 0.3586 | **0.3016** | 0.4365 | 0.6780 | 25 of 323 |
| (2) XERJ kNN, bge-small vectors | 0.0530 | 0.1032 | 0.1620 | 0.4006 | **0.3434** | 0.4396 | 0.6935 | 0 of 323 |
| (3) XERJ hybrid RRF, bge-small | 0.0643 | 0.1150 | 0.1723 | 0.4157 | **0.3551** | 0.4861 | 0.7276 | 0 of 323 |
| (4a) SQLite FTS5 BM25 (unicode61) | 0.0567 | 0.1017 | 0.1494 | 0.3628 | **0.3079** | 0.4334 | 0.6873 | 25 of 323 |
| (4b) sqlite-vec kNN, bge-small | 0.0535 | 0.1031 | 0.1620 | 0.4004 | **0.3438** | 0.4427 | 0.6935 | 0 of 323 |
| (4c) SQLite hybrid RRF, bge-small | 0.0640 | 0.1148 | 0.1714 | 0.4183 | **0.3551** | 0.4737 | 0.7245 | 0 of 323 |
| (5a) XERJ kNN, EmbeddingGemma | 0.0626 | 0.1174 | 0.2010 | 0.4402 | **0.3900** | 0.5139 | 0.7709 | 0 of 323 |
| (5b) XERJ hybrid RRF, EmbeddingGemma | 0.0633 | 0.1212 | 0.1946 | 0.4368 | **0.3763** | 0.5077 | 0.7585 | 0 of 323 |
| (5c) sqlite-vec kNN, EmbeddingGemma | 0.0626 | 0.1174 | 0.2011 | 0.4402 | **0.3902** | 0.5139 | 0.7709 | 0 of 323 |
| (5d) SQLite hybrid RRF, EmbeddingGemma | 0.0642 | 0.1208 | 0.1913 | 0.4390 | **0.3780** | 0.5077 | 0.7616 | 0 of 323 |
| (s1) XERJ BM25, stemmer analyzer | 0.0580 | 0.1039 | 0.1512 | 0.3845 | **0.3195** | 0.4520 | 0.6935 | 15 of 323 |
| (s2) SQLite FTS5 BM25, porter | 0.0570 | 0.1047 | 0.1542 | 0.3881 | **0.3223** | 0.4334 | 0.6966 | 15 of 323 |

#### Quality — FiQA-2018 (57,638 docs, 648 test queries)

| stack | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 | hit@1 | hit@10 | empty results |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| (1) XERJ BM25, default analyzer | 0.1103 | 0.1917 | 0.3021 | 0.2054 | **0.2382** | 0.2253 | 0.4877 | 0 of 648 |
| (2) XERJ kNN, bge-small vectors | 0.2050 | 0.3362 | 0.4639 | 0.3732 | **0.4035** | 0.4012 | 0.6744 | 0 of 648 |
| (3) XERJ hybrid RRF, bge-small | 0.1642 | 0.2887 | 0.4156 | 0.3106 | **0.3462** | 0.3272 | 0.6204 | 0 of 648 |
| (4a) SQLite FTS5 BM25 (unicode61) | 0.1078 | 0.1879 | 0.2946 | 0.2009 | **0.2348** | 0.2191 | 0.4830 | 0 of 648 |
| (4b) sqlite-vec kNN, bge-small | 0.2050 | 0.3362 | 0.4639 | 0.3732 | **0.4035** | 0.4012 | 0.6744 | 0 of 648 |
| (4c) SQLite hybrid RRF, bge-small | 0.1644 | 0.2824 | 0.4228 | 0.3091 | **0.3496** | 0.3302 | 0.6327 | 0 of 648 |

#### Latency — lat10__scifact__bge-small (top 10 returned, hybrid legs 50 deep; 3 passes x 300 queries, concurrency 1, encoder bge-small-en-v1.5 (sentence-transformers, torch CPU), 16 threads)

Query embedding: p50 9.33 ms, p95 12.36 ms (n=900).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 3.94 | 6.16 | 3.94 | 6.16 | 3 |
| XERJ kNN | 2.40 | 2.97 | 11.76 | 14.70 | 1 |
| XERJ hybrid RRF | 7.96 | 10.74 | 17.29 | 21.97 | 7 |
| SQLite FTS5 BM25 | 6.56 | 11.22 | 6.56 | 11.22 | - |
| sqlite-vec kNN | 2.24 | 2.67 | 11.63 | 14.72 | - |
| SQLite FTS5+vec RRF | 8.11 | 12.90 | 17.65 | 23.97 | - |

#### Latency — lat10__scifact__egemma (top 10 returned, hybrid legs 50 deep; 3 passes x 300 queries, concurrency 1, encoder EmbeddingGemma-300m (ONNX fp32, ONNX Runtime), 16 threads)

Query embedding: p50 18.42 ms, p95 32.13 ms (n=900).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 4.29 | 6.24 | 4.29 | 6.24 | 3 |
| XERJ kNN | 3.70 | 4.54 | 22.22 | 35.94 | 2 |
| XERJ hybrid RRF | 13.60 | 16.93 | 32.68 | 46.17 | 12 |
| SQLite FTS5 BM25 | 7.09 | 13.06 | 7.09 | 13.06 | - |
| sqlite-vec kNN | 5.00 | 6.47 | 23.67 | 37.58 | - |
| SQLite FTS5+vec RRF | 11.95 | 17.91 | 31.44 | 46.14 | - |

#### Latency — lat10__nfcorpus__bge-small (top 10 returned, hybrid legs 50 deep; 3 passes x 323 queries, concurrency 1, encoder bge-small-en-v1.5 (sentence-transformers, torch CPU), 16 threads)

Query embedding: p50 9.10 ms, p95 10.64 ms (n=969).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 1.66 | 3.63 | 1.66 | 3.63 | 0 |
| XERJ kNN | 2.27 | 2.78 | 11.42 | 12.99 | 1 |
| XERJ hybrid RRF | 5.80 | 8.28 | 14.84 | 18.36 | 5 |
| SQLite FTS5 BM25 | 0.71 | 4.67 | 0.71 | 4.67 | - |
| sqlite-vec kNN | 1.79 | 2.11 | 10.97 | 12.60 | - |
| SQLite FTS5+vec RRF | 2.27 | 5.75 | 11.58 | 16.01 | - |

#### Latency — lat10__nfcorpus__egemma (top 10 returned, hybrid legs 50 deep; 3 passes x 323 queries, concurrency 1, encoder EmbeddingGemma-300m (ONNX fp32, ONNX Runtime), 16 threads)

Query embedding: p50 15.09 ms, p95 25.25 ms (n=969).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 2.28 | 3.56 | 2.28 | 3.56 | 1 |
| XERJ kNN | 3.13 | 4.26 | 18.33 | 28.92 | 2 |
| XERJ hybrid RRF | 9.29 | 13.29 | 24.61 | 36.25 | 8 |
| SQLite FTS5 BM25 | 0.67 | 4.48 | 0.67 | 4.48 | - |
| sqlite-vec kNN | 3.45 | 4.70 | 18.81 | 29.18 | - |
| SQLite FTS5+vec RRF | 4.24 | 8.19 | 20.12 | 31.81 | - |

#### Latency — lat10__fiqa__bge-small (top 10 returned, hybrid legs 50 deep; 3 passes x 648 queries, concurrency 1, encoder bge-small-en-v1.5 (sentence-transformers, torch CPU), 16 threads)

Query embedding: p50 9.04 ms, p95 10.92 ms (n=1944).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 9.22 | 14.10 | 9.22 | 14.10 | 8 |
| XERJ kNN | 4.03 | 4.73 | 13.11 | 15.15 | 3 |
| XERJ hybrid RRF | 16.11 | 21.47 | 25.25 | 31.66 | 15 |
| SQLite FTS5 BM25 | 59.38 | 98.33 | 59.38 | 98.33 | - |
| sqlite-vec kNN | 22.96 | 23.79 | 32.00 | 34.38 | - |
| SQLite FTS5+vec RRF | 83.24 | 122.59 | 92.18 | 133.35 | - |

#### Latency — lat__scifact__bge-small (top 100 returned, hybrid legs 100 deep; 3 passes x 300 queries, concurrency 1, encoder bge-small-en-v1.5 (sentence-transformers, torch CPU), 16 threads)

Query embedding: p50 11.14 ms, p95 17.14 ms (n=900).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 7.29 | 10.39 | 7.29 | 10.39 | 5 |
| XERJ kNN | 5.92 | 7.42 | 17.14 | 23.69 | 4 |
| XERJ hybrid RRF | 13.06 | 18.09 | 24.60 | 34.08 | 11 |
| SQLite FTS5 BM25 | 7.52 | 13.56 | 7.52 | 13.56 | - |
| sqlite-vec kNN | 3.14 | 4.30 | 14.31 | 20.74 | - |
| SQLite FTS5+vec RRF | 10.07 | 16.71 | 21.45 | 33.15 | - |

#### Latency — lat__scifact__egemma (top 100 returned, hybrid legs 100 deep; 3 passes x 300 queries, concurrency 1, encoder EmbeddingGemma-300m (ONNX fp32, ONNX Runtime), 16 threads)

Query embedding: p50 19.73 ms, p95 37.25 ms (n=900).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 11.80 | 14.60 | 11.80 | 14.60 | 10 |
| XERJ kNN | 9.17 | 11.06 | 29.26 | 47.01 | 7 |
| XERJ hybrid RRF | 21.01 | 23.83 | 41.26 | 59.12 | 19 |
| SQLite FTS5 BM25 | 6.83 | 12.85 | 6.83 | 12.85 | - |
| sqlite-vec kNN | 5.14 | 6.65 | 25.40 | 42.85 | - |
| SQLite FTS5+vec RRF | 11.51 | 16.73 | 31.82 | 50.44 | - |

#### Latency — lat__nfcorpus__bge-small (top 100 returned, hybrid legs 100 deep; 3 passes x 323 queries, concurrency 1, encoder bge-small-en-v1.5 (sentence-transformers, torch CPU), 16 threads)

Query embedding: p50 8.93 ms, p95 10.74 ms (n=969).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 4.65 | 6.70 | 4.65 | 6.70 | 3 |
| XERJ kNN | 5.38 | 6.54 | 14.36 | 16.71 | 4 |
| XERJ hybrid RRF | 9.65 | 13.51 | 18.36 | 23.30 | 8 |
| SQLite FTS5 BM25 | 0.70 | 4.70 | 0.70 | 4.70 | - |
| sqlite-vec kNN | 2.25 | 2.74 | 11.23 | 13.13 | - |
| SQLite FTS5+vec RRF | 2.58 | 6.14 | 11.86 | 16.11 | - |

#### Latency — lat__nfcorpus__egemma (top 100 returned, hybrid legs 100 deep; 3 passes x 323 queries, concurrency 1, encoder EmbeddingGemma-300m (ONNX fp32, ONNX Runtime), 16 threads)

Query embedding: p50 16.90 ms, p95 32.52 ms (n=969).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 8.54 | 11.16 | 8.54 | 11.16 | 7 |
| XERJ kNN | 9.68 | 11.57 | 26.78 | 42.54 | 8 |
| XERJ hybrid RRF | 18.13 | 21.67 | 34.46 | 51.42 | 16 |
| SQLite FTS5 BM25 | 0.79 | 4.73 | 0.79 | 4.73 | - |
| sqlite-vec kNN | 3.95 | 5.19 | 21.14 | 36.50 | - |
| SQLite FTS5+vec RRF | 4.55 | 8.85 | 22.43 | 37.96 | - |

#### Latency — lat__fiqa__bge-small (top 100 returned, hybrid legs 100 deep; 3 passes x 648 queries, concurrency 1, encoder bge-small-en-v1.5 (sentence-transformers, torch CPU), 16 threads)

Query embedding: p50 8.31 ms, p95 10.38 ms (n=1944).

| arm | search p50 ms | search p95 ms | embed+search p50 ms | embed+search p95 ms | XERJ server `took` p50 ms |
|---|---:|---:|---:|---:|---:|
| XERJ BM25 | 13.85 | 18.56 | 13.85 | 18.56 | 12 |
| XERJ kNN | 6.66 | 7.66 | 15.03 | 17.34 | 5 |
| XERJ hybrid RRF | 20.25 | 24.99 | 28.67 | 34.36 | 18 |
| SQLite FTS5 BM25 | 56.58 | 91.83 | 56.58 | 91.83 | - |
| sqlite-vec kNN | 24.92 | 25.58 | 33.30 | 35.68 | - |
| SQLite FTS5+vec RRF | 81.30 | 117.01 | 89.84 | 126.76 | - |

#### Quality in the top-10 setting (from pass 1 of the top-10 latency runs; hybrid legs 50 deep)

| run | arm | R@1 | R@3 | R@10 | nDCG@3 | nDCG@10 | hit@1 | hit@10 |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| lat10__fiqa__bge-small | xerj.bm25 | 0.1103 | 0.1917 | 0.3021 | 0.2054 | 0.2382 | 0.2253 | 0.4877 |
| lat10__fiqa__bge-small | xerj.knn | 0.2050 | 0.3362 | 0.4639 | 0.3732 | 0.4035 | 0.4012 | 0.6744 |
| lat10__fiqa__bge-small | xerj.hybrid | 0.1647 | 0.2868 | 0.4309 | 0.3100 | 0.3531 | 0.3287 | 0.6296 |
| lat10__fiqa__bge-small | sqlite.fts | 0.1078 | 0.1879 | 0.2946 | 0.2009 | 0.2348 | 0.2191 | 0.4830 |
| lat10__fiqa__bge-small | sqlite.vec | 0.2050 | 0.3362 | 0.4639 | 0.3732 | 0.4035 | 0.4012 | 0.6744 |
| lat10__fiqa__bge-small | sqlite.hybrid | 0.1628 | 0.2812 | 0.4308 | 0.3083 | 0.3530 | 0.3287 | 0.6420 |
| lat10__nfcorpus__bge-small | xerj.bm25 | 0.0572 | 0.0976 | 0.1494 | 0.3586 | 0.3016 | 0.4365 | 0.6780 |
| lat10__nfcorpus__bge-small | xerj.knn | 0.0530 | 0.1032 | 0.1620 | 0.4006 | 0.3434 | 0.4396 | 0.6935 |
| lat10__nfcorpus__bge-small | xerj.hybrid | 0.0643 | 0.1150 | 0.1682 | 0.4157 | 0.3524 | 0.4861 | 0.7152 |
| lat10__nfcorpus__bge-small | sqlite.fts | 0.0567 | 0.1017 | 0.1494 | 0.3628 | 0.3079 | 0.4334 | 0.6873 |
| lat10__nfcorpus__bge-small | sqlite.vec | 0.0535 | 0.1031 | 0.1620 | 0.4004 | 0.3438 | 0.4427 | 0.6935 |
| lat10__nfcorpus__bge-small | sqlite.hybrid | 0.0640 | 0.1147 | 0.1704 | 0.4176 | 0.3543 | 0.4737 | 0.7214 |
| lat10__nfcorpus__egemma | xerj.bm25 | 0.0572 | 0.0976 | 0.1494 | 0.3586 | 0.3016 | 0.4365 | 0.6780 |
| lat10__nfcorpus__egemma | xerj.knn | 0.0626 | 0.1174 | 0.2010 | 0.4402 | 0.3900 | 0.5139 | 0.7709 |
| lat10__nfcorpus__egemma | xerj.hybrid | 0.0633 | 0.1203 | 0.1937 | 0.4354 | 0.3747 | 0.5077 | 0.7461 |
| lat10__nfcorpus__egemma | sqlite.fts | 0.0567 | 0.1017 | 0.1494 | 0.3628 | 0.3079 | 0.4334 | 0.6873 |
| lat10__nfcorpus__egemma | sqlite.vec | 0.0626 | 0.1174 | 0.2011 | 0.4402 | 0.3902 | 0.5139 | 0.7709 |
| lat10__nfcorpus__egemma | sqlite.hybrid | 0.0642 | 0.1192 | 0.1923 | 0.4374 | 0.3785 | 0.5077 | 0.7554 |
| lat10__scifact__bge-small | xerj.bm25 | 0.5148 | 0.6701 | 0.7843 | 0.6143 | 0.6572 | 0.5333 | 0.8033 |
| lat10__scifact__bge-small | xerj.knn | 0.5759 | 0.7064 | 0.8362 | 0.6652 | 0.7127 | 0.6067 | 0.8467 |
| lat10__scifact__bge-small | xerj.hybrid | 0.5665 | 0.7056 | 0.8431 | 0.6598 | 0.7110 | 0.5867 | 0.8567 |
| lat10__scifact__bge-small | sqlite.fts | 0.5282 | 0.6757 | 0.7989 | 0.6231 | 0.6695 | 0.5467 | 0.8200 |
| lat10__scifact__bge-small | sqlite.vec | 0.5759 | 0.7064 | 0.8362 | 0.6652 | 0.7127 | 0.6067 | 0.8467 |
| lat10__scifact__bge-small | sqlite.hybrid | 0.5716 | 0.7059 | 0.8387 | 0.6618 | 0.7130 | 0.5967 | 0.8533 |
| lat10__scifact__egemma | xerj.bm25 | 0.5148 | 0.6701 | 0.7843 | 0.6143 | 0.6572 | 0.5333 | 0.8033 |
| lat10__scifact__egemma | xerj.knn | 0.6259 | 0.8124 | 0.9186 | 0.7484 | 0.7900 | 0.6600 | 0.9233 |
| lat10__scifact__egemma | xerj.hybrid | 0.5832 | 0.7416 | 0.8781 | 0.6880 | 0.7394 | 0.6100 | 0.8933 |
| lat10__scifact__egemma | sqlite.fts | 0.5282 | 0.6757 | 0.7989 | 0.6231 | 0.6695 | 0.5467 | 0.8200 |
| lat10__scifact__egemma | sqlite.vec | 0.6259 | 0.8124 | 0.9186 | 0.7484 | 0.7900 | 0.6600 | 0.9233 |
| lat10__scifact__egemma | sqlite.hybrid | 0.6007 | 0.7481 | 0.8769 | 0.6978 | 0.7465 | 0.6267 | 0.8900 |

#### XERJ BM25 latency vs the vector field stored in the same index (SciFact, size 100, 3 passes, identical rankings)

| index | search p50 ms | search p95 ms | server took p50 ms |
|---|---:|---:|---:|
| beir_scifact_txt (text only) | 4.06 | 6.69 | 3 |
| beir_scifact_bge (+ 384-d vectors) | 6.82 | 10.02 | 5 |
| beir_scifact_egemma (+ 768-d vectors) | 12.01 | 15.32 | 10 |

Identical top-100 rankings across the three indexes: 100% of query executions.

Size probe (150 queries, 1 pass): beir_scifact_txt size=10: p50 3.19 ms, p95 5.23 ms, took p50 2.0; beir_scifact_txt size=100: p50 3.63 ms, p95 5.63 ms, took p50 3.0; beir_scifact_egemma size=10: p50 3.04 ms, p95 4.77 ms, took p50 2.0; beir_scifact_egemma size=100: p50 11.38 ms, p95 13.66 ms, took p50 10.0

#### XERJ whole-response cache (default ON): same 300 SciFact BM25 queries twice on a fresh node

| round | client p50 ms | client p95 ms | server took p50 ms | share of took = 0 |
|---|---:|---:|---:|---:|
| first | 7.80 | 9.04 | 6 | 0.000 |
| repeat | 0.70 | 1.07 | 0 | 0.997 |

#### Paired tests on per-query nDCG@10 (randomization test, 10,000 sign flips; bootstrap 95% CI)

| A | B | n | mean(A-B) | 95% CI | p | A better / B better / tie |
|---|---|---:|---:|---|---:|---|
| xerj__scifact__bge-small::bm25 | sqlite__scifact__bge-small::fts | 300 | -0.0123 | [-0.0264, +0.0015] | 0.0948 | 29 / 41 / 230 |
| xerj__scifact__bge-small::knn | sqlite__scifact__bge-small::vec | 300 | +0.0000 | [+0.0000, +0.0000] | 1.0000 | 0 / 0 / 300 |
| xerj__scifact__bge-small::hybrid | sqlite__scifact__bge-small::hybrid | 300 | -0.0034 | [-0.0147, +0.0084] | 0.5809 | 16 / 24 / 260 |
| xerj__scifact__bge-small::hybrid | xerj__scifact__bge-small::knn | 300 | -0.0074 | [-0.0321, +0.0174] | 0.5746 | 44 / 47 / 209 |
| xerj__scifact__bge-small::hybrid | xerj__scifact__bge-small::bm25 | 300 | +0.0482 | [+0.0248, +0.0719] | 0.0001 | 70 / 26 / 204 |
| sqlite__scifact__bge-small::hybrid | sqlite__scifact__bge-small::vec | 300 | -0.0039 | [-0.0312, +0.0225] | 0.7695 | 52 / 46 / 202 |
| xerj__scifact__egemma::knn | sqlite__scifact__egemma::vec | 300 | +0.0000 | [+0.0000, +0.0000] | 1.0000 | 0 / 0 / 300 |
| xerj__scifact__egemma::hybrid | sqlite__scifact__egemma::hybrid | 300 | -0.0091 | [-0.0218, +0.0032] | 0.1529 | 20 / 28 / 252 |
| xerj__scifact__egemma::hybrid | xerj__scifact__egemma::knn | 300 | -0.0570 | [-0.0858, -0.0304] | 0.0001 | 37 / 73 / 190 |
| sqlite__scifact__egemma::hybrid | sqlite__scifact__egemma::vec | 300 | -0.0479 | [-0.0764, -0.0194] | 0.0009 | 39 / 67 / 194 |
| xerj__scifact__egemma::hybrid | xerj__scifact__bge-small::hybrid | 300 | +0.0277 | [+0.0109, +0.0455] | 0.0016 | 56 / 23 / 221 |
| sqlite__scifact__egemma::hybrid | sqlite__scifact__bge-small::hybrid | 300 | +0.0334 | [+0.0163, +0.0509] | 0.0002 | 55 / 24 / 221 |
| xerj__scifact__stemmer-bm25::bm25 | xerj__scifact__bge-small::bm25 | 300 | +0.0160 | [-0.0019, +0.0338] | 0.0851 | 36 / 34 / 230 |
| sqlite__scifact__porter-fts::fts | xerj__scifact__stemmer-bm25::bm25 | 300 | +0.0143 | [+0.0009, +0.0280] | 0.0404 | 44 / 27 / 229 |
| xerj__nfcorpus__bge-small::bm25 | sqlite__nfcorpus__bge-small::fts | 323 | -0.0063 | [-0.0126, -0.0005] | 0.0426 | 60 / 71 / 192 |
| xerj__nfcorpus__bge-small::knn | sqlite__nfcorpus__bge-small::vec | 323 | -0.0004 | [-0.0013, +0.0001] | 0.7492 | 2 / 2 / 319 |
| xerj__nfcorpus__bge-small::hybrid | sqlite__nfcorpus__bge-small::hybrid | 323 | +0.0000 | [-0.0043, +0.0046] | 0.9949 | 58 / 53 / 212 |
| xerj__nfcorpus__bge-small::hybrid | xerj__nfcorpus__bge-small::knn | 323 | +0.0118 | [-0.0021, +0.0258] | 0.1029 | 111 / 88 / 124 |
| xerj__nfcorpus__bge-small::hybrid | xerj__nfcorpus__bge-small::bm25 | 323 | +0.0535 | [+0.0383, +0.0688] | 0.0001 | 141 / 48 / 134 |
| sqlite__nfcorpus__bge-small::hybrid | sqlite__nfcorpus__bge-small::vec | 323 | +0.0114 | [-0.0031, +0.0264] | 0.1390 | 104 / 95 / 124 |
| xerj__nfcorpus__egemma::knn | sqlite__nfcorpus__egemma::vec | 323 | -0.0001 | [-0.0003, +0.0000] | 0.3792 | 1 / 3 / 319 |
| xerj__nfcorpus__egemma::hybrid | sqlite__nfcorpus__egemma::hybrid | 323 | -0.0017 | [-0.0080, +0.0052] | 0.6301 | 47 / 66 / 210 |
| xerj__nfcorpus__egemma::hybrid | xerj__nfcorpus__egemma::knn | 323 | -0.0137 | [-0.0284, -0.0001] | 0.0549 | 82 / 114 / 127 |
| sqlite__nfcorpus__egemma::hybrid | sqlite__nfcorpus__egemma::vec | 323 | -0.0121 | [-0.0256, +0.0019] | 0.0877 | 88 / 111 / 124 |
| xerj__nfcorpus__egemma::hybrid | xerj__nfcorpus__bge-small::hybrid | 323 | +0.0212 | [+0.0099, +0.0326] | 0.0002 | 113 / 77 / 133 |
| sqlite__nfcorpus__egemma::hybrid | sqlite__nfcorpus__bge-small::hybrid | 323 | +0.0229 | [+0.0119, +0.0342] | 0.0001 | 123 / 67 / 133 |
| xerj__nfcorpus__stemmer-bm25::bm25 | xerj__nfcorpus__bge-small::bm25 | 323 | +0.0179 | [+0.0078, +0.0284] | 0.0007 | 71 / 54 / 198 |
| sqlite__nfcorpus__porter-fts::fts | xerj__nfcorpus__stemmer-bm25::bm25 | 323 | +0.0028 | [-0.0030, +0.0087] | 0.3462 | 62 / 55 / 206 |
| xerj__fiqa__bge-small::bm25 | sqlite__fiqa__bge-small::fts | 648 | +0.0035 | [-0.0032, +0.0098] | 0.3012 | 97 / 83 / 468 |
| xerj__fiqa__bge-small::knn | sqlite__fiqa__bge-small::vec | 648 | +0.0000 | [+0.0000, +0.0000] | 1.0000 | 0 / 0 / 648 |
| xerj__fiqa__bge-small::hybrid | sqlite__fiqa__bge-small::hybrid | 648 | -0.0034 | [-0.0103, +0.0034] | 0.3481 | 91 / 87 / 470 |
| xerj__fiqa__bge-small::hybrid | xerj__fiqa__bge-small::knn | 648 | -0.0573 | [-0.0755, -0.0388] | 0.0001 | 142 / 230 / 276 |
| xerj__fiqa__bge-small::hybrid | xerj__fiqa__bge-small::bm25 | 648 | +0.1080 | [+0.0920, +0.1237] | 0.0001 | 294 / 41 / 313 |
| sqlite__fiqa__bge-small::hybrid | sqlite__fiqa__bge-small::vec | 648 | -0.0539 | [-0.0716, -0.0357] | 0.0001 | 144 / 226 / 278 |
