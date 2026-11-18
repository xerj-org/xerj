# Xerj Packs

A **pack** is a stable documentation group for MCP tools. It does not change
the MCP wire protocol and it is not a separate install: one `xerj` binary and
one MCP registration expose every available pack.

Run the catalog from the installed binary:

```sh
xerj tools list
xerj tools list --json
```

The command reads the same specifications as the MCP `tools/list` response.
It therefore follows build capabilities such as whether the hub signing key
is present. Counts and catalog token sizes are computed for the current
release; do not copy numbers into clients or prompt text.

`word-runs-v1` counts contiguous alphanumeric/underscore runs in each complete,
compact JSON tool specification. It is a deterministic documentation-size
proxy, not a model's BPE token count.

## Pack map

| Pack | Tool names | Status |
| --- | --- | --- |
| `core` | `xerj_*` tools other than the pack-specific prefixes below | Shipping |
| `hub` | `xerj_hub_*` | Shipping in builds with the hub signing key |
| `writing` | `xerj_write_*` / `xerj_writing_*` | Planned in this branch of the release unless `tools list` reports it |
| `bio` | `xerj_bio_*` | Planned unless reported by `tools list` |
| `cyber` | `xerj_cyber_*` | Planned unless reported by `tools list`; authorization and scope are required |

The catalog is authoritative for capability status. A pack with zero tools is
still shown so release-to-release changes are visible and the site never
silently describes a planned tool as shipping.

## Recipe workflow

Recipes coordinate pack tools; they do not replace evidence. A recipe should:

1. state the user's goal, sources, authorization and output boundary;
2. select the smallest pack and inspect its current tool schemas;
3. call tools in dependency order, passing the prior result instead of
   re-deriving it;
4. retain the returned evidence or citation alongside every claim;
5. run the pack's review or verification step before presenting a result.

For the writing pack, the canonical recipe is **outline → section → review**.
Treat an outline as an argument and evidence plan, draft one section at a
time, and review citation coverage and unsupported claims before assembly.

## Authoring a recipe

Use stable tool names and the schemas printed by `xerj tools list --json` and
`tools/list`; do not duplicate an input schema in a recipe. A recipe must state
its preconditions and the result that proves success. It must also define what
to do when a tool declines: stop or ask for missing evidence rather than
manufacturing a value.

Every recipe needs a negative control when it adds operational instructions:
an out-of-scope input, missing prerequisite, or denied action must exit
non-zero (or return an explicit refusal) without producing an artifact.

