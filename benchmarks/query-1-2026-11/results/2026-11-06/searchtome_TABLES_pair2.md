### Base A: the 5,980 queries Knowl's published runs kept (same 25 exclusions)

| System | Failed | Micro R@1 | Micro R@3 | Micro nDCG@3 | Macro R@1 | Macro R@3 | Macro nDCG@3 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Knowl lexical (SQLite FTS5) | 0 | 58.51 | 78.95 | 70.60 | 58.17 | 79.04 | 70.49 |
| XERJ BM25, default (one match query) | 0 | 57.29 | 78.14 | 69.57 | 58.53 | 79.59 | 70.92 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | 0 | 23.76 | 40.75 | 33.63 | 23.72 | 44.84 | 36.05 |
| Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 0 | 70.92 | 88.49 | 81.38 | 70.76 | 88.84 | 81.47 |
| XERJ kNN only, E5 | 0 | 73.01 | 89.57 | 82.86 | 73.24 | 90.04 | 83.21 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | 0 | 72.83 | 89.43 | 82.68 | 73.15 | 90.08 | 83.20 |
| XERJ hybrid, E5, Knowl's page-level RRF | 0 | 69.48 | 87.88 | 80.45 | 70.39 | 88.77 | 81.30 |

### Base B: all 6,005 test queries, any failure counted as a miss

| System | Failed | Micro R@1 | Micro R@3 | Micro nDCG@3 | Macro R@1 | Macro R@3 | Macro nDCG@3 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Knowl lexical (SQLite FTS5) | 25 | 58.27 | 78.62 | 70.30 | 57.80 | 78.57 | 70.06 |
| XERJ BM25, default (one match query) | 0 | 57.44 | 78.22 | 69.68 | 58.67 | 79.66 | 71.02 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | 0 | 23.73 | 40.70 | 33.59 | 23.69 | 44.75 | 35.98 |
| Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 25 | 70.62 | 88.13 | 81.04 | 70.33 | 88.32 | 80.99 |
| XERJ kNN only, E5 | 0 | 73.07 | 89.61 | 82.91 | 73.25 | 90.07 | 83.23 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | 0 | 72.91 | 89.48 | 82.74 | 73.22 | 90.11 | 83.25 |
| XERJ hybrid, E5, Knowl's page-level RRF | 0 | 69.56 | 87.93 | 80.52 | 70.44 | 88.81 | 81.34 |

### Per domain, base A (R@1 / R@3 / nDCG@3, percent)

| Domain | Pages | Queries | Knowl lexical (SQLite FTS5) | XERJ BM25, default (one match query) | XERJ BM25, title^10 + body (Knowl-style field weights) | Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | XERJ kNN only, E5 | XERJ hybrid, E5, RRF inside XERJ (chunk level) | XERJ hybrid, E5, Knowl's page-level RRF |
| --- | ---: | ---: | --- | --- | --- | --- | --- | --- | --- |
| education | 8 | 435 | 47.4 / 76.3 / 64.2 | 50.6 / 79.3 / 67.3 | 26.2 / 62.1 / 47.1 | 64.4 / 91.0 / 79.8 | 67.6 / 92.4 / 82.1 | 71.5 / 92.9 / 84.1 | 65.3 / 91.0 / 80.5 |
| finance | 86 | 1214 | 46.6 / 72.5 / 62.0 | 49.9 / 73.9 / 64.0 | 18.6 / 30.1 / 25.2 | 57.5 / 80.9 / 71.4 | 58.2 / 79.8 / 70.9 | 59.7 / 81.5 / 72.5 | 59.5 / 81.0 / 72.2 |
| law | 47 | 808 | 73.5 / 88.6 / 82.6 | 73.9 / 89.4 / 83.2 | 15.3 / 32.2 / 25.1 | 85.5 / 94.7 / 90.9 | 87.0 / 95.8 / 92.2 | 84.2 / 95.8 / 91.1 | 83.9 / 95.5 / 90.8 |
| medicine | 121 | 1993 | 64.1 / 82.5 / 75.0 | 56.3 / 76.6 / 68.2 | 28.0 / 40.7 / 35.4 | 77.3 / 92.8 / 86.6 | 80.0 / 94.8 / 88.9 | 79.7 / 93.3 / 87.8 | 71.7 / 90.1 / 82.8 |
| natural_sciences | 34 | 573 | 69.8 / 85.5 / 79.2 | 70.9 / 86.9 / 80.3 | 31.2 / 69.1 / 53.7 | 80.1 / 94.2 / 88.7 | 87.4 / 98.1 / 93.9 | 84.8 / 97.0 / 92.1 | 82.4 / 95.3 / 90.1 |
| social_sciences | 117 | 957 | 47.6 / 68.8 / 60.0 | 49.6 / 71.5 / 62.5 | 22.9 / 34.9 / 29.8 | 59.8 / 79.4 / 71.4 | 59.2 / 79.3 / 71.1 | 59.0 / 80.0 / 71.6 | 59.7 / 79.7 / 71.4 |

