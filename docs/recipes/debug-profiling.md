# Diagnose XERJ CPU and memory with pprof

Use this recipe when a repeatable XERJ workload is slow or consumes unexplained memory. The debug build can emit Google pprof-compatible CPU and jemalloc in-use heap profiles without exposing a network debug endpoint.

The tooling is Linux-only and off by default. It is intended for controlled experiments, not always-on production use.

## 1. Build a symbolized profiling binary

From the repository root:

```bash
(cd engine && cargo build --profile profiling -j 32 -p xerj-server \
  --no-default-features --features debug-profiling)
```

The `profiling` profile uses thin LTO and parallel code generation for iteration and retains symbols. It is not production-codegen-equivalent. Reconfirm any winning optimization with `--profile profiling-final`, which inherits release fat LTO and one codegen unit while retaining symbols.

## 2. Capture one dimension at a time

CPU example:

```bash
python3 demo/playbooks/debug-profiling/capture.py \
  --output /tmp/xerj-cpu-run \
  --cpu-seconds 30 \
  --delay-seconds 5 \
  --workload my-ingest-v1 \
  --corpus corpus-sha256-or-stable-id \
  --concurrency 4 \
  --cache-state cold \
  --build-features debug-profiling \
  --build-profile profiling \
  --attach correctness=/tmp/correctness.json \
  --attach telemetry=/tmp/memory.ndjson \
  -- \
  engine/target/profiling/xerj --insecure --data-dir /tmp/xerj-profile-data
```

Start the workload from another terminal during the five-second delay. Repeat into fresh data and output directories with `--heap-seconds 30` instead of `--cpu-seconds 30`. Separate runs avoid CPU and heap profiler interference.

Delay is relative to process start, not a readiness check. Delay zero includes configuration, TLS setup, engine replay, and router construction.

## 3. Validate before analysis

```bash
python3 demo/playbooks/debug-profiling/inspect.py /tmp/xerj-cpu-run
```

The command verifies hashes and basic formats, reports missing evidence, and points agents to the CPU, heap, and A/B prompt templates. Without attached correctness and benchmark/telemetry evidence, performance conclusions are `INCONCLUSIVE`.

The binary SHA-256 is exact, but the wrapper cannot cryptographically bind that binary to the recorded checkout or declared Cargo profile/features; the manifest labels that relationship `UNVERIFIED`.

See the complete safety, failure, stale-lock, pprof viewing, and A/B instructions in [`demo/playbooks/debug-profiling/README.md`](../../demo/playbooks/debug-profiling/README.md).
