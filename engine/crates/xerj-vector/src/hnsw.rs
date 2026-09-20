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
//! slot-indexed slab — one contiguous `Vec<f32>` for vectors and
//! `Vec<Vec<u32>>` neighbor lists per layer; the search-time visited set is
//! a pooled epoch-tagged bitmap per [`VisitedList`] (no per-query zeroing).
//! The previous `HashMap<u64, Arc<RwLock<Node>>>` paid a hash
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
    // `as_chunks::<8>()` yields the same 8-wide windows + remainder as
    // `chunks_exact(8)` (the `x`/`y` are now `&[f32; 8]`, so the fixed
    // indexing below is bounds-check-free); required by clippy 1.98's
    // `chunks_exact_to_as_chunks`.
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    for (x, y) in ca.iter().zip(cb.iter()) {
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
struct SlabGraphStorage {
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

/// Read-only, slot-stable graph storage consumed by HNSW traversal and
/// persistence.
///
/// Algorithms are generic over this private trait, so the current contiguous
/// slab remains statically dispatched and fully inlineable. This is
/// deliberately not a `dyn` facade: a future immutable backend can implement
/// the same reads without adding a virtual call to every neighbor expansion.
trait GraphStorageRead {
    type TombstoneIds<'a>: Iterator<Item = u64>
    where
        Self: 'a;

    fn dimension(&self) -> usize;
    fn metric(&self) -> DistanceMetric;
    fn node_count(&self) -> usize;
    fn external_id(&self, slot: u32) -> u64;
    fn current_slot(&self, id: u64) -> Option<u32>;
    fn vector(&self, slot: u32) -> &[f32];
    fn inverse_norm(&self, slot: u32) -> f32;
    fn layer_count(&self, slot: u32) -> usize;
    fn neighbors(&self, slot: u32, layer: usize) -> &[u32];
    fn prefetch_vector(&self, slot: u32);
    fn tombstoned(&self, slot: u32) -> bool;
    fn contains_tombstone_id(&self, id: u64) -> bool;
    fn tombstone_count(&self) -> usize;
    fn tombstone_ids(&self) -> Self::TombstoneIds<'_>;
    fn entry_slot(&self) -> Option<u32>;
    fn entry_layer(&self) -> usize;
}

/// Borrowed view over the unchanged flat-slab representation.
#[derive(Clone, Copy)]
struct SlabGraphRead<'a> {
    slab: &'a SlabGraphStorage,
}

impl GraphStorageRead for SlabGraphRead<'_> {
    type TombstoneIds<'a>
        = std::iter::Copied<std::collections::hash_set::Iter<'a, u64>>
    where
        Self: 'a;

    #[inline(always)]
    fn dimension(&self) -> usize {
        self.slab.dim
    }

    #[inline(always)]
    fn metric(&self) -> DistanceMetric {
        self.slab.metric
    }

    #[inline(always)]
    fn node_count(&self) -> usize {
        self.slab.ids.len()
    }

    #[inline(always)]
    fn external_id(&self, slot: u32) -> u64 {
        self.slab.ids[slot as usize]
    }

    #[inline(always)]
    fn current_slot(&self, id: u64) -> Option<u32> {
        self.slab.slot_of.get(&id).copied()
    }

    #[inline(always)]
    fn vector(&self, slot: u32) -> &[f32] {
        let start = slot as usize * self.slab.dim;
        &self.slab.vectors[start..start + self.slab.dim]
    }

    #[inline(always)]
    fn inverse_norm(&self, slot: u32) -> f32 {
        self.slab.inv_norms[slot as usize]
    }

    #[inline(always)]
    fn layer_count(&self, slot: u32) -> usize {
        self.slab.neighbors[slot as usize].len()
    }

    #[inline(always)]
    fn neighbors(&self, slot: u32, layer: usize) -> &[u32] {
        &self.slab.neighbors[slot as usize][layer]
    }

    #[inline(always)]
    fn prefetch_vector(&self, slot: u32) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            use core::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
            // Preserve the slab hot path exactly: neighbor lists contain
            // allocated slots, so this direct address calculation is valid.
            let base = self
                .slab
                .vectors
                .as_ptr()
                .add(slot as usize * self.slab.dim) as *const i8;
            _mm_prefetch(base, _MM_HINT_T0);
            if self.slab.dim > 16 {
                _mm_prefetch(base.add(64), _MM_HINT_T0);
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = slot;
        }
    }

    #[inline(always)]
    fn tombstoned(&self, slot: u32) -> bool {
        self.slab.tomb[slot as usize]
    }

    #[inline(always)]
    fn contains_tombstone_id(&self, id: u64) -> bool {
        self.slab.tomb_ids.contains(&id)
    }

    #[inline(always)]
    fn tombstone_count(&self) -> usize {
        self.slab.tomb_ids.len()
    }

    #[inline]
    fn tombstone_ids(&self) -> Self::TombstoneIds<'_> {
        self.slab.tomb_ids.iter().copied()
    }

    #[inline(always)]
    fn entry_slot(&self) -> Option<u32> {
        self.slab.entry_slot
    }

    #[inline(always)]
    fn entry_layer(&self) -> usize {
        self.slab.entry_layer
    }
}

impl SlabGraphStorage {
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

    #[inline(always)]
    fn read_view(&self) -> SlabGraphRead<'_> {
        SlabGraphRead { slab: self }
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
}

#[inline(always)]
fn query_inverse(graph: &impl GraphStorageRead, query: &[f32]) -> f32 {
    if graph.metric() == DistanceMetric::Cosine {
        let norm = query.iter().map(|value| value * value).sum::<f32>().sqrt();
        if norm > 0.0 {
            1.0 / norm
        } else {
            0.0
        }
    } else {
        1.0
    }
}

#[inline(always)]
fn slot_distance(
    graph: &impl GraphStorageRead,
    query: &[f32],
    query_inverse_norm: f32,
    slot: u32,
) -> f32 {
    let vector = graph.vector(slot);
    match graph.metric() {
        DistanceMetric::Cosine => {
            1.0 - dot_unrolled(query, vector) * query_inverse_norm * graph.inverse_norm(slot)
        }
        metric => compute_distance(metric, query, vector),
    }
}

