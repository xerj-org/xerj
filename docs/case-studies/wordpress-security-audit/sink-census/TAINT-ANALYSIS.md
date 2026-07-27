# Interprocedural taint analysis on XERJ data

Once the AST scan is exported (the 6 `wp*` indices), taint analysis is a query +
graph walk over facts already in XERJ — **no local files**. `taint_analysis.py`
finds **source → sink flows**: a function that reads a request source
(`$_GET/$_POST/$_REQUEST/$_COOKIE/$_SERVER/$_FILES`) that reaches a dangerous sink
(`sql/cmd/code/deser/lfi/xss`) with **no sanitizer on the call path**.

## How it works (all from `wpaudit`)

- `wpaudit` already stores, per function: the `sources` it reads, the `sinks` it
  hits, a `sanit` flag, and its `calls` (edges). Taint is then:
  - **intra-procedural** — `sources` + `sinks` + `sanit=false` in one function;
  - **inter-procedural** — forward-DFS the call graph (unambiguous-name edges
    only) from a source function to a sink function, carrying a sanitizer-seen
    flag; stop at the first unsanitized sink.
- Flows are ranked by sink severity (cmd/code/deser > sql > lfi > xss) and joined
  to `wpsinks` for the exact dangerous call.

```bash
python3 taint_analysis.py          # -> ranked flows + taint_flows.json
```

## Result on WordPress core

```
10,000 functions in the graph, 362 read a source, 956 hit a sink
TAINT FLOWS (source -> sink, no sanitizer on path): 70
  by terminal sink class: {deser: 1, sql: 11, lfi: 30, xss: 34}
```

70 candidate flows from ~12k functions — a reviewable set, each with the full
`source -> f1 -> f2 -> ... -> sink` path.

## Honest verification of the top flows (real vs false-positive)

Reading the terminals (the discipline the whole method uses):

| flow | terminal sink | verdict |
|---|---|---|
| `save_widget` **[deser]** ($_POST) | `unserialize($serialized_instance)` | **real candidate** — request input reaches a deserialization sink; guarded by the REST `show_instance_in_rest` opt-in + hash (confirm the guard). |
| `… -> get_posts` **[sql]** | `new WP_Query()->query($args)` | **false positive** — `WP_Query::query()`, not `$wpdb->query()`; parameterized internally (receiver-type blindness). |
| `wp_ajax_trash_post -> … -> _prime_term_caches` **[sql]** | `$wpdb->get_results("… IN (%s)", intval($ids))` | **false positive** — ids are `intval`'d; and the path is *reachability*, the tainted `$_POST` value never flows to `$ids`. |
| `wp_ajax_query_attachments` **[sql]** | `WP_Query` (key allow-list) | **false positive** — auth-gated + allow-listed (read earlier). |

## What this taint pass is — and is not (state it plainly)

- It is **reachability taint** over the function-call graph: "a source function can
  transitively reach a sink function with no name-based sanitizer on the path." It
  is a fast, high-recall **triage** that turns 12k functions into ~70 candidates.
- It is **not precise data-flow.** It does not prove the specific tainted *value*
  reaches the sink argument — many SQL flows terminate in `WP_Query` (safe) or an
  `intval`-guarded `$wpdb` call, or the tainted variable simply isn't the one used
  at the sink. Those are the same false-positive drivers documented in
  `../IMPROVEMENTS.md`:
  - **receiver-typed sinks** — distinguish `$wpdb->query` from `WP_Query->query`;
  - **argument-level data-flow** — confirm the source value flows into the sink
    argument, not just that the functions are call-connected;
  - **sanitizer precision** — `intval`/`absint`/allow-list on the sink argument.
- With those refinements the 70 collapse toward the true positives (here: the
  `save_widget` deserialization candidate, then confirm its guard).

## The honest taint statement

> Every function that reads a request source is walked to every dangerous sink it
> can reach with no sanitizer on the path (70 candidate flows on WP core, ranked
> by severity, each with its full path). This is high-recall reachability triage
> over the XERJ call graph — precise per-argument data-flow and receiver-typed
> sinks are the next refinement; until then each flow is a candidate to read, not
> a confirmed bug.

This is the same contract as the rest of the workflow: the graph narrows, you
read, you verify by executing.
