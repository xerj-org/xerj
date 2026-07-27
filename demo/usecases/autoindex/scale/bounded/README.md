# Bounded shipped-path ingest diagnostics

This suite replaces expensive full-corpus debugging loops with a small,
deterministic experiment through XERJ's real ES-compatible HTTP path. It proves
bulk ingest, refresh, explicit flush, force-merge, graceful shutdown, restart
recovery, exact counts, and three sentinel lookups while collecting two
independent telemetry streams. The `body` mapping is `semantic_text`, so the
default `--embed-mode lexical` exercises XERJ's deterministic feature-hash
embedding lifecycle; it does not claim neural semantics.

- `xerj.ingest_memory.v1`: XERJ's opt-in logical ownership, jemalloc, RSS, and
  process CPU sampler (`XERJ_INGEST_MEMORY_TRACE=summary`);
- `xerj.process_sample.v1`: external Linux `/proc` status, `smaps_rollup`, I/O,
  thread, and file-descriptor observations.

This is a debugging instrument, not the FinanceBench acceptance benchmark. It
should expose memory-shape defects in seconds or minutes; a candidate still
needs the real correctness and performance benches before publication.

## Run it

Build only the server crate. Fat LTO is useful for release artifacts, but is a
poor default for edit/profile loops because it makes linking slow and obscures
rapid iteration. Use the repository's development profile or a dedicated
profiling profile for experiments; re-run accepted measurements with the exact
release profile used by the compared baseline.

```bash
cd engine
CARGO_PROFILE_RELEASE_LTO=off cargo build --release -j 32 -p xerj-server

cd ..
SUITE=demo/usecases/autoindex/scale/bounded
python3 "$SUITE/generate_corpus.py" \
  --output /tmp/xerj-bounded-corpus \
  --documents 4096
python3 "$SUITE/run_suite.py" \
  --binary engine/target/release/xerj \
  --corpus /tmp/xerj-bounded-corpus \
  --output /tmp/xerj-bounded-run \
  --profile nightly
python3 "$SUITE/analyze.py" /tmp/xerj-bounded-run
```

`run_suite.py` refuses to overwrite an output directory. Every cell uses a
fresh data directory and server process. The restart phase reuses only that
cell's durable data. The manifest records the binary SHA-256 and size, Git
commit and dirty state, corpus hash, host, configuration, correctness results,
and hashes of every raw artifact.

The analyzer fails on:

- malformed or wrong-schema telemetry;
- a missing start/stop event or non-contiguous trace sequence;
- dropped events or retain/release accounting errors;
- nonzero measured ownership at process stop;
- artifact hash/size drift;
- count or sentinel-search corruption after refresh, merge, or restart.

It also reports incremental RSS slopes and peak RSS per document. Those
relations deliberately have no arbitrary absolute threshold. Small cells have
fixed startup cost and host noise, so an accelerating slope is an investigation
signal rather than a release failure.

Once repeated pinned-host runs establish an envelope, write a versioned budget
and check later runs against it:

```bash
python3 "$SUITE/analyze.py" /tmp/xerj-bounded-run \
  --write-baseline-budget demo/usecases/autoindex/scale/bounded/budget.json \
  --budget-headroom 1.05
python3 "$SUITE/analyze.py" /tmp/xerj-candidate-run \
  --budget demo/usecases/autoindex/scale/bounded/budget.json
```

The multiplier is explicit because the repository must choose it from repeated
host variance; the suite does not invent one. A budget can cap per-cell ingest
and restart RSS plus RSS-per-added-document slopes. Do not commit a budget
generated from a single run.

## CI and nightly profiles

- **CI smoke (`--profile ci`):** generate 256 documents and run `64,256`. This validates the
  harness and shipped path; it is not a performance assertion.
- **Nightly diagnostics (`--profile nightly`):** run `256,1024,4096` at least three times on a pinned,
  quiet host. Keep all raw runs and compare medians; do not average away a
  correctness or telemetry-integrity failure.
- **CPU/heap profiles:** use the opt-in profiling build and capture profiles
  alongside this suite. Record their hashes in the run directory. Profiling
  and non-profiling timings are separate populations; never compare one as the
  performance baseline of the other.

## Prompts for an AI reviewer

The artifacts are intentionally JSON/NDJSON plus a short Markdown report, so an
agent can inspect them without a proprietary UI.

> Read `run_manifest.json`, `analysis.json`, and every trace named by the
> manifest. Verify hashes, correctness after refresh/merge/restart, sequence
> continuity, dropped/accounting errors, and stop-time ownership balance.
> Identify the first phase where RSS separates from jemalloc allocated and from
> logical attribution. Do not infer unavailable categories are zero.

> Compare two bounded-run directories produced from the same corpus and cell
> sizes. Confirm binary hashes and build/config differences first. For each
> cell, compare peak and final RSS, allocator allocated/active/resident, logical
> category peaks, write bytes, wall time, and restart RSS. Flag faster-but-wrong
> or incomplete telemetry as a failed comparison.

> Inspect the `/proc` and ingest-memory time series for one cell. Find the
> timestamp and runner phase of peak RSS. Explain whether growth is live
> attributed memory, allocator fragmentation/retention, page cache/file-backed
> RSS, or unattributed memory. State uncertainty and name the next CPU or heap
> profile needed to resolve it.
