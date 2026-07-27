# XERJ debug profiling

This toolkit answers two different questions:

- `cpu.pb`: where the process spent sampled CPU time.
- `heap.pb.gz`: which allocation call stacks retained sampled live bytes at the end of the capture.

It is Linux-only, local, bounded, and off by default. It adds no debug HTTP endpoint. A normal XERJ build does not compile either profiler.

## Build

Build the fast, symbolized iteration binary:

```bash
(cd engine && cargo build --profile profiling -j 32 -p xerj-server \
  --no-default-features --features debug-profiling,onnx-experimental)
```

`profiling` uses thin LTO and 16 codegen units to shorten the edit/profile loop. It is not production-codegen-equivalent. Do not compare it against a `release` or `profiling-final` run.

Before making a final performance claim, repeat the winning experiment with release-equivalent fat LTO and one codegen unit:

```bash
(cd engine && cargo build --profile profiling-final -j 32 -p xerj-server \
  --no-default-features --features debug-profiling,onnx-experimental)
```

Do not use the ordinary stripped `release` profile for profiles: heap addresses will be much harder or impossible to resolve.

## Capture

Start a new artifact directory and a bounded 30-second capture:

```bash
python3 demo/playbooks/debug-profiling/capture.py \
  --output /tmp/xerj-profile-before \
  --cpu-seconds 30 \
  --heap-seconds 30 \
  --delay-seconds 5 \
  --workload financebench-fb20-v1 \
  --corpus financebench-20-sha256-example \
  --concurrency 4 \
  --cache-state cold \
  --build-features debug-profiling,onnx-experimental \
  --build-profile profiling \
  --attach correctness=/tmp/fb20-correctness.json \
  --attach telemetry=/tmp/fb20-memory.ndjson \
  -- \
  engine/target/profiling/xerj --data-dir /tmp/xerj-profile-data
```

Run the workload from a second terminal. `--delay-seconds` is process-start-relative and gives the server and workload driver a bounded synchronization window; it does not probe readiness. With delay zero, profiling starts before config loading and engine replay and therefore includes initialization. The wrapper stops the whole server process group five seconds after the delay plus longest capture. An explicit `--stop-after` must preserve the same five-second dump/publication window; exact capture-boundary termination is rejected because conversion and atomic publication happen after sampling stops.

The manifest records the exact binary hash, command, repository revision/dirty state, host shape, capture settings, exit status, and artifact hashes. The binary-to-source binding is explicitly `UNVERIFIED`: the wrapper cannot prove that the binary was built from the recorded checkout. Build profile/features and workload/corpus/concurrency/cache values are likewise **declared by the operator**, not extracted from the binary. Use repeatable `--attach LABEL=PATH` options to copy and hash correctness, throughput, RSS, jemalloc, and XERJ telemetry evidence. Without those, the supplied analysis prompts require an `INCONCLUSIVE` performance verdict.

Server output is private `server.log`; when `pprof` is installed, agent-readable `*.top.txt` reports are generated too. Validate the entire bundle:

```bash
python3 demo/playbooks/debug-profiling/inspect.py /tmp/xerj-profile-before
```

The output directory must not already exist. Profile files are mode `0600`, published atomically, and never overwritten. A per-directory lock rejects concurrent capture controllers.

If XERJ reports a stale `.xerj-debug-profile.lock`, first confirm no XERJ process is using that directory. Remove only that exact lock file after confirming the previous process was killed; the controller removes it on normal and handled shutdown. Invalid requested profiling configuration fails server startup instead of silently running without artifacts.

Direct invocation is also possible:

```bash
mkdir -m 700 /tmp/xerj-profile
XERJ_DEBUG_PROFILE_DIR=/tmp/xerj-profile \
XERJ_DEBUG_CPU_SECONDS=30 \
XERJ_DEBUG_HEAP_SECONDS=30 \
XERJ_DEBUG_CPU_HZ=100 \
XERJ_DEBUG_PROFILE_DELAY_SECONDS=5 \
engine/target/profiling/xerj
```

Durations must be 1–300 seconds, delay 0–300 seconds, and CPU frequency 1–1000 Hz. The default is 100 Hz with no delay. Shutdown interrupts delay and capture waits instead of blocking for the remaining duration.

## Inspect

Install Google's `pprof`, then:

```bash
pprof -http=localhost:0 /tmp/xerj-profile/cpu.pb
pprof -http=localhost:0 /tmp/xerj-profile/heap.pb.gz
pprof -top /tmp/xerj-profile/heap.pb.gz
```

Profiles contain function names, file paths, and workload-dependent execution structure. Treat them as private diagnostic artifacts.

The manifest also contains the full server command. Do not put passwords, API keys, or tokens directly in command-line arguments. Although the output directory is mode `0700` and artifacts are `0600`, copy the bundle only through an approved private channel.

CPU sampling uses `SIGPROF` and libgcc unwinding. Upstream `pprof-rs` explicitly documents signal-safety limitations, so this tooling is for controlled debugging builds, not an always-on production profiler. Jemalloc allocation sampling is compiled only into this feature and starts inactive; cumulative allocation profiling remains disabled to avoid retaining every unique backtrace.

Prefer separate CPU-only and heap-only repetitions. A combined capture is convenient for smoke testing but the profilers can perturb one another, so its throughput and overhead results are confounded.

## Fair A/B protocol

Use a fresh data directory and artifact directory for each side. Keep the corpus, query/workload driver, concurrency, capture duration, CPU frequency, build profile, feature set, machine, and cache state identical. Verify correctness before comparing speed. Compare both absolute samples/bytes and percentage shares: percentage-only views can make an unchanged function look better when unrelated work grows.

The validator records only the operator-declared build profile. Reject cross-profile A/B comparisons. Hotspots discovered with fast `profiling` must be reconfirmed using `profiling-final` before a final production-performance claim.

Use the prompt templates in this directory to make an AI agent produce evidence-linked conclusions rather than guesses.
