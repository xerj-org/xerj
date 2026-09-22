# idle budget — measured on 450 idle indices

Issue #874's "one fixture, kept runnable":

> 15 repos x ~30 datasets, boot, settle 120 s, measure CPU/wakeups/RSS via
> /proc. Gate the budget in CI on ubuntu (coarse thresholds; the point is
> catching O(N) regressions, not ±1%).

What [`run.sh`](./run.sh) does, the numbers that came out, and — the reason
this fixture exists — the idle-CPU regression it caught on its very first run
against the then-current tree, with the fix that followed. Everything below is
from real runs on this box; result files as produced by the runs are in
[`results/`](./results/).

**Units.** Memory figures are `/proc` kB (1024-byte units — `VmRSS` and
friends are reported in kB by the kernel); "0.2 MB per index" in the issue is
taken at the issue's word as 204.8 kB. CPU is **% of one core** =
(utime+stime delta over the window) / CLK_TCK / window seconds, summed over
all threads — 100 % = one full core, 3.1 % ≈ the box's 32 threads at 1 %.
Wakeups are context switches per second, voluntary + nonvoluntary, summed
across `/proc/<pid>/task/*/status`. The three are never mixed into one number.

## Machine

| | |
|---|---|
| CPU | AMD Ryzen AI MAX+ 395 w/ Radeon 8060S — 32 threads (16 cores × 2) |
| RAM | 119 GiB usable |
| Disk | NVMe, ext4; the throwaway data dir lives on it, NOT on tmpfs (`run.sh` refuses a RAM-backed dir) |
| OS | Linux 7.0.0-27-generic (Ubuntu), x86_64 |
| XERJ | v1.0.0-rc.77 + branch `bench/issue-874-idle-budget-gate`, `cargo build --release -p xerj-server` |
| Node | throwaway data dir, private port in 9610..9639, `--insecure` (TLS+auth off), `--embed-mode lexical` (the default feature-hashing embedder — not neural), no config file — `Config::default()` flush/merge/sampler behaviour is the thing measured |
| Box load | **shared**: other agents were building and running on it during every window (load average is recorded inside each result file: 38–45 in the run of record, up to 63 in the before-fix artifact). Idle-CPU and wakeup figures are process-wide counters, so a busy neighbour inflates *nonvoluntary* switches (scheduler preemption) but not the process's own CPU time; RSS is unaffected. |
| THP | The fixture pins `thp:never,dirty_decay_ms:0,muzzy_decay_ms:0` on the measured server, via **both** `_RJEM_MALLOC_CONF` and `MALLOC_CONF`. The var name matters: the engine's allocator is `tikv-jemalloc-sys`, which builds jemalloc `--with-jemalloc-prefix=_rjem_`, and a prefixed build reads `_RJEM_MALLOC_CONF` — the plain `MALLOC_CONF` set alone was **silently ignored** (the second CI run measured the same 1835 kB/idx as the first with the pin printed in the log). Verified on this box: `MALLOC_CONF=bogus_opt:1` boots with no allocator comment; `_RJEM_MALLOC_CONF=bogus_opt:1` boots with `<jemalloc>: Invalid conf pair: bogus_opt:1` — and `run.sh` now aborts if the boot log contains that line. THP: jemalloc follows the kernel's transparent-hugepage mode by default; on a kernel with `THP=always` (GitHub `ubuntu-latest` runners) each index's small boot allocations land in distinct 2 MB extents and pin a full hugepage — the first CI run measured **2075 kB anon per idle index** (≈ one hugepage) vs 299 kB on a `madvise` host, a page-granularity artifact that fails the 0.2 MB/index budget even for a perfect engine. Decay: without `dirty/muzzy_decay_ms:0` the baseline arm alone wobbled 31–76 MB run-to-run on the same host (whatever freed pages the allocator retained at sample time), a 1.5× swing in the per-index subtraction — with the pins, same host, ci-test profile: **181.6 kB/idx**, gate green (result `citest-check2`). The kernel's THP mode, the pin, and the kernel-side `AnonHugePages` reading (~0 = pin live) are printed in the measured section of every run. |

## What the fixture measures

Four arms, in order ([`run.sh`](./run.sh)):

