# Head-to-head retrieval quality: R@k, nDCG@k, MRR@10 (measured 2026-11-06)

Retrieval quality of every retrieval arm of the 2026-11-05 head-to-head (`h2h-2026-11-05/RESULTS.md`), plus XERJ rc.101 BM25 / kNN / hybrid on the same records, scored against the eval's own gold evidence. No LLM call was made for this measurement. Every number comes from a file listed under Sources; ARITHMETIC is labelled.

**Corpus (not Wikipedia).** The 160 questions and the store of the head-to-head: 5,000 stored records = 4,305 distinct records in 95 collections (82 distinct listings: 13 listings of the two counting families are the same world and were stored twice, 695 duplicate records), synthetic records `subject | relation | type: value | source sentence` from XERJ Score v2 eval worlds. The 10 GB Wikipedia run (`INDEX_VS_EMBED_10GB.md`, HF `wikimedia/wikipedia` 20231101.en) measured indexing / embedding cost and BM25 latency on 100 title queries; it has no relevance judgments and no retrieval-quality number.

**Relevance (gold, derived and kept on our ARM build host).** A record is relevant if it is a gold evidence record of the question; a chunk (the RAG unit: 8 consecutive records of one collection) is relevant if it contains one. Point questions: the label's `support_fact_ids`. Counting questions: the evidence of every counted member returned by executing the gold set program with the frozen engine worker (it reproduced every gold answer). Every stored listing was re-rendered from its eval world and matched byte for byte (160/160), so positions map to fact ids exactly.
Scored: **90 questions** (54 point, 36 counting; median 2 relevant records, max 32). Excluded: 38 not answerable (gold UNKNOWN / INCOMPLETE, incl. 16 bench OUT_OF_SCOPE items the scorer treats as UNKNOWN), 17 counting questions whose gold count is 0 (no counted record; 17/17 checked by `host_check_zero.py`), 15 bench OUT_OF_SCOPE-complete questions (gold computed by SQL, no record pointer): their SQL read set is reported separately (11 non-empty; `all_101` in the JSON).
Metrics: R@k = |relevant in top k| / |relevant| (recall); Hit@k = any relevant in top k; nDCG@k binary gains, log2 discount; MRR@10. Duplicates count once at their first rank. 95% CI: bootstrap over collections, 4,000 draws.

**Where the lists come from.** RAG: the exact chunks sent to Opus, recovered from the stored prompts (`raw/rag/<arm>/<qid>.usr`), and the retrieval step re-run with `rag_build.py`'s settings (no LLM): identical lists 160/160 in all four arms; the top-5 list is the prefix of the top-10 list, so the top-5 and top-10 arms share one ranking. GraphRAG: the `-Sources-` text units of its local-search context, in context order (its entity / relationship / report sections are LLM-written text, not records). XERJ: `xerj v1.0.0-rc.101` x86_64 release binary, one doc per stored record (5,000 docs), same query strings and the same bge-small model as the RAG arms.

## 1. Record level (the evidence records themselves; 90 questions)
Chunk arms are read in rank order, records in stored order inside a chunk (what the LLM read); a chunk retriever cannot order records inside a chunk, which caps its R@1 / R@3 here: use table 2 to compare retrievers at their own unit.

