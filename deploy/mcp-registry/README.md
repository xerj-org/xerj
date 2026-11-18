# Listing XERJ on the official MCP Registry

`server.json` at the repo root describes XERJ for
[registry.modelcontextprotocol.io](https://registry.modelcontextprotocol.io) as
`io.github.xerj-org/xerj`. XERJ ships no npm, PyPI, crates.io or OCI package,
so the registry entry points at an **MCP Bundle** (`.mcpb`) attached to the
GitHub Release. MCP clients that install from the registry (Claude Desktop and
others) download that bundle, check its SHA-256 and run `xerj mcp`.

| File | Role |
|---|---|
| `manifest.template.json` | MCPB manifest (spec 0.3); version and tool list are filled at build time, tools from `landing/docs/agents/schemas/mcp-tools.json` |
| `xerj-mcp.sh` | launcher: picks the macOS/Linux binary for this CPU and runs `xerj mcp` |
| `build-mcpb.py` | builds `dist/xerj-mcp-<version>.mcpb` from the release archives and rewrites `server.json` (version, URL, `fileSha256`) |

The bundle holds the five release binaries (macOS arm64/x86_64, Linux
arm64/x86_64 musl, Windows x86_64), about 150 MB. Entries are sorted and
timestamped at a fixed date, so the same inputs give the same hash. The
registry asset URL must contain `mcp`, which is why the file is
`xerj-mcp-<version>.mcpb`.

The bundle runs the MCP server only. It needs a XERJ node already running
(`xerj --insecure --data-dir ~/xerj-data`); the user sets its URL in the
client (`XERJ_URL`, default `http://localhost:9200`).

## Publish a version

`mcp-publisher login github` is interactive (GitHub device flow). `xerj-org` is a
GitHub **user** account, not an organization, so `io.github.xerj-org/*` is that
account's personal namespace: sign in to GitHub as `xerj-org` when you approve
the device code. Signing in as any other account grants only that account's
namespace, and publishing fails with a permission error.

```sh
V=1.0.0-rc.97
gh release download v$V -R xerj-org/xerj -D dist/mcpb-in \
  -p '*apple-darwin.tar.gz' -p '*linux-musl.tar.gz' \
  -p '*x86_64-pc-windows-msvc.zip' -p '*.sha256'
python3 deploy/mcp-registry/build-mcpb.py --version $V --assets dist/mcpb-in
gh release upload v$V dist/xerj-mcp-$V.mcpb -R xerj-org/xerj

# mcp-publisher: brew install mcp-publisher, or the release binary
curl -L "https://github.com/modelcontextprotocol/registry/releases/latest/download/mcp-publisher_$(uname -s | tr '[:upper:]' '[:lower:]')_$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/').tar.gz" | tar xz mcp-publisher

./mcp-publisher validate
./mcp-publisher login github     # device flow: open github.com/login/device, enter the code
./mcp-publisher publish
curl -s "https://registry.modelcontextprotocol.io/v0.1/servers?search=io.github.xerj-org/xerj"
```

Commit the rewritten `server.json` afterwards so the repo matches what the
registry holds. Each registry version is immutable; publish a new one per release.

## Optional: `org.xerj/xerj` via DNS instead of GitHub

To publish under the domain namespace, change `name` to `org.xerj/xerj`, then:

```sh
openssl genpkey -algorithm Ed25519 -out mcp-key.pem      # keep private, never commit
echo "xerj.org. IN TXT \"v=MCPv1; k=ed25519; p=$(openssl pkey -in mcp-key.pem -pubout -outform DER | tail -c 32 | base64)\""
# add that TXT record on the apex xerj.org (not a subdomain), wait for DNS, then:
./mcp-publisher login dns --domain xerj.org --private-key "$(openssl pkey -in mcp-key.pem -noout -text | grep -A3 priv: | tail -n +2 | tr -d ' :\n')"
./mcp-publisher publish
```

## Other directories

- **Smithery** (`smithery mcp publish dist/xerj-mcp-<version>.mcpb -n xerj-org/xerj`):
  its CLI currently rejects a bundle whose manifest lists `tools`
  ([smithery-ai/cli#787](https://github.com/smithery-ai/cli/issues/787)). Build a
  separate copy with `--omit-tools --out dist/smithery` for it. Do not upload that copy as
  the registry asset, because its hash differs.
- **Glama** reads `glama.json` at the repo root to assign maintainers.

## Automating it later

`release.yml` could build and upload the bundle after the binaries, then publish
with GitHub OIDC (`mcp-publisher login github-oidc`, needs `permissions: id-token: write`).
The steps are the same as above; see the registry's
[GitHub Actions guide](https://github.com/modelcontextprotocol/registry/blob/main/docs/modelcontextprotocol-io/github-actions.mdx).