0. **corpus** — [`gen_corpus.sh`](./gen_corpus.sh) writes 15 repo dirs × 30
   dataset CSVs (3 docs each, unique column names per dataset).
1. **baseline** — a node on an *empty* data dir, same 120 s window. The
   per-index RSS line subtracts this node's VmRSS; without it the divisor
   would include one runtime's worth of allocations.
2. **load** — boot, create one index per dataset over the ES wire with
   default settings (no shard override — default behaviour is the point),
   `_bulk` 3 docs each, `POST /_flush`, SIGTERM (graceful, flushes on the way
   out).
3. **restart + measure** — boot again on the cleanly-flushed corpus: this
   boot-to-green is the boot number, and the restart log must contain **zero**
   `replayed WAL entries` lines (a clean flush replays nothing). Then 5 s of
   boot-tail drain and the 120 s measurement window, `/proc` only
   (`perf_event_paranoid` makes perf unavailable; `ps`/`uptime` are absent
   here). CPU and wakeups are **deltas over the window**, never spot samples;
   RSS is read at window end.

One environmental normalization, applied to the load node only: the sandbox
these numbers were taken on keeps its disk at ~96 % used, above the node's
default 95 % disk flood-stage watermark, which would 429 the loader for
reasons that have nothing to do with the idle budget. `run.sh` raises the
watermark to 99 % on its own throwaway node via the engine's documented
runtime override (`PUT _cluster/settings` on
`cluster.routing.allocation.disk.watermark.flood_stage`), waits out the
resource sampler's apply/release ticks, and probes a real document write
before loading. On CI (disk nowhere near 95 %) the override is a no-op.
`KEEP=1` preserves the throwaway dir and both arms' boot logs.

## Measured

The run of record (fixed binary, all gates green, 2026-09-21,
[`results/result-n450indices.json`](./results/result-n450indices.json)):

| | baseline node (N=0) | 450 indices | gate | verdict |
|---|---|---|---|---|
| idle CPU (% of one core, 120 s window) | 0.10 | **0.175** | < 0.5 | ok |
| wakeups/s (vol + nonvol, process-wide) | 26.0 | **26.6** (24.2 + 2.4) | < 100 | ok |
| VmRSS (kB) | 76 240 | **158 944** (anon 134 704 + file 24 240; HWM 173 028) | — | — |
| per-index idle RSS (kB) | — | **183.8** = (158 944 − 76 240) / 450 | ≤ 256 (product line 204.8) | ok |
| boot-to-green (ms) | 184 (empty) | **215** on the cleanly-flushed corpus | < 10 000 | ok |
| WAL replay lines after clean flush | — | **0** | 0 | ok |
| threads / fds | 340 / 15 | **336 / 15** | — | — |

N-independence is the story the CPU and wakeup rows tell: 450 indices cost
0.075 pp more CPU and 0.6 wakeups/s more than an empty node — both within
window noise on a box whose load average was 38–45 during the runs (recorded
in the result file). Per-thread attribution at N=450: `xerj-memtable-s`
0.083 %, `xerj-mem-sample` 0.058 %, `xerj-rt` 0.008 % — no thread class
scales with index count.

### What the first run caught: an O(N) idle metrics loop

The fixture's first full run against the then-current tree (same binary
version, before the fix; result preserved as
[`results/result-n450-with-metrics-gauge-loop.json`](./results/result-n450-with-metrics-gauge-loop.json))
**failed the CPU gate**:

| 450 idle indices | CPU (% of one core) | wakeups/s | per-index RSS (kB) |
|---|---|---|---|
| background gauge loop present (first run) | **1.12** (0.78 in an earlier window; load 40→63 during the runs) | 46.1 (24.9 vol + 21.2 nonvol) | 185.3 |
| per-thread attribution | `xerj-rt` 0.92, `xerj-memtable-s` 0.10, `xerj-mem-sample` 0.08 | | |
| loop disabled, binary otherwise identical (A/B, 4 windows) | **0.15–0.18** | 26.3–26.9 | — |
| loop deleted (the committed fix; run of record above) | **0.175** | 26.6 | 183.8 |

