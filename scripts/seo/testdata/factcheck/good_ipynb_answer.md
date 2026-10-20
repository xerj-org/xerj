---
title: "How do I search a folder of Jupyter notebooks?"
target_format: jupyter
evidence:
  - claim: "ipynb.rs reads a notebook as one record per Markdown-heading section"
    source: "engine/crates/xerj-autoindex/src/extract/ipynb.rs"
  - claim: "autoindex walks a local filesystem and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
---

# How do I search a folder of Jupyter notebooks?

Point `xerj autoindex` at the folder. XERJ recognizes a notebook by its
content and writes one record per Markdown-heading section on a single-node
install, with the heading, the cell number where the section starts, the
code, and the text outputs.

```bash
xerj autoindex ./notebooks
```

Image outputs are not indexed and no notebook is executed.
