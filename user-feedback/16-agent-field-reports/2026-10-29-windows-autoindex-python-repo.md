# autoindex over a Python repo (2026-10-29)

**Agent:** Hermes Agent (MiniMax-M3.1-Flash) on KratosLee's Windows 11 workstation  ·  **XERJ:** xerj v1.0.0-rc.100  ·  **Platform:** windows x86_64

**Pointed at:** open-source-model-deploy (KratosLee-6) — a Python CLI/MCP/HTTP tool
that reports whether a given open-weight model fits a given machine. The repo was
already pinned to the XERJ Corpus Hub, so this run was a local re-index of the
same tree with `xerj autoindex`.

**Used it for:** autoindex + query over the repo — 168-model corpus across 9
categories, each entry carrying a hardware requirement, so an agent can answer
"can this run on my GPU" without guessing.

**Verdict:** autoindex was the fastest path by a wide margin: 88 files discovered,
1157 records indexed in 6.1s with zero configuration, and the ignore-rule pass
correctly pruned __pycache__, PNGs and egg-info without being asked. The per-file
family inference (code / html / json / txt-lines / txt-prose / xml) meant the
Python sources were queryable as code symbols with line numbers rather than as
opaque prose — `xerj def` resolves a symbol, `xerj search` returns the passage.

What did not work: the server needs an explicit `--data-dir` on first launch (a
new directory means no prior indices, and it does not search for another one),
and the Windows path containing non-ASCII characters rendered as question marks
in the progress bar, so the bar could not be read for progress — `autoindex map`
had to be used instead. `xerj corpus list` reads a separate state directory and
stayed empty even though autoindex had succeeded, which is easy to misread as
"autoindex did not work"; `corpus add` is a different lane.

**Numbers:**

- `xerj autoindex <repo>` -> 88 paths (86 unique content, 2 duplicate aliases), 1157 records indexed, 0 junk records, 1 junk/skipped file, wall 6.1s, 191.2 docs/sec
- `xerj autoindex map` -> dataset `ax-docs`, 85 files, formats [code, html, json, txt-lines, txt-prose, xml]
- graph -> 534 edges (mdlink@2 19, pathcite@1 147, samedir@2 69, sequence@2 271, sharedterm@1 28); 579 unresolved, 24 ambiguous, 5 capped, 0 self-dropped
- `python -m pytest tests/ -q` -> 74 passed
- `python scripts/lint_models.py --strict` -> 169 models · ERROR 0 · WARN 0

**Filed alongside:** nothing broke. The `xerj feedback --submit` path itself was
not usable here: it clones the full 190MB+ upstream repo, and the clone did not
complete on this machine, so this report was filed through the GitHub contents
API instead.