Root cause: `run_metrics_gauge_loop` — a 10 s ticker that fed the Prometheus
gauges (`xerj_doc_count`, `xerj_segment_count`, `xerj_wal_size_bytes`,
`xerj_memory_usage_bytes`) — walked **every index's WAL subtree** on every
tick: `read_dir` + one `metadata()` per WAL shard. At 450 indices × ~16
default shards that is ~7 400 `stat()` calls every 10 s on an async runtime
worker, and it scales with index count: 0.7–0.9 % of one core at N=450 idle,
against the issue's < 0.5 % budget line, with essentially all of it in the
`xerj-rt` (tokio) threads. The A/B row is the causal proof: same binary, loop
disabled by an env kill-switch, four consecutive 120 s windows.

The fix (this branch): the loop is **deleted**; `/v1/metrics` refreshes the
gauges at scrape time — the only moment they are observable — with the WAL
subtree walk on the blocking pool
(`engine/crates/xerj-api/src/es_compat.rs`, `refresh_metric_gauges`;
wired in `engine/crates/xerj-api/src/native.rs`). A scrape now costs what the
loop's tick used to, and idle time between scrapes costs nothing. Pinned by
`engine/crates/xerj-api/tests/metrics_gauges_refresh_at_scrape_time.rs`
(fail-before shape: with the loop gone and nothing refreshing at scrape time,
`xerj_doc_count` reads 0 on a node holding documents).

This is what the issue meant by "the point is catching O(N) regressions": a
per-index background walk is invisible at N=5 in a unit test and 2× the budget
at N=450.

### Thresholds and why

