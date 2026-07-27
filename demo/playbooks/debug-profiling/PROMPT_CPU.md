# CPU profile analysis prompt

You are reviewing a XERJ CPU pprof artifact from a controlled experiment.

Inputs:

- Read `manifest.json` completely.
- Inspect `cpu.pb` with `pprof -top`, `-tree`, and focused `-list` views.
- Read the exact source at the Git revision in the manifest.
- Read the benchmark's correctness and throughput output.

Report:

1. Confirm artifact hashes, capture duration/frequency, binary hash, declared revision/build, command, host, and whether the process was terminated normally. Treat source/build binding as `UNVERIFIED` unless independently attested.
2. List the top 15 flat and cumulative stacks with both samples and percentages.
3. Group costs into PDF extraction, tokenization/embedding, HTTP/JSON, indexing, flush/merge, allocator/synchronization, storage I/O, and unknown.
4. Trace each major stack to source file and function. Clearly label inference.
5. Identify parallelism bottlenecks, lock/futex waits, repeated parsing/copying, and single-threaded phases.
6. Recommend the three smallest experiments that can falsify the suspected bottlenecks.
7. Do not claim elapsed-time or throughput causality from sample percentages alone.

End with `CONFIRMED`, `LIKELY`, and `UNKNOWN` sections.

If correctness, workload results, or comparable telemetry are not attached, label any performance conclusion `INCONCLUSIVE`; a CPU profile alone identifies sampled hot stacks, not a validated optimization win.
