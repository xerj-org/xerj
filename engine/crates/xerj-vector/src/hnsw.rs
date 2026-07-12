//! HNSW (Hierarchical Navigable Small World) graph index.
//!
//! Reference: Malkov & Yashunin 2018, "Efficient and Robust Approximate Nearest
//! Neighbor Search Using Hierarchical Navigable Small World Graphs."
//!
//! Design decisions:
//! - M=16 bidirectional connections per layer (good accuracy/memory trade-off)
//! - ef_construction=200 (high recall during build)
//! - Level selection uses the standard ln-based formula
//! - Neighbor selection uses the paper's Algorithm 4 diversity heuristic
//!   (the variant hnswlib and Lucene use) — plain closest-M packs a node's
//!   links into one tight cluster and measurably breaks navigability
//! - Graph is entirely in-memory; persistence is handled by the storage crate
//!
//! In-memory layout (2026-07-12 flat-slab rework): nodes live in a single
//! slot-indexed slab — one contiguous `Vec<f32>` for vectors, `Vec<Vec<u32>>`
//! neighbor lists per layer, and a `Vec<u64>` bitmap for the search-time
//! visited set. The previous `HashMap<u64, Arc<RwLock<Node>>>` paid a hash
//! lookup + two pointer chases + an RwLock acquisition *per neighbor
//! expansion*, which made beam search ~5× slower than the distance math
//! itself. The on-disk format and the public (external-u64-id) API are
//! unchanged.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::RwLock;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};
use xerj_common::XerjError;

use crate::distance::{compute_distance, DistanceMetric};

/// Result alias.
pub type Result<T> = std::result::Result<T, XerjError>;

// ── On-disk format constants ─────────────────────────────────────────────────
//
// Bumped monotonically on any incompatible change. Loader rejects files
// from a version it doesn't recognise (caller falls back to WAL replay).
//
// v1 — initial format (graph + id_map sidecar)
// v2 — adds trailing tombstone block: u32 num + u64 × num
//      (read before the trailing CRC). Loader handles v1 files by
//      treating the tombstone set as empty.
const HNSW_MAGIC: &[u8; 8] = b"XHNS0001";
const HNSW_FORMAT_VERSION: u32 = 2;

#[inline]
fn metric_to_u8(m: DistanceMetric) -> u8 {
    match m {
        DistanceMetric::Cosine => 0,
        DistanceMetric::L2 => 1,
        DistanceMetric::DotProduct => 2,
    }
}

#[inline]
fn u8_to_metric(b: u8) -> Result<DistanceMetric> {
    match b {
        0 => Ok(DistanceMetric::Cosine),
        1 => Ok(DistanceMetric::L2),
        2 => Ok(DistanceMetric::DotProduct),
        other => Err(corrupt(&format!("unknown distance metric byte {other}"))),
    }
}

#[inline]
fn io_to_xerj(e: std::io::Error) -> XerjError {
    XerjError::storage(format!("HNSW persistence I/O: {e}"))
}

#[inline]
fn corrupt(msg: &str) -> XerjError {
    XerjError::storage(format!("HNSW graph corrupt: {msg}"))
}

/// Dot product with eight independent accumulators.
///
/// A single-accumulator `for` loop is a strict serial FP dependency chain
/// that rustc must NOT auto-vectorize (float addition is not associative),
/// so the beam search was spending ~5 ns/dim in scalar fmadds. Eight
/// independent chains let LLVM keep multiple SIMD lanes busy; the exact
/// f32 sum differs from the serial order only in rounding, which is fine
/// here — graph distances only decide beam ORDER and callers rescore
/// exact similarities downstream.
#[inline]
fn dot_unrolled(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 8];
    let ca = a.chunks_exact(8);
    let cb = b.chunks_exact(8);
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        acc[0] += x[0] * y[0];
        acc[1] += x[1] * y[1];
        acc[2] += x[2] * y[2];
        acc[3] += x[3] * y[3];
        acc[4] += x[4] * y[4];
        acc[5] += x[5] * y[5];
        acc[6] += x[6] * y[6];
        acc[7] += x[7] * y[7];
    }
    let mut s = ((acc[0] + acc[4]) + (acc[1] + acc[5])) + ((acc[2] + acc[6]) + (acc[3] + acc[7]));
    for (x, y) in ra.iter().zip(rb.iter()) {
        s += x * y;
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Parameters
// ─────────────────────────────────────────────────────────────────────────────

/// HNSW construction parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswParams {
    /// Maximum number of bidirectional connections per node per layer.
    pub m: usize,
    /// Size of the dynamic candidate list during construction.
    pub ef_construction: usize,
    /// Distance metric.
    pub metric: DistanceMetric,
    /// Vector dimensionality.
    pub dim: usize,
}