fn greedy_layer(
    graph: &impl GraphStorageRead,
    query: &[f32],
    query_inverse_norm: f32,
    start: u32,
    layer: usize,
) -> (u32, f32) {
    let mut best = start;
    let mut best_distance = slot_distance(graph, query, query_inverse_norm, start);
    loop {
        let mut improved = false;
        if layer >= graph.layer_count(best) {
            break;
        }
        for &neighbor in graph.neighbors(best, layer) {
            let distance = slot_distance(graph, query, query_inverse_norm, neighbor);
            if distance < best_distance {
                best_distance = distance;
                best = neighbor;
                improved = true;
            }
        }
        if !improved {
            break;
        }
    }
    (best, best_distance)
}

// ── Reusable visited set ─────────────────────────────────────────────────────

/// Epoch-counter visited set for beam searches, pooled per index.
///
/// `beam` needs a "already expanded this search" set over node slots.  It
/// used to allocate and zero a `node_count/64`-word bitmap on EVERY call —
/// for a million-node graph ~125 KB of fresh pages touched per query
/// before the first distance is computed, and per CONSTRUCTION beam too.
///
/// Scheme adapted from qdrant's `VisitedList`
/// (lib/segment/src/index/visited_pool.rs, Apache-2.0): keep one list
/// alive across searches and make it look zeroed by BUMPING A GENERATION
/// — `next_iteration` increments the epoch (on u32 wrap, refilling with 0
/// and restarting at 1, visited_pool.rs:86-92) and a slot counts as
/// visited iff its epoch matches (the check half of
/// check_and_update_visited, :67-76).  Where qdrant carries a u32 counter
/// per POINT, this version keeps the old bitmap's ~1-bit-per-node memory
/// by tagging each 64-slot WORD with its epoch: a stale word means every
/// bit in it is unvisited, so the refill-on-wrap only ever touches the
/// epoch vec.  Pooled lists (keep-limit :130) are handed out by
/// [`HnswIndex::with_visited_list`].
struct VisitedList {
    current_iter: u32,
    /// Epoch per 64-slot word; `0` can never match (generations start at 1).
    epochs: Vec<u32>,
    /// Visited bits per word — meaningful only while `epochs[w] ==
    /// current_iter`.
    bits: Vec<u64>,
}

impl VisitedList {
    fn with_capacity(nodes: usize) -> Self {
        let words = nodes.div_ceil(64);
        Self {
            current_iter: 1,
            epochs: vec![0; words],
            bits: vec![0; words],
        }
    }

    /// qdrant visited_pool.rs:86-92 — bump the generation; on u32 wrap,
    /// refill and restart at 1 (once every ~4B searches per pooled list).
    fn next_iteration(&mut self) {
        self.current_iter = self.current_iter.wrapping_add(1);
        if self.current_iter == 0 {
            self.current_iter = 1;
            self.epochs.fill(0);
        }
    }

    /// Mark `slot`; `true` when it was ALREADY marked this iteration —
    /// the same decisions a zeroed bitmap's test-and-set would make.
    #[inline]
    fn check_and_set(&mut self, slot: u32) -> bool {
        let (word, bit) = ((slot / 64) as usize, slot % 64);
        if self.epochs[word] != self.current_iter {
            // Stale word: claim it with only this slot's bit set.
            self.epochs[word] = self.current_iter;
            self.bits[word] = 1 << bit;
            return false;
        }
        let mask = 1u64 << bit;
        let seen = self.bits[word] & mask != 0;
        self.bits[word] |= mask;
        seen
    }
}

