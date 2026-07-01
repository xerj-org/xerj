# XERJ Master Plan — "All Verified, All Beating ES"

Goal: reach a state where **every** performance and functional dimension
vs Elasticsearch 8.13.4 is (a) **measured** by a repeatable, CI-gated
harness and (b) **won by XERJ** — no untested cells, no ES-wins cells.

Definition of done = the Scorecard below is 100% GREEN, reproduced on
demand by one command, and regressions fail CI.

Guardrails held at **every** step (non-negotiable):
- ES-YAML conformance **1326 passed / 0 failed** (`scratchpad/conformance.sh`).
- Read p50 never regresses vs the current 1.3–2.2× win.
- Delicate hot-path engine changes → isolated worktree agent + hard gate.
- Measure-before-commit: no change lands without a before/after number.

---

## 0. Scorecard (the target — every cell must go GREEN)

Legend: 🟢 XERJ wins, measured & CI-gated · 🟡 XERJ wins but not yet
CI-gated/repeatable · 🔴 ES wins · ⚪ not yet measured.

### Performance
| Dimension | Today | Target |
|---|---|---|
| Ingest, single client, 1M | 🔴 0.85–0.94× | 🟢 ≥1.1× |
| Ingest, 8 clients, 1M | ⚪ | 🟢 ≥1.1× |
| Ingest, 1 & 8 clients, 10M / 100M (scale curve) | ⚪ | 🟢 ≥1.0×, flat/rising |
| Read p50, all query families, steady state | 🟡 (11 ops) | 🟢 (full coverage) |
| Read p95/p99, steady state | 🟡 | 🟢 |
| Read p99 / max **during heavy ingest** | 🔴 (multi-sec) | 🟢 ≤ ES |
| Mixed read+write sustained QPS | ⚪ | 🟢 |
| kNN/vector latency & recall at scale (1M×768d) | ⚪ | 🟢 |
| Update/delete-heavy throughput+latency | ⚪ | 🟢 |
| Disk footprint (index size) | ⚪ | 🟢 ≤ ES |
| RSS under sustained load | ⚪ | 🟢 |
| Cold-start + crash-recovery time | 🟡 (structural) | 🟢 measured |

### Functional
| Dimension | Today | Target |
|---|---|---|
| ES-YAML REST conformance | 🟢 1326/0 (+3 skipped) | 🟢 maintain, un-skip the 3 |
| `PARITY_BACKLOG.md` correctness defects | 🔴 N open | 🟢 0 open |
| Coverage beyond the YAML subset | 🟡 partial | 🟢 expanded + gated |

---

## Phase 0 — Verification harness (do FIRST; unblocks everything)

You cannot "beat ES everywhere" without measuring everywhere, repeatably.
Today's numbers came from ad-hoc scripts + a single-client bench.

**0.1 One-command ES reproduction.** Script the download/config/boot of ES
8.13.4 on :9201 (already documented in `es-vs-xerj-benchmark` memory) into
`scratchpad/es_up.sh` / `es_down.sh` (idempotent, cached tarball).

**0.2 Benchmark matrix runner** — `demo/playbooks/bench-matrix.mjs`,
generalising `bench-vs-es.mjs`:
- Params: `--clients 1,8,32`, `--docs 100k,1M,10M,100M`, `--ops <family>`.
- Ingest: true concurrent clients (worker_threads / parallel curl), report
  docs/s + the with-scale slope + CPU width + RSS + on-disk index size.
- Reads: **all query families**, not just 11 — match/term/bool/range/
  prefix/wildcard/fuzzy/regexp/exists/ids/ query_string/simple_query_string/
  geo_distance/ nested/ match_phrase(_prefix); aggs: terms/date_histogram/
  histogram/range/stats/percentiles/cardinality/composite/filter/missing/
  nested; plus sort-heavy, deep pagination (from+size, search_after), scroll,
  highlight, `_msearch`, `_mget`. Capture p50/p95/p99/max.
- **Mixed mode** (`--mixed`): background writers + foreground readers →
  the during-ingest tail (formalise `scratchpad/sustained_mixed_probe.sh`).
- kNN mode: 1M × {128,768} dims, measure latency **and recall@k vs exact**.
- Resources: RSS (`/proc/<pid>/status`), disk (`du` of data dir), cold-start
  (boot→first-200) and crash-recovery (kill-9→count-restored) timers.

**0.3 Scorecard generator.** Runner writes `demo/playbooks/SCORECARD.md`:
one row per cell, XERJ vs ES value, ratio, 🟢/🔴 verdict. Any 🔴 → non-zero
exit.

**0.4 CI gate.** GitHub Action (or `demo/playbooks/ci-check.sh` extension)
runs conformance + a fast scorecard subset on PRs; nightly runs the full
10M/100M matrix. A regression (new 🔴, or read p50 regress, or conformance
< 1326) fails the build.

Deliverables: `bench-matrix.mjs`, `SCORECARD.md`, `es_up.sh/es_down.sh`, CI job.
Effort: M. Risk: none (measurement only). **This turns every 🟡/⚪ into a
tracked number.**

---

## Phase 1 — Beat ES on single-client ingest (0.9× → ≥1.1×)

Close the last 6–15%. Root the work in a flamegraph of one `_bulk` (perf/
`cargo flamegraph`) to rank the remaining serial cost.

