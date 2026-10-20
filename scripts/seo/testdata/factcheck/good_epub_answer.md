---
title: "How do I search a folder of EPUB books?"
target_format: epub
evidence:
  - claim: "epub.rs reads each book as one record per chapter in reading order"
    source: "engine/crates/xerj-autoindex/src/extract/epub.rs"
  - claim: "autoindex walks a local filesystem and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
---

# How do I search a folder of EPUB books?

Point `xerj autoindex` at the folder. XERJ reads each `.epub` as one record
per chapter in reading order on a single-node install, with the chapter title
from the book's table of contents and the book's title and author on every
record.

```bash
xerj autoindex ./books
```

DRM-protected chapters are skipped rather than decrypted, and a book made only
of page images is not read, because there is no OCR.
