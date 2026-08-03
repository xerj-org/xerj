# The XERJ second brain

A second brain is a relationship layer over documents that already exist in
XERJ. It is not a graph database. There is no query language, no shortest-path,
no PageRank and no unbounded traversal. Edges are ordinary XERJ documents, and
traversal is a bounded, batched, columnar read of at most two hops per call.

The implementation contract is
[`docs/design/SECOND_BRAIN_SPEC.md`](./design/SECOND_BRAIN_SPEC.md); the source
is authoritative. A worked end-to-end run against this repository's own `docs/`
folder is in [`docs/usecases/second-brain/`](./usecases/second-brain/README.md).

## What a brain is

A brain is a name, plus two reserved indices derived from that name.

| Role | Index | Source |
|---|---|---|
| Edges | `.xerj-memory-{brain}-edges` | `authz::brain_edges_index` (`engine/crates/xerj-api/src/authz.rs:153`) |
| Nodes (default) | `.xerj-memory-{brain}` | `authz::memory_namespace_index` (`engine/crates/xerj-api/src/authz.rs:159`) |

The reserved prefix is `.xerj-memory-`
(`engine/crates/xerj-common/src/types.rs:190`), which is the same namespace the
agent-memory API uses. A brain's default nodes index is therefore the
agent-memory namespace of the same name.

Nodes are just documents. The graph layer never interprets them, it only stores
their ids as `src` and `dst` strings. Anything with a document id can be a node,
including agent-memory entries and autoindex section documents.

Edges are ordinary documents too. They live in the edges index with an explicit
mapping and ride the normal write path (WAL, memtable, segment, doc values), so
replay, flush, merge and snapshots work on them because they are documents. The
mapping is written identically by both writers, the HTTP handler
(`graph_api::edge_index_mapping`, `engine/crates/xerj-api/src/graph_api.rs:205`)
and the autoindex detector pipeline
(`detect::edge_index_mapping`, `engine/crates/xerj-autoindex/src/detect/mod.rs:101`):

| Field | Type |
|---|---|
| `edge_id`, `src`, `dst`, `type`, `detector`, `src_file`, `src_format`, `dst_format` | `keyword` |
| `weight`, `confidence` | `float` |
| `valid_at`, `invalid_at`, `created_at`, `expired_at` | `date`, format `epoch_millis` |
| `schema_version` | `integer` |
| `evidence.quote` | `text` |
| `evidence.source` | `keyword` |
| `evidence.offset` | `long` |

Timestamps are stored as epoch-millisecond numbers, never as ISO strings.

The `_id` of an edge document is derived, not random:

```
edge_id = xxh3_128("xg1\0" src "\0" type "\0" dst "\0" decimal(valid_at_ms))
```

rendered as 32 lowercase hex characters. Both writers pin the same function
(`graph_api.rs:185`, `detect/mod.rs:45`). The consequence is that the same
`(src, type, dst, valid_at)` produces the same document id, so re-asserting an
edge overwrites instead of duplicating, and a re-run of the detectors over an
unchanged corpus converges instead of growing.

Each edges index also holds one reserved meta document with `_id`
`__xerj-brain-meta` (`graph_api.rs:110`). It carries `meta_version`, `brain`,
`nodes_index` and `created_at`. It has no `src` or `dst`, and every count and
traversal filters on `exists: src`, so the meta document never appears as an
edge.

Brain names are validated the same way in both crates (`graph_api.rs:150`,
`detect/mod.rs:69`): non-empty, at most 200 characters, first character a
lowercase letter or digit, remaining characters from `a-z 0-9 _ - .`, no `..`,
and the name must not end in `-edges` (that suffix is reserved for the edge
index of the brain without it).

## Creating one

Two commands build a brain from a folder. Both take `--brain <NAME>`.

```
xerj autoindex <folder> --brain <NAME>
xerj brain <folder> [--brain <NAME>]
```

`xerj autoindex` is the discovery and indexing pipeline. Its help text
(`engine/crates/xerj-autoindex/src/cli.rs:82`) documents the flag as:

```
--brain <NAME>       second-brain name; relationship edges land in
                     .xerj-memory-<NAME>-edges (default: folder name slug)
--no-graph           skip relationship detection (wikilinks, local links,
                     section order, directory chains) — no edges are written
```

