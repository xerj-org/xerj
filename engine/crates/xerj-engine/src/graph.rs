//! Graph expansion (second brain) — batched, columnar, bounded.
//!
//! A relationship layer over documents that already exist. Edges are ordinary
//! documents in a reserved edges index (schema: SECOND_BRAIN contract §2); this
//! module is the ONLY engine-side traversal over them. It is deliberately not a
//! graph database: no query language, no shortest-path, no PageRank, and no
//! unbounded traversal — expansion is a bounded, batched read (≤2 hops per
//! call) that composes client-side by feeding `reachable` back in as the next
//! call's frontier.
//!
//! ## Why a column scan instead of per-node queries
//!
//! Expanding a frontier of F nodes as F term queries costs F query setups and F
//! segment walks. Instead, each hop resolves the WHOLE frontier to per-segment
//! keyword ordinals (one O(log T) FST probe per frontier id via
//! [`xerj_storage::doc_values::KeywordColumn::ord_for_term`]) and then makes
//! ONE pass over the segment's `src` ordinal array (plus the `dst` array for
//! direction `both`) with O(1) hash-set membership per row — cost is
//! frontier-size-independent. Everything an admitted edge needs (`edge_id`,
//! `src`, `dst`, `type`, `weight`, `valid_at`, `invalid_at`) is random-accessed
//! from doc-values columns; the hop path NEVER touches `_source` (no
//! `StoredSlices`, no JSON parse, no hydration — evidence is hydrated by the
//! HTTP layer AFTER traversal, on the bounded result, via an `ids` query).
//!
//! ## Correctness sources (same machinery the columnar search paths use)
//!
//! - Segment list: [`Index::store_snapshot`] once per request.
//! - Columns: `Index::dv_columns_for` (cached per-segment sidecar).
//! - Liveness under updates/deletes: `VersionMap::ghost_events` gate +
//!   `Index::ghost_positions_for` word-bitmaps — the exact source of truth
//!   `scored_columnar` uses, so a soft-invalidated edge re-indexed under the
//!   same `_id` never double-surfaces from its stale segment row.
//! - Unflushed edges: a bounded walk of the sharded FTS memtable
//!   (`ShardedFtsMemtable::all_docs_with_sources_arc`) per hop — correctness,
//!   not speed; the memtable is bounded by the flush threshold and the walk is
//!   reported in-band (`memtable_docs_scanned`).
//!
//! ## Honesty in-band
//!
//! Every response carries [`GraphExpandStats`]: clipped frontiers, clipped
//! edges, bi-temporally excluded edges, type-filtered edges, segments skipped
//! for missing columns (the hop never silently falls back to hydration —
//! skipped means reported), and scan-cost counters.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use xerj_common::XerjError;

use super::Index;
use xerj_storage::doc_values::{Column, KeywordColumn, NumericColumn};

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

/// The not-a-graph-database refusal, verbatim (contract §4.6). Shared with the
/// HTTP layer so the engine error reason and the API 400 body carry the exact
/// same sentence — the wording is normative, not cosmetic: it tells the caller
/// the supported alternative (iterate on `reachable`).
pub const GRAPH_HOPS_CAP_REASON: &str = "hops is capped at 2: XERJ's second brain is a \
     relationship layer over documents, not a graph database (no Cypher, no shortest-path, \
     no variable-depth traversal). Iterate: expand again from this response's 'reachable' ids.";

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
    /// 1 or 2. 0 or >2 → XerjError::InvalidQuery (see module docs).
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
/// `weight` is f64 because numeric columns store f64 bits (see
/// `build_doc_value_columns`); timestamps are exact for |v| < 2^53 (epoch-ms
/// until year ~287396).
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

/// Get a keyword column by name, or `None` when absent or numeric-typed
/// (numeric-typed = a writer violated the §2.2 type discipline; the segment is
/// treated as column-less rather than guessed at).
///
/// Raw `cols.get`, with none of `fast_aggs::dv_col`'s multi-field fallback,
/// on purpose: every call site passes a LITERAL §2.2 envelope name (`src`,
/// `dst`, `edge_id`, `type`, `weight`, `valid_at`, `invalid_at`). No
/// user-supplied or mapping-derived field name reaches these lookups, so a
/// `<field>.keyword` / `<field>.raw` multi-field suffix cannot occur here and
/// #120's brute-vs-columnar resolution gap does not apply. A miss is also
/// reported (`segments_without_columns`) rather than silently dropped.
fn kw_col<'a>(cols: &'a super::DocValueMap, name: &str) -> Option<&'a KeywordColumn> {
    match cols.get(name) {
        Some(Column::Keyword(k)) => Some(k),
        _ => None,
    }
}

