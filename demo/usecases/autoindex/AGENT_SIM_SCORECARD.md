# Agent-simulation scorecard — `xerj autoindex` vs grep-only baseline

**Date:** 2026-07-09 · **Examiner:** automated evaluator (had sole access to the secret manifest; agents never saw it)
**Corpus:** /tmp/xerj-discover/corpus — 1,995 files / 518.5 MB / 25 formats (JSONL, gz, nginx+app logs, SQL dump, SQLite, 3 CSV dialects, PDF, DOCX, HTML, XML, YAML, planted junk)
**Engine:** worktree build of `xerj` (branch feat/autoindex), fresh server on es-compat port 9274, data dir /tmp/xerj-autoindex/data-sim, booted under `systemd-run --user --scope -p MemoryMax=32G -p MemorySwapMax=0` (peak observed RSS 15.7 GB — within cap).
**Indexing run:** `xerj autoindex /tmp/xerj-discover/corpus --url http://localhost:9274 --state-dir /tmp/xerj-autoindex/state-sim --fresh` → **38.1 s wall, 31 datasets, 2,018,398 records live, 3 junk files recorded, exit 3** (completed-with-junk, as designed).

Both agents are the SAME model (headless Claude Code v2.1.206, `--max-turns 40 --allowedTools "Bash" --dangerously-skip-permissions`, one fresh invocation per question). The exam therefore measures the **access path** (ES-compat API over an auto-indexed corpus vs raw files + local tools), not the model.

Questions + pre-recorded ground truth: `EXAM_QUESTIONS.md` (written before any agent ran; exact numeric answers computed from the raw corpus by `/tmp/xerj-autoindex/gt_compute.py`).

## Exact command lines

Agent A (XERJ-backed), cwd `/tmp/xerj-autoindex/sim-a` (empty dir — cannot browse the corpus):

```
timeout 900 /home/claude/.local/bin/claude -p "A data folder was automatically indexed by 'xerj autoindex' into an Elasticsearch-compatible search engine at http://localhost:9274. You have NO filesystem access to the original folder — answer using ONLY the engine's HTTP API via curl. The indexed data lives in indices you can discover via GET /_cat/indices; there is also a catalog/data-map index describing every dataset and its fields. Question: <Q> Investigate with the API, then give a clear final answer." \
  --output-format stream-json --verbose --max-turns 40 --allowedTools "Bash" --dangerously-skip-permissions
```

Agent B (baseline, fair), cwd `/tmp/xerj-discover/corpus`:

```
timeout 900 /home/claude/.local/bin/claude -p "You are in a data folder (your current working directory). Answer the question using ONLY the files under the current directory — you may use bash, grep, find, python3, or any local tool to read and analyze them. Do NOT access the network and do NOT read or modify anything outside the current directory. Question: <Q> Investigate the files, then give a clear final answer." \
  --output-format stream-json --verbose --max-turns 40 --allowedTools "Bash" --dangerously-skip-permissions
```

Runner: `/tmp/xerj-autoindex/exam/run_one.sh`; full per-run transcripts (stream-json, every command + output) in `sim-transcripts/Q{1..10}-agent-{a,b}.jsonl`.

## Headline (honest)

**Accuracy: Agent A (XERJ) 9 correct + 1 partial of 10 · Agent B (grep/read-only) 10 correct of 10.**
**Wall clock: A median 73.5 s / total 1,031 s · B median 84.4 s / total 843 s.** A was faster on 7 of 10 questions, but its two losses were large (Q2 +177 s, Q3 +120 s) and caused by a **live-verified engine defect** (below), while B's seven losses were all ≤ 36 s.

On a 500 MB corpus that fits on local disk, **the fair baseline is not beaten — this is a tie-to-slight-baseline-edge on accuracy and a wash on speed**. A capable agent with bash is a formidable analyst: it wrote Python for SQLite, handled decimal-comma CSV, used zgrep on archives, and did byte-entropy forensics the index cannot do. Where XERJ genuinely helped:

- **Instant orientation** — the data map answered "what is in here?" in 4 API calls / 58 s with exact per-dataset counts (Q1); grep needed a 7-step directory sweep to reach the same (correct) answer.
- **Aggregations** — terms/cardinality/sum/max over millions of rows in single sub-second calls (Q4 revenue, Q5 device cardinality, Q6 status distribution, Q7 fleet breakdown). A was ~2× faster on Q7 and 1.6× on Q5.
- **Uniform access to hostile formats** — SQLite, semicolon/decimal-comma CSV, gzip, DOCX/PDF all appear as ordinary indices; A never had to know the file formats existed (Q7, Q8 at 38-40 s each).
- **Exact cross-source counts** — Q2: A reported planted-entity hit counts per source (26,288 events + 9,743 app-log lines + 11,522 nginx lines + 4,301 SQL rows + …); B established presence/absence but not exhaustive counts.

Where grep was fine or better:

- **Every narrative/lookup question** (Q2, Q3, Q9): same conclusions, and faster on Q2/Q3 because A had to work around the bool-query defect with field-by-field scans.
- **Byte-level forensics** (Q10): B *proved* sprite-atlas.bin is random noise (entropy 7.99996 bits/byte, incompressible), identified both non-UTF8 legacy encodings, and caught `exports/summary_q1.pdf` as JSON-wearing-a-.pdf. A saw only what the autoindex junk report recorded (3 files) and mis-guessed the .bin as "a legitimate texture asset" → scored partial. Raw bytes beat any index for forensics.
- **Honest scale caveat:** at 518 MB, local grep/python is always in play. The XERJ path's structural advantages (bounded-time aggregations, no per-format tooling, remote access) would need TB-scale, remote-only, or repeated-query scenarios to dominate rather than tie. This run does not prove those; it proves zero-config parity plus better ergonomics for aggregation questions.

## Per-question results

GT = pre-recorded ground truth. Wall = end-to-end `claude -p` wall clock (runlog); turns = API turns (num_turns).

| Q | Topic | Agent A verdict | A wall | A turns | Agent B verdict | B wall | B turns | Faster |
|---|---|---|---:|---:|---|---:|---:|---|
| Q1 | Inventory of the folder | **correct** — all major families w/ exact counts | 62.1 s | 5 | **correct** — same, incl. truncated-gzip caveat | 84.2 s | 7 | A |
| Q2 | Sources containing u-1042 | **correct** — 7 families + exact per-index counts | 260.7 s | 18 | **correct** — 6 families incl. postmortem HTML | 83.9 s | 7 | B |
| Q3 | 2026-03-14 incident story | **correct** — t-acme, 09:00–11:00Z, IP+trace+INC-2417 | 225.9 s | 18 | **correct** — same + on-call note & audit tie-ins | 105.6 s | 11 | B |
| Q4 | Revenue per tenant (all 120k orders) | **correct** — t-northwind $2,576,038.83; all 8 tenants exact; deduped CSV subset via catalog containment | 87.4 s | 8 | **correct** — identical totals; deduped by order_id sets | 92.9 s | 11 | A |
| Q5 | Device cardinality across sources | **correct** — 300, 4 sources identical sets | 69.0 s | 8 | **correct** — 300, 4 sources + tenant consistency | 105.5 s | 11 | A |
| Q6 | Event status distribution | **correct** — ok 684,343 (92.0%) / error 29,910 / timeout 18,613 / throttled 11,134; archive combined view too | 58.9 s | 7 | **correct** — identical counts + truncation caveat | 47.2 s | 5 | B |
| Q7 | Fleet models + top device tenant (SQLite-only fact) | **correct** — PX-9 78 / TH-100 77 / PX-11 74 / TH-200 71; t-globex 75 | 41.9 s | 6 | **correct** — identical (python3 sqlite3) | 79.1 s | 10 | A |
| Q8 | Max temp in March sensor export (semicolon CSV, decimal comma) | **correct** — 98.6 °C, dev-903fba, 2026-03-22, Pune | 39.8 s | 6 | **correct** — identical | 43.4 s | 6 | A |
| Q9 | SQL-only tenants (negative-space join) | **correct** — exactly t-initech, t-hooli, t-wonka; 0 hits in all telemetry | 77.9 s | 6 | **correct** — identical, incl. text-mention sweep | 84.5 s | 8 | A |
| Q10 | Junk / what is sprite-atlas.bin | **partial** — 3 junk files from catalog report; called .bin undecodable junk but mis-guessed it as "a legitimate texture asset" (GT: random bytes); missed non-UTF8 notes + JSON-as-.pdf | 107.3 s | 15 | **correct** — all 6 planted anomalies; proved .bin random (entropy 7.99996), found summary_q1.pdf is JSON | 116.8 s | 15 | B |
| | **Totals** | **9 correct, 1 partial** | **1,031 s** (med 73.5) | avg 9.7 | **10 correct** | **843 s** (med 84.4) | avg 9.1 | A 7 / B 3 |