When `--brain` is omitted the name is derived from the root folder's basename
(`xerj_autoindex::derive_brain_name`, `engine/crates/xerj-autoindex/src/lib.rs:429`),
falling back to `brain` when the basename sanitizes to nothing. The name is
validated at argument-parse time, so a bad name fails before the indexing work
starts (`cli.rs:211`).

`xerj brain <folder>` is the one-command form: it boots or attaches to a server,
runs the same pipeline with graph detection on, and opens the console
(`engine/crates/xerj-server/src/brain.rs:61`, dispatched at
`engine/crates/xerj-server/src/main.rs:1560`). Its own `--brain <NAME>` defaults
to the folder's name.

You can also build a brain with no folder at all, by asserting edges through the
HTTP API. The edges index is created lazily on the first `link` call.

## The endpoints

All four graph routes are mounted on the ES-compatible router, alongside the
agent-memory routes (`engine/crates/xerj-api/src/router.rs:769`):

```
POST   /_memory/{namespace}                store
GET    /_memory/{namespace}                list
DELETE /_memory/{namespace}                drop the namespace
POST   /_memory/{namespace}/_recall        recall
DELETE /_memory/{namespace}/{id}           forget one

POST   /_graph/{brain}/link                assert an edge
DELETE /_graph/{brain}/link/{edge_id}      retire an edge
GET    /_graph/{brain}/ego                 bounded neighbourhood
GET    /_graph/{brain}/overview            brain-level stats
```

Graph errors use one shape (`graph_api.rs:238`):

```json
{"error": {"type": "graph_error", "reason": "<message>"}, "status": 400}
```

The examples below assume a server started with `--insecure`, which is open
mode: one user, no configuration, no credential needed. On a secured server add
`-H "Authorization: ApiKey <key>"`; see the authorization section.

### POST /_graph/{brain}/link

Asserts an edge. Creates the edges index and its meta document on first use.
Returns 201 when the derived id is new and 200 when the same
`(src, type, dst, valid_at)` is re-asserted.

```bash
curl -s -X POST 'http://localhost:9200/_graph/notes/link' \
  -H 'Content-Type: application/json' \
  -d '{
    "src": "note-fermentation",
    "dst": "note-sourdough",
    "type": "relates_to",
    "evidence": {"quote": "sourdough is a wild fermentation", "source": "notes/bread.md", "offset": 412}
  }'
```

Body fields (`LinkBody`, `graph_api.rs:377`). Unknown fields are rejected with a
400, so a typo cannot silently drop part of an assertion.

| Field | Required | Default | Notes |
|---|---|---|---|
| `src` | yes | | non-empty; `src == dst` is a 400 |
| `dst` | yes | | non-empty |
| `type` | yes | | non-empty |
| `weight` | no | `1.0` | must be finite, clamped to `[0, 1]` |
| `confidence` | no | `1.0` | must be finite, clamped to `[0, 1]` |
| `valid_at` | no | server now | epoch-ms number or RFC3339 string |
| `created_at` | no | server now | epoch-ms number or RFC3339 string |
| `detector` | no | `"manual@1"` | free string |
| `evidence` | no | omitted | must be an object; `{quote, source, offset}` is the shape the readers expect |

When `evidence.source` is a string it is mirrored to the top-level `src_file`
field (`graph_api.rs:514`), which is what makes replacement-style invalidation a
doc-values term query rather than an object scan.

The response echoes the stored document:

```json
{"brain": "notes", "edge_id": "<32 hex chars>", "created": true, "edge": { ... }}
```

### DELETE /_graph/{brain}/link/{edge_id}

Retires an edge. It never removes the document. The same document is re-indexed
under the same `_id` with `invalid_at` and `expired_at` added
(`graph_api.rs:567`).

```bash
curl -s -X DELETE 'http://localhost:9200/_graph/notes/link/<edge_id>?invalid_at=2026-07-01T00:00:00Z'
```

`invalid_at` accepts an epoch-ms number or an RFC3339 string and defaults to
server now. `expired_at` is always server now and is not caller-settable.

Retiring an already-retired edge is a no-op that reports the standing fact:

```json
{"brain": "notes", "edge_id": "...", "invalidated": false, "already_invalid_at": 1751328000000}
```

An unknown edge id, or an unknown brain, is a 404 with
`"edge '<id>' does not exist in brain '<brain>'"`.

### GET /_graph/{brain}/ego

The bounded neighbourhood of one node, or of several seeds.

