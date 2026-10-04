---
title: "How do I search a folder of old .xls spreadsheets?"
target_format: xls
evidence:
  - claim: "autoindex walks a directory and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
expect: [FC-THING-RED]
---

# How do I search a folder of old .xls spreadsheets?

Run `xerj autoindex ./finance` on a single-node install and XERJ will open
every legacy Excel 97-2003 workbook in the folder and make its cells searchable.
