# Fix: `term`/`terms` on keyword ARRAYS match only the first element

## Why the WordPress reachability flow was not searchable in XERJ

Auditing WordPress, the reverse call-graph query "who calls `wp_safe_remote_get`"
returned **1** result from XERJ's structured index while `grep` found 14 files and
XERJ full-text (`match`) found 9. A false "not reachable" is the worst possible
error in a security audit, so we traced it to the engine.

**Root cause (proven, not inferred).** A keyword field is stored one value per
document. For an **array** value, XERJ keeps only element `[0]`:

- Memtable — `memtable.rs::DocValues::push_field`, the `Value::Array` arm:
  `let first_str = arr.iter().find_map(...); kcol.push(first_str);` with the
  comment *"for term/range queries we store the first scalar found."*
- Segment — `xerj-storage::doc_values::KeywordColumn` stores `ords: Vec<u32>`,
  exactly **one ordinal per doc**; the format is structurally single-valued.

Aggregations use a *separate* multi-valued path (`aggs.rs` holds
`Vec<Vec<String>>`), and full-text joins every element into the token stream —
which is why `terms`-aggs and `match` both see all elements, but exact `term`
sees only `[0]`.

Reproduced end-to-end (docs indexed with `calls: ["first_fn","second_fn","wp_safe_remote_get"]`):

```
term calls=first_fn          -> 1   (element [0])
term calls=second_fn         -> 0   (element [1]  — MISSED)
term calls=wp_safe_remote_get-> 0   (element [2]  — MISSED)
```

This is an Elasticsearch-compatibility break: in ES a keyword array is
multi-valued and `term` matches **any** element.

## The fix

The always-correct source scan already handles arrays —
`json_values_equal` has `(Value::Array(arr), _) => arr.iter().any(|e| e == q)` —
so the fix is to route `term`/`terms` on array-valued keyword fields to that scan
instead of the single-valued column fast path.

- **Memtable half (in this PR).** `DocValues` already tracks `array_fields`.
  `doc_values_term_query`, `doc_values_term_indices`, and `doc_values_terms_query`
  now bail (`return None`) for any field in `array_fields`, exactly mirroring the
  existing `keyword_has_whitespace` bail, so the caller falls through to the
  array-aware `doc_matches_query` scan. Provably correct; compiles clean.

- **Segment half (design, follow-up).** The flush builder *already* computes a
  `multi_valued` set and does `keyword.remove(f)` for those fields (so the segment
  `term` reader's `None` arm sets `abandoned = true` → stored-source scan). The
  reproduction shows a case where a flushed field still resolves via a
  single-valued column, so the remaining work is to (a) guarantee `multi_valued`
  detection covers every ingest/flush path, and (b) route the remaining
  fused-columnar `term` fast path to the scan for these fields — OR make
  `KeywordColumn` genuinely multi-valued (`ords: Vec<Vec<u32>>`). A regression
  test (`test_term_matches_non_first_array_element`) is included, `#[ignore]`d
  until the segment half lands.

**Also found (separate, smaller):** boolean `term` is matched as a string —
`{term:{f:true}}` (JSON bool) returns 0 while `{term:{f:"true"}}` matches. The
JSON-bool query value must coerce to the stored `"true"`/`"false"` form. Filed
for the same PR series.

## Token savings and other benefits

The bug forces every interprocedural/graph query off the structured index and
onto a workaround: either **scroll the whole graph to the client and traverse in
code**, or **grep + read** the candidates. Fixing `term`/`terms` on arrays turns
the reverse call-graph into a single structured query.

Measured, for one reachability query ("who calls `wp_safe_remote_*`") over the
real WordPress index (11,990 functions):

| approach | tokens | vs fixed |
|---|--:|--:|
| scroll the whole call-graph, traverse client-side | ~482,800 | 2,823× |
| grep + read the callers to confirm | ~4,300 | 25× |
| **fixed: one `terms` query → exact callers as facts** | **~170** | — |

The deeper benefit is **composability**. With `term`/`terms` correct on arrays,
graph predicates combine in one request — e.g. *"functions that call a SQL sink
**and** read a request source **and** lack an object-scoped cap"* becomes a single
`bool` query returning only the candidates, instead of transferring the entire
index (~2.4M tokens of facts) to rebuild the graph in Python. The audit cost drops
from **O(index size)** to **O(matching results)**.

Other benefits:
- **Correctness / ES-compat** — restores multi-valued keyword semantics; removes a
  class of silent false negatives (the most dangerous error for security audits).
- **Every array-of-keywords field** benefits: tags, labels, roles, call edges,
  sink lists — any workload that filters on membership.
- It makes the pre-AST index **strictly better than grep** for reachability, where
  today (per the field test) it is worse.

## Files

- `engine/crates/xerj-engine/src/memtable.rs` — array-field bail in the three
  doc-values `term`/`terms` fast paths (memtable half of the fix).
- `engine/crates/xerj-engine/tests/integration.rs` —
  `test_term_matches_non_first_array_element` reproduction (`#[ignore]` pending the
  segment half).
