# Before/after profile comparison prompt

Compare two XERJ profiling directories as a correctness-first A/B experiment.

Before analysis:

1. Read both manifests and reject the comparison if corpus, workload, concurrency, cache state, host, capture interval, CPU Hz, build profile, or relevant feature set differs. Do not normalize across Cargo profiles.
   Treat source/profile/features as operator declarations while `source_build_binding` is `UNVERIFIED`; do not claim the binary came from that source revision without independent build attestation.
2. Verify artifact hashes and benchmark correctness. Faster-but-wrong is a regression.
3. Produce `pprof -base BEFORE AFTER` views for CPU and heap, plus absolute top/tree views for both sides.

Report:

- elapsed time, docs/s, requests/s, peak RSS, jemalloc allocated/active/resident, disk bytes, correctness/recall, and profile sample totals;
- absolute and relative deltas with units and denominators;
- stacks removed, stacks added, and stacks shifted elsewhere;
- whether retained bytes fell or merely moved from application allocations into allocator slack, mmap/file cache, or another process;
- whether CPU fell in absolute samples or only as a percentage;
- confidence level and every uncontrolled variable.

Conclude `WIN`, `LOSS`, or `INCONCLUSIVE`. A win requires unchanged correctness/durability, a reproducible resource or throughput improvement, and no unexplained regression in another north-star budget.

A hotspot or win found with the fast `profiling` profile is provisional. Require a same-profile `profiling-final` A/B confirmation before making a production-codegen performance claim.
