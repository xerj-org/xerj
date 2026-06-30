# Code Review — xerj engine, 2026-04-25

> A no-bullshit review of the xerj engine in the spirit of Linus
> on the LKML. The codebase ships v0.5.9 today with 1305/1329
> (98.19%) ES YAML pass rate and 1.55M docs/s ingest. It is also
> carrying scars from a 671-commit "make-the-test-pass" sprint. If
> we want to ship this as OSS without being embarrassed by the
> first dependabot bug report, four CRITICAL items must land
> before the next tag.

## TL;DR

| Pri | What | Where | Cost to fix |
|-----|------|-------|------------|
| **P0** | `size` param has no cap → 2 GB allocation from one HTTP POST | `xerj-query/src/parser.rs:86` | 2 lines |
| **P0** | Bool-query recursion has no depth limit → stack overflow DOS | `xerj-query/src/parser.rs` (parse_bool callsites) | 20 lines |
| **P0** | `mget` accepts unbounded `docs[]` | `xerj-api/src/es_compat.rs:9373` | 5 lines |
| **P0** | `terms` agg has no `shard_size` cap → OOM on high-cardinality fields | `xerj-engine/src/aggs.rs:5036` | 30 lines |
| **P1** | HTTP body limit hardcoded 100 MB, not in Config | `xerj-api/src/router.rs:27` | wire to Config |
| **P1** | `memtable_max_bytes` / `wal_max_size_bytes` defaults bypass Config | `xerj-storage/src/index_store.rs:155` | wire to Config |
| **P1** | `MEMTABLE_SHARDS=16` is a compile-time const, not runtime | `xerj-storage/src/index_store.rs:176`, `xerj-engine/src/memtable.rs:321` | refactor + ABI break |
| **P1** | Merge tier sizes hardcoded (5MB/5GB) | `xerj-storage/src/merge.rs:76–77` | wire to Config |
| **P2** | KNN repeats full segment decode + JSON parse on every query | `xerj-engine/src/index.rs:2168` | segment cache, ~50 lines |
| **P2** | KNN allocates full memtable doc list when K=10 | `xerj-engine/src/index.rs:2162` | streaming scan |
| **P2** | Terms agg sorts entire bucket map then truncates | `xerj-engine/src/aggs.rs:7237` | TopK heap |

## I'm going to be direct about this

This codebase has good bones — the 16-shard memtable, the WAL design,
the per-target rustflags I helped put in for v0.5.9 — but it has the
texture of a project that grew faster than its review process. Six
hundred and seventy-one commits in a few weeks will do that. Now we
slow down and pay the bill.

The four CRITICAL items below are the kind of things that, on
LKML, would get a patch series rejected with "this is not OK,"
because **a single curl from anywhere on the internet kills your
server**. We cap `size`. We cap nesting depth. We cap batch size.
We cap cardinality. Every public search engine learns this the
hard way; let's not.

---

## CRITICAL — fix before the next release tag

### 1. `size=2_000_000_000` allocates two billion `Hit` structs

`xerj-query/src/parser.rs:86`:

```rust
let size = match obj.get("size") {
    Some(v) => v.as_u64().ok_or_else(|| QueryError::Parse(...))? as usize,
    None => 10,
};
```

That's it. No upper bound. The value flows into
`xerj-query/src/executor.rs:194`:

```rust
let limit = from + size;
...
let mut all: Vec<Hit> = segment_hits.into_iter().flatten().collect();
all.sort_unstable_by(...);
all.into_iter().skip(from).take(size).collect()
```

So `size = 2_000_000_000` from a JSON request will allocate a `Vec<Hit>`
sized to fit 2 billion elements, materialize them, sort them, and then
discard all but `size` — which is also 2 billion. Hundreds of GBs of RAM
gone. The OOM-killer takes the process. Single-curl DOS.

Elasticsearch caps this at 10000 by default (`index.max_result_window`).
Mirror that. Three lines:

```rust
const MAX_RESULT_WINDOW: usize = 10_000;
let size = ...as usize;
if from + size > MAX_RESULT_WINDOW { return invalid("from + size exceeds max_result_window"); }
```

### 2. Nested `bool` query has no depth limit → stack overflow

`xerj-query/src/parser.rs` — `parse_bool()` recurses into `parse_query()`
which recurses back into `parse_bool()`. Default Linux stack is 8 MB; a
debug build burns it at ~3000 levels of nesting. Release builds last
longer but blow eventually.

```json
{ "query": { "bool": { "filter": [{ "bool": { "filter": [...] } }] } } }
```

A 1500-deep payload kills the process. Add a depth counter on the
public entry point:

