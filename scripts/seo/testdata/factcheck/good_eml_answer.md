---
title: "How do I read email and PDFs after indexing a folder of .eml files?"
target_format: eml
evidence:
  - claim: "eml.rs extracts headers, the text body and attachments from one RFC 822 / MIME message file"
    source: "engine/crates/xerj-autoindex/src/extract/eml.rs"
  - claim: "autoindex walks a local filesystem and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
---

# How do I read email and PDFs after indexing a folder of .eml files?

Point `xerj autoindex` at the directory. XERJ types each `.eml` file, extracts
the headers, the text body and the attachments, and writes them as searchable
documents on a single-node install.

```bash
xerj autoindex ./inbox
```

The files must already be on local disk, one message per file. There is no
reader for a mail archive and no connection to a mail server.
