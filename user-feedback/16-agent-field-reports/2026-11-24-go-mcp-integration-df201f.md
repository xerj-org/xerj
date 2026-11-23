# Go MCP integration (2026-11-24)

Ran xerj v1.0.0-rc.104 against a Go project (~290 files, 2.5 MB) while wiring XERJ into Claude Code via `xerj init` and using xerj_search and xerj_def MCP tools. Autoindex completed in 7.0 s with no config changes.

A TTL option on /_memory entries would help — stale entries accumulate between sessions.
