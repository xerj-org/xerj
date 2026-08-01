# XERJ SECOND BRAIN — IMPLEMENTATION CONTRACT (v1)

Status: FROZEN. Every stream codes against this file. If reality contradicts this
file, fix the file first (one integrator-approved edit), then the code — never
silently diverge. Field names, route paths, JSON shapes, constants, and the
fixture in §8 are normative.

Contract version string: `"xerj-second-brain/1"` (returned by `GET /_graph/{brain}/overview` as `contract`).

---

## 0. Scope, invariants, stream ownership

### What this is
A relationship layer over documents that already exist. Nodes are ordinary XERJ
documents (memory entries, autoindex section docs — any doc, anywhere). Edges
are ordinary documents in one reserved index per brain. Traversal is a bounded,
batched, columnar expansion — not a graph query language.

### Hard invariants (violating any of these is a defect, not a trade-off)
1. **Edges are ordinary documents.** No storage-format change. Edges ride the
   existing WAL → memtable → segment → doc-values path. Boot replay, flush,
   merge, snapshots all work on edges because they are just docs.
2. **Bi-temporal, soft-delete only.** An edge is never physically removed by the
   API. Invalidation sets `invalid_at`/`expired_at`. "What did it believe last
   Tuesday" stays answerable via `as_of`.
3. **Hop path never hydrates `_source`.** Expansion reads doc-values columns
   only. Evidence hydration (ego endpoint) happens AFTER traversal on the
   bounded result set via an `ids` query, never during it.
4. **One column scan per segment per hop for the WHOLE frontier** (two scans
   for direction `both`: one over `src`, one over `dst`). O(1) hash-set
   membership per row. Cost is frontier-size-independent.
5. **Hops hard-capped at 2.** `hops > 2` is a 400 whose message tells the
   caller to iterate: expansion composes — feed `reachable` back in as the next
   frontier.
6. **NOT a graph database.** No Cypher, no shortest-path, no PageRank, no
   variable-depth traversal. Docs and error messages say this explicitly (exact
   wording in §4.6).
7. **Detectors are deterministic and versioned.** No LLM extraction. Same
   corpus in → byte-identical edge set out (given unchanged mtimes). Every
   derived edge carries evidence and a `detector` tag like `wikilink@1`.
8. **Honesty in-band.** Every response carries a `not_shown` object counting
   what was withheld: clipped frontiers, clipped edges, dangling node refs,
   segments skipped, expired edges excluded, agg tails. The default embedder is
   LEXICAL feature-hashing — no surface may imply neural semantics.

### Stream ownership (files each stream may touch — no overlaps)
| Stream | Files |
|---|---|
| ENGINE | `engine/crates/xerj-engine/src/index.rs` (ONE new contiguous section, see §3.1) + `engine/crates/xerj-engine/tests/` |
| API | `engine/crates/xerj-api/src/graph_api.rs` (new), `engine/crates/xerj-api/src/lib.rs` (one `pub mod graph_api;` line), `engine/crates/xerj-api/src/router.rs` (one route block), `engine/crates/xerj-api/src/memory_api.rs` (recall extension §5 + `-edges` suffix guard §1), `engine/crates/xerj-api/Cargo.toml` (add `xxhash-rust.workspace = true`) |
| AUTOINDEX | `engine/crates/xerj-autoindex/src/detect/` (new module), `engine/crates/xerj-autoindex/src/lib.rs` (hooks), `cli.rs` (flags), `Cargo.toml` if needed (xxhash-rust already present) |
| UX | `xerj-ux/src/dashboards/second-brain.js` (new), `xerj-ux/src/dashboards/registry.js`, `engine/crates/xerj-console-api/src/seed.rs` |

Build scoped only, per repo directive:
```
cargo build --release -j 32 -p xerj-engine        # engine stream
cargo build --release -j 32 -p xerj-api           # api stream (pulls engine)
cargo build --release -j 32 -p xerj-autoindex     # autoindex stream
cargo build --release -j 32 -p xerj-server        # integrator smoke
```
NEVER workspace-wide, NEVER `cargo clean`. rustfmt + `clippy -D warnings` must pass.

---

## 1. Naming, validation, index layout

- A **brain** is a namespace string. Validation: identical rules to
  `memory_api::validate_namespace` (lowercase start, `[a-z0-9._-]`, ≤200 chars,
  no `..`) **plus one new rule: the name must not end with `-edges`**.
- Edges index for brain `B`: **`.xerj-memory-{B}-edges`**. The leading dot
  passes `IndexName::validate` (xerj-common/src/types.rs:96 explicitly allows
  it) and is unreachable through user index APIs.
- Default nodes index for brain `B`: **`.xerj-memory-{B}`** (the memory
  namespace of the same name). Autoindex brains override this via the meta doc
  (§6.6); every read endpoint also accepts an explicit `nodes_index` request
  parameter (any index expression the ES-compat search path accepts: name,
  comma-list, wildcard).
- **Collision guard (touches memory_api):** `memory_api::validate_namespace`
  gains the same `-edges` suffix rejection with reason
  `"namespace suffix '-edges' is reserved for graph edge indices"`. Without
  this, `POST /_memory/kb-edges` would write memories into brain `kb`'s edge
  index. This is a deliberate breaking change; record it in the release notes.
- Node ids (`src`/`dst`) are **opaque strings** — document `_id`s in whatever
  index the caller's nodes live in. The graph layer never interprets them on
  the hop path.

---

## 2. EDGE SCHEMA

### 2.1 Exact index mapping (normative JSON)

Created lazily by `graph_api::link` on first write, and by autoindex
`ensure_index` — **both must send exactly this body** (single shared constant
per crate; keep byte-identical):

```json
{
  "mappings": {
    "properties": {
      "edge_id":        { "type": "keyword" },
      "src":            { "type": "keyword" },
      "dst":            { "type": "keyword" },
      "type":           { "type": "keyword" },
      "weight":         { "type": "float" },
      "valid_at":       { "type": "date", "format": "epoch_millis" },
      "invalid_at":     { "type": "date", "format": "epoch_millis" },
      "created_at":     { "type": "date", "format": "epoch_millis" },
      "expired_at":     { "type": "date", "format": "epoch_millis" },
      "detector":       { "type": "keyword" },
      "confidence":     { "type": "float" },
      "schema_version": { "type": "integer" },
      "src_file":       { "type": "keyword" },
      "src_format":     { "type": "keyword" },
      "dst_format":     { "type": "keyword" },
      "evidence": {
        "properties": {
          "quote":  { "type": "text" },
          "source": { "type": "keyword" },
          "offset": { "type": "long" }
        }
      }
    }
  }
}
```

### 2.2 Stored-document type discipline (LOAD-BEARING)

`build_doc_value_columns` (index.rs:12820) types columns from the **raw JSON
values**, not the mapping, and a field that is an array in ANY doc of a segment
ships NO column for that segment. Therefore every edge writer MUST emit:

- `edge_id`, `src`, `dst`, `type`, `detector`, `src_file` — JSON **string**, never array. → `KeywordColumn`.
- `weight`, `confidence` — JSON **number**. → `NumericColumn` (f64 bits).
- `valid_at`, `invalid_at`, `created_at`, `expired_at` — JSON **number, epoch
  milliseconds UTC**. Never ISO strings (a string would make it a
  KeywordColumn and kill the as-of compare). The explicit
  `"format": "epoch_millis"` in the mapping exempts these fields from the
  engine's default-date-format ingest validation (see `date_format_exclusions`,
  index.rs:1586).
- `invalid_at` / `expired_at` / `src_file`: **omit the key entirely when
  unset** — never JSON `null`. Omission lands the row in the column's
  `null_bitmap`, which is exactly the "still valid" signal the hop reads.
- `evidence` — JSON object. Objects produce no doc-values column (by design);
  evidence is read only on hydration paths, never on the hop.
- `src_format` / `dst_format` — JSON **string** when the writer knows the
  endpoint file types (autoindex stamps `CorpusFile.format`: lowercase
  extension, else sniffed family), **key omitted entirely when unknown**
  (API-asserted edges about arbitrary node ids carry neither). Additive
  optional fields — they do NOT bump `schema_version`; both writers stamp the
  same mapping and `1`.
- `schema_version` — the integer `1` for every edge this contract produces.

Every edge document's **`_id` is exactly its `edge_id` field value**. This is
what lets the hop path return edge ids straight from the `edge_id` keyword
column with zero `_source` access, and makes re-writes idempotent overwrites.

### 2.3 edge_id computation (normative)

Hash fn: `xxhash_rust::xxh3::xxh3_128` — already a workspace dependency
(`engine/Cargo.toml:61`, `xxhash-rust = { version = "0.8", features = ["xxh3"] }`),
already used for identity in `xerj-autoindex/src/ids.rs`. The API stream adds
`xxhash-rust.workspace = true` to `engine/crates/xerj-api/Cargo.toml`.

Canonical input, mirroring the `ids::doc_id` framing convention (`ax1` → `xg1`):

```rust
/// edge_id = xxh3_128("xg1\0" src "\0" type "\0" dst "\0" decimal(valid_at_ms)),
/// rendered as 32 lowercase hex chars ({:032x}).
pub fn edge_id(src: &str, edge_type: &str, dst: &str, valid_at_ms: i64) -> String {
    use xxhash_rust::xxh3::xxh3_128;
    let mut input = Vec::with_capacity(16 + src.len() + edge_type.len() + dst.len());
    input.extend_from_slice(b"xg1\x00");
    input.extend_from_slice(src.as_bytes());
    input.push(0);
    input.extend_from_slice(edge_type.as_bytes());
    input.push(0);
    input.extend_from_slice(dst.as_bytes());
    input.push(0);
    input.extend_from_slice(valid_at_ms.to_string().as_bytes());
    format!("{:032x}", xxh3_128(&input))
}
```

