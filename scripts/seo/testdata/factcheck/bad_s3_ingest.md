---
title: "How do I index an S3 bucket with XERJ?"
target_format: s3 bucket
evidence:
  - claim: "autoindex walks a directory tree"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
expect: [FC-S3-INGEST, FC-THING-RED]
---

# How do I index an S3 bucket with XERJ?

Run `xerj autoindex` against your bucket and XERJ will crawl object storage
directly, typing every key as it goes on a single-node install.
