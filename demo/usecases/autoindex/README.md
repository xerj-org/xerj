# `xerj autoindex` — point it at any folder, get an AI-searchable dataset. Zero config.

**What it is:** a subcommand of the main `xerj` binary. You give it a folder —
medical PDFs, server logs, a SQL backup, a directory nobody has looked at in
two years — and it discovers what is in there, infers structure, types, and
semantics from the data itself, and indexes everything into a running XERJ
over the ordinary ES-compatible API. No mappings to write, no per-format
scripts, no configuration files, no flags that change per corpus. The same
command works on all of it.

**Who it is for:** an AI agent (or a human) who must answer questions about a
pile of files *without* writing extraction scripts, hand-rolling mappings, or
reading files one by one. After one command, the agent's entire interface is
the ES-compat HTTP API plus a self-describing **data map**.

```
xerj autoindex <folder> [--url http://localhost:9200] [--workers N] [--prefix ax]
                        [--state-dir P] [--fresh] [--no-semantic] [--dry-run]
xerj autoindex map      [--url U] [--json] [--dataset SLUG]
xerj autoindex status   [--url U] [--state-dir P]
```

Exit codes: `0` complete · `3` completed-with-junk (junk recorded, never
fatal) · `2` usage · `1` endpoint unreachable / journal-config mismatch.

## The one-binary, zero-config promise

- **One binary.** `autoindex` ships inside `xerj` itself. It is a pure
  ES-compat HTTP client feature (it does not link the engine), so it works
  against any XERJ endpoint — local, remote, or one you did not start — and
  cannot destabilize the server. There is no sidecar tool, no Python
  dependency, no Logstash.
