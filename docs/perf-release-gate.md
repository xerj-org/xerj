# Autoindex / search release performance gate

`scripts/perf/autoindex_release_gate.sh` blocks a build that repeats the
rc.99 field failures: #2421 (state dir 1.6x source), #2422 (454 indices,
13–23 s searches), #2423 (0.2 MB/s, OOM), #2424 (planning memory, long
names), #2427 (node over its own memory cap) and #2428 (client EAGAIN abort).
It runs the user-facing path end to end, `xerj autoindex <folder>` with
default flags, against a deterministic synthetic corpus on a throwaway node
with a fixed memory cap. It then measures uncached search on all three
surfaces and fails on any budget miss.

## Run it locally

```bash
(cd engine && cargo build --release --locked -p xerj-server)
scripts/perf/autoindex_release_gate.sh --bin engine/target/release/xerj            # 1 GB tier
scripts/perf/autoindex_release_gate.sh --bin engine/target/release/xerj --tier smoke
scripts/perf/autoindex_release_gate.sh --bin engine/target/release/xerj \
    --baseline ~/xerj-prev/xerj                                             # +regression checks
scripts/perf/autoindex_release_gate.sh --bin engine/target/release/xerj \
    --engine-baseline es-baseline.json                                      # +ES/Lucene floor
```

| flag | meaning |
|---|---|
| `--tier smoke\|1gb\|5gb` | budget tier in `scripts/perf/budgets.toml` (sets corpus size, memory cap, timeout) |
| `--size-mb N` | override corpus size (budgets stay those of the tier) |
| `--seed N` | corpus seed (default 2421) |
| `--work DIR` | scratch: corpus + per-run data/state (default `$RUNNER_TEMP` or `/tmp`/xerj-perf-gate) |
| `--corpus-dir DIR` | reuse or put the corpus here (reused if its manifest matches size + seed) |
| `--out DIR` | results (`candidate.json`, `baseline.json`, `results.json`, `summary.md`, log tails) |
| `--baseline BIN` | also run the previous release binary on the same corpus; fail on >20% regression |
| `--engine-baseline FILE` | ES/Lucene floor JSON (from `es_baseline.py`); enables `[ratio.*]` budgets |
| `--autoindex-args "..."` | extra autoindex flags (default: none, the user-facing path) |
| `--keep` | keep run data/state dirs |
| `MIN_FREE_GB` env | disk reserve the guard keeps free (default 8) |

Exit 0 means every budget was met. Exit 1 means at least one was missed, and the markdown table is
printed and written to `summary.md` (and appended to `$GITHUB_STEP_SUMMARY`
in Actions). Exit 2 means a usage or environment error, e.g. not enough disk. Requires
Python ≥ 3.11 (stdlib only). On a 4-core machine a passing build finishes the
1 GB tier in under 20 minutes: corpus about 25 s, autoindex ≤ 15 min by budget, restart
plus 60 queries about 1 min.

## What a run does

1. `gen_corpus.py` writes a byte-for-byte deterministic corpus plus
   `<corpus>.queries.json` (20 queries per surface, 60 distinct strings
   drawn from phrases planted in the corpus, so each should hit) and
   `<corpus>.manifest.json`. The mix per GB is code (rs/go/py/ts, about 24%),
   markdown (12%), HTML with tables (8%), 3 large plus about 800 small
   distinct-schema CSVs, 24 JSONL, 16 logs, 4 big JSON flight-trace files with numeric
   arrays (15%), 1500 multi-array JSON/YAML, 8000 tiny files and deep Go-module
   style paths up to about 500 chars.
2. `gate.py run` starts `xerj --insecure --data-dir … --port … --disable-feedback`
   with `XERJ_MAX_PROCESS_MEMORY_MB=<tier cap>` and `XERJ_DISABLE_FEEDBACK=true`,
   then runs `xerj autoindex <corpus> --approve proceed --json` under the tier
   timeout. It samples node and client process-tree RSS and threads from `/proc`
   every 0.5 s.
3. It reads `_cat/indices/<prefix>-*`, stops the node, scans the node log for
   parent-breaker trips (`engaging the parent memory circuit breaker`), fatal
   patterns (EAGAIN / os error 11, OOM, SIGABRT/panic, EMFILE) and retry/429
   lines, and measures the state dir and data dir sizes.
4. It restarts the node (timed) so the page cache and query caches are cold,
   then runs 20 queries through `xerj search --json`, 20 through raw
   `POST /<prefix>-*/_search?request_cache=false` and 20 through `xerj mcp`
   (`tools/call xerj_search`). No query string repeats.
5. `gate.py eval` compares against `budgets.toml`.

## Budgets

See `scripts/perf/budgets.toml` for the numbers and the reasoning behind each one.

