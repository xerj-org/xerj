# Get the XERJ-Query model and serve it

What this file is: the shortest path from no model to a verified answer out of
`/_facts`. Last updated 2026-11-05 (rc.101). When this file and the
binary disagree, the binary wins (`xerj --help`). Background:
[MODEL_BUNDLE.md](./MODEL_BUNDLE.md) (what the model directory is),
[FACTS_API.md](./FACTS_API.md) (the full request/response contract).

The model is XERJ-Query, served on CPU. It answers facts and counting questions
over facts you have written to a namespace; it is not the search embedder
(search's default embedder is lexical feature-hashing — a separate thing). It is
**off by default**: nothing about a node changes until you start it with
`--facts-model-dir`.

Every step below ends with a check and the output you should see.

## 1. Install the binary

```sh
curl -fsSL https://xerj.org/get | sh      # Linux / macOS
# Windows PowerShell: irm https://xerj.org/get.ps1 | iex
```

Check: `xerj --version` prints `xerj 1.0.0-rc.101` (or later). If the shell says
`command not found`, the installer did not edit your profile — call it by full
path (`$HOME/.local/bin/xerj`) or `export PATH="$HOME/.local/bin:$PATH"`.

## 2. Get the model bundle

The installer can fetch it for you (about 462 MiB, checksum-verified, into
`~/.local/share/xerj/model`):

```sh
curl -fsSL https://xerj.org/get | sh -s -- --with-model
```

Or download the release asset yourself, verify it, and unpack it:

```sh
curl -fsSL -O https://github.com/xerj-org/xerj/releases/download/v1.0.0-rc.101/xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz
curl -fsSL -O https://github.com/xerj-org/xerj/releases/download/v1.0.0-rc.101/xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz.sha256
sha256sum -c xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz.sha256   # -> ...: OK
mkdir -p ~/xerj-model
tar -xzf xerj-model-bundle-v1-v1.0.0-rc.101.tar.gz -C ~/xerj-model --strip-components=1
```

Check: the folder has a `bundle.json` and a `facts/` directory.

```sh
jq -r .schema ~/xerj-model/bundle.json
# -> xerj-model-bundle-v1
```

## 3. Start a node pointed at the model

There is no `serve` subcommand: start the bare `xerj` process with
`--facts-model-dir`. That flag is what turns XERJ-Query on; without it the node
has no `/_facts` route. (`XERJ_FACTS_MODEL_DIR=~/xerj-model` is the environment
equivalent; the flag wins.) For a quick local trial use a dev node (`--insecure`,
no auth); for anything real, start with auth/TLS as the CLI reference describes.
A foreground boot can be awkward on some hosts — start it in the background and
poll, do not block on it.

```sh
xerj --facts-model-dir ~/xerj-model --insecure --data-dir ~/xerj-data &
```

Check: the model loaded (the first poll may still say it is loading; the bundle
takes a moment).

```sh
curl -s localhost:9200/_facts | jq '{state: .model.state, counting: .model.counting}'
# -> state "loaded"; counting reported on (a plain facts export without a counting role reports COUNTING_OFF)
```

If `/_facts` answers `404 xerj_query_not_enabled`, the node was started without
the flag. If it answers `503 MODEL_UNAVAILABLE`, the model directory is missing
or failed a load check — the error names which. Model errors are sticky until
the node restarts.

## 4. Write a couple of facts

The model answers over facts you give it. Write one document and two facts into a
namespace (here `demo`):

```sh
curl -s localhost:9200/_facts/demo/_bulk -H 'content-type: application/json' -d '{
  "documents": [{"doc_id": "d1", "version": "v1",
                 "text": "Ana Bel is 170 cm tall. Ben Cor is 182 cm tall."}],
  "facts": [
    {"fact_id": "f1", "subject": "Ana Bel", "relation": "height", "kind": "number",
     "value": "170", "unit": "cm", "doc_id": "d1", "version": "v1", "start": 11, "end": 14, "quote": "170"},
    {"fact_id": "f2", "subject": "Ben Cor", "relation": "height", "kind": "number",
     "value": "182", "unit": "cm", "doc_id": "d1", "version": "v1", "start": 40, "end": 43, "quote": "182"}
  ]
}' | jq '{committed}'
# -> a committed revision, e.g. {"committed": {"to_rev": 1, ...}}
```

Check: `curl -s localhost:9200/_facts/demo | jq '{revision, published_rev}'`
reports the revision you just committed.

## 5. Ask a question and read the verified answer

```sh
curl -s localhost:9200/_facts/demo/_ask -H 'content-type: application/json' \
  -d '{"question": "How tall is Ana Bel?"}' | jq '{final_status, final_answer, path}'
# -> {"final_status": "ANSWER", "final_answer": "170 cm", "path": "facts"}
```

**Read `final_status` and `final_answer`, not `status`/`answer`** — the plain
fields are the model's unchecked program. A counting question (e.g. "How many
people are over 175 cm?") comes back with `path: "counting"` and a counting block
carrying `program_sha256` and `partner_agreement`.

Check a historical question by revision — the engine does not infer history from
`item_id`, so name the revision:

```sh
curl -s localhost:9200/_facts/demo/_ask -H 'content-type: application/json' \
  -d '{"question": "How tall is Ana Bel?", "revision": 1}' | jq '.final_answer'
```

## 6. Or ask over MCP (no shell needed after setup)

`xerj mcp` serves the `xerj_facts_ask` tool — a thin, read-only proxy of
`POST /_facts/{ns}/_ask` — when the node it talks to was started with
`--facts-model-dir` (it asks the node once at start-up; `XERJ_MCP_FACTS=on|off`
decides instead). Register the MCP server once (per client), then call it with
`{"namespace": "demo", "question": "..."}`.

- Claude Code: `xerj init` writes `.mcp.json` and prints
  `claude mcp add --scope user xerj -- "$(command -v xerj)" mcp`.
- Cursor / Windsurf / Claude Desktop / Cline — root key `mcpServers`:
  `{"mcpServers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"],
   "env": {"XERJ_URL": "http://localhost:9200"}}}}`
- VS Code (`.vscode/mcp.json`) — root key `servers` (a `mcpServers` block is
  silently ignored):
  `{"servers": {"xerj": {"type": "stdio", "command": "/abs/path/to/xerj", "args": ["mcp"]}}}`

Check: after restarting the client, `xerj_facts_ask` is in the tool list. If
`xerj_search` is listed and `xerj_facts_ask` is not, the node `xerj mcp` reached
was started without `--facts-model-dir` (or was not up when `xerj mcp` started —
restart the client, or set `XERJ_MCP_FACTS=on`).
