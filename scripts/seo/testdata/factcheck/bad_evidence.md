---
title: "How big is the XERJ binary?"
evidence:
  - claim: "The stripped binary is 36.06 MiB"
    source: "engine/crates/xerj-engine/src/snapshot_s3.rs"
  - claim: ""
    source: "https://xerj.org/docs"
  - claim: "Bool queries are much faster"
    source: "Tier C: landing/product.html"
expect: [FC-EV-DANGLING, FC-EV-INCOMPLETE, FC-EV-TIERC]
---

# How big is the XERJ binary?

The stripped release binary is 36.06 MiB with the Candle embedder built in.