### Latency per query, cold pass (ms, nearest rank)

| System | Micro p50 | Micro p95 | Macro p50 | Macro p95 |
| --- | ---: | ---: | ---: | ---: |
| Knowl lexical (SQLite FTS5) | 3.73 | 5.13 | 2.96 | 3.84 |
| XERJ BM25, default (one match query) | 0.27 | 0.39 | 0.27 | 0.34 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | 0.25 | 0.34 | 0.25 | 0.31 |
| Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 37.92 | 52.74 | 35.45 | 41.64 |
| XERJ kNN only, E5 | 28.84 | 35.85 | 23.91 | 29.13 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | 42.67 | 51.92 | 40.16 | 46.00 |
| XERJ hybrid, E5, Knowl's page-level RRF | 33.68 | 43.00 | 32.81 | 38.61 |

### Stage timings, cold pass (ms: mean / p50 / p95)

| System | Stage | Mean | p50 | p95 |
| --- | --- | ---: | ---: | ---: |
| Knowl lexical (SQLite FTS5) | total_ms | 3.49 | 3.73 | 5.13 |
| XERJ BM25, default (one match query) | fusion_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, default (one match query) | lexical_ms | 0.28 | 0.27 | 0.38 |
| XERJ BM25, default (one match query) | lexical_took_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, default (one match query) | total_ms | 0.29 | 0.27 | 0.39 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | fusion_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | lexical_ms | 0.25 | 0.24 | 0.33 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | lexical_took_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | total_ms | 0.26 | 0.25 | 0.34 |
| Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | embed_ms_measured_separately | 16.82 | 16.90 | 21.68 |
| Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | search_and_fusion_ms_arithmetic | 22.64 | 20.70 | 34.98 |
| Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | total_ms | 39.46 | 37.92 | 52.74 |
| XERJ kNN only, E5 | embed_ms | 17.58 | 17.56 | 22.38 |
| XERJ kNN only, E5 | fusion_ms | 0.01 | 0.00 | 0.01 |
| XERJ kNN only, E5 | total_ms | 26.60 | 28.84 | 35.85 |
| XERJ kNN only, E5 | vector_ms | 8.12 | 12.55 | 14.99 |
| XERJ kNN only, E5 | vector_took_ms | 6.32 | 11.00 | 13.00 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | embed_ms | 18.20 | 18.13 | 23.00 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | fusion_ms | 0.01 | 0.01 | 0.01 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | hybrid_request_ms | 22.78 | 23.16 | 32.12 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | hybrid_took_ms | 20.63 | 21.00 | 30.00 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | total_ms | 42.05 | 42.67 | 51.92 |
| XERJ hybrid, E5, Knowl's page-level RRF | embed_ms | 17.87 | 17.80 | 22.69 |
| XERJ hybrid, E5, Knowl's page-level RRF | fusion_ms | 0.06 | 0.06 | 0.08 |
| XERJ hybrid, E5, Knowl's page-level RRF | lexical_ms | 1.56 | 1.53 | 1.96 |
| XERJ hybrid, E5, Knowl's page-level RRF | lexical_took_ms | 0.50 | 1.00 | 1.00 |
| XERJ hybrid, E5, Knowl's page-level RRF | total_ms | 33.87 | 33.68 | 43.00 |
| XERJ hybrid, E5, Knowl's page-level RRF | vector_ms | 13.28 | 12.94 | 22.40 |
| XERJ hybrid, E5, Knowl's page-level RRF | vector_took_ms | 11.71 | 11.00 | 21.00 |

### Latency per query, warm pass (ms, nearest rank)

| System | Micro p50 | Micro p95 | Macro p50 | Macro p95 |
| --- | ---: | ---: | ---: | ---: |
| XERJ BM25, default (one match query) | 0.25 | 0.38 | 0.18 | 0.30 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | 0.22 | 0.33 | 0.17 | 0.25 |
| XERJ kNN only, E5 | 28.77 | 35.61 | 23.91 | 29.18 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | 40.90 | 49.89 | 32.46 | 43.52 |
| XERJ hybrid, E5, Knowl's page-level RRF | 32.22 | 40.25 | 28.12 | 35.87 |

### Stage timings, warm pass (ms: mean / p50 / p95)