Both xerj-api (`graph_api.rs`) and xerj-autoindex (`detect/mod.rs`) implement
this function **byte-identically** (each crate has its own copy — do NOT
introduce a shared crate for one function; a unit test in each crate pins the
fixture vector below). Pin test vector:
`edge_id("note-alpha", "wikilink", "note-beta", 1753600000000) == "bef814a75bd3d914c3e561f610154304"`.

Consequences: same (src, type, dst, valid_at) → same `_id` → bulk `index`
actions overwrite instead of duplicating; re-running a detector over an
unchanged corpus converges (kill -9 safe, same property as `ids::doc_id`).
Re-asserting the same fact at a DIFFERENT `valid_at` creates a distinct edge —
that is the bi-temporal design, not a bug.

### 2.4 Exact stored edge document (normative example)

The wikilink edge `note-alpha → note-beta` from the fixture (§8), as stored:

```json
{
  "edge_id": "bef814a75bd3d914c3e561f610154304",
  "src": "note-alpha",
  "dst": "note-beta",
  "type": "wikilink",
  "weight": 1.0,
  "valid_at": 1753600000000,
  "created_at": 1753600000000,
  "detector": "wikilink@1",
  "confidence": 0.95,
  "schema_version": 1,
  "src_file": "alpha.md",
  "evidence": {
    "quote": "Alpha is the hub note. It links to [[beta]] and [[gamma]].",
    "source": "alpha.md",
    "offset": 35
  }
}
```
(`invalid_at`/`expired_at` absent = edge is live.) After soft invalidation the
same doc is re-indexed under the same `_id` with two added fields, e.g.
`"invalid_at": 1753700000000, "expired_at": 1753700000000`.

### 2.5 Brain meta document

One reserved doc per edges index, `_id` = `__xerj-brain-meta`:

```json
{
  "meta_version": 1,
  "brain": "notes",
  "nodes_index": ".xerj-memory-notes",
  "created_at": 1753600000000
}
```

It has **no** `src`/`dst`/`type`/`edge_id` fields, so it is invisible to the
hop (null `src` → null_bitmap skip) and to every count in §4 (all counts filter
on `exists src`). `graph_api::link` writes it (nodes_index = default) when it
creates the index; autoindex writes it with `nodes_index` = its dataset index
pattern (§6.6). Writers use `create`-if-absent semantics (never clobber an
existing meta doc).

---

## 3. ENGINE API (xerj-engine)

### 3.1 Placement

All engine-side graph code lives in **`engine/crates/xerj-engine/src/index.rs`**
in ONE new contiguous section appended after the existing cache-helper impl
(after the `impl Index` block that ends the prefilter helpers), delimited:

```rust
// ─────────────────────────────────────────────────────────────────────────────
// Graph expansion (second brain) — batched, columnar, bounded
// ─────────────────────────────────────────────────────────────────────────────
```

Rationale (do not re-litigate): the hop needs `dv_columns_for`,
`ghost_positions_for`, `self.store`, `self.memtable`, `self.data_dir` — all
private to the `index` module. A sibling `graph.rs` would force visibility
churn on ~6 fields/helpers; a single in-file section costs nothing and only the
ENGINE stream touches index.rs, so no cross-stream conflicts.

### 3.2 Public types and constants (exact signatures)

```rust
/// Hard ceiling on expansion depth. Depth beyond this composes client-side:
/// feed `reachable` back in as the next call's frontier.
pub const GRAPH_MAX_HOPS: u8 = 2;
/// Frontier ids admitted per hop; excess is dropped and counted
/// (`stats.frontier_clipped`). 4096 ords is a ~32KB hash set — cheap — while
/// bounding the per-segment FST-probe phase.
pub const GRAPH_MAX_FRONTIER: usize = 4096;
/// Result-edge ceiling per request; collection stops (scan continues to the
/// end of the current segment, then aborts remaining segments) and
/// `stats.edges_clipped` reports the overflow.
pub const GRAPH_MAX_RESULT_EDGES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    /// Follow src → dst (scan the `src` column).
    Out,
    /// Follow dst → src (scan the `dst` column).
    In,
    /// Both directions (two column scans per segment). The default everywhere.
    Both,
}

#[derive(Debug, Clone)]
pub struct GraphExpandRequest {
    /// Node ids to expand from. Deduped internally; clipped to
    /// GRAPH_MAX_FRONTIER (clip counted, never silent).
    pub frontier: Vec<String>,
    /// 1 or 2. 0 or >2 → XerjError::InvalidQuery (see §3.5).
    pub hops: u8,
    pub direction: GraphDirection,
    /// Edge-type allowlist; None = all types.
    pub types: Option<Vec<String>>,
    /// Bi-temporal cut: an edge is visible iff
    /// valid_at <= as_of_ms AND (invalid_at absent OR invalid_at > as_of_ms).
    pub as_of_ms: i64,
    /// true = ignore invalid_at (return soft-deleted edges too; used by
    /// ego?include_expired=true). Excluded edges are counted either way.
    pub include_expired: bool,
    /// Per-request result cap, clamped to 1..=GRAPH_MAX_RESULT_EDGES.
    pub max_result_edges: usize,
}

/// One edge as read ENTIRELY from doc-values columns — no `_source` access.
/// `weight` is f64 because numeric columns store f64 bits (index.rs:12869);
/// timestamps are exact for |v| < 2^53 (epoch-ms until year ~287396).
#[derive(Debug, Clone)]
pub struct GraphEdgeLite {
    pub edge_id: String,
    pub src: String,
    pub dst: String,
    pub edge_type: String,
    pub weight: f64,
    pub valid_at_ms: i64,
    pub invalid_at_ms: Option<i64>,
    /// Which hop discovered this edge (1 or 2).
    pub hop: u8,
}

/// In-band honesty: what the expansion did NOT show, and what it cost.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphExpandStats {
    pub frontier_clipped: u64,
    pub edges_clipped: u64,
    /// Edges skipped by the bi-temporal cut (valid_at in the future of as_of,
    /// or invalidated at/before as_of) — NOT counting include_expired=true.
    pub expired_excluded: u64,
    /// Edges skipped by the `types` allowlist.
    pub type_filtered: u64,
    pub segments_scanned: u64,
    /// Segments skipped because they lack a dv sidecar or a `src`/`dst`
    /// keyword column (legacy segment / meta-only segment / poisoned column).
    /// The hop NEVER falls back to hydration — skipped means reported.
    pub segments_without_columns: u64,
    pub memtable_docs_scanned: u64,
}

#[derive(Debug, Clone)]
pub struct GraphExpandResult {
    /// Deduped by edge_id, sorted (hop asc, weight desc, edge_id asc) — the
    /// stable order every response surface and the fixture assert on.
    pub edges: Vec<GraphEdgeLite>,
    /// Frontier ids (that were admitted) first, in input order, then newly
    /// reached ids in first-discovery order following the sorted edge list.
    /// Deduped.
    pub reachable: Vec<String>,
    pub stats: GraphExpandStats,
}

impl Index {
    /// Batched graph expansion over an edges index (§2 schema).
    ///
    /// Sync fn (no await points): all reads are memtable walks + doc-values
    /// column access — the same blocking pattern the columnar agg/search fast
    /// paths already use from async handlers.
    pub fn graph_expand(&self, req: &GraphExpandRequest) -> xerj_common::Result<GraphExpandResult>;
}
```

### 3.3 Batching contract (the algorithm, normative)

Per hop `h` in `1..=req.hops`, with `frontier_h` (hop 1 = admitted request
frontier; hop 2 = new endpoints discovered in hop 1, minus already-visited,
clipped to `GRAPH_MAX_FRONTIER` with the clip counted):

1. `snapshot = self.store_snapshot()` once per request (index.rs:12357);
   `segments_dir = self.data_dir.join("segments")` (the derivation every
   columnar path uses, e.g. index.rs:9821).
2. `any_ghosts = self.store.version_map.ghost_events() > 0` once per request
   (xerj-storage/src/version_map.rs:523).
