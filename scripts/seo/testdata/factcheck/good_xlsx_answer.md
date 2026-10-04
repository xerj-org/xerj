---
title: "How do I query a folder of Excel workbooks?"
target_format: xlsx
evidence:
  - claim: "xlsx.rs reads each worksheet as its own dataset, one record per row under the header row"
    source: "engine/crates/xerj-autoindex/src/extract/xlsx.rs"
  - claim: "autoindex walks a local filesystem and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
---

# How do I query a folder of Excel workbooks?

Point `xerj autoindex` at the directory. XERJ reads each `.xlsx` worksheet as
its own dataset on a single-node install, one record per row under the sheet's
header row, with numbers, booleans and dates kept as typed fields.

```bash
xerj autoindex ./finance
```

A sheet is read as one table, merged cells are not expanded, and a legacy
`.xls` file must be saved as `.xlsx` first.