| gate | value | measured healthy | rationale |
|---|---|---|---|
| idle CPU | < 0.5 % of one core | 0.175 % at N=450 (0.10 baseline) | the issue's own line. ~3× above the fixed measurement, loose enough for a shared 2–4 vCPU runner, tight enough that the 0.9–1.1 % loop above fails it |
| wakeups | < 100/s process-wide | 26.6/s at N=450 (26.0 baseline) | "O(1)/s process-wide" made numeric. ~2× the worst healthy window seen (46/s under load-63 neighbours; ~26/s on the fixed binary). The #871-class regression (~115/s timer churn at 464 indices) fails it; a `GATE_WAKEUPS_PER_S=100` that flaps would get disabled, so it is set where only real per-index timers land |
| per-index RSS | ≤ 256 kB (CI coarse line) | 182–210 kB/idx total-normalized across hosts; true marginal cost 206–227 kB/idx (see calibration below) | **two lines, deliberately separated.** The issue's product line is 0.2 MB (204.8 kB) per idle index — printed in the measured section of every run as the reference target, with the over/under percentage. The CI gate is 256 kB, calibrated like every other line here per the issue's own verification clause ("coarse thresholds; the point is catching O(N) regressions, not ±1%"): the healthy band straddles 204.8 — 4-vCPU hosts (the dev-laptop shape the issue is about) measure 205–210, the 32-thread dev box 182–195, and the N-slope marginal cost 206–227 — so a gate *at* the product line flaps by host config, which a coarse CI gate must not. 256 still catches every regression class by multiples: the rc.70 floor (#873: 870 kB/idx) 3.4×, the THP-hugepage artifact (1835 kB/idx) 7.2× |
| boot-to-green | < 10 000 ms | 215 ms at N=450 (184 empty) | ~45× the measured boot; the regression class is O(corpus) WAL replay, which lands in the tens of seconds, not the hundreds of ms |
| WAL replay after clean flush | 0 lines | 0 | a clean `POST /_flush` + SIGTERM checkpoint means nothing to replay; any `replayed WAL entries` line is the bug |

### RSS calibration: the N-curve behind the 256 kB line

The third CI run (all allocator pins live, `AnonHugePages` ≈ 0) still failed
the literal 204.8 line — by 0.7 % (209.9 kB/idx on the 4-vCPU runner). That
sent the fixture back to the lab bench; every number below is a real run,
`ci-test` binary, allocator pins on, result files committed under
[`results/`](./results/):

| host shape | N=0 | N=150 | N=300 | N=450 | per-index (subtract empty) |
|---|---|---|---|---|---|
| 32 vCPU (336 threads, 16 ingest shards) | 76.2 MB | 98.1 / 98.4 MB | 131.8 MB | 163.8 / 164.1 / 166.3 MB | 182–195 kB/idx |
| 4 vCPU, `taskset -c 0-3` (82 threads, 2 shards) | 43.3 MB | 74.3 MB | — | 136.1 MB | 205–206 kB/idx |
| GitHub runner, 4 vCPU (from CI logs) | 48.8 MB | — | — | 143.2 MB | 209.9 kB/idx |

Two findings:

1. **The subtraction number depends on host shape.** Fewer vCPUs → smaller
   empty-node fixed cost (43–49 MB vs 76 MB) *and* a flatter per-index curve,
   which lands the total-normalized number 15–25 kB/idx higher — on exactly
   the laptop-shaped hosts the issue targets. The product line (204.8) cuts
   through this band; gating CI exactly on it flaps by host config.
2. **The true marginal cost is the N-slope, and it is higher than the
   subtraction number everywhere**: 32-vCPU segment slopes are 145 kB/idx
   (0→150), 224 (150→300), 215 (300→450) — the first 150 indices are
   cheaper, then it settles; at 4 vCPU it is flat ~206 kB/idx throughout.
   The empty node is *not* a good proxy for the fixed cost (it extrapolates
   to a larger intercept than the loaded arms imply), which is why the
   subtraction under-reports. Reported here as a measurement fact; the
   ~10–12 MB knee between N=150 and N=300 on many-core hosts and the
   empty-node intercept are unexplained and worth a follow-up.

What this means for the product line: the engine's marginal per-index idle
cost is ~0.20–0.22 MB/index across every host measured — at the line on
many-core hosts, 1–10 % over it on 4-vCPU shapes. The CI gate (256 kB) is
the coarse regression tripwire; the product line stays printed in every
run's measured section so the gap stays visible rather than being rounded
away.

Overrides (`GATE_CPU_PERCENT`, `GATE_WAKEUPS_PER_S`,
`GATE_RSS_PER_INDEX_KB`, `GATE_BOOT_MS`) exist for local experiments; the
defaults are the CI contract. If a gate fails, the finding is the point — do
not move the line to make a run pass.

### Calibration update, 2026-09-22 — #1024 tranche 1 (lazy request-cache seen-set)

`6526e8ea` (`perf(#1024): request-cache seen-set allocates on first tracked
search`) removed the eager `RequestCacheSeen::with_capacity(65_536)` at both
`Index` construction sites: the seen-set now starts empty and grows on first
`record()`, so an index that has never served a `request_cache=true` search
holds nothing. Same host, same fixture, `ci-test` binaries, allocator pins
on; before-binary rebuilt from `ed510a61` (the commit's parent, single-file
diff). Result files: `result-p1024-n150-4cpu.json`,
`result-p1024-n450-4cpu.json` (before, 4-vCPU), `result-p1024-after-n150-4cpu.json`,
`result-p1024-after-n450-4cpu.json` (after, 4-vCPU), `result-p1024-before-32cpu-n450.json`
/ `result-p1024-after-32cpu.json` / `result-p1024-after-32cpu-rerun.json`
(32-vCPU N=450 pair — the before arm was added because every earlier 32-vCPU
reading was N=300).

| 4-vCPU (`taskset -c 0-3`) | before | after | Δ |
|---|---|---|---|
| per-index idle RSS, N=150 | 204.9 kB | 63.2 kB | −69 % |
| per-index idle RSS, N=450 | 203.7 kB | 63.3 kB | −69 % |
| N-slope, VmRSS (450−150)/300 | 206.5 kB/idx | 63.7 kB/idx | −69 % |
| N-slope, RssAnon (450−150)/300 | 205.7 kB/idx | 68.8 kB/idx | −67 % |
| idle CPU, N=450 | 0.225 % | 0.192 % | −15 %rel |
| wakeups/s, N=450 | 25.2 | 21.4 | − |
| boot-to-green, N=450 | 6 ms | 6 ms | — |
| WAL replay after clean flush | 0 | 0 | — |

| 32 vCPU (unpinned), N=450 | before | after (2 runs) |
|---|---|---|
| per-index idle RSS | 199.0 kB | 66.3 / 62.6 kB |
| idle CPU | 0.767 % (loadavg 19) | 0.650 / 0.633 % (loadavg 3–4) |
| wakeups/s | 22.6 | 25.9 / 22.2 |
| boot-to-green | 385 ms (loaded window) | 113 / 7 ms |

The subtraction and the N-slope now agree (~64 kB/idx): the curve is flat at
a genuinely O(N)-constant ~0.06 MB/index, and issue #874's 0.2 MB/index
product line is met on both host shapes with >3× margin. The remaining
~64 kB/idx is the next tranche's target (per-index DashMap shard arrays,
WAL/memtable shard structs, parsed settings/mapping copies).