impl HnswParams {
    pub fn new(dim: usize, metric: DistanceMetric) -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            metric,
            dim,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Flat slab storage
// ─────────────────────────────────────────────────────────────────────────────

/// Slot-indexed node storage. A `slot` is a dense `u32` handle assigned in
/// insertion order; all hot-path state is `slot`-addressed so beam search
/// runs on contiguous arrays with no per-node locks or hashing.
struct Inner {
    dim: usize,
    metric: DistanceMetric,
    /// slot → external node id.
    ids: Vec<u64>,
    /// external node id → slot.
    slot_of: HashMap<u64, u32>,
    /// Contiguous vector data, `slot * dim ..= slot * dim + dim`.
    vectors: Vec<f32>,
    /// slot → 1/‖v‖ (cosine metric only; 1.0 placeholder otherwise,
    /// 0.0 for zero-norm vectors so their cosine similarity is 0 — the
    /// same value the previous `cosine()` helper produced).
    inv_norms: Vec<f32>,
    /// slot → layer → neighbor slots.
    neighbors: Vec<Vec<Vec<u32>>>,
    /// slot → tombstoned? (soft delete; see `HnswIndex::mark_deleted`).
    tomb: Vec<bool>,
    /// External-id mirror of the tombstone set (for counting and
    /// persistence; may contain ids the graph no longer knows).
    tomb_ids: HashSet<u64>,
    entry_slot: Option<u32>,
    entry_layer: usize,
}

impl Inner {
    fn new(dim: usize, metric: DistanceMetric) -> Self {
        Self {
            dim,
            metric,
            ids: Vec::new(),
            slot_of: HashMap::new(),
            vectors: Vec::new(),
            inv_norms: Vec::new(),
            neighbors: Vec::new(),
            tomb: Vec::new(),
            tomb_ids: HashSet::new(),
            entry_slot: None,
            entry_layer: 0,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.ids.len()
    }

    #[inline]
    fn vec_of(&self, slot: u32) -> &[f32] {
        let s = slot as usize * self.dim;
        &self.vectors[s..s + self.dim]
    }

    /// The `1/‖q‖` scale a query needs for the cosine fast path
    /// (1.0 for other metrics, 0.0 for a zero query).
    #[inline]
    fn query_inv(&self, q: &[f32]) -> f32 {
        if self.metric == DistanceMetric::Cosine {
            let n: f32 = q.iter().map(|x| x * x).sum::<f32>().sqrt();
            if n > 0.0 {
                1.0 / n
            } else {
                0.0
            }
        } else {
            1.0
        }
    }

    /// Distance from an (external) query vector to a slot.
    ///
    /// Cosine uses cached per-slot inverse norms so the per-pair work is a
    /// single dot product: `1 - dot(q,v)·(1/‖q‖)·(1/‖v‖)`. This matches
    /// `compute_distance`'s `1 - cos` up to fp rounding — acceptable
    /// because graph distances only decide beam ORDER; callers rescore
    /// exact similarities downstream.
    #[inline]
    fn slot_dist(&self, q: &[f32], q_inv: f32, slot: u32) -> f32 {
        let v = self.vec_of(slot);
        match self.metric {
            DistanceMetric::Cosine => {
                1.0 - dot_unrolled(q, v) * q_inv * self.inv_norms[slot as usize]
            }
            _ => compute_distance(self.metric, q, v),
        }
    }

    /// Hint the CPU to start pulling `slot`'s vector into cache. The
    /// vector slab for a 50k × 128-d graph is ~25 MB (larger than L3), so
    /// beam search reads are DRAM-latency-bound without this; prefetching
    /// the next neighbor's lines while the current distance computes hides
    /// most of that latency.
    ///
    /// Safety: `_mm_prefetch` is architecturally a hint — it cannot fault,
    /// and the pointer is formed from an in-bounds slot (neighbor lists
    /// only hold allocated slots). No-op on non-x86_64.
    #[inline(always)]
    fn prefetch_slot(&self, slot: u32) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use core::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
            let base = self.vectors.as_ptr().add(slot as usize * self.dim) as *const i8;
            // First two cache lines; the hardware streamer follows the
            // sequential reads for the rest of the vector.
            _mm_prefetch(base, _MM_HINT_T0);
            if self.dim > 16 {
                _mm_prefetch(base.add(64), _MM_HINT_T0);
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = slot;
        }
    }

    /// Append a node to the slab. Returns its slot.
    fn alloc_slot(&mut self, id: u64, vector: Vec<f32>, node_level: usize, m: usize) -> u32 {
        let slot = self.ids.len() as u32;
        let inv = if self.metric == DistanceMetric::Cosine {
            let n: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            if n > 0.0 {
                1.0 / n
            } else {
                0.0
            }
        } else {
            1.0
        };
        self.ids.push(id);
        self.slot_of.insert(id, slot);
        self.vectors.extend_from_slice(&vector);
        self.inv_norms.push(inv);
        self.neighbors.push(
            (0..=node_level)
                .map(|l| Vec::with_capacity(if l == 0 { 2 * m } else { m }))
                .collect(),
        );
        self.tomb.push(self.tomb_ids.contains(&id));
        slot
    }

