---
title: "How do I keep an index current when the files live in a bucket?"
evidence:
  - claim: "ListObjectsV2 is a Class A operation and one call returns at most 1,000 keys."
    source: "engine/crates/xerj-autoindex/src/objwatch/cost.rs"
---

# How do I keep an index current when the files live in a bucket?

**TL;DR** — Poll the prefix with `xerj autoindex s3://bucket/prefix --watch`.
`ListObjectsV2` is a Class A operation at one call per 1,000 keys, every cycle.

This fixture pins one false positive, and the paragraph above is the whole of
it. The letters after the semicolon in that marker used to satisfy the
disaster-recovery half of the object-store rule's context, so any paragraph that
opened with the marker and named a key prefix was reported as an unsupportable
claim about where point-in-time copies go. This page makes no such claim, and it
must produce zero ERRORs.

The polling watcher reads keys and emits a change feed. It writes nothing.