| Arm | R@1 | R@3 | R@5 | R@10 | Hit@1 | Hit@3 | nDCG@3 | nDCG@10 | MRR@10 | nDCG@10 95% CI |
|---|---|---|---|---|---|---|---|---|---|---|
| RAG BM25 chunks | 0.084 | 0.281 | 0.448 | 0.662 | 0.200 | 0.500 | 0.264 | 0.421 | 0.412 | [0.362, 0.484] |
| RAG bge-small chunks | 0.073 | 0.256 | 0.384 | 0.566 | 0.200 | 0.456 | 0.243 | 0.363 | 0.383 | [0.300, 0.429] |
| GraphRAG local (sources) | 0.028 | 0.128 | 0.157 | 0.278 | 0.089 | 0.233 | 0.118 | 0.182 | 0.195 | [0.112, 0.269] |
| XERJ BM25 | 0.365 | 0.533 | 0.655 | 0.788 | 0.600 | 0.833 | 0.619 | 0.707 | 0.730 | [0.630, 0.779] |
| XERJ kNN (bge-small) | 0.275 | 0.525 | 0.629 | 0.735 | 0.467 | 0.789 | 0.542 | 0.620 | 0.635 | [0.550, 0.689] |
| XERJ hybrid RRF | 0.388 | 0.561 | 0.699 | 0.824 | 0.656 | 0.844 | 0.644 | 0.736 | 0.765 | [0.666, 0.803] |
| XERJ BM25 +cid filter | 0.369 | 0.535 | 0.660 | 0.808 | 0.622 | 0.844 | 0.628 | 0.722 | 0.745 | [0.645, 0.792] |
| XERJ kNN +cid filter | 0.284 | 0.568 | 0.695 | 0.807 | 0.489 | 0.867 | 0.582 | 0.674 | 0.674 | [0.613, 0.735] |
| XERJ hybrid +cid filter | 0.393 | 0.571 | 0.692 | 0.831 | 0.656 | 0.856 | 0.648 | 0.744 | 0.771 | [0.675, 0.809] |

## 2. Chunk level (8-record chunks; 90 questions)
XERJ record rankings are mapped to chunks by the rank of each chunk's first record.

| Arm | R@1 | R@3 | R@5 | R@10 | Hit@1 | Hit@3 | nDCG@3 | nDCG@10 | MRR@10 | nDCG@10 95% CI |
|---|---|---|---|---|---|---|---|---|---|---|
| RAG BM25 chunks | 0.597 | 0.840 | 0.914 | 0.963 | 0.856 | 0.989 | 0.881 | 0.913 | 0.921 | [0.877, 0.944] |
| RAG bge-small chunks | 0.495 | 0.768 | 0.853 | 0.896 | 0.744 | 0.933 | 0.791 | 0.823 | 0.840 | [0.767, 0.877] |
| GraphRAG local (sources) | 0.232 | 0.553 | 0.699 | 0.882 | 0.389 | 0.733 | 0.531 | 0.647 | 0.577 | [0.565, 0.730] |
| XERJ BM25 | 0.528 | 0.807 | 0.861 | 0.933 | 0.767 | 0.956 | 0.826 | 0.863 | 0.861 | [0.806, 0.913] |
| XERJ kNN (bge-small) | 0.480 | 0.772 | 0.870 | 0.946 | 0.700 | 0.944 | 0.770 | 0.833 | 0.815 | [0.785, 0.877] |
| XERJ hybrid RRF | 0.529 | 0.823 | 0.898 | 0.956 | 0.767 | 0.967 | 0.837 | 0.876 | 0.863 | [0.833, 0.914] |
| XERJ BM25 +cid filter | 0.540 | 0.818 | 0.894 | 0.973 | 0.789 | 0.967 | 0.847 | 0.893 | 0.876 | [0.844, 0.935] |
| XERJ kNN +cid filter | 0.514 | 0.852 | 0.920 | 0.978 | 0.733 | 1.000 | 0.844 | 0.885 | 0.856 | [0.847, 0.923] |
| XERJ hybrid +cid filter | 0.524 | 0.847 | 0.913 | 0.980 | 0.767 | 0.978 | 0.859 | 0.896 | 0.869 | [0.857, 0.931] |

Paired differences (mean [95% CI], 90 questions): XERJ hybrid - RAG BM25: record nDCG@10 +0.315 [+0.234, +0.391], chunk nDCG@10 -0.036 [-0.068, -0.007]. XERJ BM25 - RAG BM25 chunk nDCG@10 -0.050 [-0.094, -0.012]. XERJ kNN - RAG bge-small (same model, record vs chunk unit): record nDCG@10 +0.257 [+0.187, +0.328], chunk nDCG@10 +0.009 [-0.028, +0.046]. XERJ hybrid - XERJ BM25: record nDCG@10 +0.029 [-0.021, +0.083].
**Read:** per-record ranking puts the exact evidence records first (record nDCG@3 0.644 vs 0.264); at chunk granularity plain BM25 over 8-record chunks is as good as or slightly better than XERJ's record ranking mapped to chunks. The XERJ gain is precision of the evidence, not finding the right neighbourhood.

