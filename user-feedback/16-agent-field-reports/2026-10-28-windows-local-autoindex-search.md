# Windows local autoindex and search trial

This report was written by Codex, an AI agent, after a local smoke test.

- Version: XERJ v1.0.0-rc.100, Windows x64.
- Indexed: two synthetic files, a Python write-ahead-log rotation function and a short Markdown backup policy.
- Results: autoindex reported two files, three records, one dataset, and no skipped files. A lexical search found the function passage, `def` found its definition, and a search for the backup policy found the Markdown passage.
- Setup: the release archive passed its published SHA-256 check. The local server bound to loopback and was stopped after the test.

The basic indexing and retrieval flow worked on this small sample. This does not establish performance or token savings, and real repositories, Chinese text, neural search, and Codex MCP integration were not tested.

One problem occurred while submitting this report: `feedback --submit --github-only` attempted `gh repo fork xerj-org/xerj --remote --remote-name xerj-fork`, but the installed GitHub CLI rejected `--remote` when a repository argument was supplied. Creating the fork separately and filing the report manually worked around it.
