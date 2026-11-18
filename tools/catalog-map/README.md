# Autoindex maps and managed corpora are separate views

`xerj autoindex map` and MCP `xerj_map` read `autoindex-catalog` on the
configured node. That catalog lists datasets written by `xerj autoindex`.
CLI `--dataset` is a catalog dataset slug; `--prefix` scopes autoindex index
names. MCP `index` is a catalog index name or glob, not a managed corpus slug.

`xerj corpus list` lists corpora configured for the client process. A selected
corpus is queried through `xerj code <corpus> <query>` or MCP
`xerj_code_search`. The client's registry can differ from the node's environment
and does not prove that a corpus is indexed/reachable. The node's `_cat/indices`
lists node index names, including indices outside autoindex. These views are
not interchangeable and a managed corpus need not have an autoindex catalog row.

An empty map therefore does not establish that managed corpus data is absent.
Empty MCP maps keep `matched: 0` and `indexes: []`, adding a top-level
`namespace_hint`. A missing catalog (HTTP 404) stays an error with scope
guidance. CLI empty/missing results keep exit 1; diagnostics go to stderr,
including with `--json`, leaving stdout unmodified. HTTP 401/403, transport and
other server failures remain errors without being relabelled as empty catalogs.
Positive dataset maps do not gain `namespace_hint` or synthetic dataset rows.

No managed-corpus registry or index is probed to generate this hint. No extra
network requests, automatic indexing, index selection, fallback search or
schema/mapping migration are introduced. A corpus and an autoindex dataset
may happen to share a name; name similarity does not establish ownership or
equivalence. An empty page can also follow pagination/filtering, so use the
returned pattern, offset/limit and omission information when diagnosing a map.

## Reproduce the transport checks

From the repository root, with Python 3 and a built binary:

```sh
python3 tools/catalog-map/smoke.py --binary engine/target/release/xerj
```

The script uses only a private loopback HTTP fixture and temporary client cwd.
It does not run a node, read corpus configuration, index data or contact an
existing server. Ten CLI/MCP cases cover empty results, missing catalogs,
401/403 refusals and populated datasets; CLI also verifies `--json` stdout.
Requests must remain reads of `/`, catalog `_search` or dataset `_mapping`.
The client/fixture processes stop and temporary files are removed on completion.
These fixtures do not establish live managed-corpus availability or relevance.

MCP unit tests also cover additive empty-result guidance, unchanged positive
maps and real loopback 404/403 tool-error classification. The description is
generated into the published MCP schema using `scripts/mcp-schema-check.sh`;
do not edit that JSON by hand.

Reference before implementation: baseline `9cf3f29c`,
`engine/crates/xerj-autoindex/src/lib.rs:12857` (CLI map),
`engine/crates/xerj-autoindex/src/esclient.rs:2452` (`search_present` distinguishes
HTTP 404 from other errors), `engine/crates/xerj-mcp/src/map.rs:360` (bounded
map rendering), and `tools/xerj-code/README.md:103` (client-side corpus listing).
Existing query, renderer and error helpers are preserved; no external code copied.