By question type (record level, nDCG@10): point 0.857 XERJ hybrid / 0.464 RAG BM25; counting 0.554 / 0.355 (XERJ BM25 0.654, XERJ kNN 0.386): dense retrieval is weak on counting questions, whose evidence is many similar records. Bench SQL read set (11 q): XERJ hybrid 0.652, RAG BM25 0.399.

## 3. Did the context hold the evidence, and what did Opus do with it? (90 questions)
| Arm | context | distinct records in context (mean) | gold records in context (mean share) | ALL gold in context | Opus R/W/N when all gold in context | Opus R/W/N when some missing |
|---|---|---|---|---|---|---|
| RAG BM25 chunks | top5 chunks 40rec | 32.7 | 0.902 | 78.9% | 68/2/1 of 71 | 3/7/9 of 19 |
| RAG BM25 chunks | top10 chunks 80rec | 63.4 | 0.966 | 87.8% | 76/2/1 of 79 | 3/4/4 of 11 |
| RAG bge-small chunks | top5 chunks 40rec | 30.6 | 0.821 | 68.9% | 58/2/2 of 62 | 3/10/15 of 28 |
| RAG bge-small chunks | top10 chunks 80rec | 58.7 | 0.893 | 77.8% | 66/3/1 of 70 | 3/6/11 of 20 |
| GraphRAG local (sources) | all source units | 155.2 | 0.971 | 95.6% | 81/3/2 of 86 | 1/0/3 of 4 |
| XERJ BM25 | top40 records | 40.0 | 0.932 | 83.3% | not run (no LLM on this arm) | |
| XERJ BM25 | top80 records | 74.1 | 0.956 | 88.9% | not run (no LLM on this arm) | |
| XERJ kNN (bge-small) | top40 records | 40.0 | 0.886 | 77.8% | not run (no LLM on this arm) | |
| XERJ kNN (bge-small) | top80 records | 75.4 | 0.911 | 80.0% | not run (no LLM on this arm) | |
| XERJ hybrid RRF | top40 records | 40.0 | 0.924 | 81.1% | not run (no LLM on this arm) | |
| XERJ hybrid RRF | top80 records | 74.7 | 0.948 | 86.7% | not run (no LLM on this arm) | |
| XERJ hybrid +cid filter | top40 records | 35.0 | 0.979 | 94.4% | not run (no LLM on this arm) | |
| XERJ hybrid +cid filter | top80 records | 50.0 | 0.995 | 97.8% | not run (no LLM on this arm) | |

When the context held all gold evidence, Opus answered right in 58-81 of 62-86 questions per arm; in the classic RAG arms most wrong answers and declines are on questions whose evidence was not retrieved (`ctx_vs_outcome.json`, frozen scorer per-question outcomes); GraphRAG's 3 wrong answers on these 90 all had the evidence in context. Using the collection id as a filter (the question names it) raises all-evidence coverage at 40 records from 81.1% to 94.4% for XERJ hybrid.

## 4. XERJ Query-1 evidence (it does not rank; 160 questions re-asked, answers identical to the 2026-11-05 run on 160/160)
| Gold evidence group | ANSWERs | with evidence ids | ids == gold set | precision | recall |
|---|---|---|---|---|---|
| point questions | 34 | 34 | 34 | 1.000 | 1.000 |
| counting questions | 31 | 0 | 0 | n/a | 0.000 |
| all with gold evidence | 65 | 34 | 34 | 1.000 | 0.523 |