```rust
pub fn parse_query(json: &Value) -> Result<QueryNode> {
    parse_query_with_depth(json, 0)
}
fn parse_query_with_depth(json: &Value, depth: usize) -> Result<QueryNode> {
    if depth > 100 { return Err(QueryError::Parse(ParseError::Invalid("query too deeply nested".into()))); }
    ...
}
```

ES's max is 20 by default; 100 is generous and still safe.

### 3. `mget` accepts `docs[]` of arbitrary length

`xerj-api/src/es_compat.rs:9373`:

```rust
let mut docs: Vec<Value> = Vec::with_capacity(body.docs.len());
```

`body.docs` is whatever the user POSTs. A single mget with a million
entries pre-allocates a million-`Value` vec, then iterates and looks
each one up. There is also no limit on `_source` filtering inside that
loop, so the response can be terabytes.

Cap it:

```rust
const MAX_MGET_BATCH: usize = 10_000;
if body.docs.len() > MAX_MGET_BATCH { return error_response("too many docs in mget"); }
```

### 4. `terms` aggregation has no cardinality bound

`xerj-engine/src/aggs.rs:5036` — terms agg accumulates into:

```rust
let mut bucket_map: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
```

Run a `terms` agg on `user_id` across 50 million unique users and you
allocate 50 million `String` keys (each 24 bytes minimum) plus
`Vec<usize>` doc lists. ES caps this with `shard_size` (default 512;
times the number of shards). Without a cap, this is one of the easiest
ways to take down an Elasticsearch cluster, and we copied the bug.

Implement a min-heap eviction: keep `shard_size` largest buckets,
discard the rest as they come in. ~30 lines.

---

## P1 — non-configurable production parameters

These are not panics, they are not OOM. They are operator-hostility.
A serious database lets the operator tune memtable size, shard count,
WAL buffer, body limit, merge thresholds. These are all baked into
constants in xerj today. Anyone running this on a 4-core dev VM gets
the same memtable size as someone running it on a 96-core EPYC, and
neither is right.

### `body_limit_bytes` — `xerj-api/src/router.rs:27`

```rust
const DEFAULT_BODY_LIMIT_BYTES: usize = 100 * 1024 * 1024;
```

Hardcoded 100 MB request body limit. A bulk indexer that streams 200 MB
NDJSON requests can't even submit them. There is no override. Move to
`Config.api.body_limit_bytes`, default 100 MB, log on init.

### Memtable + WAL — `xerj-storage/src/index_store.rs:155–156`

```rust
memtable_max_bytes: 32 * 1024 * 1024,
wal_max_size_bytes: 128 * 1024 * 1024,
```

These show up in `IndexStoreConfig::default()`. There IS a `StorageConfig`
struct in `xerj-common`. It has these fields. They are not wired through.
The defaults always win in production. Pure plumbing fix.

### `MEMTABLE_SHARDS` — `xerj-storage/src/index_store.rs:176` and `xerj-engine/src/memtable.rs:321`

```rust
pub const MEMTABLE_SHARDS: usize = 16;
```

Compile-time. On a 96-core machine with 4 indices you have 64 effective
shards; ingest is bottlenecked on coarse-grained sharding. On a 2-core
edge VM 16 shards is cache-thrashing overkill. This needs to be a
runtime parameter, derived from `num_cpus::get()` if not set.

This one is a real refactor — the const is used in `Vec<Mutex<...>>`
sizing and in `xxh3_64(doc_id) & 15` routing. Touching it means
turning the static array into a `Box<[Mutex<...>]>` and replacing the
`& 15` with `% n`. Probably 40–60 lines across both crates. Worth it.

### Merge tier sizes — `xerj-storage/src/merge.rs:76–77`

```rust
tier_floor_bytes: 5 * 1024 * 1024,        // 5 MB
max_merged_segment_bytes: 5 * 1024 * 1024 * 1024,  // 5 GB
```

Lucene exposes these as `index.merge.policy.*`. We expose nothing.
A workload with millions of tiny segments and an operator who wants
aggressive merging has no recourse.

---

## P2 — performance pitfalls that matter

### KNN re-decodes every segment on every query

`xerj-engine/src/index.rs:2168–2196` is a loop over every segment that
calls `open_segment()`, decodes the entire stored section (often
hundreds of MB), parses the JSON with simd_json, and discards the
result at function exit. The DocValues cache at line 5317 already
proves we know how to do this right; the KNN path just doesn't use it.

The fix is: cache decoded stored sections per segment. Segments are
immutable, so the cache never invalidates until the segment is merged
away. Repeated KNN over the same index goes from "scan everything every
time" to "scan once, then memory access."

### `to_vec()` on already-owned segment buffers

Two egregious cases in the same file:

```rust
// xerj-engine/src/index.rs:1731
match simd_json::serde::from_slice(&mut stored_bytes.to_vec())  // why?
// xerj-engine/src/index.rs:2181
if let Ok(docs) = simd_json::serde::from_slice::<Vec<Value>>(&mut stored_bytes.to_vec())
```

`stored_bytes` is already a `Vec<u8>`. We then `.to_vec()` it. That's a
full memcpy of potentially 100 MB+ buffers, just so we can hand simd_json
a `&mut [u8]`. simd_json wants a mutable slice; pass `&mut stored_bytes`.
Two-character fix, big win.

### Terms agg sorts the whole bucket map

`xerj-engine/src/aggs.rs:7237`:

```rust
let mut sorted: Vec<(String, Vec<usize>)> = bucket_map.into_iter().collect();
sorted.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
sorted.truncate(size);
```

For 10 million buckets with `size=10`, this sorts 10 million elements
and discards 9_999_990 of them. TopK with a `BinaryHeap` of size `size`
costs `O(M log K)` instead of `O(M log M)`. On 10M buckets / size=10
that's ~80 million ops vs ~230 million ops, plus better cache behavior.

Every aggregation that does `sort + truncate` has this problem. Same
pattern repeats at lines 7357, 7414. Refactor once, fix three places.

---

## Patterns I want to flag, not just instances

### `.unwrap()` after a guard ten lines up is bad form

There are 600+ `.unwrap()` calls in `xerj-api/src/es_compat.rs` alone.
Most of them are followed by a comment to the effect of "safe because
of the check above." That works until someone else moves the check or
inverts the condition. We get a panic in production from a Slack
message that asks "did anyone touch the ES_compat layer recently?"

Replace with `if let Some(...)` patterns or proper error returns. We
have `?` for a reason.

### `.clone()` is not free, even on `String`

`xerj-engine/src/turbo_ingest.rs:365`:

```rust
self.entries.push((doc_id.to_string(), source.to_vec()));
```

Per-doc clones in the bulk ingest hot path. `Arc<str>` for `doc_id`
and `Arc<[u8]>` for `source` would let us push the cheap thing (Arc
clone is a refcount bump). On a 5K-doc batch we save 10 000
allocations, which matters at 1.5M docs/s.

### `match { ... _ => unreachable!() }` is a future panic

`xerj-engine/src/index.rs:1456` and `xerj-engine/src/aggs.rs:1314`.
When someone adds a new `QueryNode` variant or operator, the compiler
won't tell us — `unreachable!()` will, at runtime, in production. Use
exhaustive matches or return a typed error. The whole point of Rust's
type system is to make this kind of check compile-time.

---

## What's good

I'm not just writing this to complain. The parts that are right:

- **Per-target `target-cpu` rustflags** in `.cargo/config.toml` after
  v0.5.9. Good. The previous global `target-cpu=native` poisoning all
  cross-builds was a real bug; the fix is principled.
- **Sharded memtable design** in `IndexStore.memtable_shards`. The
  routing via `xxh3_64(doc_id) & 15` is the right idea. Just needs to
  not be hardcoded.
- **WAL writer is single-mutex** — the right call for monotonic
  `seq_no` generation. Hold time is short. Don't break this.
- **Bulk parse path** with parallel NDJSON parsing in
  `xerj-engine/src/bulk.rs` is well-engineered. The per-index
  serialization that follows it (Finding 18 in the perf audit) is the
  weak link, not the parser.
- **DocValues cache** at `xerj-engine/src/index.rs:5317` is exactly
  the right pattern for immutable-segment data. Use this template
  for the KNN segment cache.

---

## Action plan

**This release (before v0.5.10):**
1. Cap `size` (P0-1)
2. Cap bool depth (P0-2)
3. Cap mget (P0-3)
4. Cap terms cardinality (P0-4)
5. Wire `body_limit_bytes` to Config (P1)
6. Wire memtable/WAL sizes to Config (P1)

**v0.5.11:**
7. `MEMTABLE_SHARDS` runtime configurability
8. Merge policy in Config
9. KNN segment cache (P2-1)
10. Remove `to_vec()` on stored_bytes (P2-2)

**v0.5.12+:**
11. Terms agg TopK heap (P2-3)
12. `Arc<str>`/`Arc<[u8]>` for doc_id and source in turbo path
13. Eliminate `.unwrap()` from request-handling code paths

**Standing rule:** no new code lands with `unreachable!()` in a request
path or `.unwrap()` on user-controlled input.

---

*Review compiled from three parallel audit passes (stubs/hardcodes,
allocations/OOM, perf hotspots) on 96 305 LOC across 13 crates,
concrete file:line evidence preserved for each finding.*
