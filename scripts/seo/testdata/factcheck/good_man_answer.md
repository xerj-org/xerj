---
title: "How do I search the man pages installed on a Linux machine?"
target_format: man pages
evidence:
  - claim: "man.rs reads a roff man(7) page, gzipped or not, as one record per .SH section"
    source: "engine/crates/xerj-autoindex/src/extract/man.rs"
  - claim: "autoindex walks a local filesystem and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
---

# How do I search the man pages installed on a Linux machine?

Point `xerj autoindex` at the man directory. XERJ recognizes a roff page by
its `.TH` line, reads `.gz` pages directly, and writes one record per section
with the page title, such as `GREP(1)`, and the one-line summary from NAME.

```bash
xerj autoindex /usr/share/man/man1
```

Pages written in the BSD mdoc macros are not parsed and are indexed as plain
text, and there is no apropos or whatis database.
