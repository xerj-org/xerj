---
title: "How do I index the documents in an S3 bucket?"
target_format: s3 bucket
evidence:
  - claim: "xerj autoindex accepts s3:// and r2:// wherever it accepts a folder"
    source: "engine/crates/xerj-autoindex/src/objsource.rs"
---

# How do I index the documents in an S3 bucket?

Run `xerj autoindex s3://acme-docs/handbook`. It lists the prefix, streams each
object whose ETag or size changed into a local mirror under `--state-dir`, and
indexes those documents. The index still lives on local disk, on the XERJ node
that `--url` names, and nothing is written back to the bucket.