**1.1 P2.2 — no-reparse flush.** `do_flush_shard` re-parses every doc
(`index.rs:6685`) and `encode_stored_v2` parses a third time. Thread the
already-parsed `Value` + ingest tokens (from P2.1) through the memtable
drain into flush; add `encode_stored_v2_from_values`. Cuts flush CPU 2–3×
→ shorter flush → higher sustained ingest. **Byte-identical segment parity
assert** behind a debug flag. Effort: L. Risk: med (worktree agent).

**1.2 WAL append hot path.** Profile `wal_append_batch`; ensure per-shard
frame build is parallel and lock hold is minimal; confirm `soft_flush`
(no fsync on hot path) under the bench config.

**1.3 Allocation trim.** Remove residual per-doc clones in the P2.1 insert
(id clones, HashMap re-hash); reuse buffers across batch.

Verify: `bench-matrix --clients 1` ⇒ ingest ≥1.1× ES; conformance 1326/0.

---

## Phase 2 — Beat ES on the during-ingest tail (the one confirmed 🔴)

Proven this session: NOT merge CPU (P3.3 A/B was flat). It is
foreground/background contention on the **shared global rayon pool** and
**flush-drain lock**.

**2.1 Search-pool isolation.** Give foreground search/agg parallel scans a
dedicated rayon pool (`install()`), so background flush FTS build
(`index.rs:6782`), merge, and P2.1 ingest parse can't starve an
aggregation mid-request. Contained, no correctness risk. **Do first — most
likely root cause; cheap to A/B in `--mixed`.**

**2.2 P3.1 — freeze-and-swap flush.** Never drain the shard being actively
written: atomically swap in a fresh empty memtable, hand the frozen buffer
to the background flush, keep the frozen buffer query-visible (NRT) until
the segment lands. Trigger flush once per batch, not per doc. Medium risk
(read-after-write visibility) → worktree agent, hard conformance gate,
**new NRT-visibility test** (write→immediately search during flush must
see the doc). Effort: M-L.

**2.3 P3.4 — fsync folding/deferral.** Group `.seg`/`.sidx`/`.ids` fsyncs;
decouple visibility (refresh) from durability (commit) with WAL covering
the window. Recovery test required.

Verify: `bench-matrix --mixed` ⇒ read p99/max during heavy ingest ≤ ES;
crash-recovery still green.

---

## Phase 3 — Prove & win multi-client + scale (fill the biggest ⚪)

XERJ's sharded WAL/memtable should *win* here — it's the strongest
unproven case.

**3.1** 8- and 32-client ingest at 1M/10M. If XERJ doesn't win, tune
`ingest_shards` (default 16→32 on 32c), lock granularity, accept-loop.

**3.2** 10M and 100M single- & multi-client. Confirm the P2.3 flat curve
holds and reads stay 🟢 as segment count grows (may need P3.5 FST/bit-packed
doc-values to keep segments small + merges cheap).

**3.3** Sustained mixed QPS ceiling (readers + writers) vs ES.

Verify: every scale/concurrency cell 🟢 in `SCORECARD.md`.

---

## Phase 4 — Functional: 100% verified parity (close the real gaps)

**4.1** Drive `PARITY_BACKLOG.md` / `ES_COMPATIBILITY.md` to **zero open
defects** — float truncation, scripted writes, scored-total cap,
knn/async/mget edge cases, etc. Each fix ships with a YAML/REST test that
reproduces the ES behaviour.

**4.2** Un-skip the 3 skipped conformance cases (implement or justify).

**4.3** Expand coverage: pull the full ES 8.13 rest-api-spec YAML set (the
runner discovers files at runtime), add families not yet covered, wire into
the CI gate. Target: broader suite, still 0 failures.

Verify: conformance suite (expanded) 0-fail; backlog = 0.

---

## Phase 5 — Remaining dimensions to GREEN

- **kNN/vectors at scale**: 1M×768d — latency AND recall@10 vs exact; if
  behind, tune HNSW (M/efConstruction/efSearch) or add quantization.
- **Update/delete-heavy**: throughput + read-after-update latency vs ES.
- **Disk footprint**: if XERJ segments > ES, land **P3.5** (FST term dict +
  bit-packed/GCD doc-values) and/or default merge codec.
- **RSS under load**, **cold-start**, **recovery time**: measure; fix if 🔴.

---

## Sequencing & sizing

| Phase | Unblocks | Effort | Risk |
|---|---|---|---|
| 0 Harness + CI | everything | M | none |
| 1 Ingest beat | 🔴→🟢 ingest 1-client | L | med |
| 2 Tail beat | the one 🔴 read cell | M-L | med (worktree+gate) |
| 3 Multi-client/scale | biggest ⚪ | M | low |
| 4 Functional parity | 🔴 backlog | L (many small) | low-med |
| 5 Vectors/disk/RSS | remaining ⚪ | M | low |

Recommended order: **0 → 2.1 (cheap tail win) → 3 (prove the likely-already-
winning cases) → 1 → 2.2/2.3 → 4 → 5.** Rationale: Phase 0 makes progress
visible; 2.1 is a cheap shot at the only confirmed loss; Phase 3 likely
flips several ⚪ to 🟢 with no code change (just measurement) — fast morale/
credibility wins — before the heavier 1/2.2 engine work.

Every phase ends by regenerating `SCORECARD.md` and committing it, so the
"all green" state is always current and auditable.