## How Agent A used the system

- **Catalog first, every time:** all 10 runs queried `autoindex-catalog` (or `_cat/indices` then the catalog) before touching data indices — the data map (dataset → source files, formats, record counts, field types/semantics, time ranges, cross-dataset key correlations) was its orientation layer. Q4 explicitly used the catalog's precomputed 1.00 order-id containment to justify deduping the CSV subset against the SQL orders.
- **Query classes reached for:** `terms` (8/10 runs — the workhorse), `cardinality`, `sum`, `max`/`min`, `date_histogram` (incident timeline), plus `_count`, `query_string`, `wildcard`, `match_phrase`, and sorted top-k searches. No hybrid/semantic queries were needed — every exam answer was structural/lexical.
- **Self-verification behavior:** A cross-checked catalog *sampled* estimates against full-index aggregations (Q5: sampled cardinality said 244/251; full terms agg proved 300 — the catalog's own sampling caveat made it re-check rather than mis-answer).

## Engine defect found (live-verified during the exam, reproduced afterward)

`bool` queries with **only `should` clauses silently return 0 hits** on these autoindexed indices, while the identical clause standalone (or under `must`/`filter`) matches:

```
{"query":{"term":{"user_id":"u-1042"}}}                          → 1 hit
{"query":{"bool":{"must":[{"term":{"user_id":"u-1042"}}]}}}     → 1 hit
{"query":{"bool":{"filter":[{"term":{"user_id":"u-1042"}}]}}}   → 1 hit
{"query":{"bool":{"should":[{"term":{"user_id":"u-1042"}}]}}}   → 0 hits  ← ES would return 1
{"query":{"query_string":{"query":"\"u-1042\"","fields":["*"],"lenient":true}}} → 0 hits
```

ES semantics: a should-only bool requires ≥1 should to match. This bug is what made Agent A slow on Q2/Q3 (it detected the zero-results anomaly, distrusted it, and fell back to per-field scans — correct answers, 3-4× the time). Filed here as a compat ticket candidate; NOT fixed in this evaluation (no engine changes were made, so the ES-YAML conformance gate is unaffected).

## Fairness & guard

- Ground truth was recorded in `EXAM_QUESTIONS.md` before any agent ran; scoring rubric fixed in advance; ties reported as ties (doc-index lesson applied — the baseline got the same model, same turn budget, same question text, and full local tooling incl. python3/sqlite3 module/zgrep).
- **Guard scan of all 20 transcripts:** no agent read `GROUND_TRUTH.md`, `/tmp/xerj-discover/tools`, or escaped the corpus via `..`; Agent B made zero network calls; Agent A never touched the corpus filesystem. No reruns needed.
- Agent B was NOT penalized artificially: its cwd was the corpus root, prompts did not hide any tool, and its `timeout 900` was never hit (max B run: 116.8 s).

## Cleanup

Server on :9274 stopped after the exam (`systemctl --user stop xerj-sim-9274.scope`); data dir /tmp/xerj-autoindex/data-sim removed. Corpus and manifest left intact for reuse.
