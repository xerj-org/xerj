---
title: "How do I search a Slack workspace export?"
target_format: slack
evidence:
  - claim: "a Slack export is a folder of JSON files and routes to the json extractor"
    source: "engine/crates/xerj-autoindex/src/extract/json.rs"
expect: [FC-THING-AMBER]
---

# How do I search a Slack workspace export?

A Slack export is a folder of JSON files, one per channel per day, so
`xerj autoindex` types each file as JSON and indexes its fields on a
single-node install.

```bash
xerj autoindex ./slack-export
```

There is no Slack integration: XERJ reads the JSON files Slack gives you and
does not connect to a workspace.
