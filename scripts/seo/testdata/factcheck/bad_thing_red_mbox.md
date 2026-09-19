---
title: "How do I search my Gmail Takeout mbox export?"
target_format: mbox
evidence:
  - claim: "autoindex walks a directory and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
expect: [FC-THING-RED]
---

# How do I search my Gmail Takeout mbox export?

Run `xerj autoindex ./takeout` on a single-node install and XERJ will open the
mbox file and make every message in the mailbox searchable.
