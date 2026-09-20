---
title: "How do I search my Outlook PST archive?"
target_format: pst
evidence:
  - claim: "autoindex walks a directory and types each file"
    source: "engine/crates/xerj-autoindex/src/lib.rs"
expect: [FC-THING-RED]
---

# How do I search my Outlook PST archive?

Run `xerj autoindex ./archive` on a single-node install and XERJ will open the
PST file and make every message in the mailbox searchable.