| System | Stage | Mean | p50 | p95 |
| --- | --- | ---: | ---: | ---: |
| XERJ BM25, default (one match query) | fusion_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, default (one match query) | lexical_ms | 0.23 | 0.24 | 0.37 |
| XERJ BM25, default (one match query) | lexical_took_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, default (one match query) | total_ms | 0.23 | 0.25 | 0.38 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | fusion_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | lexical_ms | 0.20 | 0.22 | 0.32 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | lexical_took_ms | 0.00 | 0.00 | 0.00 |
| XERJ BM25, title^10 + body (Knowl-style field weights) | total_ms | 0.20 | 0.22 | 0.33 |
| XERJ kNN only, E5 | embed_ms | 17.60 | 17.56 | 22.42 |
| XERJ kNN only, E5 | fusion_ms | 0.01 | 0.00 | 0.01 |
| XERJ kNN only, E5 | total_ms | 26.60 | 28.77 | 35.61 |
| XERJ kNN only, E5 | vector_ms | 8.01 | 12.51 | 14.79 |
| XERJ kNN only, E5 | vector_took_ms | 6.19 | 11.00 | 13.00 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | embed_ms | 17.90 | 17.78 | 22.87 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | fusion_ms | 0.01 | 0.01 | 0.01 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | hybrid_request_ms | 16.71 | 22.81 | 30.32 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | hybrid_took_ms | 14.63 | 21.00 | 28.00 |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) | total_ms | 35.67 | 40.90 | 49.89 |
| XERJ hybrid, E5, Knowl's page-level RRF | embed_ms | 17.74 | 17.65 | 22.57 |
| XERJ hybrid, E5, Knowl's page-level RRF | fusion_ms | 0.06 | 0.06 | 0.08 |
| XERJ hybrid, E5, Knowl's page-level RRF | lexical_ms | 1.33 | 1.47 | 1.96 |
| XERJ hybrid, E5, Knowl's page-level RRF | lexical_took_ms | 0.45 | 0.00 | 1.00 |
| XERJ hybrid, E5, Knowl's page-level RRF | total_ms | 29.96 | 32.22 | 40.25 |
| XERJ hybrid, E5, Knowl's page-level RRF | vector_ms | 9.76 | 12.70 | 19.06 |
| XERJ hybrid, E5, Knowl's page-level RRF | vector_took_ms | 8.21 | 11.00 | 17.00 |

### Paired bootstrap of micro deltas (points, XERJ minus Knowl, 20,000 resamples)

| Comparison | Basis | ΔR@1 [95% CI] | ΔR@3 [95% CI] | ΔnDCG@3 [95% CI] |
| --- | --- | --- | --- | --- |
| XERJ BM25, default (one match query) vs Knowl lexical (SQLite FTS5) | 5980 | -1.22 [-2.11, -0.32] | -0.80 [-1.52, -0.08] | -1.02 [-1.62, -0.43] |
| XERJ BM25, title^10 + body (Knowl-style field weights) vs Knowl lexical (SQLite FTS5) | 5980 | -34.75 [-36.27, -33.23] | -38.19 [-39.63, -36.76] | -36.96 [-38.27, -35.65] |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) vs Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 5980 | +1.91 [+0.89, +2.93] | +0.94 [+0.23, +1.64] | +1.29 [+0.65, +1.95] |
| XERJ hybrid, E5, Knowl's page-level RRF vs Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 5980 | -1.44 [-2.26, -0.62] | -0.62 [-1.14, -0.12] | -0.93 [-1.38, -0.47] |
| XERJ kNN only, E5 vs Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 5980 | +2.09 [+1.05, +3.13] | +1.07 [+0.30, +1.86] | +1.48 [+0.78, +2.18] |
| XERJ BM25, default (one match query) vs Knowl lexical (SQLite FTS5) | 6005 | -0.83 [-1.73, +0.07] | -0.40 [-1.12, +0.32] | -0.63 [-1.24, -0.02] |
| XERJ BM25, title^10 + body (Knowl-style field weights) vs Knowl lexical (SQLite FTS5) | 6005 | -34.54 [-36.07, -33.01] | -37.92 [-39.38, -36.45] | -36.71 [-38.05, -35.38] |
| XERJ hybrid, E5, RRF inside XERJ (chunk level) vs Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 6005 | +2.28 [+1.25, +3.31] | +1.35 [+0.62, +2.05] | +1.69 [+1.02, +2.37] |
| XERJ hybrid, E5, Knowl's page-level RRF vs Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 6005 | -1.07 [-1.88, -0.25] | -0.20 [-0.75, +0.35] | -0.53 [-1.01, -0.05] |
| XERJ kNN only, E5 vs Knowl hybrid, E5 (SQLite FTS5 + sqlite-vec, RRF) | 6005 | +2.45 [+1.42, +3.51] | +1.48 [+0.68, +2.28] | +1.87 [+1.16, +2.60] |

