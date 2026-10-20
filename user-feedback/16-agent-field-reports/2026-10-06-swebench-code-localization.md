# xerj code as a file-localization retriever on SWE-bench Lite (2026-10-06)

**Agent:** Claude Opus 5.5 (Claude Code)  ·  **XERJ:** v1.0.0-rc.80 (repros on rc.81)  ·  **Platform:** Windows 11 x86_64

**Pointed at:** 100 SWE-bench Lite repos at their base commits, `.py` files only, one fresh node and `xerj autoindex` per instance (727 files / 13,255 records for scikit-learn).

**Used it for:** ranking the files an issue's gold patch edits, comparing `xerj code`'s BM25 body, its hybrid, and offline re-fusions of captured legs, under both embedders.

**Verdict:** The autoindex → query loop ran unattended for 100 lexical and 37 neural per-instance builds and its results were reproducible once settled, which made a real evaluation possible. BM25 is the strong default; with the default lexical embedder, hybrid never beat it, so I would not turn hybrid on there. With `--embed-mode neural`, hybrid on a short query (the issue's first ~300 chars) clearly helps. Most of the time went to things that degrade silently with HTTP 200 — a capped vector leg, hybrid dropping indices, `--lang` dropping the vector leg, a 30 s deadline returning partial hits, and long queries switching to per-segment BM25 statistics. The fixes for the first three landed within two days. Neural indexing on a 15 W laptop is the practical limit: django and sympy were out of reach.

**Numbers:** file-level acc@5, first-300-char query, 36 small-repo instances, paired bootstrap 95% CI vs BM25: neural shipped hybrid 0.722 vs BM25 0.528 (+0.194, CI excludes 0); lexical shipped hybrid 0.528 (n.s.). All 100 instances, lexical: semantic leg window 50 vs the shipped 10 → acc@5 +0.10 [+0.02, +0.18]. Neural embedding ≈ 1.4× slower than sentence-transformers on the same model and chunking (60 s vs 43 s per 64 KB, 8 cores).

**Filed alongside:** #1145, #1146, #1148 (all fixed); #1186 (long queries fall back to per-segment BM25 statistics).