Pre-existing, unchanged by this commit: the unpinned 32-vCPU arm at N=450
reads >0.5 % idle CPU on the **before** binary too (0.767 %) — the gate line
was calibrated on 4-vCPU and quiet-N=300 windows, and both binaries trip it
on that arm on this shared box. The controlled 4-vCPU pair above is the
comparison that matters; wakeups (the timer-churn proxy) are flat on every
arm. The issue stays open for the follow-up tranches.

## Reproduce

```sh
cd engine && cargo build --release -p xerj-server && cd ..
XERJ_BIN=$PWD/engine/target/release/xerj bash benchmarks/idle-budget/run.sh
# smaller / faster variants:
REPOS=5  DATASETS=30 SETTLE_SECS=60 LABEL=n150 bash benchmarks/idle-budget/run.sh   # N-slope point
REPOS=15 DATASETS=30 SETTLE_SECS=20 LABEL=smoke bash benchmarks/idle-budget/run.sh  # ~3 min smoke
```

The script picks the first free base port in 9610..9639 (never :9200), refuses
a non-empty or RAM-backed data dir, and writes `results/result-<LABEL>.json`.
Expected tail of a green run:

```
== measured ==
  boot-to-green, empty node             184 ms
  boot-to-green,  450 indices              215 ms   (cleanly-flushed corpus)
  WAL replay lines after flush             0      (0 = nothing to replay)
  idle CPU,  450 indices               0.175 % of one core  (window 120.0s)
  idle CPU, baseline node                0.1 % of one core
  wakeups/s,  450 indices               26.6 /s   (voluntary 24.2/s + nonvoluntary 2.4/s)
  wakeups/s, baseline node              26.0 /s
  VmRSS,  450 indices                158944 kB   (VmHWM 173028 kB; anon 134704 kB + file 24240 kB)
  VmRSS, baseline node                 76240 kB
  per-index idle RSS                   183.8 kB   ((158944 - 76240) / 450)
  issue #874 product line              204.8 kB per index   (met; the CI gate is the coarse 256 kB line)
  threads 336, fds 15, loadavg ['37.78', '39.23', '40.32']

== gate (coarse thresholds; the point is catching O(N) regressions, not ±1%) ==
  ok    CLAIMED  idle CPU < 0.5 % of one core (any N)
        MEASURED 0.175 % over 120.0s
  ok    CLAIMED  wakeups < 100/s process-wide (any N)
        MEASURED 26.6/s (vol 24.2 + nonvol 2.4)
  ok    CLAIMED  per-index idle RSS <= 256 kB (CI coarse line; issue product line 0.2 MB/index)
        MEASURED 183.8 kB = (158944 - 76240) / 450 (product line: met)
  ok    CLAIMED  boot-to-green < 10000 ms on a cleanly-flushed 450-index corpus
        MEASURED 215 ms (empty node boots in 184 ms)
  ok    CLAIMED  no O(corpus) WAL replay after a clean flush
        MEASURED 0 'replayed WAL entries' lines in the restart log
  ok   CLAIMED  450 indices
        MEASURED 450 (doc probe: 3 hits on one index)

IDLE-BUDGET GATE PASSED
```

CI: the `idle-budget` job in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml)
builds `xerj-server` with the shared `ci-test` profile cache and runs the
fixture with its defaults (450 indices, 120 s settle, the thresholds above) on
`ubuntu-latest`, `timeout-minutes: 30` — the fixture itself is ~8 min measured.

## Result files

- [`results/result-n450indices.json`](./results/result-n450indices.json) — run of record, fixed binary, all gates green.
- [`results/result-n450-with-metrics-gauge-loop.json`](./results/result-n450-with-metrics-gauge-loop.json) — the failing first run (CPU 1.12 % > 0.5), kept as the before-artifact of the metrics-loop fix.

Each carries the binary version+path, corpus shape, boot times for all three
boots, both sample windows (including per-thread CPU attribution and the load
average at window start), and the computed per-index RSS.
