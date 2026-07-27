# Heap profile analysis prompt

You are reviewing a XERJ jemalloc in-use heap pprof artifact.

Inputs:

- Read `manifest.json` completely and verify `heap.pb.gz`.
- Inspect `heap.pb.gz` with `pprof -top`, `-tree`, and focused `-list` views.
- Read the exact source at the manifest Git revision.
- Correlate the capture endpoint in time with RSS, jemalloc allocated/active/resident, and XERJ logical ownership telemetry when available.

Report:

1. Confirm the binary hash and treat the declared source/build binding as `UNVERIFIED` unless independently attested. Then list the top 20 retained-byte allocation stacks with bytes and percentages.
2. Map them to raw HTTP bodies, parsed JSON, prepared documents, vectors/model scratch, memtables, flush snapshots, merge survivors/buffers, caches, mappings, and unknown.
3. Separate live allocations from allocator fragmentation/dirty pages: `RSS - jemalloc resident`, `resident - active`, and `active - allocated` are not attributable to live heap stacks.
4. Look for duplicated representations of the same corpus and unbudgeted queues.
5. State sampling limitations: this is probabilistic, in-use heap at capture time, not every allocation and not peak RSS by itself.
6. Propose deterministic small-corpus tests that reproduce each dominant owner before another full-corpus run.

End with a byte-accounting table whose rows sum to sampled in-use bytes, plus an explicit unexplained remainder.

If correctness, RSS/jemalloc time series, or workload results are unavailable, label any resource-win conclusion `INCONCLUSIVE`; an in-use heap profile alone does not explain total RSS or peak memory.
