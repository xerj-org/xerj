---
title: "How do I search a folder of Excel spreadsheets?"
target_format: xlsx
evidence:
  - claim: "autoindex walks a directory and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
expect: [FC-THING-RED]
---

# How do I search a folder of Excel spreadsheets?

Run `xerj autoindex ./finance` on a single-node install and XERJ will type every
spreadsheet in the folder and make its cells searchable.