```bash
curl -s 'http://localhost:9200/_graph/notes/ego?node=note-sourdough&hops=2&include_nodes=true'
```

Query parameters (`EgoParams`, `graph_api.rs:665`):

| Parameter | Default | Notes |
|---|---|---|
| `node` | | the node id to expand from; mutually exclusive with `nodes` |
| `nodes` | | comma-separated seed ids; deduped, capped at 64, the clip counted in `not_shown.frontier_clipped` |
| `hops` | `1` | `1` or `2` only; anything else is a 400 |
| `direction` | `both` | `out`, `in` or `both` |
| `types` | all types | comma-separated edge-type allowlist |
| `limit` | `100` | clamped to `1..=1000` |
| `as_of` | now | epoch-ms number or RFC3339 string |
| `include_expired` | `false` | also return retired edges |
| `include_nodes` | `false` | hydrate node summaries from the nodes index |
| `nodes_index` | brain meta, else `.xerj-memory-{brain}` | authorized separately, see below |
| `include_evidence` | `true` | hydrate `evidence`, `detector`, `confidence`, `created_at`, `expired_at` on returned edges |

Omitting both `node` and `nodes` is a 400. Supplying both is a 400. A brain with
no edges index is a 404.

The response carries `brain`, `contract` (`"xerj-second-brain/1"`), `seeds`
(plus `node` when there is exactly one seed), `as_of`, `hops`, `direction`,
`edges`, `neighbors`, `not_shown`, and `nodes` when `include_nodes=true`.
Each edge carries `edge_id`, `src`, `dst`, `type`, `weight`, `hop`, `direction`,
`valid_at` and `invalid_at` (JSON null when the edge is live), plus the
hydrated fields when `include_evidence` is on.

`not_shown` is filled on every response and counts what was withheld:
`edges_clipped`, `frontier_clipped`, `expired_excluded`, `type_filtered`,
`segments_without_columns`, `memtable_docs_scanned`, `dangling_nodes` and
`dangling_ids` (up to 50 ids listed verbatim). A node id that resolves to no
document in the nodes index is reported as dangling; the edge is kept and
nothing is invented for the missing node.

Node hydration reads `title`, `text` or `body` (first 160 characters, as
`preview`), and `ax_path` (as `path`). `path` is null for nodes that carry no
`ax_path`, such as hand-written ones. It is not guessed.

### GET /_graph/{brain}/overview

Brain-level totals and breakdowns, built from three searches on the edges index
plus one count on the nodes index.

```bash
curl -s 'http://localhost:9200/_graph/notes/overview?top=10&histogram_interval=day'
```

Query parameters (`OverviewParams`, `graph_api.rs:1081`): `as_of` (default now),
`top` (default 10, clamped `1..=50`), `histogram_interval` (`day` by default,
`hour` accepted, anything else is a 400).

The response carries `brain`, `contract`, `exists`, `as_of`, `nodes_index`,
`nodes.total`, `embedder`, `edges` (`total`, `live`, `invalidated`), `types`,
`detectors`, `hubs.out`, `hubs.in`, `created_over_time`, and a `not_shown`
object reporting each aggregation's unlisted tail
(`types_not_listed`, `detectors_not_listed`, `hubs_out_not_listed`,
`hubs_in_not_listed`).

`total` counts every edge ever asserted; `live` counts the edges visible at
`as_of`; `invalidated` is the difference. `created_over_time` is a
`date_histogram` over `created_at` across all asserted edges, so retiring an
edge does not erase it from the timeline.

`embedder` reports the node store's configured embedder id, and reports
`lexical-feature-hash` for the built-in default (`graph_api.rs:1149`). The
built-in embedder is lexical feature hashing, not a neural model.

A brain that does not exist returns 404 with a short body rather than an error
envelope:

```json
{"brain": "notes", "contract": "xerj-second-brain/1", "exists": false}
```

### Recall coupled to the graph

`POST /_memory/{namespace}/_recall` takes an optional `graph` object
(`GraphRecallOpts`, `engine/crates/xerj-api/src/memory_api.rs:454`). The brain
of a namespace is the namespace itself: recall expands over
`.xerj-memory-{namespace}-edges`. When `graph` is absent, recall behaves exactly
as it did before the graph existed.