Point answers cite exactly the gold evidence (34/34). Counting answers cite no records: `final_evidence_ids` is `[]` on every counting answer by design (`docs/FACTS_API.md`, origin/main). 16 further ANSWERs are counts with gold 0 (no gold evidence) and 1 answers a not-answerable question (the run's 1 wrong answer).

## 5. Latency: retrieval is milliseconds, the LLM answer is seconds
| Step | p50 | Source |
|---|---|---|
| rank_bm25 over 660 chunks | 0.747-0.828 ms | raw/rag_index_stats.json |
| bge-small query encode (batched / one at a time, loaded laptop) | 4.13 ms / 20.4 ms | rag_index_stats.json / work/query_encode_latency.json |
| dot product over 660 chunk vectors | 0.018 ms | rag_index_stats.json |
| GraphRAG local-search context build | 96.0 ms | raw/gr_query_stats.json |
| XERJ BM25 / kNN / hybrid, 5,000 records, first execution (server took; client wall; kNN and hybrid add the query encode above) | 5.0 / 10.0 / 16.0 ms; 8.02 / 13.26 / 19.03 ms | work/rankings_xerj_rep3.json |
| Opus 5.5 answer call, RAG arms (claude -p wall incl. network + CLI; 115-124 output tokens mean incl. thinking) | 2.69-2.92 s (p95 4.77-5.4 s); GraphRAG 2.65 s | raw/*_cost.json |
| XERJ Query-1 answer (our ARM build host aarch64 shared box, 4 cores) | 1051.7 ms (2026-11-05: 1039.4 ms) | host_aggregates/xq_rerun_run_log.json, raw/xq_stats.json |

ARITHMETIC: rank_bm25 retrieval is 0.747 ms of a 2.74 s BM25-top-10 RAG answer (0.027%). The published 2.7-3.3 s are almost all the Opus call that writes the answer (RAG / GraphRAG 2.65-2.92 s; whole store in context, no retrieval at all, 3.31 s), not search. A 108 ms hybrid-search p50 and our 3 s are different things: ours is search + answer generation.
A repeated identical XERJ query is served from a result cache (took 0 ms, `work/xerj_latency.json` pass 2): only first executions are quoted. XERJ hybrid tie order depends on a per-process hash seed: across 3 server processes 106/160 hybrid and 157/160 kNN rankings were identical, and no 90-question metric of a XERJ arm moved by more than 0.0111 (JSON `stability_across_3_server_processes`).

## Exact XERJ query DSL (hybrid shown; all six bodies in the JSON)
```json
{
 "query": {
  "hybrid": {
   "queries": [
    {
     "query": {
      "match": {
       "text": "collection C3b3f01ab Within the pinned USGS catalogue slice (earthquakes, magnitude ≥ 4.5, 2020–2024 UTC), how many events occur strictly after us7000gwts’s timestamp but before the end of its UTC day?"
      }
     },
     "weight": 1.0
    },
    {
     "query": {
      "knn": {
       "field": "vec",
       "query_vector": "<384 floats: bge-small-en-v1.5 of \"collection <cid>: <question>\", L2-normalised>",
       "k": 100,
       "num_candidates": 5000
      }
     },
     "weight": 1.0
    }
   ],
   "fusion": {
    "type": "rrf",
    "k": 60
   }
  }
 },
 "size": 100,
 "_source": [
  "rkey"
 ]
}
```
BM25 = the `match` clause alone; kNN = top-level `{"knn": {"field": "vec", "query_vector": ..., "k": 100, "num_candidates": 5000}}`; the `+cid filter` variants add `{"term": {"cid": "<cid>"}}` as a bool filter / kNN `filter`. Index: `text` (text, `[cid] record`), `cid` (keyword), `vec` (dense_vector, 384, cosine, float32). After de-duplication XERJ kNN top-10 equals exact numpy cosine top-10 on 160/160 questions.

## Caveats
1. 90 scored questions (54 point, 36 counting) from our own templated eval; XERJ Query-1 was trained on this distribution, the retrievers were not (off-the-shelf BM25 and bge-small). CIs are wide.
2. Relevance definitions differ by question type (support facts / counted-member evidence / SQL read set); results are given per type in the JSON.
3. Record-level scores of chunk retrievers depend on the within-chunk order; the chunk table is the like-for-like comparison of retrievers.
4. Records were pre-extracted for every arm; no raw-document retrieval is measured here.
5. Latencies on shared machines (laptop load average up to 24.8; our ARM build host running training). XERJ Query-1 latency is a different quantity (answer, not retrieval).

## Reproduce
Scripts: `work-2026-11-06/scripts/` (`host_gold_rel.py`, `host_bench_rebuild.py`, `rag_rankings.py`, `xerj_retrieval.py`, `xerj_rep.py`, `xerj_latency.py`, `query_encode_latency.py`, `host_score_retrieval.py`, `host_ctx_vs_outcome.py`, `host_xq_evidence.py`, `host_xq_score.py`, `write_h2h_report.py`). Gold-derived files stay on our ARM build host (`~/xerj-review-2026-11-06/work`); only aggregates were copied (`measure/host_aggregates/`).
