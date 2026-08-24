---
title: "How do I search a folder of PDFs?"
target_format: pdf
evidence:
  - claim: "pdf.rs extracts text from PDF files during autoindex"
    source: "engine/crates/xerj-autoindex/src/extract/pdf.rs"
  - claim: "autoindex walks a local filesystem and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
---

# How do I search a folder of PDFs?

Point `xerj autoindex` at the directory. XERJ types each file, extracts the text
with its PDF extractor, and writes one searchable document per file on a
single-node install.

```bash
xerj autoindex ./papers
```

Query it over the Elasticsearch-shaped API on the same box. There is no crawler
and no object-store reader: the files must already be on local disk.