// The eight parameters are the algorithm's own state (Malkov & Yashunin
// Algorithm 2) plus the pooled visited set; bundling them would hide the
// mapping to the paper.
#[allow(clippy::too_many_arguments)]
fn beam<G, A>(
    graph: &G,
    query: &[f32],
    query_inverse_norm: f32,
    entry: u32,
    ef: usize,
    layer: usize,
    admit: &A,
    visited: &mut VisitedList,
) -> Vec<(f32, u32)>
where
    G: GraphStorageRead,
    A: Fn(u32) -> bool + ?Sized,
{
    let node_count = graph.node_count();
    if entry as usize >= node_count {
        return vec![];
    }

    let entry_distance = slot_distance(graph, query, query_inverse_norm, entry);
    let mut candidates: BinaryHeap<Reverse<(ordered_float::OrderedFloat, u32)>> =
        BinaryHeap::with_capacity(ef * 4);
    let mut results: BinaryHeap<(ordered_float::OrderedFloat, u32)> =
        BinaryHeap::with_capacity(ef + 1);

    candidates.push(Reverse((
        ordered_float::OrderedFloat(entry_distance),
        entry,
    )));
    if admit(entry) {
        results.push((ordered_float::OrderedFloat(entry_distance), entry));
    }
    visited.check_and_set(entry);

    while let Some(Reverse((candidate_distance, candidate))) = candidates.pop() {
        if let Some(&(farthest, _)) = results.peek() {
            if candidate_distance > farthest && results.len() >= ef {
                break;
            }
        }
        if layer >= graph.layer_count(candidate) {
            continue;
        }
        let neighbors = graph.neighbors(candidate, layer);
        for &neighbor in neighbors.iter().take(2) {
            graph.prefetch_vector(neighbor);
        }
        for (index, &neighbor) in neighbors.iter().enumerate() {
            if index + 2 < neighbors.len() {
                graph.prefetch_vector(neighbors[index + 2]);
            }
            if visited.check_and_set(neighbor) {
                continue;
            }
            let distance = slot_distance(graph, query, query_inverse_norm, neighbor);
            let farthest = results
                .peek()
                .map(|&(distance, _)| distance.0)
                .unwrap_or(f32::MAX);
            if distance < farthest || results.len() < ef {
                candidates.push(Reverse((ordered_float::OrderedFloat(distance), neighbor)));
                if admit(neighbor) {
                    results.push((ordered_float::OrderedFloat(distance), neighbor));
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
    }

    let mut ordered: Vec<(f32, u32)> = results
        .into_iter()
        .map(|(distance, slot)| (distance.0, slot))
        .collect();
    ordered.sort_unstable_by(|left, right| left.0.partial_cmp(&right.0).unwrap());
    ordered
}

fn select_diverse(
    graph: &impl GraphStorageRead,
    candidates: &[(f32, u32)],
    max_neighbors: usize,
) -> Vec<(f32, u32)> {
    let mut sorted = candidates.to_vec();
    sorted.sort_unstable_by(|left, right| left.0.partial_cmp(&right.0).unwrap());
    let mut kept = Vec::with_capacity(max_neighbors);
    for &(candidate_distance, candidate) in &sorted {
        if kept.len() >= max_neighbors {
            break;
        }
        let vector = graph.vector(candidate);
        let inverse_norm = graph.inverse_norm(candidate);
        let diverse = kept.iter().all(|&(_, retained)| {
            slot_distance(graph, vector, inverse_norm, retained) >= candidate_distance
        });
        if diverse {
            kept.push((candidate_distance, candidate));
        }
    }
    kept
}

fn backlink_distances(graph: &impl GraphStorageRead, base: u32, layer: usize) -> Vec<(f32, u32)> {
    let vector = graph.vector(base);
    let inverse_norm = graph.inverse_norm(base);
    graph
        .neighbors(base, layer)
        .iter()
        .map(|&neighbor| {
            (
                slot_distance(graph, vector, inverse_norm, neighbor),
                neighbor,
            )
        })
        .collect()
}

/// True when `slot` is the current slot for its external ID. A reused
/// external ID orphans its old slot: the orphan stays in the slab as
/// traversal-only topology and must never surface as a hit or be
/// persisted as graph metadata.
#[inline]
fn is_live_slot(graph: &impl GraphStorageRead, slot: u32) -> bool {
    graph.current_slot(graph.external_id(slot)) == Some(slot)
}

/// The live slot with the highest layer, preferring the stored entry
/// point when it is still live. This is the entry the graph should
/// advertise: after an external-ID reuse the stored `entry_slot` /
/// `entry_layer` pair can describe an orphan slot (or pair the current
/// identity with the orphan's layer), which persisted as a corrupt
/// header and resurfaced stale vectors.
fn highest_live_entry(graph: &impl GraphStorageRead) -> Option<(u32, usize)> {
    let slot_layer = |slot: u32| graph.layer_count(slot).saturating_sub(1);
    let mut best = graph
        .entry_slot()
        .filter(|&slot| is_live_slot(graph, slot))
        .map(|slot| (slot, slot_layer(slot)));
    for slot in 0..graph.node_count() as u32 {
        if !is_live_slot(graph, slot) {
            continue;
        }
        let layer = slot_layer(slot);
        if best.is_none_or(|(_, best_layer)| layer > best_layer) {
            best = Some((slot, layer));
        }
    }
    best
}

fn encode_graph_bytes(
    params: &HnswParams,
    graph: &impl GraphStorageRead,
) -> Result<(Vec<u8>, usize)> {
    let mut buffer = Vec::with_capacity(1024 * 1024);
    buffer.write_all(HNSW_MAGIC).map_err(io_to_xerj)?;
    buffer
        .write_u32::<LittleEndian>(HNSW_FORMAT_VERSION)
        .map_err(io_to_xerj)?;
    buffer
        .write_u32::<LittleEndian>(params.m as u32)
        .map_err(io_to_xerj)?;
    buffer
        .write_u32::<LittleEndian>(params.ef_construction as u32)
        .map_err(io_to_xerj)?;
    buffer
        .write_u8(metric_to_u8(params.metric))
        .map_err(io_to_xerj)?;
    buffer
        .write_u32::<LittleEndian>(graph.dimension() as u32)
        .map_err(io_to_xerj)?;

    // Derive the entry header from live topology rather than trusting the
    // stored entry metadata: after an external-ID reuse the stored pair can
    // name an orphan slot or an inflated layer, and a reload would reject
    // or mis-descend the graph.
    let live_entry = highest_live_entry(graph);
    let entry_id = live_entry.map(|(slot, _)| graph.external_id(slot));
    buffer
        .write_u64::<LittleEndian>(entry_id.unwrap_or(u64::MAX))
        .map_err(io_to_xerj)?;
    buffer
        .write_u32::<LittleEndian>(live_entry.map_or(0, |(_, layer)| layer) as u32)
        .map_err(io_to_xerj)?;

    // Live slots only (a reused external ID orphans its old slot).
    let live: Vec<u32> = (0..graph.node_count() as u32)
        .filter(|&slot| is_live_slot(graph, slot))
        .collect();
    buffer
        .write_u32::<LittleEndian>(live.len() as u32)
        .map_err(io_to_xerj)?;
    for &slot in &live {
        buffer
            .write_u64::<LittleEndian>(graph.external_id(slot))
            .map_err(io_to_xerj)?;
        buffer
            .write_u32::<LittleEndian>(graph.layer_count(slot).saturating_sub(1) as u32)
            .map_err(io_to_xerj)?;
        for &value in graph.vector(slot) {
            buffer
                .write_f32::<LittleEndian>(value)
                .map_err(io_to_xerj)?;
        }
        for layer in 0..graph.layer_count(slot) {
            let neighbors = graph.neighbors(slot, layer);
            buffer
                .write_u32::<LittleEndian>(neighbors.len() as u32)
                .map_err(io_to_xerj)?;
            for &neighbor in neighbors {
                buffer
                    .write_u64::<LittleEndian>(graph.external_id(neighbor))
                    .map_err(io_to_xerj)?;
            }
        }
    }

    buffer
        .write_u32::<LittleEndian>(graph.tombstone_count() as u32)
        .map_err(io_to_xerj)?;
    for tombstone in graph.tombstone_ids() {
        buffer
            .write_u64::<LittleEndian>(tombstone)
            .map_err(io_to_xerj)?;
    }
    let crc = crc32fast::hash(&buffer);
    buffer.write_u32::<LittleEndian>(crc).map_err(io_to_xerj)?;
    Ok((buffer, live.len()))
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
    inner: RwLock<SlabGraphStorage>,
    /// Reusable [`VisitedList`]s for beam searches — qdrant's pool with a
    /// keep-limit (visited_pool.rs:130): concurrent searches each check one
    /// out, and overflow concurrency just allocates and drops transiently
    /// (the old behaviour).  Memory is bounded at
    /// `VISITED_POOL_KEEP_LIMIT` × ~1 bit per node.
    visited_pool: std::sync::Mutex<Vec<VisitedList>>,
}

/// Pool keep-limit — room for the concurrent search workers; beyond it,
/// lists drop instead of hoarding memory (qdrant's POOL_KEEP_LIMIT idea).
const VISITED_POOL_KEEP_LIMIT: usize = 16;

impl HnswIndex {
    /// Create a new empty index.
    pub fn new(params: HnswParams) -> Self {
        let inner = SlabGraphStorage::new(params.dim, params.metric);
        Self {
            params,
            inner: RwLock::new(inner),
            visited_pool: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Run `f` with a pooled [`VisitedList`] sized for `nodes` slots, at a
    /// fresh generation.  Lock order is always inner → visited_pool (every
    /// caller holds an `inner` guard when it takes a list), so the pool
    /// mutex can never participate in a lock cycle.
    fn with_visited_list<T>(&self, nodes: usize, f: impl FnOnce(&mut VisitedList) -> T) -> T {
        let mut list = self
            .visited_pool
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| VisitedList::with_capacity(nodes));
        if list.epochs.len() < nodes.div_ceil(64) {
            let words = nodes.div_ceil(64);
            list.epochs.resize(words, 0);
            list.bits.resize(words, 0);
        }
        list.next_iteration();
        let out = f(&mut list);
        let mut pool = self.visited_pool.lock().unwrap();
        if pool.len() < VISITED_POOL_KEEP_LIMIT {
            pool.push(list);
        }
        out
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
        self.inner
            .read()
            .unwrap()
            .read_view()
            .contains_tombstone_id(id)
    }

    /// Number of tombstones currently held. Operators can monitor
    /// this to decide when to schedule a compaction.
    pub fn tombstone_count(&self) -> usize {
        self.inner.read().unwrap().read_view().tombstone_count()
    }

    /// True when node `id` exists and stores exactly `v` (bit-exact f32
    /// compare). Used by the stale-graph rebuild (RC4 W2 item 17) to skip
    /// re-inserting docs whose graph vector already matches the document
    /// source, making a rebuild pass O(N·dim) compares + O(changed·log N)
    /// inserts instead of a full O(N·log N) reconstruction.
    pub fn vector_matches(&self, id: u64, v: &[f32]) -> bool {
        let g = self.inner.read().unwrap();
        let graph = g.read_view();
        match graph.current_slot(id) {
            Some(slot) => graph.vector(slot) == v,
            None => false,
        }
    }

    pub fn params(&self) -> &HnswParams {
        &self.params
    }

    /// Number of indexed vectors.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().read_view().node_count()
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
        self.insert_at_level(id, vector, self.random_level())
    }

    fn insert_at_level(&self, id: u64, vector: Vec<f32>, node_level: usize) -> Result<()> {
        if vector.len() != self.params.dim {
            return Err(XerjError::invalid_mapping(format!(
                "HNSW insert: vector dim {} != index dim {}",
                vector.len(),
                self.params.dim
            )));
        }

        let m = self.params.m;
        let ef = self.params.ef_construction;

        let mut g = self.inner.write().unwrap();
        let reuses_entry_identity = g
            .entry_slot
            .is_some_and(|entry| g.ids[entry as usize] == id);

        // Query view of the new vector for the construction beams (the
        // slab owns the canonical copy after alloc).
        let q = vector.clone();
        let q_inv = query_inverse(&g.read_view(), &q);

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
            curr_ep = greedy_layer(&g.read_view(), &q, q_inv, curr_ep, layer).0;
        }

        // Search and connect from min(node_level, ep_layer) down to 0.
        for layer in (0..=node_level.min(ep_layer)).rev() {
            let m_layer = if layer == 0 { 2 * m } else { m };
            let candidates = self.with_visited_list(g.read_view().node_count(), |visited| {
                beam(
                    &g.read_view(),
                    &q,
                    q_inv,
                    curr_ep,
                    ef,
                    layer,
                    &|_| true,
                    visited,
                )
            });

            // Select up to M diverse nearest (Algorithm 4 heuristic).
            let selected = select_diverse(&g.read_view(), &candidates, m_layer);

            // Connect new node → selected.
            g.neighbors[slot as usize][layer] = selected.iter().map(|&(_, s)| s).collect();

            // Back-links: add new node to each selected neighbor, and
            // re-select when a neighbor exceeds its link budget.
            for &(_, sel) in &selected {
                g.neighbors[sel as usize][layer].push(slot);
                if g.neighbors[sel as usize][layer].len() > m_layer {
                    let dists = backlink_distances(&g.read_view(), sel, layer);
                    let pruned = select_diverse(&g.read_view(), &dists, m_layer);
                    g.neighbors[sel as usize][layer] = pruned.iter().map(|&(_, s)| s).collect();
                }
            }

            if !candidates.is_empty() {
                curr_ep = candidates[0].1;
            }
        }

        // Update entry point if the new node reached a higher layer.
        if reuses_entry_identity {
            let (entry, layer) = highest_live_entry(&g.read_view()).unwrap_or((slot, node_level));
            g.entry_slot = Some(entry);
            g.entry_layer = layer;
        } else if node_level > g.entry_layer {
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
        let graph = g.read_view();
        let ep_slot = match graph.entry_slot() {
            Some(s) => s,
            None => return Ok(vec![]),
        };
        let q_inv = query_inverse(&graph, query);

        // Greedy descent from the top layer to layer 1.
        let mut curr_ep = ep_slot;
        for layer in (1..=graph.entry_layer()).rev() {
            curr_ep = greedy_layer(&graph, query, q_inv, curr_ep, layer).0;
        }

        // Beam search at layer 0. Tombstoned slots are kept as *traversal*
        // candidates (otherwise we lose access to their neighbours and
        // recall collapses) but excluded from the result set. The whole
        // search runs under one read guard, so it sees a consistent
        // tombstone set even if mark_deleted runs mid-query.
        let admit = |slot: u32| {
            is_live_slot(&graph, slot) && !graph.tombstoned(slot) && filter(graph.external_id(slot))
        };
        let candidates = self.with_visited_list(graph.node_count(), |visited| {
            beam(&graph, query, q_inv, curr_ep, ef.max(k), 0, &admit, visited)
        });

        Ok(candidates
            .into_iter()
            .filter(|&(_, s)| admit(s))
            .take(k)
            .map(|(dist, slot)| (graph.external_id(slot), dist))
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

    /// Atomically **and durably** write the graph to `path` (temp file +
    /// fsync + rename + parent-dir fsync via `xerj_common::fsio`), so a
    /// crash or power loss during save leaves either the old file or the
    /// complete new file (caller falls back to WAL replay when absent).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_to_xerj)?;
        }
        let g = self.inner.read().unwrap();
        let (buf, node_count) = encode_graph_bytes(&self.params, &g.read_view())?;
        drop(g);

        // Atomic + durable publish (RC4 W2 item 19). The previous
        // fs::write + rename left both the file bytes and the directory
        // entry in the volatile page cache: a power loss shortly after a
        // flush could leave a truncated/empty graph.bin (CRC-rejected at
        // the next open) or no file at all — and because the id-map/graph
        // pair then fails to load or fails its freshness stamp, every kNN
        // after recovery silently degraded to permanent-stale/brute.
        // write_file_durable fsyncs the temp file, renames, then fsyncs
        // the parent directory so the rename itself survives power loss.
        xerj_common::fsio::write_file_durable(path, &buf).map_err(io_to_xerj)?;
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
        let mut g = SlabGraphStorage::new(dim, metric);
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
            visited_pool: std::sync::Mutex::new(Vec::new()),
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
        let graph = g.read_view();
        let entry_slot = match graph.current_slot(entry) {
            Some(slot) => slot,
            None => return vec![],
        };
        let q_inv = query_inverse(&graph, query);
        let admit = |slot: u32| !graph.tombstoned(slot);
        let candidates = self.with_visited_list(graph.node_count(), |visited| {
            beam(
                &graph,
                query,
                q_inv,
                entry_slot,
                ef.max(k),
                0,
                &admit,
                visited,
            )
        });
        candidates
            .into_iter()
            .take(k)
            .map(|(dist, slot)| (graph.external_id(slot), dist))
            .collect()
    }

    /// The layer-0 start node the hierarchy descent chooses for `query`,
    /// plus its distance.
    pub fn debug_descent(&self, query: &[f32]) -> Option<(u64, f32)> {
        let g = self.inner.read().unwrap();
        let graph = g.read_view();
        let ep = graph.entry_slot()?;
        let q_inv = query_inverse(&graph, query);
        let mut curr = ep;
        let mut curr_dist = slot_distance(&graph, query, q_inv, curr);
        for layer in (1..=graph.entry_layer()).rev() {
            let (s, d) = greedy_layer(&graph, query, q_inv, curr, layer);
            curr = s;
            curr_dist = d;
        }
        Some((graph.external_id(curr), curr_dist))
    }

    /// Graph structure summary: (entry_point, entry_layer, level histogram,
    /// layer-0 out-degree histogram, layer-0 BFS reachable-node count from
    /// the entry point).
    #[allow(clippy::type_complexity)]
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
        let graph = g.read_view();
        let ep = graph.entry_slot().map(|slot| graph.external_id(slot));
        let mut level_hist: std::collections::BTreeMap<usize, usize> = Default::default();
        let mut deg0_hist: std::collections::BTreeMap<usize, usize> = Default::default();
        for slot in 0..graph.node_count() as u32 {
            *level_hist.entry(graph.layer_count(slot) - 1).or_default() += 1;
            *deg0_hist.entry(graph.neighbors(slot, 0).len()).or_default() += 1;
        }
        let mut reach = 0usize;
        if let Some(start) = graph.entry_slot() {
            let mut seen = vec![false; graph.node_count()];
            let mut stack = vec![start];
            seen[start as usize] = true;
            while let Some(c) = stack.pop() {
                reach += 1;
                for &nb in graph.neighbors(c, 0) {
                    if !seen[nb as usize] {
                        seen[nb as usize] = true;
                        stack.push(nb);
                    }
                }
            }
        }
        (
            ep,
            graph.entry_layer(),
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

    struct FragmentedTestGraph {
        ids: Vec<u64>,
        vectors: Vec<Vec<f32>>,
        inverse_norms: Vec<f32>,
        neighbors: Vec<Vec<Vec<u32>>>,
        tombstones: Vec<u64>,
        entry: Option<u32>,
    }

    impl GraphStorageRead for FragmentedTestGraph {
        type TombstoneIds<'a>
            = std::iter::Copied<std::slice::Iter<'a, u64>>
        where
            Self: 'a;

        fn dimension(&self) -> usize {
            self.vectors.first().map_or(0, Vec::len)
        }

        fn metric(&self) -> DistanceMetric {
            DistanceMetric::Cosine
        }

        fn node_count(&self) -> usize {
            self.ids.len()
        }

        fn external_id(&self, slot: u32) -> u64 {
            self.ids[slot as usize]
        }

        fn current_slot(&self, id: u64) -> Option<u32> {
            self.ids
                .iter()
                .rposition(|candidate| *candidate == id)
                .map(|slot| slot as u32)
        }

        fn vector(&self, slot: u32) -> &[f32] {
            &self.vectors[slot as usize]
        }

        fn inverse_norm(&self, slot: u32) -> f32 {
            self.inverse_norms[slot as usize]
        }

        fn layer_count(&self, slot: u32) -> usize {
            self.neighbors[slot as usize].len()
        }

        fn neighbors(&self, slot: u32, layer: usize) -> &[u32] {
            &self.neighbors[slot as usize][layer]
        }

        fn prefetch_vector(&self, _slot: u32) {}

        fn tombstoned(&self, slot: u32) -> bool {
            self.tombstones.contains(&self.external_id(slot))
        }

        fn contains_tombstone_id(&self, id: u64) -> bool {
            self.tombstones.contains(&id)
        }

        fn tombstone_count(&self) -> usize {
            self.tombstones.len()
        }

        fn tombstone_ids(&self) -> Self::TombstoneIds<'_> {
            self.tombstones.iter().copied()
        }

        fn entry_slot(&self) -> Option<u32> {
            self.entry
        }

        fn entry_layer(&self) -> usize {
            0
        }
    }

    fn unit_vec(dim: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        v[hot] = 1.0;
        v
    }

    fn make_index(dim: usize) -> HnswIndex {
        HnswIndex::new(HnswParams::new(dim, DistanceMetric::Cosine))
    }

    fn checked_in_pre_view_v2_fixture() -> Vec<u8> {
        let hex = include_str!("../tests/fixtures/hnsw-v2-pre-storage-view.hex").trim();
        assert_eq!(hex.len() % 2, 0);
        // `as_chunks::<2>()` over `chunks_exact(2)` (clippy 1.98); the
        // `% 2 == 0` assert above guarantees an empty remainder.
        let (pairs, _rem) = hex.as_bytes().as_chunks::<2>();
        pairs
            .iter()
            .map(|pair| {
                let digit = |byte: u8| match byte {
                    b'0'..=b'9' => byte - b'0',
                    b'a'..=b'f' => byte - b'a' + 10,
                    b'A'..=b'F' => byte - b'A' + 10,
                    _ => panic!("checked-in fixture contains non-hex byte"),
                };
                digit(pair[0]) << 4 | digit(pair[1])
            })
            .collect()
    }

    fn legacy_encode_graph_bytes(params: &HnswParams, graph: &SlabGraphStorage) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(1024 * 1024);
        buffer.write_all(HNSW_MAGIC).unwrap();
        buffer
            .write_u32::<LittleEndian>(HNSW_FORMAT_VERSION)
            .unwrap();
        buffer.write_u32::<LittleEndian>(params.m as u32).unwrap();
        buffer
            .write_u32::<LittleEndian>(params.ef_construction as u32)
            .unwrap();
        buffer.write_u8(metric_to_u8(params.metric)).unwrap();
        buffer.write_u32::<LittleEndian>(params.dim as u32).unwrap();
        buffer
            .write_u64::<LittleEndian>(
                graph
                    .entry_slot
                    .map(|slot| graph.ids[slot as usize])
                    .unwrap_or(u64::MAX),
            )
            .unwrap();
        buffer
            .write_u32::<LittleEndian>(graph.entry_layer as u32)
            .unwrap();
        let live: Vec<u32> = (0..graph.ids.len() as u32)
            .filter(|&slot| graph.slot_of.get(&graph.ids[slot as usize]) == Some(&slot))
            .collect();
        buffer.write_u32::<LittleEndian>(live.len() as u32).unwrap();
        for &slot in &live {
            buffer
                .write_u64::<LittleEndian>(graph.ids[slot as usize])
                .unwrap();
            let layers = &graph.neighbors[slot as usize];
            buffer
                .write_u32::<LittleEndian>(layers.len().saturating_sub(1) as u32)
                .unwrap();
            let start = slot as usize * graph.dim;
            for &value in &graph.vectors[start..start + graph.dim] {
                buffer.write_f32::<LittleEndian>(value).unwrap();
            }
            for neighbors in layers {
                buffer
                    .write_u32::<LittleEndian>(neighbors.len() as u32)
                    .unwrap();
                for &neighbor in neighbors {
                    buffer
                        .write_u64::<LittleEndian>(graph.ids[neighbor as usize])
                        .unwrap();
                }
            }
        }
        buffer
            .write_u32::<LittleEndian>(graph.tomb_ids.len() as u32)
            .unwrap();
        for &id in &graph.tomb_ids {
            buffer.write_u64::<LittleEndian>(id).unwrap();
        }
        let crc = crc32fast::hash(&buffer);
        buffer.write_u32::<LittleEndian>(crc).unwrap();
        buffer
    }

    fn legacy_search(
        graph: &SlabGraphStorage,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: &dyn Fn(u64) -> bool,
    ) -> Vec<(u64, f32)> {
        let Some(mut entry) = graph.entry_slot else {
            return vec![];
        };
        let query_inverse_norm = if graph.metric == DistanceMetric::Cosine {
            let norm = query.iter().map(|value| value * value).sum::<f32>().sqrt();
            if norm > 0.0 {
                1.0 / norm
            } else {
                0.0
            }
        } else {
            1.0
        };
        let distance = |slot: u32| {
            let start = slot as usize * graph.dim;
            let vector = &graph.vectors[start..start + graph.dim];
            match graph.metric {
                DistanceMetric::Cosine => {
                    1.0 - dot_unrolled(query, vector)
                        * query_inverse_norm
                        * graph.inv_norms[slot as usize]
                }
                metric => compute_distance(metric, query, vector),
            }
        };
        for layer in (1..=graph.entry_layer).rev() {
            let mut best_distance = distance(entry);
            while let Some(neighbors) = graph.neighbors[entry as usize].get(layer) {
                let mut improved = false;
                for &neighbor in neighbors {
                    let candidate_distance = distance(neighbor);
                    if candidate_distance < best_distance {
                        entry = neighbor;
                        best_distance = candidate_distance;
                        improved = true;
                    }
                }
                if !improved {
                    break;
                }
            }
        }

        let mut visited = vec![0u64; graph.ids.len().div_ceil(64)];
        let test_and_set = |bits: &mut [u64], slot: u32| {
            let (word, bit) = ((slot / 64) as usize, slot % 64);
            let seen = bits[word] & (1 << bit) != 0;
            bits[word] |= 1 << bit;
            seen
        };
        let admit = |slot: u32| !graph.tomb[slot as usize] && filter(graph.ids[slot as usize]);
        let width = ef.max(k);
        let mut candidates: BinaryHeap<Reverse<(ordered_float::OrderedFloat, u32)>> =
            BinaryHeap::with_capacity(width * 4);
        let mut results: BinaryHeap<(ordered_float::OrderedFloat, u32)> =
            BinaryHeap::with_capacity(width + 1);
        candidates.push(Reverse((
            ordered_float::OrderedFloat(distance(entry)),
            entry,
        )));
        if admit(entry) {
            results.push((ordered_float::OrderedFloat(distance(entry)), entry));
        }
        test_and_set(&mut visited, entry);
        while let Some(Reverse((candidate_distance, candidate))) = candidates.pop() {
            if let Some(&(farthest, _)) = results.peek() {
                if candidate_distance > farthest && results.len() >= width {
                    break;
                }
            }
            let Some(neighbors) = graph.neighbors[candidate as usize].first() else {
                continue;
            };
            for &neighbor in neighbors {
                if test_and_set(&mut visited, neighbor) {
                    continue;
                }
                let neighbor_distance = distance(neighbor);
                let farthest = results
                    .peek()
                    .map(|&(value, _)| value.0)
                    .unwrap_or(f32::MAX);
                if neighbor_distance < farthest || results.len() < width {
                    candidates.push(Reverse((
                        ordered_float::OrderedFloat(neighbor_distance),
                        neighbor,
                    )));
                    if admit(neighbor) {
                        results.push((ordered_float::OrderedFloat(neighbor_distance), neighbor));
                        if results.len() > width {
                            results.pop();
                        }
                    }
                }
            }
        }
        let mut ordered: Vec<_> = results
            .into_iter()
            .map(|(value, slot)| (value.0, slot))
            .collect();
        ordered.sort_unstable_by(|left, right| left.0.partial_cmp(&right.0).unwrap());
        ordered
            .into_iter()
            .filter(|&(_, slot)| admit(slot))
            .take(k)
            .map(|(value, slot)| (graph.ids[slot as usize], value))
            .collect()
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

    /// Cluster-G pin, part 1: the pooled visited list must make exactly
    /// the same seen/unseen decisions as a zeroed bitmap — across the
    /// 64-slot word boundary (63/64/65), across `next_iteration` (an old
    /// generation must read as unvisited), and across the u32 wrap
    /// (refill + restart at 1, qdrant visited_pool.rs:86-92).
    #[test]
    fn visited_list_epoch_semantics_match_a_zeroed_bitmap() {
        let mut v = VisitedList::with_capacity(200);

        // Bitmap oracle over the same slots.
        let mut bitmap = vec![false; 200];
        let step = |v: &mut VisitedList, bitmap: &mut Vec<bool>, slot: u32| {
            let got = v.check_and_set(slot);
            let want = std::mem::replace(&mut bitmap[slot as usize], true);
            assert_eq!(got, want, "slot {slot} decision diverged from bitmap");
        };

        // First iteration: fresh word claims, including the 63/64/65
        // boundary, plus a duplicate inside an already-live word.
        v.next_iteration();
        for slot in [0u32, 63, 64, 65, 130, 63, 0] {
            step(&mut v, &mut bitmap, slot);
        }

        // New generation: everything unvisited again, same slots re-tested
        // (word 0 is stale-then-reclaimed; word 2 still live from slot 130
        // must NOT leak its old bit into slot 129's decision).
        v.next_iteration();
        for slot in bitmap.iter_mut() {
            *slot = false;
        }
        for slot in [129u32, 130, 0, 130, 65, 129] {
            step(&mut v, &mut bitmap, slot);
        }

        // u32 wrap: current_iter = u32::MAX, one more next_iteration hits
        // 0 → refill + restart at 1.  Everything must read unvisited even
        // though generation 1 ran before.
        v.current_iter = u32::MAX;
        v.next_iteration();
        assert_eq!(v.current_iter, 1, "wrap must restart at generation 1");
        for slot in bitmap.iter_mut() {
            *slot = false;
        }
        for slot in [0u32, 64, 130, 0, 130] {
            step(&mut v, &mut bitmap, slot);
        }
    }

    /// Cluster-G pin, part 2: reusing one pooled list across successive
    /// searches (the entire point of the pool) must not change any search
    /// result — a stale visited bit would silently narrow a later beam.
    /// The reuse here is REAL: `search` checks the same list out of the
    /// pool 5 times, so iterations 2..5 run on non-zeroed memory.
    #[test]
    fn pooled_visited_list_reuse_does_not_change_results() {
        let idx = make_index(8);
        for i in 0..600u64 {
            let v: Vec<f32> = (0..8)
                .map(|d| ((i * 31 + d * 7) % 97) as f32 / 97.0)
                .collect();
            idx.insert(i, v).unwrap();
        }
        let queries: Vec<Vec<f32>> = (0..10)
            .map(|q| {
                (0..8)
                    .map(|d| ((q * 13 + d * 3) % 89) as f32 / 89.0)
                    .collect()
            })
            .collect();
        for q in &queries {
            let first = idx.search(q, 10, 64).unwrap();
            assert_eq!(first.len(), 10, "fixture: every query must fill the page");
            for _ in 0..4 {
                assert_eq!(
                    idx.search(q, 10, 64).unwrap(),
                    first,
                    "repeat search must be identical — pooled-list reuse leaked state"
                );
            }
        }
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
    fn reused_entry_external_id_with_lower_layer_saves_a_reloadable_graph() {
        let idx = HnswIndex::new(HnswParams::new(2, DistanceMetric::Cosine));
        idx.insert_at_level(7, vec![1.0, 0.0], 3).unwrap();
        idx.insert_at_level(7, vec![0.0, 1.0], 0).unwrap();

        let structure = idx.debug_structure();
        assert_eq!(structure.0, Some(7));
        assert_eq!(structure.1, 0);
        let runtime_hit = idx.search(&[1.0, 0.0], 1, 8).unwrap()[0];
        assert_eq!(runtime_hit.0, 7);
        assert!(
            runtime_hit.1 > 0.9,
            "the superseded slot must be traversal-only, not a stale hit"
        );

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.bin");
        idx.save_to(&path).unwrap();
        let reopened = HnswIndex::load_from(&path).unwrap();

        assert_eq!(reopened.len(), 1);
        assert!(reopened.vector_matches(7, &[0.0, 1.0]));
        let structure = reopened.debug_structure();
        assert_eq!(structure.0, Some(7));
        assert_eq!(structure.1, 0);
    }

    #[test]
    fn reused_entry_id_falls_back_to_the_highest_remaining_live_node() {
        let idx = HnswIndex::new(HnswParams::new(2, DistanceMetric::L2));
        for (id, vector, level) in [
            (7, vec![1.0, 0.0], 3),
            (8, vec![0.5, 0.5], 2),
            (7, vec![0.0, 1.0], 0),
        ] {
            idx.insert_at_level(id, vector, level).unwrap();
        }

        let structure = idx.debug_structure();
        assert_eq!(structure.0, Some(8));
        assert_eq!(structure.1, 2);
        assert_eq!(idx.search(&[0.0, 1.0], 1, 16).unwrap()[0].0, 7);
        assert_eq!(idx.search(&[0.5, 0.5], 1, 16).unwrap()[0].0, 8);

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.bin");
        idx.save_to(&path).unwrap();
        let reopened = HnswIndex::load_from(&path).unwrap();
        assert_eq!(reopened.len(), 2);
        let structure = reopened.debug_structure();
        assert_eq!(structure.0, Some(8));
        assert_eq!(structure.1, 2);
        assert_eq!(reopened.search(&[0.0, 1.0], 1, 16).unwrap()[0].0, 7);
        assert_eq!(reopened.search(&[0.5, 0.5], 1, 16).unwrap()[0].0, 8);
    }

    #[test]
    fn writer_repairs_a_stale_reused_id_entry_header() {
        let idx = HnswIndex::new(HnswParams::new(2, DistanceMetric::Cosine));
        idx.insert_at_level(7, vec![1.0, 0.0], 3).unwrap();
        idx.insert_at_level(7, vec![0.0, 1.0], 0).unwrap();

        // Recreate the stale in-memory state produced before this fix.
        // Persistence must derive its header from live topology rather than
        // pairing the orphan slot's layer with the current slot's identity.
        {
            let mut graph = idx.inner.write().unwrap();
            graph.entry_slot = Some(0);
            graph.entry_layer = 3;
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("graph.bin");
        idx.save_to(&path).unwrap();
        let reopened = HnswIndex::load_from(&path).unwrap();
        assert_eq!(reopened.len(), 1);
        assert!(reopened.vector_matches(7, &[0.0, 1.0]));
        let structure = reopened.debug_structure();
        assert_eq!(structure.0, Some(7));
        assert_eq!(structure.1, 0);
    }

    #[test]
    fn slab_read_view_preserves_search_and_graph_bytes() {
        for metric in [
            DistanceMetric::Cosine,
            DistanceMetric::L2,
            DistanceMetric::DotProduct,
        ] {
            let index = HnswIndex::new(HnswParams::new(3, metric));
            let ids = [17, (1u64 << 48) + 9, 23, 41, 99];
            let vectors = [
                vec![1.0, 0.0, 0.0],
                vec![1.0, 0.0, 0.0],
                vec![0.5, 0.5, 0.0],
                vec![0.0, 1.0, 0.0],
                vec![0.0, 0.0, 1.0],
            ];
            for (&id, vector) in ids.iter().zip(vectors) {
                index.insert(id, vector).unwrap();
            }
            index.mark_deleted(ids[4]);
            index.mark_deleted(999_999_999_999);

            for query in [[1.0, 0.0, 0.0], [0.25, 0.75, 0.0]] {
                let expected = {
                    let slab = index.inner.read().unwrap();
                    legacy_search(&slab, &query, 4, 32, &|id| id != ids[3])
                };
                let actual = index
                    .search_filtered(&query, 4, 32, &|id| id != ids[3])
                    .unwrap();
                assert_eq!(
                    actual, expected,
                    "metric, tie, or filter behavior changed for {metric:?}"
                );
            }

            let slab = index.inner.read().unwrap();
            let view = slab.read_view();
            assert_eq!(view.node_count(), slab.ids.len());
            assert_eq!(view.entry_slot(), slab.entry_slot);
            assert_eq!(view.entry_layer(), slab.entry_layer);
            for slot in 0..view.node_count() as u32 {
                assert_eq!(view.external_id(slot), slab.ids[slot as usize]);
                assert_eq!(
                    view.current_slot(view.external_id(slot)),
                    slab.slot_of.get(&slab.ids[slot as usize]).copied()
                );
                let start = slot as usize * slab.dim;
                assert_eq!(view.vector(slot), &slab.vectors[start..start + slab.dim]);
                assert_eq!(view.inverse_norm(slot), slab.inv_norms[slot as usize]);
                assert_eq!(view.layer_count(slot), slab.neighbors[slot as usize].len());
                assert_eq!(view.tombstoned(slot), slab.tomb[slot as usize]);
                for layer in 0..view.layer_count(slot) {
                    assert_eq!(
                        view.neighbors(slot, layer),
                        slab.neighbors[slot as usize][layer]
                    );
                }
            }
            assert_eq!(
                encode_graph_bytes(&index.params, &view).unwrap().0,
                legacy_encode_graph_bytes(&index.params, &slab),
                "writer bytes changed for {metric:?}"
            );
        }
    }

    #[test]
    fn generic_read_boundary_accepts_fragmented_backend() {
        let graph = FragmentedTestGraph {
            ids: vec![17, (1u64 << 48) + 5, 91],
            vectors: vec![vec![1.0, 0.0], vec![0.8, 0.2], vec![0.0, 1.0]],
            inverse_norms: vec![1.0, 1.0 / 0.68f32.sqrt(), 1.0],
            neighbors: vec![vec![vec![1]], vec![vec![0, 2]], vec![vec![1]]],
            tombstones: vec![91],
            entry: Some(0),
        };
        let query = [1.0, 0.0];
        let inverse = query_inverse(&graph, &query);
        let mut visited = VisitedList::with_capacity(graph.node_count());
        visited.next_iteration();
        let candidates = beam(
            &graph,
            &query,
            inverse,
            graph.entry_slot().unwrap(),
            8,
            0,
            &|slot| !graph.tombstoned(slot),
            &mut visited,
        );
        let hits: Vec<_> = candidates
            .into_iter()
            .map(|(distance, slot)| (graph.external_id(slot), distance))
            .collect();
        assert_eq!(hits[0], (17, 0.0));
        assert_eq!(hits.len(), 2);

        let params = HnswParams::new(2, DistanceMetric::Cosine);
        let (bytes, nodes) = encode_graph_bytes(&params, &graph).unwrap();
        assert_eq!(nodes, 3);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fragmented.bin");
        std::fs::write(&path, bytes).unwrap();
        let loaded = HnswIndex::load_from(&path).unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(loaded.is_deleted(91));
        assert_eq!(loaded.search(&query, 3, 8).unwrap()[0], (17, 0.0));
    }

    #[test]
    fn checked_in_pre_view_v2_fixture_reads_and_current_writer_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let fixture_path = directory.path().join("pre-view.bin");
        std::fs::write(&fixture_path, checked_in_pre_view_v2_fixture()).unwrap();
        let old = HnswIndex::load_from(&fixture_path).unwrap();
        let large_id = (1u64 << 48) + 3;
        assert_eq!(old.len(), 2);
        assert!(old.vector_matches(17, &[1.0, 0.0]));
        assert!(old.is_deleted(large_id));
        assert!(old.is_deleted(999_999_999_999));
        assert_eq!(old.search(&[1.0, 0.0], 2, 16).unwrap(), vec![(17, 0.0)]);

        let output_path = directory.path().join("current-writer.bin");
        old.save_to(&output_path).unwrap();
        let reopened = HnswIndex::load_from(&output_path).unwrap();
        assert_eq!(
            reopened.search(&[1.0, 0.0], 2, 16).unwrap(),
            vec![(17, 0.0)]
        );
        assert!(reopened.is_deleted(large_id));
        assert!(reopened.is_deleted(999_999_999_999));
    }

    #[test]
    fn reused_external_id_writer_omits_orphaned_slot() {
        let index = make_index(3);
        index.insert(17, vec![1.0, 0.0, 0.0]).unwrap();
        index.insert(23, vec![0.0, 1.0, 0.0]).unwrap();
        index.insert(17, vec![0.0, 0.0, 1.0]).unwrap();
        assert_eq!(index.len(), 3, "old slot remains in the mutable slab");
        assert!(index.vector_matches(17, &[0.0, 0.0, 1.0]));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reused-id.bin");
        index.save_to(&path).unwrap();
        let reopened = HnswIndex::load_from(&path).unwrap();
        assert_eq!(reopened.len(), 2);
        assert!(reopened.vector_matches(17, &[0.0, 0.0, 1.0]));
        assert!(reopened.vector_matches(23, &[0.0, 1.0, 0.0]));
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
