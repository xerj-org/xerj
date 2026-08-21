---
title: "What does the XERJ benchmark board actually say?"
evidence:
  - claim: "55 WIN / 26 TIE / 4 LOSE / 3 N/A across 88 cells at 100k docs"
    source: "Tier A: demo/playbooks/SCORECARD.md"
  - claim: "The four losses are read p99 under a sustained open-loop writer"
    source: "demo/playbooks/SCORECARD.md"
---

# What does the XERJ benchmark board actually say?

Against a live Elasticsearch 8.13.4 on a single-node box, XERJ ingests 1.72×
faster. It also loses four cells: read p99 under a sustained open-loop writer,
13.57 ms against 3.45 ms.

The index is 1.61× smaller on disk, 176.2 MB against 283.0 MB at 100k docs. That
is a different measurement from the older disk ratio, not a better result.

kNN at k=10 lands at 1.18×, which the board scores as a tie, with recall@10 at
100% on both engines.

Conformance is 99.8% of the ES-YAML suite, on a curated 200-file subset.
