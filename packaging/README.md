# Packaging and listings

Files that MCP catalogues and agent tools read to find and install XERJ.
New listings should use this description:

> XERJ: the search engine for AI agents. A new way for AI to search for
> data: one local Rust binary that indexes your code, docs, specs and logs,
> plus Corpus Hub packs, and returns cited answers.

| File | Read by | Notes |
| --- | --- | --- |
| [`/server.json`](../server.json) | Official MCP Registry (`registry.modelcontextprotocol.io`), and through it the VS Code / GitHub MCP gallery and PulseMCP | Namespace `io.github.xerj-org/xerj`. The mcpb package entry must point at a bundle that is attached to a release. |
| [`mcpb/manifest.json`](mcpb/manifest.json) | MCP Bundle clients (Claude Desktop and others), Smithery uploads | Template; `@VERSION@` is filled in by `scripts/mcp/build_mcpb.py`. |
| [`docker-mcp/Dockerfile`](docker-mcp/Dockerfile) | Docker MCP Catalog (`docker/mcp-registry`) | Runs `xerj mcp` over stdio from the published musl binary. |
| [`/.claude-plugin/marketplace.json`](../.claude-plugin/marketplace.json) and [`/plugins/xerj`](../plugins/xerj) | Claude Code (`claude plugin marketplace add xerj-org/xerj`), Anthropic's directory portal, cursor.directory plugins | One plugin with one MCP server, `xerj mcp`. |
| [`/glama.json`](../glama.json) | Glama | Claims the listing for the `xerj-org` maintainer. |

## Release a new MCP bundle

The release workflow does not build an `.mcpb` yet, so it is a manual step
after a release is published:

```bash
python3 scripts/mcp/build_mcpb.py --tag v1.0.0-rc.98 --out /tmp/mcpb --update-server-json
gh release upload v1.0.0-rc.98 /tmp/mcpb/xerj-mcp-1.0.0-rc.98.mcpb
mcp-publisher login github     # device-code login as a member of xerj-org
mcp-publisher publish
```

The script downloads the release archives, checks each against its
`.sha256` asset, joins the two macOS builds into one universal binary, and
zips them with the manifest (macOS universal, Linux x86_64, Windows x86_64).
`fileSha256` in `server.json` is the hash of the exact file it wrote, so
upload that file rather than rebuilding it.
