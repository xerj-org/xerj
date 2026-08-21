---
title: "How do I search log files with XERJ?"
target_format: log files
evidence:
  - claim: "logs.rs extracts structured fields from log files"
    source: "engine/crates/xerj-autoindex/src/extract/logs.rs"
expect: [FC-COLUMNAR-LOGS, FC-CLI-SUBCOMMAND]
---

# How do I search log files with XERJ?

Point `xerj autoindex` at a directory of logs on a single-node box and XERJ
builds a columnar logs index over them, backed by the `xerj-logs` module.

Then run `xerj query 'level:ERROR'` to pull the failures out.
