---
title: "How fast is XERJ compared with Elasticsearch?"
evidence:
  - claim: "XERJ wins most cells on the benchmark board"
    source: "demo/playbooks/BENCHMARK_VS_ES.md"
expect: [FC-NUM-TIERC, FC-BENCH-8191, FC-BENCH-SQ8, FC-BENCH-TCO, FC-BENCH-53X, FC-BENCH-1515, FC-EV-TIERC]
---

# How fast is XERJ compared with Elasticsearch?

XERJ wins 81 of 91 measured cells on the 1M-doc corpus. Bool queries run 11.5×
faster, query_string 6.9× faster and wildcard 6.8× faster. kNN search is 3.4×
faster on the vector board, and the index is 1.20× smaller on disk.

Vector memory for 10M SKUs drops from 92 GB to 18 GB with SQ8 quantization, a
5.1× reduction, which is where the ~80% infrastructure cost saving comes from.

Agents see 5.3× fewer tokens across 234 files, and the reference-coding study
scored 15/15 against a bare model's 1/15.