```bash
curl -s -X POST 'http://localhost:9200/_memory/notes/_recall' \
  -H 'Content-Type: application/json' \
  -d '{"query": "fermentation", "k": 10,
       "graph": {"mode": "blend", "hops": 2, "weight": 0.3}}'
```

| Field | Required | Default | Notes |
|---|---|---|---|
| `mode` | yes | | `restrict` or `blend`; anything else is a 400 |
| `seeds` | for `restrict` | for `blend`, the ids of the top 5 base-recall hits | `restrict` with absent or empty seeds is a 400 |
| `hops` | no | `1` | `1` or `2` |
| `types` | no | all | edge-type allowlist |
| `weight` | no | `0.3` | blend weight, clamped to `[0, 1]`; ignored for `restrict` |
| `as_of` | no | now | epoch-ms number or RFC3339 string |

`restrict` narrows recall to the documents reachable from the seeds. `blend`
leaves the candidate set alone and re-ranks it by
`(1 - w) * normalized_score + w * proximity`, where proximity is `1.0` for a
seed and `0.5^hop * clamp(weight, 0, 1)` for a node reached at that hop, taking
the maximum over paths (`memory_api.rs:916`). Blended hits carry a
`graph_proximity` field.

The response gains a `graph` object with `mode`, `seeds`, `hops`, `as_of`,
`reachable` (a count) and its own `not_shown`. If the brain has no edges index,
recall proceeds ungated and `not_shown.no_edges_index` is `true` rather than the
request failing quietly (`memory_api.rs:957`). The reachable set folded into a
`restrict` filter is capped at 10,000 ids (`memory_api.rs:478`).

Note that `/_memory/{namespace}/_recall` requires exactly one of `query` or
`vector`, and rejects unknown fields with a 400.

## Evidence on links

Every edge the detectors emit carries the text that taught it. The assembled
document sets `evidence.quote`, `evidence.source` and `evidence.offset` for
every detected edge (`detect/mod.rs:570`), where `source` is the root-relative
path of the file the edge was read from and `offset` is the byte offset within
the section text. The quote is the full trimmed line containing that offset,
clipped to 240 characters (`detect::line_at`, `detect/mod.rs:416`). The same
path is mirrored into the top-level `src_file` field.

Two of the seven detectors do not have an author's sentence to quote, because
they fire on file layout rather than on text. They record a generated rationale
in the same field instead, with offset 0:

- `samedir` writes `"<a> and <b> share directory <dir>"` (`detect/samedir.rs:54`)
- `sequence` writes `"<label> precedes <label> of <file>"`, or
  `"<label> opens <file>"` for the first section (`detect/sequence.rs:40`)

The console reads that distinction back out and renders it differently: a quote
is shown in quotation marks, a structural rationale is shown prefixed with
`WHY —` and never wearing quotation marks, and an edge with no evidence at all
is shown as `no evidence recorded — asserted, not detected`
(`xerj-ux/src/ux/ego-ledger.js:111`, `xerj-ux/src/ux/brain-map.js:1452`).

That last case is the one that matters for hand-asserted links. `POST
/_graph/{brain}/link` stores `evidence` only when the caller supplies it. When
you assert a link by hand or from an agent without a quote, the stored document
simply has no `evidence` field, and the reader says so. Nothing is fabricated to
fill the gap.

The honest claim is "every link shows its evidence", not "every link has a
quote".

## The seven detectors, and what they do not do

Seven detectors ship. They are the complete contents of
`engine/crates/xerj-autoindex/src/detect/`, registered in
`detect::default_detectors` (`detect/mod.rs:383`):

| Detector | Edge `type` | `detector` tag | Weight | Confidence | Fires on |
|---|---|---|---|---|---|
| `wikilink` | `wikilink` | `wikilink@2` | 1.0 | 0.95 | `[[Target]]` or `[[Target\|alias]]` resolving to a corpus file |
| `mdlink` | `mdlink` | `mdlink@2` | 0.9 | 0.9 | `[text](relative/path.md)` resolving to a corpus file |
| `href` | `href` | `href@2` | 0.7 | 0.85 | `<a href="...">` to a corpus file, html-sniffed files only |
| `pathcite` | `pathcite` | `pathcite@1` | 0.6 | 0.7 | a bare file path in prose that resolves to a corpus file |
| `cratecite` | `cratecite` | `cratecite@1` | 0.5 | 0.6 | a crate directory name in prose, linking to that directory's `Cargo.toml` |
| `sequence` | `sequence` | `sequence@2` | 0.8 | 0.99 | the file card, then each section preceding the next within one file |
| `samedir` | `same_dir` | `samedir@2` | 0.3 | 0.4 | files sharing a directory, chained in path order, never a clique |

