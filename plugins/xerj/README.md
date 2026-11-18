# XERJ plugin for Claude Code

XERJ: the search engine for AI agents. A new way for AI to search for data:
one local Rust binary that indexes your code, docs, specs and logs, plus
Corpus Hub packs, and returns cited answers.

This plugin registers one MCP server, `xerj mcp`, over stdio. It needs the
`xerj` binary on your PATH and, for the local-index tools, a running node.

```bash
# 1. Install the binary: https://xerj.org/docs/install
# 2. Start a node
xerj --insecure --data-dir ~/xerj-data
# 3. Add this marketplace and install the plugin
claude plugin marketplace add xerj-org/xerj
claude plugin install xerj@xerj
```

`XERJ_URL` defaults to `http://localhost:9200`. For a node with auth on, set
`XERJ_AUTH` (for example `ApiKey <key>`) in your environment; on a loopback
node the key is read from `<data-dir>/admin.key`, so you can leave it unset.
The Corpus Hub tools work without a local index.

The tool list and schemas are in
[`landing/docs/agents/schemas/mcp-tools.json`](../../landing/docs/agents/schemas/mcp-tools.json).
