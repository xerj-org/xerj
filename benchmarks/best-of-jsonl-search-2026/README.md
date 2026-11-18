# Full-text search over one JSONL file: ripgrep, SQLite FTS5, DuckDB FTS, XERJ

Run on 2026-10-19 (America/Los_Angeles) for the page
[Best local full-text search for JSONL and CSV files (2026)](https://xerj.org/compare/best-local-full-text-search-jsonl-csv-2026).
Every number on that page that comes from this directory is reproducible with the
three scripts here.

## Method

- **Corpus**: `gen_corpus.py` writes deterministic synthetic log records (seed
  20261019) with fields `ts`, `level`, `service`, `user`, `msg`. Planted terms live
  only in `msg`, so a line grep and a field query count the same records.
  - 50,000 records, 9,724,646 bytes (`python3 gen_corpus.py logs-50k.jsonl 50000`)
  - 300,000 records, 58,303,466 bytes (`python3 gen_corpus.py logs.jsonl`)
- **Queries**: a rare term (`kestrel`, ~0.1 %), a common term (`timeout`, ~5 %) and a
  phrase (`disk quota exceeded`, ~0.5 %), each counted, not fetched.
- **Index time**: wall clock from the raw file to a queryable index.
  ripgrep has no index. SQLite: Python `json` + `executemany` into an FTS5 table.
  DuckDB: `read_json_auto` + `PRAGMA create_fts_index` on `msg`. XERJ: `xerj autoindex
  --no-graph` against a fresh node (`--insecure`, default settings).
- **Query latency**: median of 20 runs after 3 warm-up runs. ripgrep is a process
  spawn per query; SQLite and DuckDB are in-process calls; XERJ is an HTTP `_count`
  over loopback, so its figure includes the HTTP round trip.
- **DuckDB phrase**: `match_bm25` has no phrase operator, so the phrase row uses
  `conjunctive := 1` (all three terms present). On this corpus that returns the same
  count as the phrase.
- **Host**: Linux 6.12 x86-64, 8 CPUs, 16 GB RAM, shared with other processes
  (another XERJ node was running). Treat absolute times as one machine's, not a
  ranking you can transfer.
- **Versions**: ripgrep 14.1.1, SQLite 3.46.1 (Python `sqlite3`), DuckDB 1.5.6 with
  the `fts` extension, xerj v1.0.0-rc.97.

Run: `CORPUS=logs-50k.jsonl XERJ=/path/to/xerj python3 run_bench.py` (DuckDB runs in a
subprocess, `duck.py`; set `DUCKDB_PYTHON` to a Python that has `duckdb` installed).

## Results, 50,000 records (`results-50k-2026-10-19.json`)

| Tool | Index time | Index size | Rare term | Common term | Phrase | Hits (rare / common / phrase) |
|---|---|---|---|---|---|---|
| ripgrep 14.1.1 | none | none | 3.27 ms | 3.38 ms | 2.90 ms | 51 / 2,485 / 253 |
| SQLite FTS5 3.46.1 | 0.29 s | 13.8 MB | 0.02 ms | 0.08 ms | 0.05 ms | 51 / 2,485 / 253 |
| DuckDB FTS 1.5.6 | 0.61 s | 3.7 MB | 7.25 ms | 9.19 ms | 8.13 ms | 51 / 2,485 / 253 |
| xerj v1.0.0-rc.97 | 70.57 s | 112.3 MB (data dir) | 0.35 ms | 0.31 ms | 0.31 ms | 51 / 2,485 / 253 |

All four tools returned identical counts. XERJ's node peaked at 9,578,548 kB resident
(VmHWM) while indexing 9.7 MB of JSONL; `autoindex-50k.stderr.txt` shows the time
went to the index phase (~42 s) and finalize (catalog ~11 s, verify ~16 s).

## Results, 300,000 records (`results-300k-2026-10-19.json`, `oom-300k-dmesg.txt`)

ripgrep (11.95 / 12.25 / 8.79 ms), SQLite FTS5 (1.84 s to index, 74.4 MB; 0.03 /
0.42 / 0.21 ms) and DuckDB FTS (2.24 s, 15.5 MB; 11.08 / 18.94 / 12.76 ms) all
returned 338 / 15,061 / 1,484. **XERJ did not finish**: about 13 minutes in (279,429 documents were
visible at the 11-minute mark and the journal records the upsert as committed), the
kernel OOM-killed the node at 9,972,316 kB anonymous RSS. That is a XERJ memory problem on this workload,
published as measured. (The first XERJ attempt in `run.log` indexed nothing because
the corpus was a symlink, which autoindex does not follow; the script now hard-links.)