The `@N` suffix on the tag is the detector version. It is bumped on any
behaviour change, so old edges stay attributable to the rules that produced
them.

Detectors are deterministic and versioned. There is no LLM in this path. The
same corpus produces a byte-identical edge set on every run, given unchanged
file mtimes, because edge identity is a hash of `(src, type, dst, valid_at)` and
`valid_at` is the source file's mtime rather than the wall clock
(`detect/mod.rs:316`).

**All seven detectors are structural. They find explicit citations and file
layout. None of them looks at what a document is about.** If two documents cover
the same topic but neither links to nor names the other, no detector will
connect them. Point XERJ at a folder of recipes and sourdough will not connect
to kimchi, even though both are fermentation. A corpus of plain-text notes that
do not cross-reference each other produces a graph whose only edges are
`sequence` within each file and `same_dir` between neighbouring files.

This is a known gap, not a bug, and it is tracked as
[issue #164](https://github.com/xerj-org/xerj/issues/164), "Second brain cannot
connect documents by topic, only by explicit citation". A shared-term detector
is proposed there. Until it lands, do not expect topic linking, and read
`same_dir` for what it is: directory co-location, weighted 0.3, the floor of the
detector set, standing in for a topic signal that does not exist yet.

## The hop cap

`hops` is `1` or `2`. Anything else is a 400 whose body carries this exact
sentence (`GRAPH_HOPS_CAP_REASON`, `engine/crates/xerj-engine/src/graph.rs:69`):

```
hops is capped at 2: XERJ's second brain is a relationship layer over documents,
not a graph database (no Cypher, no shortest-path, no variable-depth traversal).
Iterate: expand again from this response's 'reachable' ids.
```

(That is one line in the source and in the response; it is wrapped here to fit.)

`hops=0` is refused by the same check. Called directly on the engine rather
than through HTTP, `hops=0` gets `hops must be at least 1.` prepended to that
same sentence (`graph.rs:242`).

The same string is used by `/_graph/{brain}/ego` (`graph_api.rs:774`) and by the
`graph` block of `/_memory/{namespace}/_recall` (`memory_api.rs:843`), so the
wording does not vary by surface.

Depth beyond two hops composes on the client: take ids from the previous
response and expand again from them. `ego` accepts up to 64 seeds per call
through `nodes=` for exactly this.

Other bounds in the engine, all reported rather than silent
(`graph.rs:55`-`63`, `graph.rs:120`):

| Bound | Value | Reported as |
|---|---|---|
| Hops | 2 | 400 with the sentence above |
| Frontier ids per hop | 4096 | `not_shown.frontier_clipped` |
| Result edges per request | 100,000 | `not_shown.edges_clipped` |
| `ego` seeds per call | 64 | `not_shown.frontier_clipped` |
| `ego` returned edges | `limit`, clamped to 1000 | `not_shown.edges_clipped` |
| `restrict` reachable ids | 10,000 | `graph.not_shown.reachable_clipped` |

A segment that lacks the doc-values columns the hop needs is skipped and counted
in `segments_without_columns`. The hop never falls back to reading `_source`, so
a skipped segment is always reported.

## Bi-temporal behaviour

Links are retired, not deleted. `DELETE /_graph/{brain}/link/{edge_id}`
re-indexes the same document under the same `_id`, adding two timestamps:

- `invalid_at` is when the fact stopped being true. It is caller-settable and
  defaults to server now.
- `expired_at` is when the system recorded that. It is always server now.

Those are the two clocks. `valid_at` and `created_at` are their counterparts on
the assertion side: `valid_at` is when the fact became true, `created_at` is when
the system recorded it.

Visibility at a given instant is decided by `bitemporal_visible`
(`graph.rs:198`):

```
visible(as_of) = valid_at <= as_of AND (invalid_at is absent OR invalid_at > as_of)
```

`include_expired=true` disables only the second half. An edge asserted after
`as_of` stays invisible either way, because it did not exist in the world being
asked about. Edges excluded by this rule are counted in
`not_shown.expired_excluded`.

Passing `as_of` to `ego` or `overview` therefore answers "what did this brain
believe at that moment", including edges that have since been retired. Since a
different `valid_at` yields a different derived `edge_id`, re-asserting a fact at
a new `valid_at` creates a distinct edge rather than mutating the old one, which
is what keeps the history intact.

One consequence to be aware of: because retirement is a re-index of the same
document rather than a delete, an edges index never shrinks through the graph
API. Reclaiming space means deleting documents through the ordinary index APIs,
which discards the history.

## The reserved namespace is a tenant boundary

The `.xerj-memory-*` namespace, which holds every agent-memory namespace and
every second brain, is reserved. Reaching an index in it requires a principal
that holds the matching privilege on that specific index
(`engine/crates/xerj-api/src/authz.rs`, module documentation).

The resource a brain authorizes against is its edges index,
`.xerj-memory-{brain}-edges`. Who can read a brain:

| Principal | Reach |
|---|---|
| open mode (`--insecure`, or no admin key configured) | every brain; one user, no configuration |
| the configured admin key | every brain |
| a key minted with `role_descriptors` naming the edges index | that brain, at the granted privilege |
| a key minted without `role_descriptors` | no brain |
| no or invalid credential | nothing |

`ego` and `overview` need `read`; `link` and `unlink` need `write`. Creating the
edges index lazily counts as writing the brain, not as a separate `manage` step.

To grant brain `alice` to a key, name both of its indices at mint time. The
second name is the nodes index, needed for `ego`'s node hydration and
`overview`'s note count:

```bash
curl -s -X POST 'http://localhost:9200/_security/api_key' \
  -H 'Content-Type: application/json' \
  -d '{"name": "alice-agent",
       "role_descriptors": {"alice": {"indices": [
         {"names": [".xerj-memory-alice-edges", ".xerj-memory-alice"],
          "privileges": ["read", "write"]}]}}}'
```

A denial is an ES-shaped 403 with `error.type` `security_exception`
(`authz.rs:242`), naming the resource, the action and the grant that would fix
it.

Two properties are worth calling out because they are deliberate:

- **Authorization runs before the existence check.** Every graph handler
  authorizes the caller for the brain before it looks at whether the brain
  exists, so a brain that is not yours and a brain that does not exist return
  the same 403 and the status code cannot be used to enumerate brains.
- **`ego?nodes_index=` is authorized separately.** The nodes index can be
  redirected by the caller, and the brain's meta document (writable by anyone
  with `write` on the brain) can point it anywhere, so hydration is authorized
  against the resolved index in its own right. Without that, `write` on one
  brain would be a read primitive for every index on the node.

