# MCP tool catalog size

What XERJ's MCP tool list costs an agent before it makes a single call
(#1785, acceptance item 2).

Every MCP client loads the server's whole `tools/list` answer (each tool's
name, description and input schema) into the model's context at the start of
a session. That cost is paid once per session whether the agent uses one tool
or none.

## Method

- Input: `landing/docs/agents/schemas/mcp-tools.json`, the live `tools/list`
  response (`scripts/mcp-schema-check.sh --write`), pinned to the server by
  `engine/crates/xerj-mcp/tests/published_schema_drift.rs`.
- Each tool object is serialised as compact JSON, the form it has on the wire.
- Bytes are exact. Tokens are counted with tiktoken 0.14.0 `o200k_base` and
  `cl100k_base`. Claude's tokenizer is not public and every client wraps tools
  in its own prompt format, so the token counts are an estimate, not a bill.
- `no_hub_key` is the same list minus the three `xerj_hub_*` tools, which a
  build with no hub signing key does not serve.
- Not measured: `xerj mcp --http` without `--allow-writes`, which serves a
  smaller read-only subset.

```sh
pip install tiktoken
python3 scripts/mcp/catalog_tokens.py            # print
python3 scripts/mcp/catalog_tokens.py --write    # update results/catalog.json
```

`engine/crates/xerj-mcp/tests/catalog_size.rs` recomputes the bytes from
`tool_specs_with()` and fails when `results/catalog.json` records a different
tool count or bytes more than 10% off, so these numbers cannot quietly go
stale.

## Results (2026-11-22, main 95fb2235a)

| catalog | tools | bytes | o200k_base tokens | cl100k_base tokens |
|---|---:|---:|---:|---:|
| with hub key | 25 | 68,595 | 15,863 | 15,857 |
| no hub key | 22 | 64,158 | 14,837 | 14,837 |

The largest tools (bytes / o200k_base tokens):

| tool | bytes | tokens |
|---|---:|---:|
| `xerj_search` | 10,642 | 2,519 |
| `xerj_hybrid_search` | 7,490 | 1,776 |
| `xerj_batch_search` | 3,835 | 850 |
| `xerj_memory_recall` | 3,705 | 859 |
| `xerj_def` | 3,610 | 813 |

The smallest is `xerj_status` (722 bytes, 153 tokens). The full per-tool
table is in `results/catalog.json`.

What this means: about 16k tokens of the context window go to the tool list
in every session, and `xerj_search` alone is about one sixth of it. The
`xerj_hub_*` tools add about 1,000 tokens. Shrinking the longest descriptions
is the cheapest way to cut this number.
