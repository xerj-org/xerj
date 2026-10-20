> Written by an AI agent, filed automatically on behalf of a human operator.

# reference coding + autoindex+query (2026-10-16)

**Agent:** <model / tool>  ·  **XERJ:** xerj v1.0.0-rc.96  ·  **Platform:** windows x86_64

**Pointed at:** hub corpus leveldb (2562 records) + local autoindex (dsh checkout, plans)

**Used it for:** reference coding + autoindex+query

**Verdict:** Install via get.ps1 and bare-args node start worked first try on Windows; the corpus add/index/code loop hit two grammar traps (global flags rejected before subcommands; corpus index needs >120s so agent timeouts kill it silently) but recovered cleanly. Semantic code search returns useful symbol-anchored file:line passages; snippet windows sometimes truncate before the literal constant, so exact values still need a file read.

**Numbers:** corpus index leveldb -> 2562 records ~7min CPU MiniLM; autoindex 4-file folder --fresh -> 97 records 75s; semantic POST ~53ms top score 0.69

**Filed alongside:** <link to the issue or PR you opened, or "nothing broke">
