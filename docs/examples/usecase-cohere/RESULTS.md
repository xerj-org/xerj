# XERJ + Cohere worked example, measured 2026-11-05 (laptop: 32 cores, 119 GB RAM, x86_64; private ports)
Build: origin/main 7458a225c + PRs #2984 (live Postgres) #2985 (Cohere rerank) #2983 (Cohere examples) #2988 (rows to facts), release profile. Facts node: rc.101 release binary + A_s47 bundle.
Corpus: 13 public-domain US government PDFs (NIST AI RMF, AI 600-1, CSF 2.0, SP 800-53r5, 800-61r3, 800-30r1, Privacy Framework; CMS claims manual ch.1/3/12/26, benefit policy ch.15, program integrity ch.3), 22.9 MB. Postgres: 200,000 synthetic claims rows (generated), Postgres 16 in Docker, logical replication.
(b) autoindex: 14 files -> 3,033 records in 7.9 s, client peak RSS 238 MB, index 70 MB store.
(c) connect: snapshot 200,000 rows at ~47,100 rows/s; 60 single-row UPDATEs commit->visible p50 19.7 ms, p95 21.7 ms, max 34.6 ms; DELETE removed, INSERT appeared.
(d) rows-to-facts: 1,000 rows (CSV export of the table) -> 6,000 facts in 1.3 s. 8 questions: 5 ANSWER all correct (West count 200 via counting w/ partner; denied+West 34 via counting w/ partner; 3 lookups), 3 UNKNOWN (denied count, telehealth count, total amount = sum not supported), 0 wrong. ~170-380 ms per ask.
(e) Cohere (MOCK server, no key): _search rerank provider cohere applied on window 30 (shape only; mock scores meaningless); Command RAG cookbook indexed 3,033 records with MOCK 256-dim vectors in 7.6 s; ask -> 2 citations resolved to file+page; mock call counts embed 33, rerank 2, chat 1.
Search: policy-docs match p50 0.8 ms p95 2.3 ms; claims filter+sum agg (200k rows) p50 0.5 ms p95 0.8 ms; denied+West = 6,665 rows, sum 33,459,315 (matches Postgres).
RSS: search node 787 MB (200k rows + 3,033 docs + rag index); facts node with model 1.05 GB.
