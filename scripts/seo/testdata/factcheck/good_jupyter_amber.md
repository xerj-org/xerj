---
title: "How do I search a folder of Jupyter notebooks?"
target_format: jupyter
evidence:
  - claim: ".ipynb files are JSON and route to the json extractor"
    source: "engine/crates/xerj-autoindex/src/extract/json.rs"
expect: [FC-THING-AMBER]
---

# How do I search a folder of Jupyter notebooks?

A notebook is a JSON file, so `xerj autoindex` types it as JSON and indexes its
fields on a single-node install.

```bash
xerj autoindex ./notebooks
```

There is no notebook-aware handling: XERJ does not split a notebook into cells
or separate code from prose.