    /// Greedy hill-climb to the locally-closest node at `layer`.
    fn greedy_layer(&self, q: &[f32], q_inv: f32, start: u32, layer: usize) -> (u32, f32) {
        let mut best = start;
        let mut best_dist = self.slot_dist(q, q_inv, start);
        loop {
            let mut improved = false;
            let nbrs = &self.neighbors[best as usize];
            if layer >= nbrs.len() {
                break;
            }
            for &nb in &nbrs[layer] {
                let d = self.slot_dist(q, q_inv, nb);
                if d < best_dist {
                    best_dist = d;
                    best = nb;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        (best, best_dist)
    }

    /// Beam search at `layer`, returning up to `ef` candidates sorted by
    /// distance ascending. `admit` gates entry into the RESULT set only;
    /// non-admitted nodes are still traversed (excluding them from
    /// traversal would disconnect their neighbours and collapse recall).
    fn beam(
        &self,
        q: &[f32],
        q_inv: f32,
        entry: u32,
        ef: usize,
        layer: usize,
        admit: &dyn Fn(u32) -> bool,
    ) -> Vec<(f32, u32)> {
        let n = self.len();
        if entry as usize >= n {
            return vec![];
        }
        // Bitmap visited set: ~6 KB for a 50k graph, allocation-cheap and
        // O(1) test-and-set (the old per-search HashSet<u64> hashed every
        // neighbor expansion).
        let mut visited = vec![0u64; n.div_ceil(64)];
        #[inline]
        fn test_and_set(bits: &mut [u64], i: u32) -> bool {
            let (w, b) = ((i / 64) as usize, i % 64);
            let seen = bits[w] & (1 << b) != 0;
            bits[w] |= 1 << b;
            seen
        }

        let entry_dist = self.slot_dist(q, q_inv, entry);
        // Min-heap of (dist, slot) candidates to explore.
        let mut candidates: BinaryHeap<Reverse<(ordered_float::OrderedFloat, u32)>> =
            BinaryHeap::with_capacity(ef * 4);
        // Max-heap of (dist, slot) for the result set W.
        let mut w: BinaryHeap<(ordered_float::OrderedFloat, u32)> =
            BinaryHeap::with_capacity(ef + 1);

        candidates.push(Reverse((ordered_float::OrderedFloat(entry_dist), entry)));
        if admit(entry) {
            w.push((ordered_float::OrderedFloat(entry_dist), entry));
        }
        test_and_set(&mut visited, entry);

        while let Some(Reverse((dist_c, c))) = candidates.pop() {
            // Pruning: if the closest unexplored candidate is farther than
            // the farthest kept result and W is full, stop.
            if let Some(&(farthest, _)) = w.peek() {
                if dist_c > farthest && w.len() >= ef {
                    break;
                }
            }
            let nbrs = &self.neighbors[c as usize];
            if layer >= nbrs.len() {
                continue;
            }
            let list = &nbrs[layer];
            // Start pulling the first vectors while the loop spins up.
            for &nb in list.iter().take(2) {
                self.prefetch_slot(nb);
            }
            for (i, &nb) in list.iter().enumerate() {
                if i + 2 < list.len() {
                    self.prefetch_slot(list[i + 2]);
                }
                if test_and_set(&mut visited, nb) {
                    continue;
                }
                let d = self.slot_dist(q, q_inv, nb);
                let farthest_w = w.peek().map(|&(f, _)| f.0).unwrap_or(f32::MAX);
                if d < farthest_w || w.len() < ef {
                    candidates.push(Reverse((ordered_float::OrderedFloat(d), nb)));
                    if admit(nb) {
                        w.push((ordered_float::OrderedFloat(d), nb));
                        if w.len() > ef {
                            w.pop();
                        }
                    }
                }
            }
        }

        let mut results: Vec<(f32, u32)> = w.into_iter().map(|(d, s)| (d.0, s)).collect();
        results.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        results
    }

    /// Neighbor selection — Malkov & Yashunin Algorithm 4 (the "heuristic"
    /// variant used by hnswlib and Lucene): walk candidates in ascending
    /// distance-to-base order and keep a candidate only if it is closer to
    /// the base than to every already-kept neighbor. Enforces direction
    /// diversity in each node's link list; plain closest-M packed all links
    /// into one tight cluster (recall@10 0.565 @ ef=100 on 50k random
    /// 128-d where the heuristic-built graph reaches ~0.95+).
    fn select_diverse(&self, cands: &[(f32, u32)], m: usize) -> Vec<(f32, u32)> {
        let mut sorted = cands.to_vec();
        sorted.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let mut kept: Vec<(f32, u32)> = Vec::with_capacity(m);
        for &(d_c, c) in &sorted {
            if kept.len() >= m {
                break;
            }
            let cv = self.vec_of(c);
            let ci = self.inv_norms[c as usize];
            // Diverse iff no already-kept neighbor is closer to the
            // candidate than the base is.
            let diverse = kept.iter().all(|&(_, r)| self.slot_dist(cv, ci, r) >= d_c);
            if diverse {
                kept.push((d_c, c));
            }
        }
        kept
    }

    /// Distances from `base` to each of its current neighbors at `layer`
    /// (used when a saturated node re-selects after gaining a back-link).
    fn backlink_dists(&self, base: u32, layer: usize) -> Vec<(f32, u32)> {
        let bv = self.vec_of(base);
        let bi = self.inv_norms[base as usize];
        self.neighbors[base as usize][layer]
            .iter()
            .map(|&nb| (self.slot_dist(bv, bi, nb), nb))
            .collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HNSW index
// ─────────────────────────────────────────────────────────────────────────────

/// A thread-safe HNSW approximate nearest-neighbor index.
///
/// Soft deletes: deleting a doc removes its entry from the caller's id
/// maps but the node and its neighbour edges stay in the graph until a
/// graph compaction (planned v0.7+). Tombstoned nodes are skipped as
/// results but still traversed. Tombstones persist with the graph
/// (format v2+).
pub struct HnswIndex {
    params: HnswParams,
    inner: RwLock<Inner>,
}

impl HnswIndex {
    /// Create a new empty index.
    pub fn new(params: HnswParams) -> Self {
        let inner = Inner::new(params.dim, params.metric);
        Self {
            params,
            inner: RwLock::new(inner),
        }
    }

    /// Mark `id` as deleted. Search will skip the node and never
    /// return it as a hit. The node and its neighbour edges remain
    /// until a graph compaction (planned v0.7+). Idempotent.
    pub fn mark_deleted(&self, id: u64) {
        let mut g = self.inner.write().unwrap();
        if let Some(&slot) = g.slot_of.get(&id) {
            g.tomb[slot as usize] = true;
        }
        g.tomb_ids.insert(id);
    }

    /// Reverse of `mark_deleted` — used in tests and on un-delete /
    /// reindex paths.
    pub fn unmark_deleted(&self, id: u64) {
        let mut g = self.inner.write().unwrap();
        if let Some(&slot) = g.slot_of.get(&id) {
            g.tomb[slot as usize] = false;
        }
        g.tomb_ids.remove(&id);
    }

    /// True if `id` has been tombstoned via `mark_deleted`.
    pub fn is_deleted(&self, id: u64) -> bool {
        self.inner.read().unwrap().tomb_ids.contains(&id)
    }

    /// Number of tombstones currently held. Operators can monitor
    /// this to decide when to schedule a compaction.
    pub fn tombstone_count(&self) -> usize {
        self.inner.read().unwrap().tomb_ids.len()
    }

    pub fn params(&self) -> &HnswParams {
        &self.params
    }

    /// Number of indexed vectors.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // ── Level selection ───────────────────────────────────────────────────

    fn random_level(&self) -> usize {
        let mut rng = rand::thread_rng();
        let ml = 1.0 / (self.params.m as f64).ln();
        let level = (-rng.gen::<f64>().ln() * ml) as usize;
        level.min(16) // cap at 16 layers
    }

    // ── Core HNSW algorithm ───────────────────────────────────────────────

    /// Insert a single vector with the given ID.
    ///
    /// # Panics
    /// Does not panic — returns `Err` if the vector dimension doesn't match.
    pub fn insert(&self, id: u64, vector: Vec<f32>) -> Result<()> {
        if vector.len() != self.params.dim {
            return Err(XerjError::invalid_mapping(format!(
                "HNSW insert: vector dim {} != index dim {}",
                vector.len(),
                self.params.dim
            )));
        }

        let node_level = self.random_level();
        let m = self.params.m;
        let ef = self.params.ef_construction;

        let mut g = self.inner.write().unwrap();

        // Query view of the new vector for the construction beams (the
        // slab owns the canonical copy after alloc).
        let q = vector.clone();
        let q_inv = g.query_inv(&q);

        // The new slot has no in-edges yet, so allocating it before the
        // beams run cannot perturb them — and it means the back-link
        // re-selection below can score the new node like any other
        // neighbor (the pre-slab code dropped the fresh back-link on the
        // floor here, disconnecting the graph: layer-0 BFS from the entry
        // reached 34 of 50k nodes and recall@10 was 0.000).
        let slot = g.alloc_slot(id, vector, node_level, m);

        // Handle first insertion.
        let (ep_slot, ep_layer) = match g.entry_slot {
            None => {
                g.entry_slot = Some(slot);
                g.entry_layer = node_level;
                debug!("HNSW: first node id={id} level={node_level}");
                return Ok(());
            }
            Some(ep) => (ep, g.entry_layer),
        };

        // Greedy descent from the top layer down to node_level+1.
        let mut curr_ep = ep_slot;
        for layer in (node_level + 1..=ep_layer).rev() {
            curr_ep = g.greedy_layer(&q, q_inv, curr_ep, layer).0;
        }

        // Search and connect from min(node_level, ep_layer) down to 0.
        for layer in (0..=node_level.min(ep_layer)).rev() {
            let m_layer = if layer == 0 { 2 * m } else { m };
            let candidates = g.beam(&q, q_inv, curr_ep, ef, layer, &|_| true);

            // Select up to M diverse nearest (Algorithm 4 heuristic).
            let selected = g.select_diverse(&candidates, m_layer);

            // Connect new node → selected.
            g.neighbors[slot as usize][layer] = selected.iter().map(|&(_, s)| s).collect();

            // Back-links: add new node to each selected neighbor, and
            // re-select when a neighbor exceeds its link budget.
            for &(_, sel) in &selected {
                g.neighbors[sel as usize][layer].push(slot);
                if g.neighbors[sel as usize][layer].len() > m_layer {
                    let dists = g.backlink_dists(sel, layer);
                    let pruned = g.select_diverse(&dists, m_layer);
                    g.neighbors[sel as usize][layer] = pruned.iter().map(|&(_, s)| s).collect();
                }
            }

            if !candidates.is_empty() {
                curr_ep = candidates[0].1;
            }
        }

        // Update entry point if the new node reached a higher layer.
        if node_level > g.entry_layer {
            g.entry_slot = Some(slot);
            g.entry_layer = node_level;
        }

        trace!("HNSW: inserted id={id} level={node_level}");
        Ok(())
    }

    /// K-nearest neighbor search.
    ///
    /// Returns up to `k` `(id, distance)` pairs sorted by distance ascending.
    pub fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<(u64, f32)>> {
        self.search_filtered(query, k, ef, &|_| true)
    }

    /// KNN search with a filter predicate applied during graph traversal.
    ///
    /// `filter(id)` returns `true` if the document with that ID should be
    /// included in results. The filter is pushed into the beam search to avoid
    /// retrieving far more candidates than necessary.
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: &dyn Fn(u64) -> bool,
    ) -> Result<Vec<(u64, f32)>> {
        if query.len() != self.params.dim {
            return Err(XerjError::invalid_query(format!(
                "HNSW search: query dim {} != index dim {}",
                query.len(),
                self.params.dim
            )));
        }

        let g = self.inner.read().unwrap();
        let ep_slot = match g.entry_slot {
            Some(s) => s,
            None => return Ok(vec![]),
        };
        let q_inv = g.query_inv(query);

        // Greedy descent from the top layer to layer 1.
        let mut curr_ep = ep_slot;
        for layer in (1..=g.entry_layer).rev() {
            curr_ep = g.greedy_layer(query, q_inv, curr_ep, layer).0;
        }

        // Beam search at layer 0. Tombstoned slots are kept as *traversal*
        // candidates (otherwise we lose access to their neighbours and
        // recall collapses) but excluded from the result set. The whole
        // search runs under one read guard, so it sees a consistent
        // tombstone set even if mark_deleted runs mid-query.
        let admit = |s: u32| !g.tomb[s as usize] && filter(g.ids[s as usize]);
        let candidates = g.beam(query, q_inv, curr_ep, ef.max(k), 0, &admit);

        Ok(candidates
            .into_iter()
            .filter(|&(_, s)| admit(s))
            .take(k)
            .map(|(dist, s)| (g.ids[s as usize], dist))
            .collect())
    }

    /// Insert a batch of `(id, vector)` items **serially**, one after another.
    ///
    /// This is a thin convenience wrapper over [`insert`](Self::insert): it
    /// forwards each item in order and returns on the first error. It offers
    /// no parallel speedup over calling `insert` in a loop yourself — despite
    /// this crate depending on `rayon`, no parallelism happens here.
    ///
    /// Insertion is kept serial on purpose: concurrent HNSW graph mutation
    /// races on neighbour-list updates and can corrupt connectivity/recall,
    /// so the batch API trades throughput for a provably consistent graph.
    ///
    /// Each item is validated by `insert` (dimension check); a mismatch
    /// returns `Err` and leaves already-inserted items in the graph (this
    /// API is not transactional).
    pub fn insert_batch(&self, items: Vec<(u64, Vec<f32>)>) -> Result<()> {
        for (id, vector) in items {
            self.insert(id, vector)?;
        }
        Ok(())
    }

    // ── Persistence ──────────────────────────────────────────────────────
    //
    // Wire format (little-endian, single file, written atomically via
    // tmp + rename):
    //
    //   magic:        8 bytes  = "XHNS0001"
    //   format_ver:   u32      = HNSW_FORMAT_VERSION
    //   m:            u32
    //   ef_construction:u32
    //   metric:       u8       = DistanceMetric repr (0=cosine,1=l2,2=dot)
    //   dim:          u32
    //   entry_point:  u64      = entry node id (u64::MAX = no entry)
    //   entry_layer:  u32
    //   num_nodes:    u32
    //   for each node:
    //     id:          u64
    //     max_layer:   u32   (so neighbours.len() = max_layer + 1)
    //     vector:      f32 × dim
    //     for layer 0..=max_layer:
    //       num_neigh: u32
    //       neigh_ids: u64 × num_neigh
    //   [v2] tombstones: u32 num + u64 × num
    //   crc32c:       u32   (over all preceding bytes)
    //
    // Pre-v0.6.2 the graph was rebuilt on every restart by replaying
    // every vector from the WAL — O(N log N) startup cost on a 10 M-
    // vector index. With save_to / load_from that startup cost
    // becomes O(file size / disk bw) and the graph is byte-identical
    // to what was running pre-restart.

    /// Atomically write the graph to `path`. Writes to `<path>.tmp`
    /// then `rename`s, so a crash during save leaves either the old
    /// file or no file (caller falls back to WAL replay).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_to_xerj)?;
        }
        let tmp_path = path.with_extension("tmp");
        let mut buf: Vec<u8> = Vec::with_capacity(1024 * 1024);

        buf.write_all(HNSW_MAGIC).map_err(io_to_xerj)?;
        buf.write_u32::<LittleEndian>(HNSW_FORMAT_VERSION)
            .map_err(io_to_xerj)?;
        buf.write_u32::<LittleEndian>(self.params.m as u32)
            .map_err(io_to_xerj)?;
        buf.write_u32::<LittleEndian>(self.params.ef_construction as u32)
            .map_err(io_to_xerj)?;
        buf.write_u8(metric_to_u8(self.params.metric))
            .map_err(io_to_xerj)?;
        buf.write_u32::<LittleEndian>(self.params.dim as u32)
            .map_err(io_to_xerj)?;

        let g = self.inner.read().unwrap();
        let ep_id = g.entry_slot.map(|s| g.ids[s as usize]);
        buf.write_u64::<LittleEndian>(ep_id.unwrap_or(u64::MAX))
            .map_err(io_to_xerj)?;
        buf.write_u32::<LittleEndian>(g.entry_layer as u32)
            .map_err(io_to_xerj)?;

        // Live slots only (a re-used external id orphans its old slot;
        // the map wrote each external id once and so do we).
        let live: Vec<u32> = (0..g.len() as u32)
            .filter(|&s| g.slot_of.get(&g.ids[s as usize]) == Some(&s))
            .collect();
        buf.write_u32::<LittleEndian>(live.len() as u32)
            .map_err(io_to_xerj)?;
        for &s in &live {
            buf.write_u64::<LittleEndian>(g.ids[s as usize])
                .map_err(io_to_xerj)?;
            let nbrs = &g.neighbors[s as usize];
            let max_layer = nbrs.len().saturating_sub(1) as u32;
            buf.write_u32::<LittleEndian>(max_layer)
                .map_err(io_to_xerj)?;
            for &v in g.vec_of(s) {
                buf.write_f32::<LittleEndian>(v).map_err(io_to_xerj)?;
            }
            for layer_neighbors in nbrs {
                buf.write_u32::<LittleEndian>(layer_neighbors.len() as u32)
                    .map_err(io_to_xerj)?;
                for &nb in layer_neighbors {
                    buf.write_u64::<LittleEndian>(g.ids[nb as usize])
                        .map_err(io_to_xerj)?;
                }
            }
        }

        // v2 — tombstone block. Empty for indices that have never had
        // a delete; v1 readers don't know to skip past this, so the
        // format version was bumped (loader checks).
        buf.write_u32::<LittleEndian>(g.tomb_ids.len() as u32)
            .map_err(io_to_xerj)?;
        for &t in g.tomb_ids.iter() {
            buf.write_u64::<LittleEndian>(t).map_err(io_to_xerj)?;
        }
        let node_count = live.len();
        drop(g);

        let crc = crc32fast::hash(&buf);
        buf.write_u32::<LittleEndian>(crc).map_err(io_to_xerj)?;

        // Atomic publish.
        std::fs::write(&tmp_path, &buf).map_err(io_to_xerj)?;
        std::fs::rename(&tmp_path, path).map_err(io_to_xerj)?;
        debug!(
            path = %path.display(),
            bytes = buf.len(),
            nodes = node_count,
            "HNSW graph persisted"
        );
        Ok(())
    }

    /// Read a graph back from disk. Validates magic + version + CRC.
    /// Returns `Err(...)` on any inconsistency so the caller can fall
    /// back to WAL replay rather than silently load a corrupt graph.
    pub fn load_from(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(io_to_xerj)?;
        if bytes.len() < HNSW_MAGIC.len() + 4 + 4 {
            return Err(corrupt("file too small"));
        }
        // Verify CRC over everything before the trailing u32.
        let payload_len = bytes.len() - 4;
        let stored_crc = (&bytes[payload_len..])
            .read_u32::<LittleEndian>()
            .map_err(io_to_xerj)?;
        let computed_crc = crc32fast::hash(&bytes[..payload_len]);
        if stored_crc != computed_crc {
            return Err(corrupt(&format!(
                "CRC mismatch: stored={stored_crc:#x} computed={computed_crc:#x}"
            )));
        }

        let mut cur = std::io::Cursor::new(&bytes[..payload_len]);
        let mut magic = [0u8; 8];
        cur.read_exact(&mut magic).map_err(io_to_xerj)?;
        if &magic != HNSW_MAGIC {
            return Err(corrupt(&format!(
                "bad magic: expected {HNSW_MAGIC:?}, got {magic:?}"
            )));
        }
        let format_ver = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)?;
        if !(1..=HNSW_FORMAT_VERSION).contains(&format_ver) {
            return Err(corrupt(&format!(
                "unsupported format version {format_ver}, this build supports 1..={HNSW_FORMAT_VERSION}"
            )));
        }

        let m = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
        let ef_construction = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
        let metric = u8_to_metric(cur.read_u8().map_err(io_to_xerj)?)?;
        let dim = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
        let ep_raw = cur.read_u64::<LittleEndian>().map_err(io_to_xerj)?;
        let entry_layer = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
        let num_nodes = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;

        let params = HnswParams {
            m,
            ef_construction,
            metric,
            dim,
        };

        // Pass 1: read raw nodes (neighbors keyed by external id).
        let mut g = Inner::new(dim, metric);
        let mut raw_neighbors: Vec<Vec<Vec<u64>>> = Vec::with_capacity(num_nodes);
        for _ in 0..num_nodes {
            let id = cur.read_u64::<LittleEndian>().map_err(io_to_xerj)?;
            let max_layer = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
            let mut vector = vec![0f32; dim];
            for v in vector.iter_mut() {
                *v = cur.read_f32::<LittleEndian>().map_err(io_to_xerj)?;
            }
            let mut neighbors: Vec<Vec<u64>> = Vec::with_capacity(max_layer + 1);
            for _ in 0..=max_layer {
                let nn = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
                let mut layer = Vec::with_capacity(nn);
                for _ in 0..nn {
                    layer.push(cur.read_u64::<LittleEndian>().map_err(io_to_xerj)?);
                }
                neighbors.push(layer);
            }
            g.alloc_slot(id, vector, max_layer, m);
            raw_neighbors.push(neighbors);
        }

        // Pass 2: map neighbor external ids → slots (ids referencing
        // unknown nodes are dropped, mirroring the old map's `get` skips).
        for (slot, layers) in raw_neighbors.into_iter().enumerate() {
            let converted: Vec<Vec<u32>> = layers
                .into_iter()
                .map(|layer| {
                    layer
                        .into_iter()
                        .filter_map(|nid| g.slot_of.get(&nid).copied())
                        .collect()
                })
                .collect();
            g.neighbors[slot] = converted;
        }

        g.entry_slot = if ep_raw == u64::MAX {
            None
        } else {
            match g.slot_of.get(&ep_raw).copied() {
                Some(s) => Some(s),
                None => return Err(corrupt("entry point references unknown node id")),
            }
        };
        g.entry_layer = entry_layer;

        // v2+: read trailing tombstone block. v1 files have no
        // tombstones and we treat the set as empty.
        if format_ver >= 2 {
            let num_t = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
            for _ in 0..num_t {
                let t = cur.read_u64::<LittleEndian>().map_err(io_to_xerj)?;
                if let Some(&slot) = g.slot_of.get(&t) {
                    g.tomb[slot as usize] = true;
                }
                g.tomb_ids.insert(t);
            }
        }

        Ok(Self {
            params,
            inner: RwLock::new(g),
        })
    }

    // ── Debug / diagnostic hooks (dev tooling only) ──────────────────────

    /// Run the layer-0 beam search from an explicit entry node (bypassing
    /// the hierarchy descent). Lets a probe distinguish "descent lands in
    /// the wrong region" from "layer-0 beam cannot navigate".
    pub fn debug_search_from(
        &self,
        entry: u64,
        query: &[f32],
        k: usize,
        ef: usize,
    ) -> Vec<(u64, f32)> {
        let g = self.inner.read().unwrap();
        let entry_slot = match g.slot_of.get(&entry) {
            Some(&s) => s,
            None => return vec![],
        };
        let q_inv = g.query_inv(query);
        let admit = |s: u32| !g.tomb[s as usize];
        let candidates = g.beam(query, q_inv, entry_slot, ef.max(k), 0, &admit);
        candidates
            .into_iter()
            .take(k)
            .map(|(dist, s)| (g.ids[s as usize], dist))
            .collect()
    }

    /// The layer-0 start node the hierarchy descent chooses for `query`,
    /// plus its distance.
    pub fn debug_descent(&self, query: &[f32]) -> Option<(u64, f32)> {
        let g = self.inner.read().unwrap();
        let ep = g.entry_slot?;
        let q_inv = g.query_inv(query);
        let mut curr = ep;
        let mut curr_dist = g.slot_dist(query, q_inv, curr);
        for layer in (1..=g.entry_layer).rev() {
            let (s, d) = g.greedy_layer(query, q_inv, curr, layer);
            curr = s;
            curr_dist = d;
        }
        Some((g.ids[curr as usize], curr_dist))
    }

    /// Graph structure summary: (entry_point, entry_layer, level histogram,
    /// layer-0 out-degree histogram, layer-0 BFS reachable-node count from
    /// the entry point).
    pub fn debug_structure(
        &self,
    ) -> (
        Option<u64>,
        usize,
        Vec<(usize, usize)>,
        Vec<(usize, usize)>,
        usize,
    ) {
        let g = self.inner.read().unwrap();
        let ep = g.entry_slot.map(|s| g.ids[s as usize]);
        let mut level_hist: std::collections::BTreeMap<usize, usize> = Default::default();
        let mut deg0_hist: std::collections::BTreeMap<usize, usize> = Default::default();
        for nbrs in &g.neighbors {
            *level_hist.entry(nbrs.len() - 1).or_default() += 1;
            *deg0_hist.entry(nbrs[0].len()).or_default() += 1;
        }
        let mut reach = 0usize;
        if let Some(start) = g.entry_slot {
            let mut seen = vec![false; g.len()];
            let mut stack = vec![start];
            seen[start as usize] = true;
            while let Some(c) = stack.pop() {
                reach += 1;
                for &nb in &g.neighbors[c as usize][0] {
                    if !seen[nb as usize] {
                        seen[nb as usize] = true;
                        stack.push(nb);
                    }
                }
            }
        }
        (
            ep,
            g.entry_layer,
            level_hist.into_iter().collect(),
            deg0_hist.into_iter().collect(),
            reach,
        )
    }
}

// ordered_float for BinaryHeap ordering
mod ordered_float {
    #[derive(Clone, Copy, PartialEq)]
    pub struct OrderedFloat(pub f32);

