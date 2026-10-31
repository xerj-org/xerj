# ingest-neural-bulk

A/B harness for neural-embedding `_bulk` ingest. It answers one question: does
a change to the server make the same `xerj autoindex` run faster, slower, or
noisier, with neural embeddings on and the CPU held fixed?

## Run

```
scripts/bench/neural-bulk-ab.sh [--dry-run] CONTROL_SHA CANDIDATE_SHA [N=5]
```

`--dry-run` prints every command and writes nothing. Without it the script:

1. Verifies `corpus/okta-bulk.tar.zst` against `corpus/SHA256SUMS`, extracts
   it, and verifies each extracted file against `corpus/MANIFEST.tsv`.
2. Builds each SHA once, in its own `git worktree`, with
   `cargo build --release -j 2 -p xerj-server`. The same SHA twice builds once.
   `CONTROL_BIN` / `CANDIDATE_BIN` skip a build.
3. Runs control, candidate, control, candidate, ... N times each. Every run
   gets a fresh data dir and a fresh client state dir.
4. Starts the server on the first free port from 9560 (never 9200) under
   `systemd-run --user --scope -p CPUQuota=400% -p MemoryMax=16G`, falling
   back to `taskset -c 0-3` with a warning. Env: `XERJ_NUM_CPUS=4`,
   `XERJ_PROF=1`, `XERJ_EMBED_MODE=neural`, `HF_HOME` (default
   `~/.cache/huggingface`; warm it first or run 1 includes the model download).
5. Drives it with
   `xerj autoindex <corpus> --prefix bench --bulk-mb 8 --workers 4 --progress json --approve proceed`.
6. Stops the server on exit, including on Ctrl-C, and removes the arm worktrees.

Output goes to `benchmarks/ingest-neural-bulk/results/<date>/`: `meta.json`,
`report.md`, and per run `run-<arm>-<i>/{server.log,client.log,summary.json}`.
Set `RESULTS_DATE` to avoid clobbering an earlier run on the same day.

## Control-vs-control precondition

Before any candidate number is published, run the harness with the same SHA
for both arms. The two arms' median `bulk total_ms p95` must differ by less
than 10%. `report.md` prints PASS or FAIL for this. If it fails, the host is
too noisy for the effect being measured.

## Corpus

`corpus/okta-bulk.tar.zst` is 203 files, 11,257,358 bytes raw, 461 KB
compressed, stored directly in git (the repo has no `.gitattributes` and no
binary-size policy). It holds the three Okta Management OpenAPI YAML copies
(golang, nodejs, python SDKs) and the first 200 `okta-sdk-golang/okta/docs`
files by sorted name. `MANIFEST.tsv` gives upstream repo, commit, source path,
in-tarball path, bytes and sha256 for each file. The golang spec lives under
`.generator/` upstream; `autoindex` skips hidden directories, so it is stored
as `generator/` and `source_path` records the original.

## Columns in `summary.json` and `report.md`

| field | source |
|---|---|
| `wall_s` | `wall_s` in the client's `xerj-done` JSON object (plain `wall=` as fallback) |
| `wall_wrapper_s` | shell-measured client wall time, for cross-checking |
| `records` | `records` in the same `xerj-done` object |
| `records_per_s` | `records / wall_s` |
| `bulk_total_ms_p50`, `_p95` | nearest-rank percentiles of `total_ms=` on server `XERJ_PROF bulk` lines (`engine/crates/xerj-engine/src/bulk.rs`) |
| `embed_ms_p50`, `_p95` | `embed_ms=` anywhere on a server `XERJ_PROF` line. Null on builds that do not emit it. |
| `retries_window_failed` | server log count of `semantic embedding window failed; retrying per item` |
| `retries_embed` | server log count of `embed retry` |
| `client_bulk_retries` | `bulk_retries` in the `xerj-done` object (items re-sent after a 429); absent means 0 |
| `retries` | sum of the three above |
| `http_429` | log lines in server and client logs containing the whole word `429`. The server does not log requests, so this is indicative; `client_bulk_retries` is the authoritative backpressure count. |
| `peak_rss_kb` | `VmHWM` from `/proc/<server pid>/status`, read after the client exits and before the server stops |
| `cpu_s` | `utime + stime` from `/proc/<server pid>/stat`, divided by `CLK_TCK` |
| `ok` | client exit 0 and `xerj-done` reported `ok=true`. Failed runs are listed in the report and excluded from statistics. |

`report.md` shows, per metric, each arm's median and p95 across ok runs (with
N below 20 the nearest-rank p95 is the max), the control arm's max-min spread,
and the candidate-minus-control median delta. A delta is marked `EXCEEDS
SPREAD` only when its magnitude is larger than the control spread. A spread of
zero (for example N=1) flags any difference, so read it with N of at least 5.

## Not covered

- The client is not confined to the CPU quota; only the server is.
- `passages/s` is not computed: the server does not report a passage count
  that the harness can read.
- Throughput is end-to-end with a cold index, per `docs/CONTRIBUTION_REVIEW.md`
  matched-build guidance. Do not compare it with warm or partial-corpus numbers.