3. **Per segment** in `snapshot.segments`:
   a. `let Some(cols) = self.dv_columns_for(&segments_dir, &meta.id)` — the
      cached sidecar read (index.rs:14150). None → `segments_without_columns += 1`, skip.
   b. Pull needed columns from the `DocValueMap` (BTreeMap, index.rs:44):
      `src`, `dst` (per direction), `edge_id`, `type`, `weight`, `valid_at`,
      `invalid_at` — via `Column::Keyword(k)` / `Column::Numeric(n)` matches.
      A missing/`Numeric`-typed `src` (resp. `dst` for In/Both) →
      `segments_without_columns += 1`, skip segment. Missing `type`/`weight`/
      `valid_at`/`edge_id` columns → same skip (schema-corrupt segment; honesty
      over guessing). A missing `invalid_at` column is NORMAL (segment where no
      edge was ever invalidated) and means "no edge here is invalidated".
   c. Build the frontier ordinal set: for each id in `frontier_h`,
      `k_src.ord_for_term(id)` (O(log T) FST probe, doc_values.rs:443); collect
      `HashSet<u32>`. Empty set → skip segment without scanning.
   d. **THE scan** — one pass for the whole frontier:
      `for (pos, &ord) in k_src.ords.iter().enumerate()`:
      skip if `k_src.null_bitmap.contains(pos as u32)`;
      admit iff `frontier_ords.contains(&ord)` (O(1)).
      Direction `Both` runs this twice (once over `src` ords, once over `dst`
      ords) with the same admit logic; an edge admitted by both scans is
      emitted once (dedupe on edge_id).
   e. **Per admitted position** (random access, still no `_source`):
      - Liveness: if `any_ghosts`, fetch once per segment
        `self.ghost_positions_for(&meta.id, meta.doc_count)` (index.rs:16873,
        `Option<Arc<Vec<u64>>>` word-bitmap; None → conservatively
        `segments_without_columns += 1`, skip segment); skip positions whose
        bit is set (deleted OR superseded rows — Lucene ghost semantics, same
        source of truth `scored_columnar` uses at index.rs:14570).
      - Type filter: `k_type.ord_for(pos)` → `term_for_ord` compare against the
        allowlist (resolve allowlist→ord-set per segment first; absent term =
        nothing matches). Filtered → `type_filtered += 1`.
      - Bi-temporal cut: `n_valid.get(pos)` (doc_values.rs:147; recover
        `f64::from_bits(v as u64) as i64`); `n_invalid.get(pos)` if the column
        exists. Apply the §3.2 visibility rule → else `expired_excluded += 1`.
      - Emit `GraphEdgeLite` with `edge_id/src/dst/type` read via
        `ord_for(pos)` + `term_for_ord` on their keyword columns, `hop = h`.
      - Stop collecting at `max_result_edges` (count the rest of this
        segment's admits + skip remaining segments into `edges_clipped`).
4. **Memtable** (unflushed edges — correctness, not speed): one bounded walk
   per hop over every shard:
   `for s in 0..self.memtable.shard_count() { self.memtable.with_shard(s, |m| m.all_docs_with_sources_arc()) }`
   (memtable.rs:858). Per (id, source): `memtable_docs_scanned += 1`; skip if
   `self.store.version_map.get(&id)` says deleted; admit if
   `source["src"]` (and/or `"dst"` per direction) is a JSON string in the
   frontier hash set; apply the same type/bi-temporal filters from source
   fields; emit with values read from the source. The memtable is bounded by
   the flush threshold (~10k docs), so O(memtable) per hop is fine and is
   reported in-band.
5. After the final hop: global dedupe by edge_id (a memtable row wins over any
   segment row with the same edge_id — it is the newer version), sort
   (hop asc, weight desc, edge_id asc), build `reachable`, return.

Explicitly forbidden on this path: `StoredSlices` access, `simd_json` parsing,
`hydrate_prefiltered_unsorted`, per-frontier-id queries, and any per-edge
`_source` read.

### 3.4 Engine helpers reused (verified names — cite these in doc comments)

| Helper | Where | Role in the hop |
|---|---|---|
| `Index::dv_columns_for(&self, segments_dir: &Path, segment_id: &str) -> Option<Resident<DocValueMap>>` | index.rs:14150 | cached per-segment doc-values sidecar (`Resident<T> = Arc<CacheResident<T>>`, index.rs:43; `DocValueMap = BTreeMap<String, Column>`, index.rs:44) |
| `KeywordColumn::{ord_for_term, ord_for, term_for_ord}`, `.ords`, `.null_bitmap`, `.per_ord_count` | xerj-storage/src/doc_values.rs:343–460 | frontier→ordinal resolution, THE scan, dst/type/edge_id random access |
| `NumericColumn::get(&self, doc_id: u32) -> Option<i64>` (+ `f64::from_bits(v as u64)`) | doc_values.rs:147 | weight / valid_at / invalid_at reads (numeric columns store f64 BITS — never use the i64 raw) |
| `Index::ghost_positions_for(&self, seg_id: &str, expect_docs: u64) -> Option<Arc<Vec<u64>>>` | index.rs:16873 | liveness (deleted + superseded rows) without hydration |
| `VersionMap::ghost_events() -> u64` | xerj-storage/src/version_map.rs:523 | request-level "any ghosts?" gate (same gate as `scored_columnar`, index.rs:14521) |
| `Index::store_snapshot() -> IndexSnapshot` | index.rs:12357 | immutable segment list (`segments: Vec<SegmentMeta>`, segment.rs:324) |
| `ShardedFtsMemtable::{shard_count, with_shard}` + `FtsMemtable::all_docs_with_sources_arc()` | xerj-engine/src/memtable.rs:681/695/858 | unflushed-edge coverage |
| `Index::row_seqs_for(&self, seg_id, expect_docs)` | index.rs:16244 | NOT needed if ghost bitmaps are used (they subsume the supersede check); listed so nobody reinvents it |

### 3.5 Errors

`graph_expand` returns `Err(XerjError::InvalidQuery { reason })`
(xerj-common/src/error.rs:38) for: `hops == 0` or `hops > GRAPH_MAX_HOPS`
(reason MUST contain the §4.6 not-a-graph-database sentence), empty `frontier`
after dedup. Everything else is a normal result with honest `stats` — a brain
with no edges expands to zero edges, zero error.

### 3.6 Engine tests (fixture-bound)

`engine/crates/xerj-engine/tests/graph_expand.rs`: build the §8 fixture edges
via `index_document` on a `.xerj-memory-notes-edges` index; assert (a) exact
§8.4 expansion pre-flush (memtable path), (b) byte-identical result post
`flush()` (columnar path), (c) post-soft-invalidate `as_of` time travel:
expanding at `as_of < invalid_at` still sees the edge, at `>= invalid_at` does
not and `expired_excluded == 1`, (d) hop-2 + caps: `max_result_edges = 2`
yields `edges_clipped > 0`.

---

## 4. HTTP API (xerj-api)

New module `engine/crates/xerj-api/src/graph_api.rs`, modeled line-for-line on
the `memory_api.rs` adapter discipline: thin handlers that compose
`es_compat::{create_index, index_doc, get_doc, search}` +
`Engine::get_index(...)` + `Index::graph_expand(...)`; reuse the local
`drain_json` / `OptionalJson` / error-shape patterns (copy the private fns —
do not make memory_api's pub).

Route registration in `router.rs`, appended directly under the existing
"Agent-Memory API" block (~line 753):

```rust
// ── Second-Brain Graph API ─────────────────────────────────────────────────
// Edges are ordinary documents in reserved `.xerj-memory-{brain}-edges`
// indices; traversal is a bounded columnar expansion (Index::graph_expand),
// NOT a graph query language. See graph_api.rs.
.route("/_graph/:brain/link", post(graph_api::link))
.route("/_graph/:brain/link/:edge_id", delete(graph_api::unlink))
.route("/_graph/:brain/ego", get(graph_api::ego))
.route("/_graph/:brain/overview", get(graph_api::overview))
```

Shared conventions:
- All timestamps in request/response JSON are **epoch milliseconds (numbers)**.
  Inputs additionally accept RFC3339 strings (parsed with `chrono`); outputs
  are always numbers.
- Error body shape (identical to memory_api but typed `graph_error`):
  `{"error": {"type": "graph_error", "reason": "<msg>"}, "status": <n>}`.
- Auth: process-wide API-key middleware **plus** per-brain authorization
  (issue #79). A brain authorizes against its edges index,
  `.xerj-memory-{brain}-edges`, which sits in the reserved `.xerj-memory-*`
  namespace that `xerj-api::authz` gates on every surface — the graph API,
  `/_memory/*`, the raw ES-compat index routes and the native
  `/v1/indices/{name}/…` routes alike. Reach: open mode (`--insecure`,
  point-at-a-folder) and the admin key get every brain; a key minted with
  `role_descriptors` naming the index gets that brain at the granted
  privilege; a key minted without them gets **no** brain. `ego`/`overview`
  need `read`, `link`/`unlink` need `write`, and reading node summaries
  additionally needs `read` on the resolved nodes index.
- Unknown brain on read endpoints → 404 **for a caller authorized for it**.
  An unauthorized caller gets 403 whether or not the brain exists — the
  authorization check runs before the existence probe, so the status code
  cannot be used to enumerate brains. `link` creates lazily (like
  `ensure_backing_index`), which is part of `write`, not a separate `manage`.

### 4.1 POST /_graph/{brain}/link — assert an edge

Request (`deny_unknown_fields`):
```json
{
  "src": "note-alpha",                    // required, non-empty string
  "dst": "note-beta",                     // required, non-empty, != src
  "type": "wikilink",                     // required, non-empty
  "weight": 1.0,                          // optional, default 1.0, finite, clamped [0,1]
  "valid_at": 1753600000000,              // optional, default = server now
  "created_at": 1753600000000,            // optional, default = server now (fixtures/imports)
  "confidence": 0.95,                     // optional, default 1.0, clamped [0,1]
  "detector": "manual@1",                 // optional, default "manual@1"
  "evidence": {                           // optional
    "quote": "…", "source": "alpha.md", "offset": 35
  }
}
```

Handler: validate brain (§1) and body (400 on violation; self-edge reason:
`"self-edges are not allowed (src == dst)"`); ensure edges index exists with
the §2.1 mapping + §2.5 meta doc; compute `edge_id` (§2.3); build the §2.4
document; `es_compat::index_doc` with `_id = edge_id`.

Response — 201 when the ES-compat result is `created`, 200 when `updated`:
```json
{
  "brain": "notes",
  "edge_id": "bef814a75bd3d914c3e561f610154304",
  "created": true,
  "edge": { /* the exact stored document, §2.4 */ }
}
```

### 4.2 DELETE /_graph/{brain}/link/{edge_id} — soft invalidate

Query params: `invalid_at` (optional epoch-ms/RFC3339; default = server now).
NEVER removes the document.

Handler: `es_compat::get_doc` on the edges index → 404
(`"edge '<id>' does not exist in brain '<brain>'"`) if absent. If the doc
already has `invalid_at` → 200:
```json
{ "brain": "notes", "edge_id": "…", "invalidated": false, "already_invalid_at": 1753650000000 }
```
Else re-index the full source under the same `_id` with
`invalid_at = <param>` and `expired_at = <server now>` added → 200:
```json
{ "brain": "notes", "edge_id": "…", "invalidated": true, "invalid_at": 1753700000000, "expired_at": 1753700000000 }
```
`invalid_at` = when the fact stopped being true (caller-suppliable);
`expired_at` = when the system recorded that (always server now). This is the
bi-temporal pair; both are set on the same call.

### 4.3 GET /_graph/{brain}/ego — neighborhood of one node

Query params:
| param | type | default | notes |
|---|---|---|---|
| `node` | string | — | exactly one of `node`/`nodes` required (400 otherwise); `node` is the 1-seed case |
| `nodes` | comma-list | — | multi-seed frontier, mutually exclusive with `node`; deduped, then clamped to 64 seeds with the clip counted in `not_shown.frontier_clipped` |
| `hops` | int | 1 | 1 or 2; >2 → 400 with §4.6 wording |
| `direction` | `out`\|`in`\|`both` | `both` | |
| `types` | comma-list | all | |
| `limit` | int | 100 | max returned edges, clamped 1..=1000 |
| `as_of` | epoch-ms/RFC3339 | now | |
| `include_expired` | bool | false | |
| `include_nodes` | bool | false | hydrate node summaries |
| `nodes_index` | string | meta doc's `nodes_index`, else `.xerj-memory-{brain}` | |
| `include_evidence` | bool | true | hydrate evidence for RETURNED edges |

Handler: brain/index existence check (404) → `graph_expand` with
`frontier = <seeds>` (the engine expands a whole frontier at
frontier-size-independent cost), `max_result_edges = limit` → post-traversal
hydration:
- `include_evidence`: ONE `es_compat::search` on the edges index with
  `{"query": {"ids": {"values": [<returned edge_ids>]}}, "size": <n>}`
  (bounded ≤1000; rides the existing `build_ids_prefilter_cached` fast path,
  index.rs:16282, for free) → attach `evidence`, `created_at`, `detector`,
  `confidence`, `expired_at` to each edge.
- `include_nodes`: ONE `es_compat::search` on `nodes_index` with an `ids`
  query for all reachable ids → summaries. Ids that resolve nowhere are
  **dangling** (edge kept, honesty counted, ids listed up to 50).

Response (fixture-exact instance in §8.5):
```json
{
  "brain": "notes",
  "contract": "xerj-second-brain/1",
  "node": "note-alpha",
  "seeds": ["note-alpha"],
  "as_of": 1753700000000,
  "hops": 1,
  "direction": "both",
  "edges": [
    {
      "edge_id": "…", "src": "…", "dst": "…", "type": "wikilink",
      "weight": 1.0, "hop": 1, "direction": "out",
      "valid_at": 1753600000000, "invalid_at": null,
      "detector": "wikilink@1", "confidence": 0.95,
      "evidence": { "quote": "…", "source": "alpha.md", "offset": 35 }
    }
  ],
  "neighbors": [
    { "id": "note-gamma", "hop": 1, "via_edge": "<edge_id of first sorted edge that reached it>" }
  ],
  "nodes": { "note-beta": { "title": "…", "preview": "…", "path": "notes/beta.md", "index": ".xerj-memory-notes" } },
  "not_shown": {
    "edges_clipped": 0,
    "frontier_clipped": 0,
    "expired_excluded": 0,
    "type_filtered": 0,
    "segments_without_columns": 0,
    "memtable_docs_scanned": 0,
    "dangling_nodes": 0,
    "dangling_ids": []
  }
}
```
Rules: `edges` in the §3.2 stable order, each annotated with
`direction: "out"|"in"` relative to the expansion (an edge admitted by both
scans reports `"out"`; with multi-seed, "out" means src is one of the seeds);
`seeds` echoes the ADMITTED seed list (post-dedupe, post-clamp) and `node` is
present only when there is exactly one seed; `neighbors` excludes every seed,
first-discovery order; `edge.invalid_at` is `null` in JSON when unset
(response-side null is fine — §2.2's omit-rule binds stored docs, not
responses); `nodes` present only when `include_nodes=true` — summary =
`{title, preview, path, index}` where `title` = `_source.title` if string else
null, `preview` = first 160 chars of `_source.text` else `_source.body` else
null, `path` = `_source.ax_path` if string else null (never guessed), `index`
= the index the doc was found in. `not_shown` maps `GraphExpandStats` 1:1
(handler-clipped seeds fold into `frontier_clipped`) plus the dangling fields;
`segments_scanned`/`memtable_docs_scanned` ride along as cost honesty.

### 4.4 GET /_graph/{brain}/overview — brain-level stats (dashboard feed)

Query params: `as_of` (default now), `top` (default 10, clamp 1..=50 — size of
every top-N list), `histogram_interval` (`"day"` default | `"hour"`).

Handler composes exactly three `es_compat::search` calls on the edges index
(size 0, `track_total_hits: true`):
1. totals: `{"query": {"exists": {"field": "src"}}}` → `edges.total`.
2. live slice: query = `exists src` AND live-at-as_of
   (`bool.filter[exists src, range valid_at lte as_of].must_not[range invalid_at lte as_of]`)
   with aggs `{"by_type": {"terms": {"field": "type", "size": top}}, "by_detector": {"terms": {"field": "detector", "size": top}}, "top_src": {"terms": {"field": "src", "size": top}}, "top_dst": {"terms": {"field": "dst", "size": top}}}`
   → live count, per-type/per-detector live counts, hub lists.
3. timeline: `exists src` with
   `{"created": {"date_histogram": {"field": "created_at", "calendar_interval": "<interval>"}}}`.

Plus one size-0 `match_all` count on `nodes_index` → `nodes.total` (0 when
that index does not exist yet — a brain fed through the link API alone
truthfully has no stored notes).

Response:
```json
{
  "brain": "notes",
  "contract": "xerj-second-brain/1",
  "exists": true,
  "as_of": 1753700000000,
  "nodes_index": ".xerj-memory-notes",
  "nodes": { "total": 0 },
  "embedder": "lexical-feature-hash",
  "edges": { "total": 8, "live": 8, "invalidated": 0 },
  "types":     [ { "type": "wikilink", "live": 4 }, { "type": "same_dir", "live": 4 } ],
  "detectors": [ { "detector": "wikilink@1", "live": 4 }, { "detector": "samedir@1", "live": 4 } ],
  "hubs": {
    "out": [ { "id": "note-alpha", "live_edges": 3 } ],
    "in":  [ { "id": "note-gamma", "live_edges": 3 } ]
  },
  "created_over_time": [ { "t": 1753574400000, "count": 8 } ],
  "not_shown": {
    "types_not_listed": 0,
    "detectors_not_listed": 0,
    "hubs_out_not_listed": 0,
    "hubs_in_not_listed": 0
  }
}
```
`invalidated = total - live`. Each `*_not_listed` = that terms agg's
`sum_other_doc_count`. `embedder` is the honesty marker for §0-invariant 8 —
the literal string `"lexical-feature-hash"` unless the node backing store is
configured with an external embedder (then its configured id verbatim). Unknown
brain → `{"brain": "...", "contract": "...", "exists": false}` with 404 status.

### 4.5 (moved to §5 — recall integration lives in memory_api.rs)

### 4.6 Not-a-graph-database wording (normative strings)

- 400 for `hops > 2` (both engine reason and HTTP): `"hops is capped at 2:
  XERJ's second brain is a relationship layer over documents, not a graph
  database (no Cypher, no shortest-path, no variable-depth traversal). Iterate:
  expand again from this response's 'reachable' ids."`
- graph_api.rs module doc + docs pages MUST contain: `"This is not a graph
  database. There is no query language, no shortest-path, no PageRank, and no
  unbounded traversal — by design. Edges are ordinary documents; expansion is
  a bounded, batched read (≤2 hops per call)."`

---

## 5. RECALL INTEGRATION — POST /_memory/{ns}/_recall graph modes

Extends `memory_api::RecallBody` (the struct is `deny_unknown_fields`; the new
field is additive and optional — absent means behavior is bit-identical to
today):

```rust
/// Optional graph coupling for recall. The namespace's brain is the namespace
/// itself: edges live in `.xerj-memory-{ns}-edges`.
#[serde(default)]
pub graph: Option<GraphRecallOpts>,

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRecallOpts {
    /// "restrict" | "blend" (required).
    pub mode: String,
    /// Seed node ids. REQUIRED for restrict (400 if absent/empty).
    /// Optional for blend (default: ids of the top-5 base-recall hits).
    #[serde(default)]
    pub seeds: Option<Vec<String>>,
    /// 1 (default) or 2.
    #[serde(default)]
    pub hops: Option<u8>,
    /// Edge-type allowlist.
    #[serde(default)]
    pub types: Option<Vec<String>>,
    /// Blend weight in [0,1], default 0.3. Ignored for restrict.
    #[serde(default)]
    pub weight: Option<f32>,
    /// Bi-temporal cut, default now. Epoch-ms number or RFC3339 string.
    #[serde(default)]
    pub as_of: Option<serde_json::Value>,
}
```

Both modes call `Index::graph_expand` on the edges index with
`direction: Both`. Constant: `MAX_RESTRICT_IDS: usize = 10_000`.

**restrict** — "recall only within reach of these seeds":
1. Expand seeds (hops, types, as_of) → `reachable` (includes seeds).
2. Clip `reachable` to `MAX_RESTRICT_IDS` (count clipped).
3. Run the EXISTING recall exactly as today, with the reachable set folded into
   the filter: `filter_out = {"bool": {"filter": [<caller filter if any>, {"ids": {"values": [<reachable>]}}]}}`.
   (The `ids` query is already supported by the parser; nothing new in
   xerj-query.)
4. Empty reachable set → normal empty-hits response (plus the `graph` object),
   not an error.

**blend** — "let graph proximity pull related memories up":
1. If `seeds` absent: run base recall first with `fetch = max(k*4, 50)` (the
   existing recency-blend over-fetch convention, memory_api.rs:486), seeds =
   top-5 hit ids. If present: same over-fetch, seeds as given.
2. Expand seeds → per-node proximity, computed during expansion:
   `proximity(seed) = 1.0`;
   reached at hop h via edge e: `proximity = max(proximity, 0.5^h * clamp(e.weight, 0, 1))`
   (max over all paths; deterministic).
3. Re-rank the fetched candidates:
   `blended = (1 - w) * norm_score + w * proximity`, where `norm_score` is the
   same min-max normalization `blend_recency` uses (memory_api.rs:606) and
   nodes absent from the proximity map get 0. Truncate to k. Hits keep their
   original relevance `score`; each gains `"graph_proximity": <f64>`.
4. Composition with `recency_weight`: apply the graph blend FIRST, then the
   existing recency blend over the graph-blended ordering (each re-rank is
   order+truncate only, so they compose; document this in the handler).

Response gains a top-level `graph` object (both modes):
```json
"graph": {
  "mode": "restrict",
  "seeds": ["note-alpha"],
  "hops": 1,
  "as_of": 1753700000000,
  "reachable": 4,
  "not_shown": {
    "frontier_clipped": 0, "edges_clipped": 0, "expired_excluded": 0,
    "type_filtered": 0, "segments_without_columns": 0,
    "reachable_clipped": 0
  }
}
```

Errors (400, memory_error shape — this endpoint keeps its existing error type):
unknown `mode`; restrict without seeds
(`"graph.mode 'restrict' requires non-empty graph.seeds"`); `hops` 0 or >2
(§4.6 wording); non-existent edges index → NOT an error: recall proceeds
ungated with `"graph": {"mode": …, "reachable": 0, "not_shown": {…, "no_edges_index": true}}`
(an agent must be able to opt into graph recall before its first link exists).

---

## 6. AUTOINDEX CONTRACT (xerj-autoindex)

### 6.1 CLI surface

`xerj autoindex <folder>` grows:
- `--brain <name>` — brain name; default `sanitize_slug(<root folder basename>)`
  (dataset.rs:82). Validation as §1 (reject `-edges` suffix).
- `--no-graph` — disable edge detection entirely.
- Graph detection is **ON by default** (a behavior change: creates
  `.xerj-memory-{brain}-edges`; release notes + `--no-graph` escape hatch).
- `autoindex map` / `status` gain one summary line each (edges written /
  edge count via `Es::count` on the edges index with an `exists src` query).

### 6.2 Module layout

```
engine/crates/xerj-autoindex/src/detect/
  mod.rs        // trait, ctx types, edge_id (§2.3 copy), registry, bulk builder
  wikilink.rs   // [[target]]                       type "wikilink"  w 1.0  conf 0.95
  mdlink.rs     // [text](relative/path.md)         type "mdlink"    w 0.9  conf 0.9
  href.rs       // <a href="relative-or-corpus-url">type "href"      w 0.7  conf 0.85
  pathcite.rs   // bare path token in prose         type "pathcite"  w 0.6  conf 0.7
  cratecite.rs  // crate-dir name in prose          type "cratecite" w 0.5  conf 0.6
  sequence.rs   // card → s_0, s_i -> s_{i+1}       type "sequence"  w 0.8  conf 0.99
  samedir.rs    // chain over sorted files per dir  type "same_dir"  w 0.3  conf 0.4
```

Detector tags as of the cross-file-type revision: `wikilink@2`, `mdlink@2`,
`href@2`, `pathcite@1`, `cratecite@1`, `sequence@2`, `samedir@2`. The @2 bumps
record two behavior changes (per the "bump on ANY behavior change" rule):
every file-level endpoint moved from the target's `s0` section to its
**file-card node**, and edges gained `src_format`/`dst_format`. Edges written
by @1 detectors remain in place, attributable, and time-travelable.

### 6.3 Detector trait (exact shape)

```rust
/// Edge-schema version stamped into every emitted edge (`schema_version`).
pub const EDGE_SCHEMA_VERSION: u32 = 1;

/// Locator of the per-file card node (see §6.6.2a). Cannot collide with
/// content locators — extractor locators are letter+digit shaped.
pub const FILE_CARD_LOCATOR: &str = "file";

/// Corpus-wide resolution table, built once after Phase A (plan assignment):
/// every indexed file's rel path → identity + its file-card node id.
pub struct CorpusIndex {
    /// key: root-relative path with forward slashes (`FileEntry.rel`).
    pub files: std::collections::BTreeMap<String, CorpusFile>,
    /// lowercase file-stem → rel paths bearing it (wikilink resolution;
    /// BTreeMap so ambiguity resolution below is deterministic).
    pub by_stem: std::collections::BTreeMap<String, Vec<String>>,
    /// exact file NAME (final segment, case-sensitive) → rel paths bearing it
    /// (pathcite suffix resolution).
    pub by_name: std::collections::BTreeMap<String, Vec<String>>,
    /// crate-directory basename → rels of `Cargo.toml` files directly inside a
    /// directory of that name (cratecite table; DIRECTORY name, contents never
    /// read).
    pub crate_dirs: std::collections::BTreeMap<String, Vec<String>>,
}
pub struct CorpusFile {
    pub rel: String,
    pub file_key: String,      // ids::file_key output
    pub dataset_slug: String,
    /// ids::doc_id(dataset_slug, file_key, FILE_CARD_LOCATOR) — the file's
    /// CARD node, a real emitted doc (§6.6.2a). Every file-level edge lands
    /// here. (Pre-@2 this was the `s0` section id, which does not exist for
    /// row/line/page families — links to a CSV or PDF pointed at a ghost.)
    pub anchor_doc_id: String,
    pub mtime_ms: i64,
    pub dir: String,           // parent dir rel path, "" for root
    pub family: String,        // sniffed Family::as_str from the frozen plan
    /// File-type label stamped as src_format/dst_format: lowercase extension
    /// when the name has one ("md", "rs", "pdf"), else `family`.
    pub format: String,
}

/// One detected edge before identity/envelope assembly.
pub struct EdgeDraft {
    pub src: String,           // node doc id (usually the section containing the evidence)
    pub dst: String,           // node doc id (target file's anchor_doc_id)
    pub edge_type: &'static str,
    pub weight: f32,
    pub confidence: f32,
    pub valid_at_ms: i64,      // §6.4 determinism rule
    pub src_file: String,      // rel path that taught this edge (top-level keyword)
    pub quote: String,         // evidence.quote (≤240 chars, trimmed)
    pub offset: u64,           // evidence.offset (byte offset in section text; 0 for structural)
    pub src_format: String,    // CorpusFile.format of the endpoints; "" = unknown,
    pub dst_format: String,    // stored key omitted (§2.2)
}

/// Per-section textual context. `text` is the exact section string that became
/// the node doc's `body` (post `split_sections`).
pub struct SectionCtx<'a> {
    pub corpus: &'a CorpusIndex,
    pub file: &'a CorpusFile,
    /// Human label from the locator ("section 3", "page 2 section 0") —
    /// §6.6.2's `section_label`. Used verbatim in sequence rationales.
    pub section_label: &'a str,
    /// (doc id, label) of the previously STAGED section of the same file,
    /// None for the first. Stream-order truth — the only source that can name
    /// a PDF page boundary's predecessor.
    pub prev_section: Option<(&'a str, &'a str)>,
    pub section_doc_id: &'a str,
    pub text: &'a str,
}

/// Deterministic, versioned edge detector. NO network, NO LLM, NO clock reads
/// (all time comes from ctx mtimes) — same inputs must yield the same drafts.
pub trait EdgeDetector: Sync {
    /// Versioned tag stored in `detector`, e.g. "wikilink@1". Bump the @N on
    /// ANY behavior change.
    fn tag(&self) -> &'static str;
    /// Per-section textual detection. Default: no-op.
    fn detect_text(&self, _ctx: &SectionCtx<'_>, _out: &mut Vec<EdgeDraft>) {}
    /// Corpus-structural detection, called once after Phase A. Default: no-op.
    fn detect_structure(&self, _corpus: &CorpusIndex, _out: &mut Vec<EdgeDraft>) {}
}

/// The registry. Order is normative (fixture edge ordering depends on it only
/// via edge sort, so this is cosmetic — but keep it).
pub fn default_detectors() -> Vec<Box<dyn EdgeDetector>>;
// wikilink, mdlink, href, pathcite, cratecite, sequence, samedir
```

### 6.4 Determinism + edge identity rules

- `valid_at_ms` = **mtime of `src_file`** (from the walk metadata), truncated
  to ms. Re-running over an unchanged corpus reproduces identical `valid_at` →
  identical `edge_id` (§2.3) → bulk `index` overwrites; the run converges
  exactly like `ids::doc_id` does for node docs (ids.rs module docs).
- `created_at` = run wall-clock at bulk-assembly time (the ONE non-deterministic
  field; it does not participate in `edge_id`).
- Self-edges (src == dst) are dropped by the assembler, silently — with a
  per-run counter surfaced in the summary (`edges_self_dropped`).

### 6.5 Per-detector emission rules (normative)

All detectors stamp `src_format` = the containing file's `CorpusFile.format`
and `dst_format` = the target file's; every `anchor_doc_id` below is the
target's FILE CARD (§6.6.2a).

- **wikilink@2**: scan section text for `[[target]]` / `[[target|alias]]`
  (target = text before `|`, trimmed). Resolve: exact rel-path match first
  (with or without extension), else `by_stem[target.to_lowercase()]`; if
  multiple candidates, pick the lexicographically smallest rel (deterministic;
  count the ambiguity in the run summary as `edges_ambiguous`). Unresolvable →
  no edge, count `edges_unresolved`. src = section doc id; dst = target's
  `anchor_doc_id`; quote = the full trimmed line containing the link (≤240
  chars); offset = byte offset of the `[[` within the section text.
- **mdlink@2**: `[text](url)` where url has no scheme and no leading `//`;
  strip fragment/query; resolve relative to the containing file's dir, then as
  root-relative; must resolve to a corpus file. Same evidence rules (offset of
  the `[`).
- **href@2**: `<a href="…">` (html-extracted files only, case-insensitive
  attr); same scheme-less resolution as mdlink. Offset = offset of the `href`
  attribute value start in the section text (best effort; 0 when the extractor
  lost byte positions).
- **pathcite@1**: bare path tokens in section text — maximal
  `[A-Za-z0-9_./-]` runs ending `\.[A-Za-z0-9_]+`, leading `/` `./` `../`
  stripped (a trailing `:line` never enters the token; `:` is outside the
  class). Resolve: exact rel-path match; else, for tokens with ≥2 path
  segments, path-SUFFIX match at a `/` boundary (several candidates →
  smallest rel + `edges_ambiguous`; none → `edges_unresolved`). Bare
  one-segment names (`index.rs`) are NEVER suffix-matched: if any corpus file
  bears the name the mention counts as ambiguous, otherwise it is ignored as
  prose noise (not counted — "e.g", domains, version numbers are not
  citations). Self-citations are skipped silently. src = section doc id;
  dst = target's `anchor_doc_id`; quote = full trimmed line; offset = byte
  offset of the (stripped) token. UI name: **"cites file"**.
- **cratecite@1**: whole-word occurrences of a `crate_dirs` key (a corpus
  directory directly containing `Cargo.toml`, keyed by directory basename) in
  section text. Only names containing `-` or `_` are citable (a crate dir
  named `server` would turn ordinary English into edges). Word boundaries
  exclude `[A-Za-z0-9_\-./]`, except a trailing sentence period (`.` not
  followed by an alphanumeric) which does not suppress the match. Underscore
  import spellings (`xerj_fts`) are NOT matched — that equivalence cannot be
  verified without reading manifests. >1 dirs sharing a basename → smallest
  rel + `edges_ambiguous`. dst = that `Cargo.toml`'s `anchor_doc_id`; quote =
  full trimmed line; offset = byte offset of the mention. UI name:
  **"cites crate"**.
- **sequence@2** (structural per file, emitted from `detect_text` to avoid a
  second pass): the file's FIRST staged section gets
  `anchor_doc_id → section doc id`, quote = `"{label} opens {rel}"`; every
  later section gets `prev section doc id → section doc id`, quote =
  `"{prev_label} precedes {label} of {rel}"`; offset 0. Labels per §6.6.2.
  The opener is what connects a file's card (where citations land) to its
  content — without it a cited file's text would be unreachable from the
  ego of its own card.
- **samedir@2** (structural): per directory with ≥2 indexed files, sort files
  by rel; emit a CHAIN — one edge per consecutive pair
  (`left.anchor_doc_id → right.anchor_doc_id`). A chain, not a clique: cliques
  are O(n²) and would drown real signal; chain keeps directory cohesion
  reachable within the 2-hop cap for small dirs at O(n). quote =
  `"{left.rel} and {right.rel} share directory {dir}"` (dir `"."` for root),
  offset 0, `src_file` = left.rel, `valid_at_ms` = left file's mtime.

### 6.6 Pipeline hooks (where, exactly)

All hooks live in `lib.rs::run_index`:
1. **After Phase A / plan finalization** (once `FileAssignment`s + dataset
   slugs + file_keys exist, before the Phase B par-iter): build `CorpusIndex`;
   run every detector's `detect_structure`; `Es::ensure_index` the edges index
   with the §2.1 mapping; `create` the §2.5 meta doc with `nodes_index` =
   comma-list of this run's dataset index names (e.g. `"ax-notes-docs"`);
   bulk-write structural edges (§6.7).
2. **Inside the Phase B per-file sink** (the closure that stages each
   `RawRecord`): after the node action is staged for a text-section record,
   run `detect_text` for each detector with the section text
   (`fields["body"]`), the section label, the previously staged section's
   (doc id, label), and the just-computed node `id`. Append resulting edge
   actions to a per-worker edge buffer (NOT the node staging file — different
   target index).

   **Text-section locators** (`lib.rs::section_label`) are exactly two forms:
   `s{i}` (from `emit_document` — md/txt/html/docx prose) and `p{page}-s{sec}`
   (from the PDF extractor — page-major, so stream order IS the lexicographic
   (page, sec) reading order). Any other locator (row/line/byte/table) is not
   a text section and never reaches `detect_text`. Labels: `s3` → "section 3";
   `p2-s0` → "page 2 section 0". The pipeline tracks the previously staged
   section per file in stream order; that predecessor — not locator
   arithmetic — feeds sequence@2, which is what lets PDF sections chain
   across page boundaries.

2a. **File-card node** (before extraction, once per Phase-B file): stage one
   card doc to the file's dataset index at `_id = anchor_doc_id`
   (`ids::doc_id(slug, file_key, FILE_CARD_LOCATOR)`), fields:
   `title` = filename, `ax_path`, `ax_paths`, `ax_file`,
   `ax_locator = "file"`, `ax_dataset`, `ax_run`, `ax_format`. No body — it
   is anchor infrastructure, not content, and is not counted as an extracted
   record. This is the node that makes every file-level edge hydratable
   (row/line/page families have no `s0` doc; pre-@2 their anchors were
   ghosts). Replacement safety: the card carries `ax_file`, so the existing
   delete-before-replace `delete_by_query` removes and the re-run re-stages
   it under the same deterministic id.
3. **Replacement invalidation** (same site as the existing
   `delete_by_query(index, {"term": {"ax_file": key}})` cleanup, lib.rs ~1000):
   when a file is being replaced, soft-invalidate its prior edges FIRST:
   `Es::search` the edges index for
   `{"query": {"bool": {"filter": [{"term": {"src_file": "<rel>"}}], "must_not": [{"exists": {"field": "invalid_at"}}]}}, "size": 1000, "_source": true}`
   (page via repeat until empty), re-index each hit's full source with
   `invalid_at = expired_at = now`. Top-level `src_file` exists precisely so
   this query rides the keyword doc-values prefilter instead of a brute
   `evidence.source` object scan.
4. **Summary line** (the final printed report): edges written / unresolved /
   ambiguous / self-dropped / invalidated, per detector tag.

### 6.7 Bulk write contract

Edge actions use the exact node-side format (lib.rs ~949):
```
{"index":{"_index":".xerj-memory-{brain}-edges","_id":"<edge_id>"}}
{<edge document, §2.4 shape>}
```
Buffered per worker, cut at the same `--bulk-mb` threshold, sent via the
existing `Es::bulk` + `record_bulk_outcome` (lib.rs:88) error discipline.
Edges for a file are sent only after that file's node bulk has been accepted
(same happens-before the staging file already provides). Cross-file dangling
windows (edge lands before its dst file's docs) are permitted — final state
converges, and readers surface dangling honestly (§4.3).

### 6.8 Autoindex tests

`detect/mod.rs` unit tests: the §2.3 pin vector; wikilink/mdlink resolution
incl. ambiguity determinism; pathcite exact/suffix/bare-refusal/noise rules;
cratecite whole-word + citability rules; samedir chain (not clique) on the §8
fixture folder; sequence opener + chaining (incl. a PDF page boundary).
Integration (behind the existing mock-ES test pattern): index the §8.1 folder
twice, assert the edge set is byte-identical across runs and matches §8.3's
(src, dst, type, detector) tuples with autoindex-derived node ids per the
§8.3a delta (file cards, @2 tags, sequence openers).

---

## 7. UX DATA CONTRACT (xerj-ux + xerj-console-api)

### 7.1 Dashboard module — `xerj-ux/src/dashboards/second-brain.js`

```js
export const secondBrain = {
  id:   'second-brain',
  name: 'Second Brain',
  render: ({ data, time }) => ({
    title:  'SECOND BRAIN',
    // Amended 2026-07-30 (integrator, honesty review): was "EVERY LINK HAS A
    // QUOTE". The schema does not enforce a quote — §4.1 `evidence` is
    // optional (manual/agent asserts may omit it) and structural detectors
    // record a rationale, not note text. The surface may not claim more than
    // the schema enforces.
    kicker: 'WHAT YOUR NOTES BELIEVE · EVERY LINK SHOWS ITS EVIDENCE · REPLAYABLE AT ANY MOMENT',
    meta:   [time, 'XERJ-GRAPH'],
    panels: [
      { id: 'edgesLive',    eyebrow: 'BELIEVED AT THIS MOMENT',   cols: 3, type: 'metric' },
      { id: 'edgesTotal',   eyebrow: 'EVER ASSERTED',             cols: 3, type: 'metric' },
      { id: 'invalidated',  eyebrow: 'RETIRED · KEPT FOR REPLAY', cols: 3, type: 'metric' },
      { id: 'detectors',    eyebrow: 'WHAT TAUGHT THIS BRAIN',    cols: 3, type: 'metric' },
      { id: 'typeDist',     eyebrow: 'HOW NOTES CONNECT',         cols: 6, type: 'dist',   /* Dist */ },
      { id: 'edgeTimeline', eyebrow: 'NEW LINKS PER DAY',         cols: 6, type: 'series', /* Series */ },
      { id: 'ego',          eyebrow: 'THE LEDGER · ONE NOTE, EVERYTHING IT TOUCHES', cols: 12, type: 'ego' },
      { id: 'hubs',         eyebrow: 'CENTERS OF GRAVITY',        cols: 6, type: 'topn' },
      { id: 'notShown',     eyebrow: 'WHAT THIS VIEW DID NOT SHOW', cols: 6, type: 'honesty' },
    ],
  }),
};
```

Integrator edit (UX review, 2026-07-28): the ids/kinds/cols above are
UNCHANGED from v1; only the human-visible titles moved to user words.
LANGUAGE RULE (binding for every console surface of this feature): panel
titles and labels say "link / believed since / retired / what taught
this" — schema vocabulary (`edge`, `src`/`dst`, `valid_at`, `as_of`,
`edge_id`) may appear only inside the evidence paper-trail (the raw
developer view) and in API documentation. The four metric panels carry a
visual value story, not bare counters: believed-vs-retired composition
(the ONE two-series comparison — `--z-accent` vs the validated `--z-cmp`
token, both segments directly labelled), cumulative growth spark,
retired share, and per-detector count rows.
Import surface identical to vector-index.js (`Num` from `../ux/text.js`,
`Dist`/`Series` from `../ux/charts.js`, `TopN` from `../ux/layout.js`). Two
new panel renderers are UX-stream-owned: `ego` (node-link view of the §4.3
response — render `edges`+`neighbors` only, cap 100 drawn edges, show the cap)
and `honesty` (definition list over `not_shown`; MUST always render, showing
zeros — the panel exists to prove absence). The subtitle under `edgeTimeline`
must carry the embedder honesty string when `overview.embedder` is
`"lexical-feature-hash"`: `"recall is lexical (feature hashing) — not neural"`.

Amendment (map + statistics revision, 2026-07-30): the dashboard now renders
SIX additional panels around the nine above, in this page order —
`controls` (12, brain switcher · trail · FIND), `map` (12, mount point for
`ux/brain-map.js`; the panel body owns only the mount + the permanent
"GROUPED BY LINK STRUCTURE — CITATIONS AND FOLDER NEIGHBORHOOD, NOT MEANING"
disclosure), `scrub` (12, the ONE belief-time caret for the whole page,
hoisted out of `ego`), then after `edgeTimeline`: `notes` (4, note total +
by-file-type tally the page itself ran), `crossings` (4, links across file
types from the §2.1 `src_format`/`dst_format` stamps; brains indexed before
stamping show the REINDEX message, never a guess), `reads` (4, the page's own
attributed fetch log — what/how much/why — plus the server's per-index search
counters framed as since-boot). The original nine keep their ids/kinds/cols;
`ego` remains the terminal evidence surface (the anti-hairball rationale in
`ux/ego-ledger.js` stands). §7.4's seed skeleton stays at the nine v1 panels
on purpose — the SPA registry is the render truth and absent ids are created
on boot, so the seed is a durable skeleton, not the composition.

### 7.2 registry.js (exact edits)

```js
import { secondBrain } from './second-brain.js';           // with the other imports
// DEFAULT_GROUP gains:
'second-brain':   'ai',
// the for-loop array and `all` array gain `secondBrain`, appended after
// `agentMemory` (AI group order: ai-overview, rag-quality, vector-index,
// agent-memory, second-brain).
```

### 7.3 `data` shape the render receives (mock now, live later)

Per seed.rs honesty rules, panels bind mock data via builtin keys first; the
mock MUST be shaped as the live payloads so the flip is a fetch swap:

```js
data = {
  brain: 'notes',
  overview: /* EXACTLY the §4.4 response body */,
  ego:      /* EXACTLY the §4.3 response body for the currently selected node */,
  metrics: {           // derived by the data layer from `overview`, pre-formatted
    edgesLive:   { formatted: '8',  hint: 'held true right now' },   // as-of-aware: 'held true at <ts> UTC' when scrubbed
    edgesTotal:  { formatted: '8',  hint: 'nothing is ever deleted' },
    invalidated: { formatted: '0',  hint: 'drag belief time left to revisit' },
    detectors:   { formatted: '2',  hint: 'deterministic, versioned' },
  },
  series: { created: overview.created_over_time.map(b => b.count) },
};
```
The mock fixture data file MUST be the §8 fixture verbatim (same ids, same
counts) so UX screenshots and backend tests display the same world.

### 7.4 seed.rs (exact DashboardSpec entry)

Appended to the specs list in `default_specs()` (after the agent-memory
entry), panels mirroring §7.1 ids/kinds/titles/cols 1:1:

```rust
DashboardSpec {
    registry_id: "second-brain",
    name: "Second Brain",
    section: Some("dashboards"),
    group: Some("ai"),
    panels: vec![
        p("edgesLive",    "metric",  "BELIEVED AT THIS MOMENT",             3),
        p("edgesTotal",   "metric",  "EVER ASSERTED",                       3),
        p("invalidated",  "metric",  "RETIRED · KEPT FOR REPLAY",           3),
        p("detectors",    "metric",  "WHAT TAUGHT THIS BRAIN",              3),
        p("typeDist",     "dist",    "HOW NOTES CONNECT",                   6),
        p("edgeTimeline", "series",  "NEW LINKS PER DAY",                   6),
        p("ego",          "ego",     "THE LEDGER · ONE NOTE, EVERYTHING IT TOUCHES", 12),
        p("hubs",         "topn",    "CENTERS OF GRAVITY",                  6),
        p("notShown",     "honesty", "WHAT THIS VIEW DID NOT SHOW",         6),
    ],
},
```
**Do NOT bump `SEED_REVISION`** — adding a new default id is covered by the
absent-id-creates rule on every boot (seed.rs module docs); the migration
revision is only for changing EXISTING skeletons.

---

## 8. FIXTURE — the one worked example every stream tests against

Brain: `notes`. Edges index: `.xerj-memory-notes-edges`. All fixture writes go
through `POST /_graph/notes/link` with explicit ids/timestamps so the fixture
runs without autoindex; §8.3's table is ALSO the expected autoindex output
modulo node-id derivation (autoindex integration asserts the same
(src rel, dst rel, type, detector) tuples with `ids::doc_id`-derived node ids).

### 8.1 The five notes (folder `notes/`, all mtime = 1753600000000)

| file | node id (fixture) | body |
|---|---|---|
| `alpha.md`   | `note-alpha`   | `Alpha is the hub note. It links to [[beta]] and [[gamma]].` |
| `beta.md`    | `note-beta`    | `Beta continues the thread and references [[gamma]].` |
| `gamma.md`   | `note-gamma`   | `Gamma is the sink note with no outgoing links.` |
| `delta.md`   | `note-delta`   | `Delta cites [[alpha]] as its source.` |
| `epsilon.md` | `note-epsilon` | `Epsilon stands alone.` |

### 8.2 Wiki-link occurrences (exact byte offsets in the bodies above)

alpha→beta @35, alpha→gamma @48, beta→gamma @41, delta→alpha @12.
epsilon has none — it participates only via same_dir.

### 8.3 The exact edges produced (valid_at = created_at = 1753600000000)

samedir@1 chains sorted rels: alpha.md → beta.md → delta.md → epsilon.md → gamma.md.

| # | edge_id | src | dst | type | weight | conf | detector | evidence.quote | evidence.source | offset |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `bef814a75bd3d914c3e561f610154304` | note-alpha | note-beta | wikilink | 1.0 | 0.95 | wikilink@1 | `Alpha is the hub note. It links to [[beta]] and [[gamma]].` | alpha.md | 35 |
| 2 | `11c2d0ef216cd6e99a3907a0b53c1452` | note-alpha | note-gamma | wikilink | 1.0 | 0.95 | wikilink@1 | same line as #1 | alpha.md | 48 |
| 3 | `9bbf7d2068321ac0fa71d95e21fae2fd` | note-beta | note-gamma | wikilink | 1.0 | 0.95 | wikilink@1 | `Beta continues the thread and references [[gamma]].` | beta.md | 41 |
| 4 | `cead55986c364ad5ff6f0894daf61f77` | note-delta | note-alpha | wikilink | 1.0 | 0.95 | wikilink@1 | `Delta cites [[alpha]] as its source.` | delta.md | 12 |
| 5 | `63b747655365aa16d38188aa49966f40` | note-alpha | note-beta | same_dir | 0.3 | 0.4 | samedir@1 | `alpha.md and beta.md share directory .` | alpha.md | 0 |
| 6 | `a61e6caacb5e485baf6d45184f23ec67` | note-beta | note-delta | same_dir | 0.3 | 0.4 | samedir@1 | `beta.md and delta.md share directory .` | beta.md | 0 |
| 7 | `3efff61b58c978943e6fd2a1e4eeaee8` | note-delta | note-epsilon | same_dir | 0.3 | 0.4 | samedir@1 | `delta.md and epsilon.md share directory .` | delta.md | 0 |
| 8 | `7c07cdc441f0a3faa29be8946df3e7a4` | note-epsilon | note-gamma | same_dir | 0.3 | 0.4 | samedir@1 | `epsilon.md and gamma.md share directory .` | epsilon.md | 0 |

(Every edge_id above was computed with the §2.3 function; #5's src_file is
alpha.md, etc. — src_file = evidence.source for all fixture edges. Note edges
#1 and #5 share (src,dst) but differ in type → distinct edge_ids: parallel
edges of different types are legal and expected.)

### 8.3a Autoindex-output delta (cross-file-type revision)

The table above is the API-seeded fixture (abstract `note-*` ids, @1 tags) and
stays normative for §8.4–§8.6 — those edge_ids are pinned. **Autoindex output
over the same folder now differs in exactly three ways** (asserted by the
`detect/e2e.rs` suite):

1. Detector tags are `wikilink@2` / `samedir@2` (and node-id derivation is
   autoindex's, as before): wikilink src = the s0 SECTION doc (where the
   evidence lives), wikilink dst and both samedir endpoints = FILE CARDS.
2. Five additional `sequence@2` opener edges, one per file:
   `card → s0`, quote `"section 0 opens {rel}"`, weight 0.8, conf 0.99.
   Total fixture edge count: 13 (4 wikilink + 4 samedir + 5 sequence).
3. Every edge carries `src_format: "md"`, `dst_format: "md"`.

### 8.4 Expected `graph_expand` (engine-level)

Request: `frontier=["note-alpha"], hops=1, direction=Both, types=None,
as_of_ms=1753700000000, include_expired=false, max_result_edges=1000`.

Expansion admits only edges touching the frontier: from `note-alpha`, hop 1,
direction both, that is #1 and #2 and #5 (src side) plus #4 (dst side) — and
NOT #3 (beta→gamma touches no frontier id at hop 1). Result edges in EXACTLY
this order (hop asc, weight desc, edge_id asc):

1. `11c2d0ef216cd6e99a3907a0b53c1452` alpha→gamma wikilink 1.0 hop 1
2. `bef814a75bd3d914c3e561f610154304` alpha→beta  wikilink 1.0 hop 1
3. `cead55986c364ad5ff6f0894daf61f77` delta→alpha wikilink 1.0 hop 1
4. `63b747655365aa16d38188aa49966f40` alpha→beta  same_dir 0.3 hop 1

`reachable = ["note-alpha", "note-gamma", "note-beta", "note-delta"]`.
`stats = { frontier_clipped: 0, edges_clipped: 0, expired_excluded: 0,
type_filtered: 0, segments_scanned: <n>, segments_without_columns: 0,
memtable_docs_scanned: <m> }` (`<n>`/`<m>` depend on flush state; every other
field is exact). The same request with `hops: 2` additionally returns, at hop
2 (frontier = gamma, beta, delta): #3 (beta→gamma), #6 (beta→delta),
#7 (delta→epsilon), #8 (epsilon→gamma) — hop-2 block sorted
`9bbf… (w 1.0), 3eff…, 7c07…, a61e…` — and reachable gains `note-epsilon`.

### 8.5 Expected ego response (HTTP-level, normative JSON)

`GET /_graph/notes/ego?node=note-alpha&hops=1&as_of=1753700000000` (defaults:
direction both, limit 100, include_evidence true, include_nodes false):

```json
{
  "brain": "notes",
  "contract": "xerj-second-brain/1",
  "node": "note-alpha",
  "seeds": ["note-alpha"],
  "as_of": 1753700000000,
  "hops": 1,
  "direction": "both",
  "edges": [
    { "edge_id": "11c2d0ef216cd6e99a3907a0b53c1452", "src": "note-alpha", "dst": "note-gamma",
      "type": "wikilink", "weight": 1.0, "hop": 1, "direction": "out",
      "valid_at": 1753600000000, "invalid_at": null, "created_at": 1753600000000,
      "detector": "wikilink@1", "confidence": 0.95,
      "evidence": { "quote": "Alpha is the hub note. It links to [[beta]] and [[gamma]].", "source": "alpha.md", "offset": 48 } },
    { "edge_id": "bef814a75bd3d914c3e561f610154304", "src": "note-alpha", "dst": "note-beta",
      "type": "wikilink", "weight": 1.0, "hop": 1, "direction": "out",
      "valid_at": 1753600000000, "invalid_at": null, "created_at": 1753600000000,
      "detector": "wikilink@1", "confidence": 0.95,
      "evidence": { "quote": "Alpha is the hub note. It links to [[beta]] and [[gamma]].", "source": "alpha.md", "offset": 35 } },
    { "edge_id": "cead55986c364ad5ff6f0894daf61f77", "src": "note-delta", "dst": "note-alpha",
      "type": "wikilink", "weight": 1.0, "hop": 1, "direction": "in",
      "valid_at": 1753600000000, "invalid_at": null, "created_at": 1753600000000,
      "detector": "wikilink@1", "confidence": 0.95,
      "evidence": { "quote": "Delta cites [[alpha]] as its source.", "source": "delta.md", "offset": 12 } },
    { "edge_id": "63b747655365aa16d38188aa49966f40", "src": "note-alpha", "dst": "note-beta",
      "type": "same_dir", "weight": 0.3, "hop": 1, "direction": "out",
      "valid_at": 1753600000000, "invalid_at": null, "created_at": 1753600000000,
      "detector": "samedir@1", "confidence": 0.4,
      "evidence": { "quote": "alpha.md and beta.md share directory .", "source": "alpha.md", "offset": 0 } }
  ],
  "neighbors": [
    { "id": "note-gamma", "hop": 1, "via_edge": "11c2d0ef216cd6e99a3907a0b53c1452" },
    { "id": "note-beta",  "hop": 1, "via_edge": "bef814a75bd3d914c3e561f610154304" },
    { "id": "note-delta", "hop": 1, "via_edge": "cead55986c364ad5ff6f0894daf61f77" }
  ],
  "not_shown": {
    "edges_clipped": 0, "frontier_clipped": 0, "expired_excluded": 0,
    "type_filtered": 0, "segments_without_columns": 0,
    "memtable_docs_scanned": 0, "dangling_nodes": 0, "dangling_ids": []
  }
}
```
(`memtable_docs_scanned` is asserted as ≥0, exact-zero only post-flush; every
other byte is normative. When the fixture runs WITHOUT node docs stored,
`include_nodes=true` must return `"nodes": {}` and
`"dangling_nodes": 4, "dangling_ids": ["note-alpha","note-beta","note-delta","note-gamma"]`
— dangling honesty is part of the fixture.)

### 8.6 Fixture time-travel assertion (shared)

After `DELETE /_graph/notes/link/bef814a75bd3d914c3e561f610154304?invalid_at=1753650000000`:
- ego at `as_of=1753640000000` → all four §8.5 edges (belief last Tuesday intact);
- ego at `as_of=1753700000000` → three edges (`bef814…` gone),
  `not_shown.expired_excluded: 1`;
- ego with `include_expired=true` at the same as_of → four edges again, the
  invalidated one carrying `"invalid_at": 1753650000000, "expired_at": <set>`.
- recall restrict from seed `note-delta`, hops 1, at `as_of=1753700000000`:
  both-direction hop-1 admits #4 (src=delta → alpha), #7 (src=delta →
  epsilon), #6 (dst=delta → beta); sorted edge order is #4 (`cead…`, w 1.0),
  #7 (`3eff…`, w 0.3), #6 (`a61e…`, w 0.3), so discovery order gives
  `reachable = ["note-delta","note-alpha","note-epsilon","note-beta"]`,
  `"reachable": 4` in the response's `graph` object.

---

## 9. Verification gates (integrator)

1. `cargo test -p xerj-engine graph_` — §3.6 suite green pre- and post-flush.
2. `cargo test -p xerj-api graph_api` + memory_api recall-graph tests
   (restrict/blend/no-edges-index) green — handler tests follow the existing
   in-process `test_state()` pattern (memory_api.rs:841).
3. `cargo test -p xerj-autoindex detect` green; double-run convergence test.
4. Live smoke against a booted server (per xerj-sandbox-boot memory): the §8
   curl sequence, then `GET overview` matches §4.4 counts; ES-YAML suite is
   untouched surface but run the `search` + `bulk` suites as a regression
   canary since memory_api::validate_namespace changed.
5. rustfmt + clippy -D warnings on the three touched crates.
6. Every response of every new endpoint contains `not_shown` — grep-able
   invariant: a new endpoint without it does not merge.