- **Zero config, no predefined scenarios.** Nothing is hardcoded: no field
  names, no per-corpus code paths. File types are detected by **content
  sniffing (magic bytes)**, not extensions — a JSON file wearing a `.pdf`
  extension is indexed as JSON ([verify run](#verified-claims), P12); a CSV
  wearing `.log` is sniffed as CSV (surprise-corpus run). Field types, date
  formats, keyword-vs-text semantics, entity columns, and cross-dataset key
  correlations are all inferred from bounded samples of the data.
- **Why inference lives in the client:** XERJ's dynamic mapping is coarse
  (ISO date strings map as `text`, no `.keyword` subfields) and ingest is
  silently lenient (junk into typed fields returns 201). `autoindex`
  therefore infers types itself, PUTs explicit mappings, and validates and
  coerces every record client-side before it ships. That is the core reason
  the feature exists.
- **Built for scale.** Streaming extractors (client memory stays flat
  regardless of file size — measured ~250 MB peak RSS on a 4.61 GB corpus),
  sampling-based inference, parallel workers, and an incremental resume
  journal: `kill -9` the run and re-run it, and it resumes and converges to
  identical counts (idempotent `_id`s — proven twice, see below). Junk files
  are skipped and *recorded with reasons*, never fatal. (One honest server-side
  scale caveat below.)

## Quickstart

```bash
# 1. a running XERJ (any endpoint works; default localhost:9200)
xerj --insecure --data-dir /tmp/xerj-data &

# 2. index a folder — this is the whole setup
xerj autoindex ~/my-data-folder

# 3. see what it found
xerj autoindex map

# 4. search it like any Elasticsearch
curl localhost:9200/ax-*/_search -H 'Content-Type: application/json' \
  -d '{"query":{"match":{"body":"outage"}}}'
```

Real output of step 2 on the small demo corpus used in the recipe draft
(4 files: a JSONL app log, a semicolon/decimal-comma CSV with German headers,
an HTML postmortem, and a random-bytes junk file), captured live 2026-07-09:

```
autoindex: 4 files (0 MB) under /tmp/xerj-autoindex/docs-demo/corpus
phase A: sniffing + sampling 4 files…
phase A: 3 datasets inferred, 1 junk/skipped files
phase B: indexing 3 files with 8 workers → http://localhost:9280

done in 0.2s — 3 datasets, 5801 records live, 0 junk records, 1 junk/skipped files
  ax-logs                                        5000 docs
  ax-exports                                      800 docs
  ax-docs                                           1 docs

next: `xerj autoindex map --url http://localhost:9280` for the data map; search via GET /ax-*/_search
```

The decimal-comma `temperatur_c` column came out `double`, the `zeitpunkt`
date-only strings came out typed `date`, and the `.bin` file was recorded as
junk with a reason — with zero flags.

## Pipeline stages

```
walk → sniff → sample → infer → map → extract → bulk → correlate → catalog → verify
```

1. **Walk** — recursive traversal; symlinks off by default
   (`--follow-symlinks` is loop-safe: a planted `loop/a/back -> loop` cycle
   did not recurse in the surprise-corpus run); deep paths and hostile
   filenames (`Q1 Report (final).docx`) handled.
2. **Sniff** — magic-bytes + content detection per file. Extensions are a
   hint at best, never trusted. Formats covered by streaming extractors:
   JSON/JSONL, CSV (dialect detection: comma/semicolon/tab, decimal comma,
   quoted multiline fields, BOM), structured logs (nginx CLF, app logs,
   syslog), SQL dumps, SQLite, PDF, DOCX, HTML, XML, YAML, plain text,
   gzip-compressed variants of the above.
3. **Sample** — bounded per-file sampling (default 500 records,
   `--sample N`); inference cost does not grow with file size.
4. **Infer** — field types (long/double/boolean/date/keyword/text), date
   *encodings* (ISO-ms, epoch-ms, nginx CLF, comma-ms app logs, MySQL
   datetime, RFC-2822 — all verified normalized to one typed `date` field),
   keyword-vs-text by cardinality, entity signals (emails, IPs, UUIDs, URLs).
5. **Map** — explicit mappings PUT per dataset (dates as
   `strict_date_optional_time||epoch_millis`, top-level `keyword` fields for
   exact filtering, `semantic_text` on prose bodies unless `--no-semantic`).
6. **Extract + bulk** — parallel workers stream full files (not just the
   sample) through client-side coercion into `_bulk` requests (default 8 MB
   cuts) with deterministic, idempotent `_id`s.
7. **Correlate** — cross-dataset key-overlap and time-alignment detection
   (e.g. the verify corpus's German `geraet` column was correlated with
   `device_id` in the SQLite fleet table).
8. **Catalog** — everything discovered is written to the `autoindex-catalog`
   index: datasets, per-field types/semantics/cardinality/examples, time
   ranges, correlations, junk files with reasons, and engine gotchas.
9. **Verify** — post-run count checks per dataset; resume journal marked
   finished.

## The data map: an agent's first move

`xerj autoindex map` renders the catalog as a briefing: one table of datasets
(index, record count, formats, time field, time range), a per-dataset field
table (type, semantic, cardinality, null%, real example values),
**ready-to-send example queries** per dataset, the junk report, and a
"Gotchas" section listing verified engine quirks (which hybrid-query form
actually works, the lexical-embedder honesty note, keyword-subfield and
multi-index-alias traps). `--json` emits the raw catalog documents for
programmatic use; agents can also just query `autoindex-catalog` directly.

In the agent evaluation below, the XERJ-backed agent's *first move in all 10
runs* was to read the catalog — it answered "what is in this folder?" with
exact per-dataset counts in 4 API calls, and used the catalog's precomputed
order-id containment correlation to correctly dedupe a CSV subset against its
SQL source.

## Evaluation results

Three independent evaluations were run against this exact worktree binary.
Full artifacts live in this directory; every number below traces to one of
them (see the [verified-claims table](#verified-claims)).

### 1. Ground-truth verification: 80/81 checks

A 518.5 MB / 1,995-file / 25-format corpus with a secret manifest (planted
tenants, users, devices, an incident, entities, five date encodings, junk
files). One zero-config run, then 81 itemized checks against the manifest:
**80 pass**. Highlights: exact planted counts for cross-source entities
(e.g. hot user u-1042: app 29,247 / events 78,616 / nginx 34,782 / sql_audit
4,453 / chat 666 / orders 7,799), all five date encodings typed as `date`
with correct monthly histogram peaks, decimal-comma CSV inferred `double`
with exact stats, JSON-as-`.pdf` magic-sniffed and indexed as JSON, Latin-1
note decoded correctly. The **one miss:** a Shift-JIS note was indexed as
mojibake instead of being decoded or recorded as junk.

A separate independently-built 201 MB "surprise" corpus (11 hostile files:
12-level-deep JSON, symlink loop, 0-byte `.pdf`, BOM'd Japanese+emoji CSV
headers, a 200 MB single JSONL file, tar.gz, mixed UTF-8/Latin-1 log,
disguised extensions) ran zero-config with no crash: 9 datasets, the 200 MB
file's 1,142,044 records counted exactly, both encodings of the mixed log
decoded, and a `kill -9` at 18 s mid-file followed by a re-run converged to
byte-identical counts across all 9 datasets.

### 2. Scale prover

- Largest corpus completing end-to-end: **923 MB / 2,601,752 records in
  77.3 s** (11.9 MB/s, 33.7k records/s for the *whole* pipeline, defaults,
  8 workers). Client CPU was only 13.9 s of that — throughput is
  server-bound, not extractor-bound.
- Sustained bulk rate on the 4.61 GB corpus before the server died: **53.7k
  records/s** (~34.6 MB/s of JSONL), zero bulk errors.
- **Client memory is bounded and flat:** peak client RSS 245–257 MB on
  4.61 GB, 160–168 MB on 923 MB — 5× more input moved the client high-water
  mark only ~1.5×.
- **Honest blocker:** the *server's* ingest heap is currently NOT bounded —
  it retains ~7–10 KB per doc write and was cgroup-OOM-killed at ~41.8 GB
  after ~4.25–5.1M docs on the 11.68M-record corpus (with *and* without
  semantic embedding — the embedder is not the driver). The client
  architecture is TB-ready; the server is not until this is fixed. Ticket
  with repro, evidence, suspects, and suggested fix shape:
  [`scale/TICKET_server-unbounded-ingest-heap.md`](scale/TICKET_server-unbounded-ingest-heap.md).
- **Resume proof:** the fully verified kill-and-converge result is from the
  surprise run — `kill -9` at 18 s mid-200 MB-file, re-run, byte-identical
  counts across all 9 datasets, no duplicates (idempotent `_id`s). A
  dedicated scale resume test (run C: 923 MB subset, fresh server + fresh
  `--state-dir`, harness `scale/resume_test.sh`) exercised the same journal
  path; client HWM during resume was 160 MB.

### 3. Agent simulation: XERJ-backed agent vs grep-only baseline

Same model (headless Claude Code, 40-turn budget, Bash only), 10 exam
questions with ground truth pre-recorded from the secret manifest *before*
any agent ran. Agent A got only the ES API of an autoindexed corpus (no
filesystem access); Agent B got the raw corpus and full local tooling
(grep/find/python3/sqlite3/zgrep). Full methodology, exact command lines,
guard scan, and transcripts: [`AGENT_SIM_SCORECARD.md`](AGENT_SIM_SCORECARD.md),
[`EXAM_QUESTIONS.md`](EXAM_QUESTIONS.md), [`sim-transcripts/`](sim-transcripts/).

**Honest headline: on a 518 MB local corpus the fair baseline is not beaten.**
Accuracy A 9 correct + 1 partial vs B 10/10; wall clock a wash (A median
73.5 s vs B 84.4 s; A faster on 7/10 questions but slower in total, 1,031 s
vs 843 s, largely because a live-verified engine defect — `bool` with only
`should` clauses silently returns 0 hits — cost A ~5 minutes of workarounds
on Q2/Q3).

| Q | Topic | A (XERJ) | A wall | B (grep) | B wall | Faster |
|---|---|---|---:|---|---:|---|
| Q1 | Folder inventory | correct | 62.1 s | correct | 84.2 s | A |
| Q2 | Sources containing u-1042 | correct | 260.7 s | correct | 83.9 s | B |
| Q3 | 2026-03-14 incident story | correct | 225.9 s | correct | 105.6 s | B |
| Q4 | Revenue per tenant (120k orders) | correct | 87.4 s | correct | 92.9 s | A |
| Q5 | Device cardinality across sources | correct | 69.0 s | correct | 105.5 s | A |
| Q6 | Event status distribution | correct | 58.9 s | correct | 47.2 s | B |
| Q7 | SQLite-only fleet fact | correct | 41.9 s | correct | 79.1 s | A |
| Q8 | Decimal-comma CSV max temp | correct | 39.8 s | correct | 43.4 s | A |
| Q9 | SQL-only tenants (negative-space join) | correct | 77.9 s | correct | 84.5 s | A |
| Q10 | Junk forensics (sprite-atlas.bin) | **partial** | 107.3 s | correct | 116.8 s | B |
| | **Totals** | **9 + 1 partial** | **1,031 s** | **10/10** | **843 s** | **A 7 / B 3** |

Where XERJ genuinely helped: instant zero-config orientation via the data
map; sub-second aggregations over millions of rows (~2× faster on the SQLite
fleet question); uniform access to format-hostile sources (SQLite,
decimal-comma CSV, gzip, DOCX appear as ordinary indices); exact per-source
hit counts on cross-source entity lookups. Where grep was fine or better:
narrative/lookup questions (ties, faster on 3), and strictly better at
byte-level forensics — the baseline *proved* the junk `.bin` was random noise
via entropy analysis, which no index abstraction can do.

**Scale caveat, stated plainly:** at ~500 MB local, grep/python is always in
play. The structural advantages (bounded-time aggregations, no per-format
tooling, remote-only access, repeated queries) need TB-scale or remote-only
scenarios to dominate rather than tie — and this run does not prove those.

## Verified claims

Every number in this README, mapped to the run that produced it.

| Claim | Value | Source run / artifact |
|---|---|---|
| Ground-truth accuracy | 80/81 itemized checks pass | Verify run on 518 MB manifest corpus, 2026-07-09; verdict recorded in evaluation report; questions/GT in `EXAM_QUESTIONS.md` lineage |
| Only verify miss | Shift-JIS note indexed as mojibake | Same verify run, P12 (4/5) |
| Junk never fatal; JSON-as-.pdf sniffed | recorded with reasons; indexed as JSON | Same verify run, P12 |
| 5 date encodings → typed `date` | ISO-ms, epoch-ms, nginx CLF, comma-ms, MySQL datetime, RFC-2822 | Same verify run, P9 (15/15 + 9/9) |
| Surprise-corpus robustness | 11 hostile files, no crash, 9 datasets; 200 MB JSONL = exactly 1,142,044 records | Independent surprise run, /tmp/xerj-autoindex/surprise (201 MB) |
| kill -9 resume converges | byte-identical counts across 9 datasets | Surprise run: kill -9 at 18 s mid-200MB-file + re-run |
| Resume at scale exercised | run C: 923 MB subset, fresh server/state, resume client HWM 160 MB | Scale prover run C (`scale/resume_test.sh`) |
| End-to-end throughput | 923 MB / 2,601,752 records / 77.3 s (33.7k rec/s) | Scale run A (defaults, 8 workers) |
| Sustained bulk rate | 53.7k rec/s (3,005,314 docs / 56 s), zero bulk errors | Scale run 2 (4.61 GB corpus, `--no-semantic`) |
| Client RSS bounded | peak 245–257 MB on 4.61 GB; 160–168 MB on 923 MB | Scale runs 1/2/A/C, /proc RSS polled every 2 s |
| Server heap NOT bounded | OOM at ~41.8 GB anon-RSS after ~4.25–5.1M docs; ~7–10 KB/doc; idle 17.5 GB vs 1.4 GB on disk | Scale runs 1+2 under 40G cgroup; `scale/TICKET_server-unbounded-ingest-heap.md` |
| Agent-sim accuracy/speed | A 9+1 partial vs B 10/10; medians 73.5 s vs 84.4 s; totals 1,031 s vs 843 s | `AGENT_SIM_SCORECARD.md` + `sim-transcripts/` (20 transcripts, guard-scanned) |
| Sim indexing run | 518 MB / 1,995 files → 31 datasets, 2,018,398 records, 38.1 s, exit 3 | `AGENT_SIM_SCORECARD.md` header |
| bool should-only = 0 hits defect | reproduced with 5-query minimal case | `AGENT_SIM_SCORECARD.md` §Engine defect (live-verified during exam) |
| Quickstart demo output | 4 files → 3 datasets, 5,801 records, 0.2 s, exit 3; rerun resumes with 0 files to index, counts identical | Live docs-demo run 2026-07-09, /tmp/xerj-autoindex/docs-demo (autoindex-run.txt, rerun-output.txt, map-output.txt, q*.json) |

## Honest limitations

- **The built-in embedder is LEXICAL.** `semantic_text` auto-embeds into
  384-dim hash-bucket vectors — this gives **hybrid lexical+vector**
  retrieval, *not* neural semantic understanding (proven: a cooking query
  once ranked a PostgreSQL doc first). Do not expect synonym-level recall
  from the default embedder; the `map` output says so in its Gotchas section.
- **Server-side ingest heap is unbounded** (~7–10 KB retained per doc
  write). Corpora beyond a few million docs will OOM the server until the
  ticketed cache-budget fix lands. TB-scale is a client-architecture claim
  today, not an end-to-end one.
- **`bool` should-only queries return 0 hits** on this engine (ES would
  match). Use `must`/`filter` or standalone clauses. Live-verified during
  the agent exam; compat ticket candidate.
- **Shift-JIS (and likely other non-UTF-8/non-Latin-1 legacy encodings)**
  can be indexed as mojibake rather than decoded or quarantined — the one
  verify-run miss.
- **Exotic CSV headers may become positional names**: a UTF-8-BOM CSV with
  Japanese+emoji headers was typed correctly (double temperature, date
  column) but its headers became `field`, `field_2`, …
- **Archives are not extracted**: a `tar.gz` bundle is recorded as junk with
  a reason, not unpacked.
- **Byte-level forensics is out of scope**: the index tells you a file is
  binary junk and why it was skipped; proving it is random noise (entropy
  analysis) needs the raw bytes.
- **Semantic queries ignore `_source` filtering** on this engine: hits carry
  the full ~8 KB vector field; strip client-side.
- At **local, sub-GB scale**, a capable agent with grep/python ties this
  system on accuracy. The wins are orientation speed, aggregations, hostile
  formats, and remote access — not raw question-answering power.

## Files in this directory

| Path | What |
|---|---|
| `README.md` | This document |
| `RECIPE_DRAFT.md` | Draft recipe in `docs/recipes/` voice (live-captured demo run) |
| `AGENT_SIM_SCORECARD.md` | Full agent-vs-baseline evaluation: methodology, per-question table, defect report, fairness guard |
| `EXAM_QUESTIONS.md` | The 10 exam questions + pre-recorded ground truth |
| `sim-transcripts/` | All 20 agent transcripts (stream-json) + extracted metrics + ground-truth computation output |
| `run-eval.sh` | Evaluation runner |
| `scale/` | Scale-prover harness (`gen_corpus.py`, `run_with_rss.sh`, `resume_test.sh`, `analyze_run.py`, `collect_counts.sh`) and the server-heap ticket |