| check | smoke (200 MB) | 1gb | 5gb (release) | why |
|---|---|---|---|---|
| autoindex completes (exit 0/3, node alive, no OOM/abort/EAGAIN, within timeout) | 6 min | 15 min | 30 min | #2423, #2427, #2428 |
| throughput MB/s ≥ (absolute backstop) | 1.5 | 3.0 | 3.0 | about 0.5x the low end of ES on a 4-vCPU runner; rc.99 did 0.2–0.6 |
| node peak RSS ≤ memory cap (MB) | 2048 | 3072 | 3072 | #2427: rc.99 hit 3.9 GB (autoindex) / 5.6 GB (`_bulk`) under a 3 GB cap |
| client peak RSS (MB) ≤ | 768 | 1024 | 1536 | #2424, #2428 |
| state dir / source ≤ | 1.2 | 1.2 | 1.2 | #2421: 8.4 GB state for 5 GB |
| index + state on disk ≤ (MB) | 250 | 1300 | 6400 | about 1.2x source, stricter than ES force-merged (1.42x) |
| indices per prefix ≤ | 40 | 40 | 40 | #2422: 454; #2430 caps a run at 32 |
| uncached search p95 / max (ms) | 1000 / 2000 | 1000 / 2000 | 1000 / 2000 | #2422: 13–23 s |
| search errors | 0 | 0 | 0 | |
| queries with hits ≥ | 90% | 90% | 90% | catches "fast because it found nothing" |
| parent-breaker trips | 0 | 0 | 0 | #2427 |
| bulk retries / 429s ≤ | 20 | 20 | 50 | #2427 |
| node restart (s) ≤ | 60 | 60 | 120 | |
| regression vs `--baseline` binary | 20% | 20% | 20% | throughput, p95, node/client RSS, state ratio, index bytes |
| index bytes vs ES (force-merged) ≤ | 1.25x | 1.0x | 1.0x | ES floor (`[ratio.*]`) |
| ingest MB/s vs ES (default mapping) ≥ | 0.3x | 0.5x | 0.5x | ES floor; stretch goal 0.8x |
| search p95 vs ES ≤ | — | 1.5x | 1.5x | ES floor |

### Lucene / Elasticsearch floor

Absolute budgets drift with runner hardware, so index size and indexing speed are
also held against Elasticsearch on the same corpus and the same machine.
`scripts/perf/es_baseline.py` bulk-loads the corpus into a throwaway ES. It writes
one doc per line for jsonl/csv/log and one doc per file otherwise, using the default
dynamic mapping, 1 shard and 0 replicas. Ingest time runs until the index is refreshed
and flushed. It then **force-merges to one segment**, and `index_bytes` is the merged
size, because the unmerged size swings with the segment count (1.77x vs 1.42x source).
Finally it runs the same 60 queries uncached and writes:

```json
{"engine": "elasticsearch-9.x (lucene 10.x)", "corpus_mb": 1024, "seed": 2421,
 "index_bytes": 0, "ingest_mb_s": 0.0, "search_p95_ms": 0.0}
```

Pass that file as `--engine-baseline` and the `[ratio.<tier>]` keys apply:
`xerj_index_bytes ≤ k·es_index_bytes`, `xerj_ingest_MBps ≥ k·es_ingest_MBps`,
and `xerj_p95 ≤ k·es_p95`. In CI, ES runs on the same runner, so the floor scales with
the runner automatically. ES always runs **before** xerj and is stopped before xerj
starts, never at the same time: ES uses about 5.5 GB RSS with a 4 GB heap, and xerj has a 3 GB cap.
The last good baseline is cached and used as a fallback when the live ES step fails.

Reference floor, 1 GB corpus with seed 2421, 4 pinned cores on a heavily loaded shared box,
single noisy run (perf-floor `results.md`, 2026-10-25):

| engine | MiB/s | index bytes / source | peak RSS MB |
|---|---|---|---|
| ES 9.5.5 default dynamic mapping, 4 GB heap (refreshed, 28 segments) | 9.05 | 1.77 | 5,551 |
| same, after force-merge to 1 segment (+48.7 s) | | 1.42 | |
| Lucene 10.5.1 StandardAnalyzer, stored fields on, 4 threads | 25.4 | 0.78 | 3,036 |
| Lucene 10.5.1 StandardAnalyzer, stored fields off, 4 threads | 36.0 | 0.30 | 2,518 |
| xerj rc.99 `_bulk` text-only (cap 3072 MB; died at about 900 s) | 0.37 | n/a | 5,643 |
| xerj rc.99 autoindex (stopped at 38% after 661 s) | 0.59 | about 0.92 (est.) | about 2,960 |

So rc.99 runs at about 0.04–0.07x of ES and misses the 0.5x floor by an order of magnitude.

Runner sizing: public-repo `ubuntu-latest` has 4 vCPU, 16 GB and a 14 GB SSD, which is what the budgets
assume. Private repos get 2 vCPU and 8 GB there, so use the `smoke` tier with a 2 GB ES heap,
or a larger runner.

```bash
python3 scripts/perf/gen_corpus.py --out /tmp/c1g --size-mb 1024 --seed 2421
docker run -d --name es -p 9200:9200 -e discovery.type=single-node \
  -e xpack.security.enabled=false -e ES_JAVA_OPTS="-Xms4g -Xmx4g" \
  docker.elastic.co/elasticsearch/elasticsearch:9.5.5
python3 scripts/perf/es_baseline.py --es http://127.0.0.1:9200 --corpus /tmp/c1g \
  --queries /tmp/c1g.queries.json --out es-baseline.json
```

## CI

The harness is implemented, but `.github/workflows/perf-gate.yml` is not
present and the publication job in `release.yml` depends only on `build`.
A successful release build therefore does **not** establish that these
performance budgets passed. Workflow wiring remains open in
[#2433](https://github.com/xerj-org/xerj/issues/2433), alongside the remaining
memory failures. The YAML in that issue describes the intended PR, nightly
and release jobs; it is not a workflow currently running in this repository.

Use the local commands above for measurements until that integration lands.
Keep the raw `candidate.json`, any baseline JSON, `results.json`, and node and
client logs with the verdict. Do not loosen budgets or report an incomplete
run as a passing release gate.

## Changing a budget

Edit `scripts/perf/budgets.toml` in the same PR as the change that needs it,
with a comment explaining why. Loosening a budget needs a linked issue.
