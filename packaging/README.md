# Packaging and listings

Files that MCP catalogues and agent tools read to find and install XERJ.
New listings should use this description:

> XERJ: the search engine for AI agents. A new way for AI to search for
> data: one local Rust binary that indexes your code, docs, specs and logs,
> plus Corpus Hub packs, and returns cited answers.

| File | Read by | Notes |
| --- | --- | --- |
| [`/server.json`](../server.json) | Official MCP Registry (`registry.modelcontextprotocol.io`), and through it the VS Code / GitHub MCP gallery and PulseMCP | Namespace `io.github.xerj-org/xerj`. The mcpb package entry must point at a bundle that is attached to a release. |
| [`mcpb/manifest.json`](mcpb/manifest.json), [`mcpb/xerj-mcp.sh`](mcpb/xerj-mcp.sh) | MCP Bundle clients (Claude Desktop and others), Smithery uploads | Manifest template and launcher; `scripts/mcp/build_mcpb.py` (the only bundle builder) fills in the version and the tool list. |
| [`docker-mcp/Dockerfile`](docker-mcp/Dockerfile) | Docker MCP Catalog (`docker/mcp-registry`) | Runs `xerj mcp` over stdio from the published musl binary. |
| [`/.claude-plugin/marketplace.json`](../.claude-plugin/marketplace.json) and [`/plugins/xerj`](../plugins/xerj) | Claude Code (`claude plugin marketplace add xerj-org/xerj`), Anthropic's directory portal, cursor.directory plugins | One plugin with one MCP server, `xerj mcp`. |
| [`/glama.json`](../glama.json) | Glama | Claims the listing for the `xerj-org` maintainer. |

## Release a new MCP bundle

The release workflow does not build an `.mcpb` yet, so it is a manual step
after a release is published:

```bash
python3 scripts/mcp/build_mcpb.py --tag v1.0.0-rc.104 --out dist --update-server-json
gh release create mcpb-v1.0.0-rc.104 dist/xerj-mcp-1.0.0-rc.104.mcpb -R xerj-org/xerj
mcp-publisher login github     # device-code login as xerj-org
mcp-publisher publish
```

The script downloads the release archives (or reads them from `--assets
<dir>`), checks each against its `.sha256` asset, and zips the five
binaries (macOS arm64/x86_64, Linux arm64/x86_64 musl, Windows x86_64)
with the manifest and the `xerj-mcp.sh` launcher, which picks the binary
for the host CPU. Entries are sorted with a fixed timestamp, so the same
release gives the same bytes. `fileSha256` in `server.json` is the hash of
the exact file it wrote, so upload that file rather than rebuilding it.
Full steps, Smithery and the DNS namespace: [`deploy/mcp-registry/README.md`](../deploy/mcp-registry/README.md).