Enforcement is not limited to these four routes. A brain's edges live in an
ordinary index whose name merely starts with a dot, so it is nameable through
the generic ES-compatible and native index routes. Those doors are closed at two
layers that a handler cannot route around: the authorization middleware, which
resolves targets from the request path and from request bodies (`_bulk`,
`_msearch`, `_mget`, `_aliases`, `_reindex`, index templates and others), and
the engine's index guard, which is inside the only funnel to index data. The
second layer answers "not found" rather than "forbidden", which is what lets
fan-out verbs such as `POST /_search` and `_cat/indices` filter instead of
failing.

Two limits stated plainly in the source:

- This makes the reserved namespace a real boundary and confines a scoped key to
  its grants. It does not turn XERJ into a general multi-tenant authorization
  system. A key minted without `role_descriptors` keeps its historical reach over
  ordinary, non-reserved indices.
- An explicit grant is not second-guessed. A scoped key minted with
  `names: ["*"]` can reach the reserved namespace, because `*` matches every
  index and that is what the operator asked for. Only a superuser can mint one.
  Operators who want brain isolation must grant concrete names, or a prefix that
  excludes `.xerj-memory-`.

The module documentation at the top of `memory_api.rs` still describes the
pre-#79 model in which `/_memory/*` had no per-namespace authorization. The
handlers do call `authz::authorize_memory_namespace` (for example
`memory_api.rs:318` and `memory_api.rs:490`); the comment is stale, the code is
not.

## What is not covered here

- Console and MCP surfaces for the second brain exist but are not documented in
  this file.
- Edge fields `src_format` and `dst_format` are written by the detectors and are
  in the mapping, but no endpoint documented above returns them in a shaped
  response field of their own; they arrive as part of an edge's `_source` when
  read through the ordinary search API.
- There are no published performance figures for graph expansion in this
  repository, so none are quoted here.
