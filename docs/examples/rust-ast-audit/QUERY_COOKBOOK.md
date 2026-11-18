# Query cookbook — auditing Rust with XERJ

Every query below runs against the live AST substrate (three indices:
`rustfns`, `rustcalls`, `rustroutes`) built by `rust_ast_index.py` + `ingest.py`.
Hit counts are from XERJ's own source at the current `main` (5,095 functions,
87,137 call edges, 477 routes; 197 files, 100% function coverage, 0 parse errors).

The point of each query is that it narrows ~207k lines of Rust to a candidate set
an agent can actually *read* — and expresses a scoped question (source **and**
sink in the same reachable function; recursion **cycle** with no depth guard)
that a file-scoped grep cannot.

`$X = http://127.0.0.1:9310`

## Reachability: what an unauthenticated request can hit

```bash
# Every axum-handler-shaped function (takes Path/Query/Json/State/... + async).
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{"filter":[
  {"term":{"is_handler_shaped":true}},{"term":{"is_test":false}}]}}}'
# → 298
```

```bash
# The route table itself: method + path → handler.
curl -s -H 'Content-Type: application/json' "$X/rustroutes/_search" -d '{"query":{"wildcard":{"path":"*sql*"}},
  "_source":["method","path","handler","file","line"]}'
# → POST /_sql → sql_query (crates/xerj-api/src/router.rs:637)
```

## Panic / process-abort DoS

```bash
# Recursion with NO depth bound — the stack-overflow shape. (Direct self-calls;
# the call-graph CYCLE version is find_recursion_cycles.py, below.)
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{
  "filter":[{"term":{"calls_self":true}},{"term":{"is_test":false}}],
  "must_not":[{"term":{"has_depth_guard":true}}]}}}'
# → 377 (broad; the cycle finder is the sharp tool)
```

```bash
# Narrowing `as` casts on a codebase with a documented truncation-bug history.
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{"filter":[
  {"term":{"has_narrowing_cast":true}},{"term":{"is_test":false}}]}}}'
# → 375
```

## Unbounded allocation

```bash
# Allocation whose size is derived from a parameter (with_capacity/reserve/vec!/repeat).
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{"filter":[
  {"range":{"alloc_from_param_count":{"gte":1}}},{"term":{"is_test":false}}]}}}'
# → 137
```

```bash
# The cross-product blowup shape: an allocation argument that MULTIPLIES two values
# (fields.len() * like.len() — the exact shape of PR #69's F6).
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"term":{"alloc_product":true}}}'
```

## Path traversal / filesystem escape

```bash
# `.join(param)` — a path built from an attacker-influenced component — with NO
# lexical/canonical containment check in the same function.
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{
  "filter":[{"term":{"path_join_from_param":true}},{"term":{"is_test":false}}],
  "must_not":[{"term":{"validators":"containment"}}]}}}'
# → 17  (down from 56 with the blunt sink filter; the argument-provenance signal
#         is what sharpens it)
```

## Unsafe inventory (100% of sites)

```bash
# Every function containing unsafe (block or unsafe fn), non-test.
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{"filter":[
  {"term":{"unsafe_any":true}},{"term":{"is_test":false}}]}}}'
# → 19    (+2 test-only = 21 total; matches the extractor's independent count)

# Group by the unsafe operation kind (ffi_call, uninit, from_raw, ptr_arith, …).
curl -s -H 'Content-Type: application/json' "$X/rustfns/_search" -d '{"size":0,"query":{"term":{"unsafe_any":true}},
  "aggs":{"ops":{"terms":{"field":"unsafe_ops","size":20}}}}'
```

## Concurrency

```bash
# A lock guard bound to a name, with an `.await` later in the function body:
# a lock potentially held across a suspension point.
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{"filter":[
  {"term":{"lock_across_await":true}},{"term":{"is_test":false}}]}}}'
# → 15
```

## Deserialization surface

```bash
curl -s -H 'Content-Type: application/json' "$X/rustfns/_count" -d '{"query":{"bool":{"filter":[
  {"term":{"sinks":"deserialize"}},{"term":{"is_test":false}}]}}}'
# → 147
```

## The query grep cannot express: recursion CYCLES

A stack-overflow in a recursive-descent parser is rarely direct self-recursion —
it is a cycle across several functions (`parse_or → parse_and → parse_condition →
parse_or`). No single file or regex shows it; the call graph does.

```bash
python3 find_recursion_cycles.py <ast-out-dir>
# → 194 cycles, 186 unguarded. Ranks the query-parser cycle at #5;
#   the same cycle is UNGUARDED pre-#69 and GUARDED on current main.
#   Surfaced an UNFIXED sibling: the /_sql WHERE-clause parser (see FINDINGS.md).
```

## Note on the boolean-term quirk

The WordPress case study documented a XERJ bug where boolean `term` filters had to
be written as strings (`"true"`). **That is fixed on current main** — both
`{"term":{"unsafe_any":true}}` and `{"term":{"unsafe_any":"true"}}` return 21.
Verified, not assumed.