/// Get a numeric column by name (same honesty rule as [`kw_col`]).
fn num_col<'a>(cols: &'a super::DocValueMap, name: &str) -> Option<&'a NumericColumn> {
    match cols.get(name) {
        Some(Column::Numeric(n)) => Some(n),
        _ => None,
    }
}

/// Numeric columns store f64 BITS in an i64 slot (one representation for the
/// whole column, integers exact to 2^53). Decode before use — using the raw
/// i64 silently corrupts every timestamp/weight compare.
#[inline]
fn bits_to_f64(v: i64) -> f64 {
    f64::from_bits(v as u64)
}

/// Epoch-ms from a memtable JSON number. Writers emit integer epoch-ms, but a
/// float that reaches us (e.g. a client JSON-encoding 1.7536e12) still means
/// the same instant, so accept it rather than mis-classifying the row.
#[inline]
fn json_ms(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// Bi-temporal visibility (contract §3.2). Returns `false` (and the caller
/// counts `expired_excluded`) for edges asserted after `as_of` or — unless
/// `include_expired` — invalidated at/before `as_of`. `include_expired` only
/// disables the invalidation half: a future-asserted edge is invisible either
/// way (it did not exist yet in the world being asked about).
#[inline]
fn bitemporal_visible(
    valid_at_ms: i64,
    invalid_at_ms: Option<i64>,
    as_of_ms: i64,
    include_expired: bool,
) -> bool {
    if valid_at_ms > as_of_ms {
        return false;
    }
    if include_expired {
        return true;
    }
    match invalid_at_ms {
        Some(inv) => inv > as_of_ms,
        None => true,
    }
}

/// Collected-edge bookkeeping: `from_memtable` implements the dedupe rule that
/// a memtable row wins over any segment row with the same edge_id (it is the
/// newer version — relevant only in the conservative ghost-bitmap-unavailable
/// path; when ghost bitmaps resolve, the stale segment row never surfaces).
struct Collected {
    edge: GraphEdgeLite,
    from_memtable: bool,
}

impl Index {
    /// Batched graph expansion over an edges index (contract §2 schema).
    ///
    /// Sync fn (no await points): all reads are memtable walks + doc-values
    /// column access — the same blocking pattern the columnar agg/search fast
    /// paths already use from async handlers.
    ///
    /// Explicitly forbidden on this path: `StoredSlices` access, JSON parsing
    /// of stored docs, hydration helpers, per-frontier-id queries, and any
    /// per-edge `_source` read. Evidence hydration happens AFTER traversal in
    /// the HTTP layer, on the bounded result set, via an `ids` query.
    pub fn graph_expand(&self, req: &GraphExpandRequest) -> xerj_common::Result<GraphExpandResult> {
        if req.hops == 0 || req.hops > GRAPH_MAX_HOPS {
            // Same normative sentence for both bounds: the caller's fix
            // (iterate on `reachable`) is identical, and the HTTP layer
            // forwards this reason verbatim as its 400 body.
            let reason = if req.hops == 0 {
                format!("hops must be at least 1. {GRAPH_HOPS_CAP_REASON}")
            } else {
                GRAPH_HOPS_CAP_REASON.to_string()
            };
            return Err(XerjError::invalid_query(reason));
        }

        let mut stats = GraphExpandStats::default();

        // Dedupe the request frontier preserving input order (the order is
        // part of the `reachable` contract), then clip with the clip counted.
        let mut frontier: Vec<String> = Vec::with_capacity(req.frontier.len().min(16));
        {
            let mut seen: HashSet<&str> = HashSet::with_capacity(req.frontier.len());
            for id in &req.frontier {
                if seen.insert(id.as_str()) {
                    frontier.push(id.clone());
                }
            }
        }
        if frontier.is_empty() {
            return Err(XerjError::invalid_query(
                "graph expansion requires a non-empty frontier (after dedup)",
            ));
        }
        if frontier.len() > GRAPH_MAX_FRONTIER {
            stats.frontier_clipped += (frontier.len() - GRAPH_MAX_FRONTIER) as u64;
            frontier.truncate(GRAPH_MAX_FRONTIER);
        }
        let admitted_frontier = frontier.clone();

        let cap = req.max_result_edges.clamp(1, GRAPH_MAX_RESULT_EDGES);
        let scan_src = matches!(req.direction, GraphDirection::Out | GraphDirection::Both);
        let scan_dst = matches!(req.direction, GraphDirection::In | GraphDirection::Both);

        // Point-in-time capture once per request: segment list + "any ghosts?"
        // gate — the same pair `scored_columnar` snapshots, so liveness
        // decisions are consistent across every segment of this expansion.
        let snapshot = self.store_snapshot();
        let segments_dir = self.data_dir.join("segments");
        let any_ghosts = self.store.version_map.ghost_events() > 0;

        // Every node ever admitted to a frontier or discovered as an endpoint;
        // hop-2 frontier construction subtracts it ("minus already-visited").
        let mut visited: HashSet<String> = frontier.iter().cloned().collect();
        let mut collected: HashMap<String, Collected> = HashMap::new();
        // True once the result cap overflowed: the current container finishes
        // (counting overflow into `edges_clipped`) and everything after —
        // remaining segments, the memtable walk, further hops — is aborted.
        let mut capped = false;

        for hop in 1..=req.hops {
            if capped || frontier.is_empty() {
                break;
            }
            let frontier_set: HashSet<&str> = frontier.iter().map(String::as_str).collect();
            // edge_ids first collected during THIS hop, for next-frontier
            // construction (order does not matter here; the walk below sorts).
            let mut hop_new: Vec<String> = Vec::new();

            // ── Segments: one columnar pass per segment for the whole frontier ──
            for meta in &snapshot.segments {
                if capped {
                    break;
                }
                let Some(cols) = self.dv_columns_for(&segments_dir, &meta.id) else {
                    stats.segments_without_columns += 1;
                    continue;
                };
                let cols = cols.value();
                // Required columns. Missing/mistyped `src`/`dst` (needed for
                // the scan AND for emission) or `edge_id`/`type`/`weight`/
                // `valid_at` (needed for emission/filtering) → the segment is
                // schema-corrupt or edge-free (e.g. meta-doc-only): skip it,
                // reported — never guess, never hydrate. A missing
                // `invalid_at` column is NORMAL (no edge in this segment was
                // ever invalidated) and means "no edge here is invalidated".
                let (Some(k_src), Some(k_dst), Some(k_edge), Some(k_type)) = (
                    kw_col(cols, "src"),
                    kw_col(cols, "dst"),
                    kw_col(cols, "edge_id"),
                    kw_col(cols, "type"),
                ) else {
                    stats.segments_without_columns += 1;
                    continue;
                };
                let (Some(n_weight), Some(n_valid)) =
                    (num_col(cols, "weight"), num_col(cols, "valid_at"))
                else {
                    stats.segments_without_columns += 1;
                    continue;
                };
                let n_invalid = match cols.get("invalid_at") {
                    None => None,
                    Some(Column::Numeric(n)) => Some(n),
                    // Present but keyword-typed = a writer stored ISO strings;
                    // an as-of compare against strings would be a lie.
                    Some(Column::Keyword(_)) => {
                        stats.segments_without_columns += 1;
                        continue;
                    }
                };

                // Frontier → per-segment ordinal sets (one O(log T) FST probe
                // per frontier id). Both empty → nothing of the frontier
                // exists in this segment; skip without scanning.
                let src_ords: HashSet<u32> = if scan_src {
                    frontier
                        .iter()
                        .filter_map(|id| k_src.ord_for_term(id))
                        .collect()
                } else {
                    HashSet::new()
                };
                let dst_ords: HashSet<u32> = if scan_dst {
                    frontier
                        .iter()
                        .filter_map(|id| k_dst.ord_for_term(id))
                        .collect()
                } else {
                    HashSet::new()
                };
                if src_ords.is_empty() && dst_ords.is_empty() {
                    continue;
                }

                // Liveness bitmap (deleted + superseded rows). Unbuildable →
                // conservatively skip the whole segment, reported — admitting
                // possibly-stale rows would silently resurrect overwritten
                // edges (e.g. a soft-invalidation would un-happen).
                let ghosts = if any_ghosts {
                    match self.ghost_positions_for(meta.id.as_str(), meta.doc_count) {
                        Some(bm) => Some(bm),
                        None => {
                            stats.segments_without_columns += 1;
                            continue;
                        }
                    }
                } else {
                    None
                };

                // Per-segment type-allowlist ordinal set. A term absent from
                // this segment's dictionary matches nothing here.
                let type_ords: Option<HashSet<u32>> = req.types.as_ref().map(|ts| {
                    ts.iter()
                        .filter_map(|t| k_type.ord_for_term(t))
                        .collect::<HashSet<u32>>()
                });

                stats.segments_scanned += 1;

                // THE scan: one pass over the ordinal array(s) for the WHOLE
                // frontier, O(1) membership per row. Direction `both` reads
                // both ords arrays in the same position loop — cost-identical
                // to two passes, and an edge admitted by both sides is
                // processed exactly once.
                let doc_count = k_src.doc_count;
                for pos in 0..doc_count {
                    let via_src =
                        scan_src && k_src.ord_for(pos).is_some_and(|o| src_ords.contains(&o));
                    let admitted = via_src
                        || (scan_dst && k_dst.ord_for(pos).is_some_and(|o| dst_ords.contains(&o)));
                    if !admitted {
                        continue;
                    }
                    // Liveness before anything else (a ghost row is not an
                    // edge at all — its live version surfaces elsewhere).
                    if let Some(bm) = &ghosts {
                        let p = pos as usize;
                        if (bm[p / 64] >> (p % 64)) & 1 == 1 {
                            continue;
                        }
                    }
                    // Type filter (counted).
                    if let Some(t_ords) = &type_ords {
                        match k_type.ord_for(pos) {
                            Some(o) if t_ords.contains(&o) => {}
                            _ => {
                                stats.type_filtered += 1;
                                continue;
                            }
                        }
                    }
                    // Bi-temporal cut (counted). A null valid_at is a
                    // schema-corrupt row (writers always emit it); such a row
                    // cannot be placed on the timeline, so it is skipped.
                    let Some(valid_at_ms) = n_valid.get(pos).map(|v| bits_to_f64(v) as i64) else {
                        continue;
                    };
                    let invalid_at_ms = n_invalid
                        .and_then(|c| c.get(pos))
                        .map(|v| bits_to_f64(v) as i64);
                    if !bitemporal_visible(
                        valid_at_ms,
                        invalid_at_ms,
                        req.as_of_ms,
                        req.include_expired,
                    ) {
                        stats.expired_excluded += 1;
                        continue;
                    }
                    // Emit — keyword random access only, still no `_source`.
                    let Some(edge_id) = k_edge.ord_for(pos).and_then(|o| k_edge.term_for_ord(o))
                    else {
                        continue;
                    };
                    if collected.contains_key(edge_id) {
                        continue; // rediscovered (earlier hop or the other column) — no cap cost
                    }
                    if collected.len() >= cap {
                        stats.edges_clipped += 1;
                        capped = true; // finish counting this segment, then abort
                        continue;
                    }
                    let (Some(src), Some(dst), Some(edge_type), Some(weight)) = (
                        k_src.ord_for(pos).and_then(|o| k_src.term_for_ord(o)),
                        k_dst.ord_for(pos).and_then(|o| k_dst.term_for_ord(o)),
                        k_type.ord_for(pos).and_then(|o| k_type.term_for_ord(o)),
                        n_weight.get(pos).map(bits_to_f64),
                    ) else {
                        continue;
                    };
                    let edge_id = edge_id.to_string();
                    hop_new.push(edge_id.clone());
                    collected.insert(
                        edge_id.clone(),
                        Collected {
                            edge: GraphEdgeLite {
                                edge_id,
                                src: src.to_string(),
                                dst: dst.to_string(),
                                edge_type: edge_type.to_string(),
                                weight,
                                valid_at_ms,
                                invalid_at_ms,
                                hop,
                            },
                            from_memtable: false,
                        },
                    );
                }
            }

            // ── Memtable: unflushed edges (correctness, not speed) ────────────
            // Bounded by the flush threshold; the walk cost is reported
            // in-band. Values come from the in-memory source JSON — that is
            // not `_source` hydration (no segment I/O, no stored-section
            // parse), it IS the memtable's native representation.
            // Once `capped` flips mid-walk the loop still finishes the
            // memtable (it is one container — same rule as a segment):
            // remaining admits fall into the `len() >= cap` arm and are
            // counted into `edges_clipped`.
            if !capped {
                for (id, source) in self.memtable.all_docs_with_sources_arc() {
                    stats.memtable_docs_scanned += 1;
                    // Tombstoned (deleted-by-API) rows are not edges.
                    if self.store.version_map.get(&id).is_some_and(|v| v.deleted) {
                        continue;
                    }
                    let src = source.get("src").and_then(Value::as_str);
                    let dst = source.get("dst").and_then(Value::as_str);
                    let via_src = scan_src && src.is_some_and(|s| frontier_set.contains(s));
                    let admitted =
                        via_src || (scan_dst && dst.is_some_and(|d| frontier_set.contains(d)));
                    if !admitted {
                        continue;
                    }
                    // Emission needs both endpoints + the envelope fields; a
                    // row missing any of them (only the brain-meta doc or a
                    // non-§2 writer) is not an edge.
                    let (Some(src), Some(dst)) = (src, dst) else {
                        continue;
                    };
                    let Some(edge_id) = source.get("edge_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(edge_type) = source.get("type").and_then(Value::as_str) else {
                        continue;
                    };
                    if let Some(allow) = &req.types {
                        if !allow.iter().any(|t| t == edge_type) {
                            stats.type_filtered += 1;
                            continue;
                        }
                    }
                    let Some(valid_at_ms) = source.get("valid_at").and_then(json_ms) else {
                        continue;
                    };
                    let invalid_at_ms = source.get("invalid_at").and_then(json_ms);
                    if !bitemporal_visible(
                        valid_at_ms,
                        invalid_at_ms,
                        req.as_of_ms,
                        req.include_expired,
                    ) {
                        stats.expired_excluded += 1;
                        continue;
                    }
                    let Some(weight) = source.get("weight").and_then(Value::as_f64) else {
                        continue;
                    };
                    let edge = GraphEdgeLite {
                        edge_id: edge_id.to_string(),
                        src: src.to_string(),
                        dst: dst.to_string(),
                        edge_type: edge_type.to_string(),
                        weight,
                        valid_at_ms,
                        invalid_at_ms,
                        hop,
                    };
                    match collected.get_mut(edge_id) {
                        Some(existing) => {
                            // Memtable wins over a segment row with the same
                            // edge_id (it is the newer version); the earliest
                            // discovery hop is kept so the sort/frontier
                            // semantics don't shift.
                            if !existing.from_memtable {
                                let keep_hop = existing.edge.hop.min(hop);
                                existing.edge = GraphEdgeLite {
                                    hop: keep_hop,
                                    ..edge
                                };
                                existing.from_memtable = true;
                            }
                        }
                        None => {
                            if collected.len() >= cap {
                                stats.edges_clipped += 1;
                                capped = true;
                                continue;
                            }
                            hop_new.push(edge.edge_id.clone());
                            collected.insert(
                                edge.edge_id.clone(),
                                Collected {
                                    edge,
                                    from_memtable: true,
                                },
                            );
                        }
                    }
                }
            }

            // ── Next frontier: endpoints newly discovered this hop ───────────
            // Walked in the hop's stable order (weight desc, edge_id asc — hop
            // is constant within the block) so a hop-2 frontier clip drops the
            // same ids on every run.
            if hop < req.hops && !capped {
                let mut hop_edges: Vec<&GraphEdgeLite> = hop_new
                    .iter()
                    .filter_map(|eid| collected.get(eid).map(|c| &c.edge))
                    .collect();
                hop_edges.sort_by(|a, b| {
                    b.weight
                        .total_cmp(&a.weight)
                        .then_with(|| a.edge_id.cmp(&b.edge_id))
                });
                let mut next: Vec<String> = Vec::new();
                for e in hop_edges {
                    for id in [&e.src, &e.dst] {
                        if !visited.contains(id) {
                            visited.insert(id.clone());
                            next.push(id.clone());
                        }
                    }
                }
                if next.len() > GRAPH_MAX_FRONTIER {
                    stats.frontier_clipped += (next.len() - GRAPH_MAX_FRONTIER) as u64;
                    next.truncate(GRAPH_MAX_FRONTIER);
                }
                frontier = next;
            }
        }

        // ── Final order + reachable ──────────────────────────────────────────
        let mut edges: Vec<GraphEdgeLite> = collected.into_values().map(|c| c.edge).collect();
        edges.sort_by(|a, b| {
            a.hop
                .cmp(&b.hop)
                .then_with(|| b.weight.total_cmp(&a.weight))
                .then_with(|| a.edge_id.cmp(&b.edge_id))
        });

        let mut reachable: Vec<String> = Vec::with_capacity(admitted_frontier.len() + edges.len());
        let mut seen: HashSet<String> = HashSet::with_capacity(admitted_frontier.len());
        let push_unique = |out: &mut Vec<String>, seen: &mut HashSet<String>, id: &str| {
            if seen.insert(id.to_string()) {
                out.push(id.to_string());
            }
        };
        for id in &admitted_frontier {
            push_unique(&mut reachable, &mut seen, id);
        }
        for e in &edges {
            push_unique(&mut reachable, &mut seen, &e.src);
            push_unique(&mut reachable, &mut seen, &e.dst);
        }

        Ok(GraphExpandResult {
            edges,
            reachable,
            stats,
        })
    }
}
