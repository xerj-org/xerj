# Head-to-head retrieval: the commenter-style SQLite stack (measured 2026-11-06, synthesis lane)

Same 5,000 stored records, same 160 query strings (`collection <cid> <question>`), same bge-small-en-v1.5 vectors
(`work/emb_records.npy`, `work/emb_queries.npy`, written by `scripts/xerj_retrieval.py`) as the XERJ arms in
`h2h_retrieval.md`. Stack: SQLite 3.53.4 FTS5 (default unicode61 tokenizer, `ORDER BY rank` = bm25), sqlite-vec v0.1.9
`vec0` (cosine, exact), RRF k=60 over 100 hits per leg, ties by insertion order (as in the BEIR SQLite stack,
`measure/scripts/sqlite_bench.py`). Rankings: `work/sqlite_h2h/rankings_sqlite.json` (record keys only, no gold), built by
`scripts/sqlite_h2h_rankings.py`; scored on our ARM build host by `scripts/host_score_retrieval_sqlite.py` (gold stayed on our ARM build host);
aggregates `measure/host_aggregates/h2h_retrieval_agg_sqlite.json`. 90 scored questions, as in `h2h_retrieval.md`.

| Arm (record level) | R@1 | R@3 | R@5 | R@10 | nDCG@3 | nDCG@10 | MRR@10 | all gold in top 40 |
|---|---|---|---|---|---|---|---|---|
| SQLite FTS5 BM25 | 0.327 | 0.541 | 0.643 | 0.816 | 0.635 | 0.718 | 0.744 | 85.6% |
| sqlite-vec kNN (bge-small) | 0.275 | 0.525 | 0.629 | 0.735 | 0.542 | 0.620 | 0.635 | 77.8% |
| SQLite FTS5 + sqlite-vec, RRF | 0.373 | 0.566 | 0.685 | 0.834 | 0.645 | 0.738 | 0.761 | 81.1% |

| Arm (chunk level, 8-record chunks) | R@1 | R@3 | R@5 | R@10 | nDCG@3 | nDCG@10 | MRR@10 |
|---|---|---|---|---|---|---|---|
| SQLite FTS5 BM25 | 0.512 | 0.810 | 0.881 | 0.954 | 0.831 | 0.872 | 0.867 |
| sqlite-vec kNN (bge-small) | 0.480 | 0.772 | 0.870 | 0.946 | 0.770 | 0.833 | 0.815 |
| SQLite FTS5 + sqlite-vec, RRF | 0.529 | 0.834 | 0.896 | 0.956 | 0.845 | 0.879 | 0.863 |

Paired differences, record nDCG@10 (mean [95% CI], cluster bootstrap over collections, 4,000 draws): XERJ hybrid − SQLite
hybrid −0.0022 [−0.0316, +0.0226]; XERJ BM25 − SQLite FTS5 −0.0114 [−0.0340, +0.0112]; XERJ kNN − sqlite-vec 0 (identical
lists after de-duplication on 160/160 questions; both equal the exact numpy cosine top-10). SQLite FTS5 − RAG BM25 chunks:
record +0.297 [+0.206, +0.381], chunk −0.041 [−0.084, −0.004].

Latency (search only, query vector precomputed, in-process in-memory database, second pass, laptop core 20): FTS5 p50
0.784 ms, sqlite-vec 1.386 ms, hybrid 2.189 ms.