    impl Eq for OrderedFloat {}

    impl PartialOrd for OrderedFloat {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for OrderedFloat {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            self.0
                .partial_cmp(&other.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_vec(dim: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        v[hot] = 1.0;
        v
    }

    fn make_index(dim: usize) -> HnswIndex {
        HnswIndex::new(HnswParams::new(dim, DistanceMetric::Cosine))
    }

    #[test]
    fn insert_and_search_basic() {
        let idx = make_index(4);
        idx.insert(0, vec![1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.insert(1, vec![0.0, 1.0, 0.0, 0.0]).unwrap();
        idx.insert(2, vec![0.0, 0.0, 1.0, 0.0]).unwrap();
        idx.insert(3, vec![0.0, 0.0, 0.0, 1.0]).unwrap();

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = idx.search(&query, 1, 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 0, "nearest should be id=0");
    }

    #[test]
    fn insert_batch_serial_is_correct() {
        // insert_batch is a serial wrapper over insert; a batch build must
        // produce a queryable graph whose nearest neighbour is exact.
        let idx = make_index(4);
        let items: Vec<(u64, Vec<f32>)> = vec![
            (0, vec![1.0, 0.0, 0.0, 0.0]),
            (1, vec![0.0, 1.0, 0.0, 0.0]),
            (2, vec![0.0, 0.0, 1.0, 0.0]),
            (3, vec![0.0, 0.0, 0.0, 1.0]),
        ];
        idx.insert_batch(items).unwrap();
        assert_eq!(idx.len(), 4, "all batched items must be inserted");

        let results = idx.search(&[0.0, 0.0, 1.0, 0.0], 1, 10).unwrap();
        assert_eq!(results[0].0, 2, "nearest to the id=2 unit vector is id=2");
    }

    #[test]
    fn insert_batch_dim_mismatch_errors_and_is_not_atomic() {
        // A bad item mid-batch returns Err; items before it are already in
        // the graph (documented non-transactional behaviour).
        let idx = make_index(4);
        let err = idx
            .insert_batch(vec![
                (0, vec![1.0, 0.0, 0.0, 0.0]),
                (1, vec![0.0, 1.0]), // wrong dim
                (2, vec![0.0, 0.0, 1.0, 0.0]),
            ])
            .unwrap_err();
        assert!(matches!(err, XerjError::InvalidMapping { .. }));
        assert_eq!(idx.len(), 1, "only the pre-error item was inserted");
    }

    #[test]
    fn empty_index_returns_empty() {
        let idx = make_index(4);
        let results = idx.search(&[1.0, 0.0, 0.0, 0.0], 5, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn filtered_search_excludes_nodes() {
        let idx = make_index(4);
        for i in 0..10u64 {
            idx.insert(i, unit_vec(4, (i % 4) as usize)).unwrap();
        }

        // Only allow even IDs
        let results = idx
            .search_filtered(&[1.0, 0.0, 0.0, 0.0], 5, 20, &|id| id % 2 == 0)
            .unwrap();

        for (id, _) in &results {
            assert_eq!(id % 2, 0, "only even ids expected, got {id}");
        }
    }

    #[test]
    fn dimension_mismatch_errors() {
        let idx = make_index(4);
        let err = idx.insert(0, vec![1.0, 0.0]).unwrap_err();
        assert!(matches!(err, XerjError::InvalidMapping { .. }));
    }

    #[test]
    fn search_returns_at_most_k() {
        let idx = make_index(4);
        for i in 0..20u64 {
            idx.insert(i, vec![i as f32, 0.0, 0.0, 0.0]).unwrap();
        }
        let results = idx.search(&[5.0, 0.0, 0.0, 0.0], 3, 10).unwrap();
        assert!(results.len() <= 3);
    }

    #[test]
    fn save_load_roundtrip() {
        // Build a small index, save to a tempfile, reload, verify the
        // loaded graph returns the same neighbours for the same query.
        let idx = make_index(4);
        for i in 0..50u64 {
            let v = vec![(i as f32).sin(), (i as f32).cos(), 0.5, -0.5];
            idx.insert(i, v).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.bin");
        idx.save_to(&path).expect("save");

        let reloaded = HnswIndex::load_from(&path).expect("load");
        assert_eq!(idx.len(), reloaded.len());

        let q = vec![0.3_f32, 0.7, 0.5, -0.5];
        let a = idx.search(&q, 5, 32).unwrap();
        let b = reloaded.search(&q, 5, 32).unwrap();
        assert_eq!(a.len(), b.len());
        // The graph reload must reproduce the same neighbour set; allow
        // permutations only when distances tie exactly.
        let aset: std::collections::HashSet<u64> = a.iter().map(|(id, _)| *id).collect();
        let bset: std::collections::HashSet<u64> = b.iter().map(|(id, _)| *id).collect();
        assert_eq!(aset, bset, "neighbour set differs after save/load");
    }

    #[test]
    fn load_rejects_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.bin");
        std::fs::write(&path, b"garbage").unwrap();
        assert!(HnswIndex::load_from(&path).is_err());
    }

    #[test]
    fn tombstone_excludes_from_search_results() {
        let idx = make_index(4);
        for i in 0..20u64 {
            idx.insert(i, vec![i as f32, 0.0, 0.0, 0.0]).unwrap();
        }
        // Tombstone id=5; query for it.
        idx.mark_deleted(5);
        assert!(idx.is_deleted(5));
        assert_eq!(idx.tombstone_count(), 1);

        let results = idx.search(&[5.0, 0.0, 0.0, 0.0], 5, 32).unwrap();
        for (id, _) in &results {
            assert_ne!(*id, 5, "tombstoned id should never be returned");
        }
    }

    #[test]
    fn tombstones_persist_across_save_load() {
        let idx = make_index(4);
        for i in 0..30u64 {
            idx.insert(i, vec![i as f32, 0.0, 0.0, 0.0]).unwrap();
        }
        idx.mark_deleted(7);
        idx.mark_deleted(12);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hnsw.bin");
        idx.save_to(&path).unwrap();

        let reloaded = HnswIndex::load_from(&path).unwrap();
        assert!(reloaded.is_deleted(7));
        assert!(reloaded.is_deleted(12));
        assert!(!reloaded.is_deleted(8));
        assert_eq!(reloaded.tombstone_count(), 2);

        // And the search still respects them.
        let r = reloaded.search(&[7.0, 0.0, 0.0, 0.0], 10, 32).unwrap();
        for (id, _) in &r {
            assert_ne!(*id, 7);
            assert_ne!(*id, 12);
        }
    }

    #[test]
    fn large_random_build_recall_regression() {
        // Regression for two 2026-07-12 defects that only show at scale:
        //  1. back-link pruning dropped the new node (map-miss), leaving
        //     the graph disconnected — recall@10 was 0.000 at 50k;
        //  2. closest-M neighbor selection (no diversity heuristic) gave a
        //     much flatter recall/ef curve.
        // 5k × 32-d keeps the test fast while still exercising saturated
        // back-link pruning (>> M² nodes). At ef=400 over 5k random
        // vectors, a healthy graph is essentially exact.
        struct Rng(u64);
        impl Rng {
            fn next(&mut self) -> f32 {
                // xorshift64*
                self.0 ^= self.0 >> 12;
                self.0 ^= self.0 << 25;
                self.0 ^= self.0 >> 27;
                ((self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 33) as f64 / 2147483648.0) as f32 - 1.0
            }
        }
        let (n, dim, k) = (5000usize, 32usize, 10usize);
        let mut rng = Rng(0xC0FFEE);
        let vecs: Vec<Vec<f32>> = (0..n)
            .map(|_| (0..dim).map(|_| rng.next()).collect())
            .collect();
        let idx = make_index(dim);
        for (i, v) in vecs.iter().enumerate() {
            idx.insert(i as u64, v.clone()).unwrap();
        }
        // Layer-0 BFS from the entry must reach (almost) every node.
        let (_, _, _, _, reach) = idx.debug_structure();
        assert!(
            reach as f64 >= 0.99 * n as f64,
            "layer-0 graph disconnected: BFS reached {reach}/{n}"
        );

        let cos = |a: &[f32], b: &[f32]| -> f32 {
            let (mut d, mut na, mut nb) = (0f32, 0f32, 0f32);
            for i in 0..a.len() {
                d += a[i] * b[i];
                na += a[i] * a[i];
                nb += b[i] * b[i];
            }
            d / (na.sqrt() * nb.sqrt()).max(1e-12)
        };
        let mut total_hits = 0usize;
        let queries = 20usize;
        for qi in 0..queries {
            let q: Vec<f32> = (0..dim).map(|_| rng.next()).collect();
            let mut scored: Vec<(u64, f32)> = vecs
                .iter()
                .enumerate()
                .map(|(i, v)| (i as u64, cos(&q, v)))
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let exact: std::collections::HashSet<u64> =
                scored[..k].iter().map(|&(i, _)| i).collect();
            let got = idx.search(&q, k, 400).unwrap();
            total_hits += got.iter().filter(|(id, _)| exact.contains(id)).count();
            let _ = qi;
        }
        let recall = total_hits as f64 / (queries * k) as f64;
        assert!(
            recall >= 0.9,
            "recall@10 regression: got {recall:.3}, expected >= 0.9"
        );
    }
}
