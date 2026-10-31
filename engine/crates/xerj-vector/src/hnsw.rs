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
/// A single-accumulator `for` loop is a strict serial f32 dependency chain.
/// LLVM will not reassociate it, so it stays scalar even at `x86-64-v3`.
/// Eight independent chains are what the beam-search inner loop actually
/// vectorizes. The release baseline is `x86-64-v2` (no AVX2; #2161), and
/// the same source is compiled again in [`dot_avx2`] with AVX2 and FMA
/// enabled. Rust does not contract `acc += x * y` into an FMA, so the two
/// compilations are bit-identical; only the instruction width changes.
/// Callers rescore exact similarities downstream — this value only orders
/// the beam.
#[inline(always)]
fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
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

/// [`dot_scalar`] compiled with AVX2 and FMA available to LLVM.
///
/// # Safety
///
/// The current CPU must support both AVX2 and FMA. The `*_avx2` entry
/// points check `is_x86_feature_detected` before calling in.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
#[inline]
unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    dot_scalar(a, b)
}

/// `AVX2 == true` selects [`dot_avx2`]. That monomorphization is only
/// reached from a `#[target_feature(enable = "avx2,fma")]` wrapper.
#[inline(always)]
fn dot<const AVX2: bool>(a: &[f32], b: &[f32]) -> f32 {
    // `AVX2` is read on every architecture so the const parameter is used
    // when the x86 block below is compiled out.
    if AVX2 && cfg!(any(target_arch = "x86", target_arch = "x86_64")) {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            // SAFETY: the corresponding `*_avx2` wrapper checked both features
            // and carries `target_feature(enable = "avx2,fma")`.
            return unsafe { dot_avx2(a, b) };
        }
    }
    dot_scalar(a, b)
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

/// Upper bound on one vector-storage chunk (#2427).
///
/// The slab used to keep every vector in one contiguous `Vec<f32>`, which
/// grows by doubling. With 384-dim embeddings a 1 GB autoindex corpus
/// reaches ~393K vectors: the next grow is a single 384 MiB -> 768 MiB
/// `realloc`, so old and new buffers are both live during the copy
/// (~1.15 GB for one index) and the freed half stays dirty in the
/// allocator afterwards. The heap profile of the 1 GB gate attributed the
/// largest untracked allocation on the ingest node to exactly that grow.
/// Fixed-size chunks never move existing vectors, never copy, and leave at
/// most one partly filled chunk of slack.
const VECTOR_CHUNK_BYTES: usize = 4 << 20;

/// Append-only vector storage in fixed-size chunks. `vector(slot)` stays a
/// shift + mask + slice, and a vector never straddles two chunks.
struct ChunkedVectors {
    dim: usize,
    /// log2(vectors per chunk).
    chunk_shift: u32,
    chunks: Vec<Vec<f32>>,
    len: usize,
}

impl ChunkedVectors {
    fn new(dim: usize) -> Self {
        let per_vector = dim.max(1) * std::mem::size_of::<f32>();
        let per_chunk = (VECTOR_CHUNK_BYTES / per_vector).max(1);
        // Largest power of two <= per_chunk, so slot -> chunk is a shift.
        let chunk_shift = usize::BITS - 1 - per_chunk.leading_zeros();
        Self {
            dim,
            chunk_shift,
            chunks: Vec::new(),
            len: 0,
        }
    }

    #[inline(always)]
    fn per_chunk(&self) -> usize {
        1usize << self.chunk_shift
    }

    fn push(&mut self, vector: &[f32]) {
        debug_assert_eq!(vector.len(), self.dim);
        if self.len & (self.per_chunk() - 1) == 0 {
            // Exact capacity: the chunk is filled by `extend_from_slice`
            // and never reallocates.
            self.chunks
                .push(Vec::with_capacity(self.per_chunk() * self.dim));
        }
        let chunk = self.chunks.last_mut().expect("chunk allocated above");
        chunk.extend_from_slice(vector);
        self.len += 1;
    }

    #[inline(always)]
    fn get(&self, slot: u32) -> &[f32] {
        let slot = slot as usize;
        let chunk = &self.chunks[slot >> self.chunk_shift];
        let start = (slot & (self.per_chunk() - 1)) * self.dim;
        &chunk[start..start + self.dim]
    }

    /// Bytes reserved for vector data (capacity, not length).
    fn reserved_bytes(&self) -> usize {
        self.chunks
            .iter()
            .map(|c| c.capacity() * std::mem::size_of::<f32>())
            .sum::<usize>()
            + self.chunks.capacity() * std::mem::size_of::<Vec<f32>>()
    }

    /// Largest single allocation backing the vectors.
    #[cfg(test)]
    fn largest_allocation_bytes(&self) -> usize {
        self.chunks
            .iter()
            .map(|c| c.capacity() * std::mem::size_of::<f32>())
            .max()
            .unwrap_or(0)
    }
}

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
    /// Vector data by slot, in fixed-size chunks (#2427).
    vectors: ChunkedVectors,
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
    /// Sum of `node_level` over all slots (layers above 0), for the O(1)
    /// heap estimate in [`SlabGraphStorage::heap_bytes`].
    upper_layer_slots: usize,
    /// Ids mutated while [`HnswIndex::compact`] rebuilds this graph
    /// (#2796); `None` when no compaction is running.
    journal: Option<CompactionJournal>,
}

/// Ids inserted or (un)tombstoned while a compaction rebuilds the graph
/// (#2796). Recorded under the graph write lock, so every mutation the
/// rebuild's snapshot missed is in here, and replayed onto the rebuilt
/// graph before it replaces this one.
#[derive(Default)]
struct CompactionJournal {
    inserted: Vec<u64>,
    flagged: Vec<u64>,
}

impl CompactionJournal {
    fn len(&self) -> usize {
        self.inserted.len() + self.flagged.len()
    }
}

/// One journaled id brought up to date on the rebuilt graph: its current
/// vector when it was (re)inserted, and whether it is tombstoned now.
struct CompactionReplay {
    id: u64,
    vector: Option<Vec<f32>>,
    deleted: bool,
}

impl CompactionReplay {
    fn collect(graph: &SlabGraphStorage, journal: &CompactionJournal) -> Vec<Self> {
        let view = graph.read_view();
        let inserted: HashSet<u64> = journal.inserted.iter().copied().collect();
        let mut seen = HashSet::new();
        journal
            .inserted
            .iter()
            .chain(&journal.flagged)
            .copied()
            .filter(|id| seen.insert(*id))
            .map(|id| CompactionReplay {
                id,
                vector: inserted
                    .contains(&id)
                    .then(|| view.current_slot(id).map(|slot| view.vector(slot).to_vec()))
                    .flatten(),
                deleted: graph.tomb_ids.contains(&id),
            })
            .collect()
    }
}

/// A node's external id and vector, as fed to [`HnswIndex::insert_batch`].
type IdVector = (u64, Vec<f32>);

/// What [`HnswIndex::compact`] did (#2796).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactStats {
    /// Nodes before the rebuild, dead ones included.
    pub nodes_before: usize,
    /// Nodes in the rebuilt graph that replaced it.
    pub nodes_after: usize,
}

/// A compaction swaps in the rebuilt graph once the mutations journaled
/// since its last catch-up round are at most this many. The final round
/// replays them serially under the write lock, so this bounds how long
/// searches and inserts wait on the swap.
const COMPACT_FINAL_ROUND_MAX: usize = 256;
/// Unlocked catch-up rounds before a compaction under sustained writes
/// gives up (the graph is left as it was; a later call tries again).
const COMPACT_MAX_CATCH_UP_ROUNDS: usize = 8;

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
        self.slab.vectors.get(slot)
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
            let base = self.slab.vectors.get(slot).as_ptr() as *const i8;
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
            vectors: ChunkedVectors::new(dim),
            inv_norms: Vec::new(),
            neighbors: Vec::new(),
            tomb: Vec::new(),
            tomb_ids: HashSet::new(),
            entry_slot: None,
            entry_layer: 0,
            upper_layer_slots: 0,
            journal: None,
        }
    }

    /// O(1) estimate of the heap this graph holds (#2427). Vector storage is
    /// exact (reserved capacity); neighbour lists are bounded by the layer
    /// degree caps (`2m` on layer 0, `m` above), which is what pruning keeps
    /// them at; hash tables count one control byte per entry slot.
    fn heap_bytes(&self, m: usize) -> usize {
        use std::mem::size_of;
        let nodes = self.ids.len();
        let layer_slots = nodes + self.upper_layer_slots;
        let neighbor_ids = nodes * 2 * m + self.upper_layer_slots * m;
        self.vectors.reserved_bytes()
            + self.ids.capacity() * size_of::<u64>()
            + self.inv_norms.capacity() * size_of::<f32>()
            + self.tomb.capacity()
            + self.neighbors.capacity() * size_of::<Vec<Vec<u32>>>()
            + layer_slots * size_of::<Vec<u32>>()
            + neighbor_ids * size_of::<u32>()
            + self.slot_of.capacity() * (size_of::<(u64, u32)>() + 1)
            + self.tomb_ids.capacity() * (size_of::<u64>() + 1)
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
        self.vectors.push(&vector);
        self.inv_norms.push(inv);
        self.neighbors.push(
            (0..=node_level)
                .map(|l| Vec::with_capacity(if l == 0 { 2 * m } else { m }))
                .collect(),
        );
        self.tomb.push(self.tomb_ids.contains(&id));
        self.upper_layer_slots += node_level;
        slot
    }
}

/// A plan reused at commit time may have been searched on a graph up to
/// `1 / PLAN_STALENESS_DIVISOR` smaller than the one it is linked into.
const PLAN_STALENESS_DIVISOR: usize = 8;
/// Upper bound on the inserts planned concurrently in one wave.
const MAX_PLAN_WAVE: usize = 256;

/// The read-only half of one insert (#2427): the neighbours chosen for a
/// vector, searched under the shared lock, waiting to be linked.
struct PlannedInsert {
    id: u64,
    vector: Vec<f32>,
    level: usize,
    /// Node count of the graph the plan was searched on.
    nodes: usize,
    /// Entry point `(slot, layer)` of that graph.
    entry: Option<(u32, usize)>,
    /// Selected neighbours per layer (index = layer) for layers
    /// `0..=min(level, entry layer)`; `None` when the graph was empty.
    links: Option<Vec<Vec<(f32, u32)>>>,
}

impl SlabGraphStorage {
    /// Greedy descent plus the `ef`-beam and diverse selection on every
    /// layer the new node links into. Reads the graph only. `None` when the
    /// graph has no entry point yet.
    fn plan_links(
        &self,
        query: &[f32],
        level: usize,
        m: usize,
        ef: usize,
    ) -> Option<Vec<Vec<(f32, u32)>>> {
        let graph = self.read_view();
        let ep_slot = self.entry_slot?;
        let ep_layer = self.entry_layer;
        let q_inv = query_inverse(&graph, query);

        // Greedy descent from the top layer down to level+1.
        let mut curr_ep = ep_slot;
        for layer in (level + 1..=ep_layer).rev() {
            curr_ep = greedy_layer(&graph, query, q_inv, curr_ep, layer).0;
        }

        // Search from min(level, ep_layer) down to 0, selecting up to M
        // diverse nearest per layer (Algorithm 4 heuristic).
        let top = level.min(ep_layer);
        let mut links = vec![Vec::new(); top + 1];
        for layer in (0..=top).rev() {
            let m_layer = if layer == 0 { 2 * m } else { m };
            let candidates = beam(&graph, query, q_inv, curr_ep, ef, layer, &|_| true);
            links[layer] = select_diverse(&graph, &candidates, m_layer);
            if let Some(&(_, nearest)) = candidates.first() {
                curr_ep = nearest;
            }
        }
        Some(links)
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
fn slot_distance<const AVX2: bool>(
    graph: &impl GraphStorageRead,
    query: &[f32],
    query_inverse_norm: f32,
    slot: u32,
) -> f32 {
    let vector = graph.vector(slot);
    match graph.metric() {
        DistanceMetric::Cosine => {
            1.0 - dot::<AVX2>(query, vector) * query_inverse_norm * graph.inverse_norm(slot)
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
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: both features are present; `greedy_layer_avx2` enables them.
            return unsafe { greedy_layer_avx2(graph, query, query_inverse_norm, start, layer) };
        }
    }
    greedy_layer_impl::<false>(graph, query, query_inverse_norm, start, layer)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn greedy_layer_avx2(
    graph: &impl GraphStorageRead,
    query: &[f32],
    query_inverse_norm: f32,
    start: u32,
    layer: usize,
) -> (u32, f32) {
    greedy_layer_impl::<true>(graph, query, query_inverse_norm, start, layer)
}

#[inline(always)]
fn greedy_layer_impl<const AVX2: bool>(
    graph: &impl GraphStorageRead,
    query: &[f32],
    query_inverse_norm: f32,
    start: u32,
    layer: usize,
) -> (u32, f32) {
    let mut best = start;
    let mut best_distance = slot_distance::<AVX2>(graph, query, query_inverse_norm, start);
    loop {
        let mut improved = false;
        if layer >= graph.layer_count(best) {
            break;
        }
        for &neighbor in graph.neighbors(best, layer) {
            let distance = slot_distance::<AVX2>(graph, query, query_inverse_norm, neighbor);
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

fn beam<G, A>(
    graph: &G,
    query: &[f32],
    query_inverse_norm: f32,
    entry: u32,
    ef: usize,
    layer: usize,
    admit: &A,
) -> Vec<(f32, u32)>
where
    G: GraphStorageRead,
    A: Fn(u32) -> bool + ?Sized,
{
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: both features are present; `beam_avx2` enables them.
            return unsafe { beam_avx2(graph, query, query_inverse_norm, entry, ef, layer, admit) };
        }
    }
    beam_impl::<G, A, false>(graph, query, query_inverse_norm, entry, ef, layer, admit)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn beam_avx2<G, A>(
    graph: &G,
    query: &[f32],
    query_inverse_norm: f32,
    entry: u32,
    ef: usize,
    layer: usize,
    admit: &A,
) -> Vec<(f32, u32)>
where
    G: GraphStorageRead,
    A: Fn(u32) -> bool + ?Sized,
{
    beam_impl::<G, A, true>(graph, query, query_inverse_norm, entry, ef, layer, admit)
}

#[inline(always)]
fn beam_impl<G, A, const AVX2: bool>(
    graph: &G,
    query: &[f32],
    query_inverse_norm: f32,
    entry: u32,
    ef: usize,
    layer: usize,
    admit: &A,
) -> Vec<(f32, u32)>
where
    G: GraphStorageRead,
    A: Fn(u32) -> bool + ?Sized,
{
    let node_count = graph.node_count();
    if entry as usize >= node_count {
        return vec![];
    }
    let mut visited = vec![0u64; node_count.div_ceil(64)];
    #[inline]
    fn test_and_set(bits: &mut [u64], slot: u32) -> bool {
        let (word, bit) = ((slot / 64) as usize, slot % 64);
        let seen = bits[word] & (1 << bit) != 0;
        bits[word] |= 1 << bit;
        seen
    }

    let entry_distance = slot_distance::<AVX2>(graph, query, query_inverse_norm, entry);
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
    test_and_set(&mut visited, entry);

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
            if test_and_set(&mut visited, neighbor) {
                continue;
            }
            let distance = slot_distance::<AVX2>(graph, query, query_inverse_norm, neighbor);
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
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: both features are present; `select_diverse_avx2` enables them.
            return unsafe { select_diverse_avx2(graph, candidates, max_neighbors) };
        }
    }
    select_diverse_impl::<false>(graph, candidates, max_neighbors)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn select_diverse_avx2(
    graph: &impl GraphStorageRead,
    candidates: &[(f32, u32)],
    max_neighbors: usize,
) -> Vec<(f32, u32)> {
    select_diverse_impl::<true>(graph, candidates, max_neighbors)
}

#[inline(always)]
fn select_diverse_impl<const AVX2: bool>(
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
            slot_distance::<AVX2>(graph, vector, inverse_norm, retained) >= candidate_distance
        });
        if diverse {
            kept.push((candidate_distance, candidate));
        }
    }
    kept
}

fn backlink_distances(graph: &impl GraphStorageRead, base: u32, layer: usize) -> Vec<(f32, u32)> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: both features are present; `backlink_distances_avx2` enables them.
            return unsafe { backlink_distances_avx2(graph, base, layer) };
        }
    }
    backlink_distances_impl::<false>(graph, base, layer)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn backlink_distances_avx2(
    graph: &impl GraphStorageRead,
    base: u32,
    layer: usize,
) -> Vec<(f32, u32)> {
    backlink_distances_impl::<true>(graph, base, layer)
}

#[inline(always)]
fn backlink_distances_impl<const AVX2: bool>(
    graph: &impl GraphStorageRead,
    base: u32,
    layer: usize,
) -> Vec<(f32, u32)> {
    let vector = graph.vector(base);
    let inverse_norm = graph.inverse_norm(base);
    graph
        .neighbors(base, layer)
        .iter()
        .map(|&neighbor| {
            (
                slot_distance::<AVX2>(graph, vector, inverse_norm, neighbor),
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

#[cfg(test)]
fn encode_graph_bytes(
    params: &HnswParams,
    graph: &impl GraphStorageRead,
) -> Result<(Vec<u8>, usize)> {
    encode_graph_to(params, graph, Vec::with_capacity(1024 * 1024))
}

/// Feeds every byte written through it into a CRC32 hasher.
struct CrcWriter<W> {
    inner: W,
    hasher: crc32fast::Hasher,
}

impl<W: Write> Write for CrcWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Counts the bytes written through it.
struct CountingWriter<W> {
    inner: W,
    count: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W) -> Self {
        Self { inner, count: 0 }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Encode the graph into `out` and return it with the live node count.
///
/// This is the whole on-disk format; nothing proportional to the graph is held in memory. Saving a
/// graph used to build the whole file in a `Vec` that grew by doubling, so
/// every save — each flush, including the memory-pressure drain — added
/// one to two times the graph size to RSS (#2427: +1.0 GB for a 0.93 GB
/// graph). Writes go through a 1 MiB buffer and the CRC is computed over
/// the same stream.
fn encode_graph_to<W: Write>(
    params: &HnswParams,
    graph: &impl GraphStorageRead,
    out: W,
) -> Result<(W, usize)> {
    let mut buffer = std::io::BufWriter::with_capacity(
        1024 * 1024,
        CrcWriter {
            inner: out,
            hasher: crc32fast::Hasher::new(),
        },
    );
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
    let CrcWriter { mut inner, hasher } = buffer
        .into_inner()
        .map_err(|e| io_to_xerj(e.into_error()))?;
    inner
        .write_u32::<LittleEndian>(hasher.finalize())
        .map_err(io_to_xerj)?;
    Ok((inner, live.len()))
}

// ─────────────────────────────────────────────────────────────────────────────
// HNSW index
// ─────────────────────────────────────────────────────────────────────────────

/// A thread-safe HNSW approximate nearest-neighbor index.
///
/// Soft deletes: deleting a doc removes its entry from the caller's id
/// maps but the node and its neighbour edges stay in the graph until
/// [`HnswIndex::compact`] rebuilds it from the live nodes (#2796).
/// Tombstoned nodes are skipped as results but still traversed.
/// Tombstones persist with the graph (format v2+).
pub struct HnswIndex {
    params: HnswParams,
    inner: RwLock<SlabGraphStorage>,
    /// Test probe: how many construction searches ran at once (#2427).
    #[cfg(test)]
    plan_probe: PlanProbe,
}

#[cfg(test)]
#[derive(Default)]
struct PlanProbe {
    in_flight: std::sync::atomic::AtomicUsize,
    max_in_flight: std::sync::atomic::AtomicUsize,
}

impl HnswIndex {
    /// Estimated heap bytes held by this graph (vectors, neighbour lists,
    /// id maps, tombstones). O(1); takes the read lock briefly. Reported as
    /// the `vector_graph` ingest-memory category (#2427).
    pub fn heap_bytes(&self) -> usize {
        self.inner.read().unwrap().heap_bytes(self.params.m)
    }

    /// Create a new empty index.
    pub fn new(params: HnswParams) -> Self {
        let inner = SlabGraphStorage::new(params.dim, params.metric);
        Self {
            params,
            inner: RwLock::new(inner),
            #[cfg(test)]
            plan_probe: PlanProbe::default(),
        }
    }

    /// Mark `id` as deleted. Search will skip the node and never
    /// return it as a hit. The node and its neighbour edges remain
    /// until [`Self::compact`] (#2796). Idempotent.
    pub fn mark_deleted(&self, id: u64) {
        let mut g = self.inner.write().unwrap();
        if let Some(&slot) = g.slot_of.get(&id) {
            g.tomb[slot as usize] = true;
        }
        g.tomb_ids.insert(id);
        if let Some(journal) = g.journal.as_mut() {
            journal.flagged.push(id);
        }
    }

    /// Reverse of `mark_deleted` — used in tests and on un-delete /
    /// reindex paths.
    pub fn unmark_deleted(&self, id: u64) {
        let mut g = self.inner.write().unwrap();
        if let Some(&slot) = g.slot_of.get(&id) {
            g.tomb[slot as usize] = false;
        }
        g.tomb_ids.remove(&id);
        if let Some(journal) = g.journal.as_mut() {
            journal.flagged.push(id);
        }
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

    /// `(live, dead)` node counts (#2796). A node is dead when it is
    /// tombstoned or was superseded by a later insert under the same id;
    /// dead nodes keep their vector and edges in memory and in the saved
    /// graph until [`Self::compact`] drops them. O(live).
    pub fn live_and_dead_counts(&self) -> (usize, usize) {
        let g = self.inner.read().unwrap();
        let live = g
            .slot_of
            .values()
            .filter(|&&slot| !g.tomb[slot as usize])
            .count();
        (live, g.ids.len() - live)
    }

    /// Rebuild the graph from its live nodes only, dropping every
    /// tombstoned or superseded node and the tombstone list (#2796).
    ///
    /// External ids and vectors are unchanged, so callers' id maps stay
    /// valid. The rebuild runs without holding the graph lock, planning
    /// inserts in parallel on the caller's rayon pool; searches and
    /// inserts continue against the old graph meanwhile. Mutations made
    /// during the rebuild are journaled and replayed onto the new graph,
    /// in unlocked rounds until at most `COMPACT_FINAL_ROUND_MAX` remain,
    /// and the final round and the swap happen under the write lock.
    ///
    /// Peak memory is the old graph plus the new one (live nodes only).
    /// Returns `Ok(None)` without changing anything when another
    /// compaction is running, or when writes outpace the catch-up rounds.
    /// Tombstones of dropped nodes are forgotten: re-inserting such an id
    /// later makes it live, as on a graph that never held it.
    pub fn compact(&self) -> Result<Option<CompactStats>> {
        let Some((nodes_before, items)) = self.compact_begin() else {
            return Ok(None);
        };
        let fresh = HnswIndex::new(self.params.clone());
        let outcome = self.compact_into(&fresh, items, nodes_before);
        if !matches!(outcome, Ok(Some(_))) {
            self.inner.write().unwrap().journal = None;
        }
        outcome
    }

    /// Start journaling and snapshot the live nodes in slot order.
    /// `None` when a compaction is already running.
    fn compact_begin(&self) -> Option<(usize, Vec<IdVector>)> {
        {
            let mut g = self.inner.write().unwrap();
            if g.journal.is_some() {
                return None;
            }
            g.journal = Some(CompactionJournal::default());
        }
        // Journaling is on before the snapshot, so a write landing between
        // the two is replayed (at worst twice, which leaves a dead node).
        let g = self.inner.read().unwrap();
        let view = g.read_view();
        let items = (0..view.node_count() as u32)
            .filter(|&slot| is_live_slot(&view, slot) && !view.tombstoned(slot))
            .map(|slot| (view.external_id(slot), view.vector(slot).to_vec()))
            .collect();
        Some((view.node_count(), items))
    }

    fn compact_into(
        &self,
        fresh: &HnswIndex,
        items: Vec<IdVector>,
        nodes_before: usize,
    ) -> Result<Option<CompactStats>> {
        fresh.insert_batch(items)?;
        for _ in 0..=COMPACT_MAX_CATCH_UP_ROUNDS {
            let journal = {
                let mut g = self.inner.write().unwrap();
                let pending = g.journal.as_ref().map_or(0, CompactionJournal::len);
                if pending <= COMPACT_FINAL_ROUND_MAX {
                    // Final round: serial inserts only. A rayon wait here
                    // could steal a job that needs this graph's lock.
                    let journal = g.journal.take().unwrap_or_default();
                    for replay in CompactionReplay::collect(&g, &journal) {
                        fresh.apply_replay(replay, false)?;
                    }
                    let slab = std::mem::replace(
                        &mut *fresh.inner.write().unwrap(),
                        SlabGraphStorage::new(self.params.dim, self.params.metric),
                    );
                    let nodes_after = slab.ids.len();
                    let old = std::mem::replace(&mut *g, slab);
                    drop(g);
                    drop(old);
                    debug!(nodes_before, nodes_after, "HNSW graph compacted");
                    return Ok(Some(CompactStats {
                        nodes_before,
                        nodes_after,
                    }));
                }
                std::mem::take(g.journal.as_mut().expect("journal checked above"))
            };
            let replays = {
                let g = self.inner.read().unwrap();
                CompactionReplay::collect(&g, &journal)
            };
            let (with_vectors, flags_only): (Vec<_>, Vec<_>) =
                replays.into_iter().partition(|r| r.vector.is_some());
            fresh.insert_batch(
                with_vectors
                    .iter()
                    .map(|r| (r.id, r.vector.clone().expect("partitioned on vector")))
                    .collect(),
            )?;
            for replay in with_vectors.into_iter().chain(flags_only) {
                fresh.apply_replay(replay, true)?;
            }
        }
        Ok(None)
    }

    /// Bring one journaled id up to date on a graph being rebuilt by
    /// [`Self::compact`]. `inserted` says the vector is already in.
    fn apply_replay(&self, replay: CompactionReplay, inserted: bool) -> Result<()> {
        if let Some(vector) = replay.vector.filter(|_| !inserted) {
            self.insert(replay.id, vector)?;
        }
        if replay.deleted {
            self.mark_deleted(replay.id);
        } else {
            self.unmark_deleted(replay.id);
        }
        Ok(())
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
        self.check_dim(&vector)?;
        let plan = self.plan_insert(id, vector, node_level);
        self.commit_planned(plan);
        Ok(())
    }

    fn check_dim(&self, vector: &[f32]) -> Result<()> {
        if vector.len() != self.params.dim {
            return Err(XerjError::invalid_mapping(format!(
                "HNSW insert: vector dim {} != index dim {}",
                vector.len(),
                self.params.dim
            )));
        }
        Ok(())
    }

    /// Phase 1 of an insert (#2427): the construction search — greedy
    /// descent, the `ef_construction` beam on every linked layer and the
    /// diverse-neighbour selection — runs under the SHARED read lock and
    /// touches nothing. This is where nearly all insert CPU goes, so plans
    /// for different vectors can be computed concurrently.
    fn plan_insert(&self, id: u64, vector: Vec<f32>, level: usize) -> PlannedInsert {
        #[cfg(test)]
        {
            use std::sync::atomic::Ordering;
            let now = self.plan_probe.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.plan_probe
                .max_in_flight
                .fetch_max(now, Ordering::SeqCst);
        }
        let g = self.inner.read().unwrap();
        let links = g.plan_links(&vector, level, self.params.m, self.params.ef_construction);
        #[cfg(test)]
        self.plan_probe
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        PlannedInsert {
            id,
            vector,
            level,
            nodes: g.ids.len(),
            entry: g.entry_slot.map(|slot| (slot, g.entry_layer)),
            links,
        }
    }

    /// Phase 2 of an insert: allocate the slot and wire the planned links
    /// and back-links under the exclusive write lock. All graph mutation
    /// stays serial, so neighbour lists never race. A plan searched on a
    /// graph that has since grown by more than `nodes / PLAN_STALENESS_DIVISOR`
    /// (or whose entry point moved above the plan's top layer) is searched
    /// again here, so a stale snapshot can never cost link quality beyond
    /// what a bounded number of concurrent in-flight inserts would.
    fn commit_planned(&self, plan: PlannedInsert) {
        let m = self.params.m;
        let ef = self.params.ef_construction;
        let PlannedInsert {
            id,
            vector,
            level,
            nodes,
            entry,
            links,
        } = plan;

        let mut g = self.inner.write().unwrap();
        if let Some(journal) = g.journal.as_mut() {
            journal.inserted.push(id);
        }
        let current_entry = g.entry_slot.map(|slot| (slot, g.entry_layer));
        let grown = g.ids.len().saturating_sub(nodes);
        let entry_rose =
            current_entry != entry && entry.is_none_or(|(_, planned_layer)| level > planned_layer);
        let links = if links.is_none() || entry_rose || grown > nodes / PLAN_STALENESS_DIVISOR {
            g.plan_links(&vector, level, m, ef)
        } else {
            links
        };

        let reuses_entry_identity = g
            .entry_slot
            .is_some_and(|entry| g.ids[entry as usize] == id);

        // The new slot has no in-edges yet, and the plan was searched without
        // it, so allocating it now cannot perturb the planned links — and it
        // means the back-link re-selection below can score the new node like
        // any other neighbor (the pre-slab code dropped the fresh back-link
        // on the floor here, disconnecting the graph: layer-0 BFS from the
        // entry reached 34 of 50k nodes and recall@10 was 0.000).
        let slot = g.alloc_slot(id, vector, level, m);

        // Handle first insertion.
        let Some(links) = links else {
            g.entry_slot = Some(slot);
            g.entry_layer = level;
            debug!("HNSW: first node id={id} level={level}");
            return;
        };

        for (layer, selected) in links.iter().enumerate().rev() {
            let m_layer = if layer == 0 { 2 * m } else { m };
            // Connect new node → selected.
            g.neighbors[slot as usize][layer] = selected.iter().map(|&(_, s)| s).collect();

            // Back-links: add new node to each selected neighbor, and
            // re-select when a neighbor exceeds its link budget.
            for &(_, sel) in selected {
                g.neighbors[sel as usize][layer].push(slot);
                if g.neighbors[sel as usize][layer].len() > m_layer {
                    let dists = backlink_distances(&g.read_view(), sel, layer);
                    let pruned = select_diverse(&g.read_view(), &dists, m_layer);
                    g.neighbors[sel as usize][layer] = pruned.iter().map(|&(_, s)| s).collect();
                }
            }
        }

        // Update entry point if the new node reached a higher layer.
        if reuses_entry_identity {
            let (entry, layer) = highest_live_entry(&g.read_view()).unwrap_or((slot, level));
            g.entry_slot = Some(entry);
            g.entry_layer = layer;
        } else if level > g.entry_layer {
            g.entry_slot = Some(slot);
            g.entry_layer = level;
        }

        trace!("HNSW: inserted id={id} level={level}");
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
        let candidates = beam(&graph, query, q_inv, curr_ep, ef.max(k), 0, &admit);

        Ok(candidates
            .into_iter()
            .filter(|&(_, s)| admit(s))
            .take(k)
            .map(|(dist, slot)| (graph.external_id(slot), dist))
            .collect())
    }

    /// Insert a batch of `(id, vector)` items.
    ///
    /// The batch goes in waves (#2427). Each wave's construction searches
    /// ([`plan_insert`](Self::plan_insert), read-only) run in parallel on
    /// the caller's rayon pool against the same graph snapshot; the plans
    /// are then committed one by one under the write lock, so every
    /// neighbour-list mutation stays serial and the graph stays consistent.
    /// A wave is at most `len / PLAN_STALENESS_DIVISOR` items (capped at
    /// `MAX_PLAN_WAVE`, at least one), so an item never misses more than
    /// that fraction of the graph as candidate neighbours — on a small
    /// graph the waves are single items and insertion is exactly serial.
    ///
    /// Items are validated in order (dimension check); on a mismatch the
    /// items before it are inserted and `Err` is returned (this API is not
    /// transactional).
    pub fn insert_batch(&self, items: Vec<(u64, Vec<f32>)>) -> Result<()> {
        use rayon::prelude::*;

        let mut items = items;
        let invalid = items
            .iter()
            .position(|(_, vector)| vector.len() != self.params.dim);
        let error = invalid.map(|at| self.check_dim(&items[at].1).unwrap_err());
        if let Some(at) = invalid {
            items.truncate(at);
        }

        let mut pending = items
            .into_iter()
            .map(|(id, vector)| (id, vector, self.random_level()))
            .peekable();
        while pending.peek().is_some() {
            let wave_len = (self.len() / PLAN_STALENESS_DIVISOR).clamp(1, MAX_PLAN_WAVE);
            let wave: Vec<(u64, Vec<f32>, usize)> = pending.by_ref().take(wave_len).collect();
            let plans: Vec<PlannedInsert> = if wave.len() == 1 {
                wave.into_iter()
                    .map(|(id, vector, level)| self.plan_insert(id, vector, level))
                    .collect()
            } else {
                wave.into_par_iter()
                    .map(|(id, vector, level)| self.plan_insert(id, vector, level))
                    .collect()
            };
            for plan in plans {
                self.commit_planned(plan);
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
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
        // Atomic + durable publish (RC4 W2 item 19). The previous
        // fs::write + rename left both the file bytes and the directory
        // entry in the volatile page cache: a power loss shortly after a
        // flush could leave a truncated/empty graph.bin (CRC-rejected at
        // the next open) or no file at all — and because the id-map/graph
        // pair then fails to load or fails its freshness stamp, every kNN
        // after recovery silently degraded to permanent-stale/brute.
        // write_file_durable_streaming fsyncs the temp file, renames, then fsyncs
        // the parent directory so the rename itself survives power loss.
        // Stream straight into the temp file (#2427): the read lock is held
        // while the bytes go to the page cache, and released before fsync.
        let mut written = (0u64, 0usize);
        xerj_common::fsio::write_file_durable_streaming(path, |file| {
            let g = self.inner.read().unwrap();
            let (mut out, node_count) =
                encode_graph_to(&self.params, &g.read_view(), CountingWriter::new(file))
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
            drop(g);
            out.flush()?;
            written = (out.count, node_count);
            Ok(())
        })
        .map_err(io_to_xerj)?;
        let (bytes, node_count) = written;

        debug!(
            path = %path.display(),
            bytes,
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
            #[cfg(test)]
            plan_probe: PlanProbe::default(),
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
        let candidates = beam(&graph, query, q_inv, entry_slot, ef.max(k), 0, &admit);
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
        let mut curr_dist = slot_distance::<false>(&graph, query, q_inv, curr);
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

    /// #2427: the slab's vector storage must grow in bounded chunks. One
    /// contiguous `Vec<f32>` doubled on every grow; at the 1 GB gate's ~393K
    /// 384-dim vectors that is a single 384 MiB -> 768 MiB realloc with both
    /// buffers live, the largest untracked allocation in the ingest heap
    /// profile. Chunks keep every allocation <= VECTOR_CHUNK_BYTES and the
    /// unused tail <= one chunk, and vectors read back intact across chunk
    /// boundaries.
    #[test]
    fn slab_vector_storage_grows_in_bounded_chunks_2427() {
        let dim = 384;
        let per_vector = dim * std::mem::size_of::<f32>();
        let n = 3 * (VECTOR_CHUNK_BYTES / per_vector) + 7;
        let mut slab = SlabGraphStorage::new(dim, DistanceMetric::Cosine);
        for i in 0..n {
            let v: Vec<f32> = (0..dim).map(|d| (i * dim + d) as f32).collect();
            slab.alloc_slot(i as u64, v, 0, 16);
        }
        let view = slab.read_view();
        for i in (0..n).step_by(97).chain([n - 1]) {
            let v = view.vector(i as u32);
            assert_eq!(v.len(), dim);
            assert_eq!(v[0], (i * dim) as f32, "slot {i}");
            assert_eq!(v[dim - 1], (i * dim + dim - 1) as f32, "slot {i}");
        }
        let largest = slab.vectors.largest_allocation_bytes();
        assert!(
            largest <= VECTOR_CHUNK_BYTES,
            "largest vector allocation {largest} B exceeds the {VECTOR_CHUNK_BYTES} B chunk bound"
        );
        let used = n * per_vector;
        let slack = slab.vectors.reserved_bytes() - used;
        assert!(
            slack <= VECTOR_CHUNK_BYTES + 4096,
            "vector storage reserves {slack} B beyond the {used} B in use"
        );
        assert!(slab.heap_bytes(16) >= used);
    }

    /// #2427: `HnswIndex::heap_bytes` is what the `vector_graph` trace
    /// category reports; it must cover the vectors and grow with inserts.
    #[test]
    fn hnsw_heap_bytes_tracks_inserted_vectors_2427() {
        let dim = 16;
        let index = HnswIndex::new(HnswParams::new(dim, DistanceMetric::Cosine));
        let empty = index.heap_bytes();
        for i in 0..200u64 {
            let v: Vec<f32> = (0..dim)
                .map(|d| ((i as usize * 31 + d) % 17) as f32 + 1.0)
                .collect();
            index.insert(i, v).unwrap();
        }
        let full = index.heap_bytes();
        assert!(full > empty);
        assert!(full >= 200 * dim * std::mem::size_of::<f32>());
    }

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
            for &value in graph.vectors.get(slot) {
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
            let vector = graph.vectors.get(slot);
            match graph.metric {
                DistanceMetric::Cosine => {
                    1.0 - dot_scalar(query, vector)
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

    /// #2427: a batch's construction searches run concurrently on the
    /// caller's rayon pool (only the link writes are serial), and the
    /// resulting graph keeps the recall of a serial build.
    #[test]
    fn insert_batch_plans_in_parallel_and_keeps_recall_2427() {
        let dim = 16;
        let n = 4000usize;
        let vector_for = |i: usize| -> Vec<f32> {
            (0..dim as u64)
                .map(|d| {
                    let h = (i as u64 * dim as u64 + d + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    ((h >> 40) % 2001) as f32 / 1000.0 - 1.0
                })
                .collect()
        };
        let idx = make_index(dim);
        let items: Vec<(u64, Vec<f32>)> = (0..n).map(|i| (i as u64, vector_for(i))).collect();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        pool.install(|| idx.insert_batch(items)).unwrap();
        assert_eq!(idx.len(), n);
        let max_in_flight = idx
            .plan_probe
            .max_in_flight
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            max_in_flight > 1,
            "construction searches never overlapped (max in flight {max_in_flight})"
        );

        let cosine = |a: &[f32], b: &[f32]| compute_distance(DistanceMetric::Cosine, a, b);
        let mut hits = 0usize;
        let queries = 50usize;
        for q in 0..queries {
            let query = vector_for(n + q);
            let mut exact: Vec<(f32, u64)> = (0..n)
                .map(|i| (cosine(&query, &vector_for(i)), i as u64))
                .collect();
            exact.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let truth: HashSet<u64> = exact.iter().take(10).map(|&(_, id)| id).collect();
            let found = idx.search(&query, 10, 100).unwrap();
            hits += found.iter().filter(|(id, _)| truth.contains(id)).count();
        }
        let recall = hits as f64 / (queries * 10) as f64;
        assert!(recall >= 0.95, "recall@10 {recall:.3} < 0.95");
    }

    /// #2427: inserts from many threads at once (plans racing other
    /// threads' commits, including stale-plan re-searches) leave a
    /// consistent graph: every node is present, neighbour lists are in
    /// bounds and within their degree caps, and recall holds.
    #[test]
    fn concurrent_inserts_keep_the_graph_consistent_2427() {
        let dim = 16;
        let threads = 8usize;
        let per_thread = 400usize;
        let n = threads * per_thread;
        let vector_for = |i: usize| -> Vec<f32> {
            (0..dim as u64)
                .map(|d| {
                    let h = (i as u64 * dim as u64 + d + 7).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    ((h >> 40) % 2001) as f32 / 1000.0 - 1.0
                })
                .collect()
        };
        let idx = std::sync::Arc::new(make_index(dim));
        std::thread::scope(|scope| {
            for t in 0..threads {
                let idx = std::sync::Arc::clone(&idx);
                scope.spawn(move || {
                    for i in (t * per_thread)..((t + 1) * per_thread) {
                        idx.insert(i as u64, vector_for(i)).unwrap();
                    }
                });
            }
        });
        assert_eq!(idx.len(), n);
        {
            let g = idx.inner.read().unwrap();
            let m = idx.params.m;
            for (slot, layers) in g.neighbors.iter().enumerate() {
                for (layer, list) in layers.iter().enumerate() {
                    let cap = if layer == 0 { 2 * m } else { m };
                    assert!(list.len() <= cap, "slot {slot} layer {layer} over cap");
                    for &neighbor in list {
                        assert!((neighbor as usize) < n);
                        assert_ne!(neighbor as usize, slot, "self link");
                        assert!(g.neighbors[neighbor as usize].len() > layer);
                    }
                }
            }
        }
        let cosine = |a: &[f32], b: &[f32]| compute_distance(DistanceMetric::Cosine, a, b);
        let mut hits = 0usize;
        let queries = 50usize;
        for q in 0..queries {
            let query = vector_for(n + q);
            let mut exact: Vec<(f32, u64)> = (0..n)
                .map(|i| (cosine(&query, &vector_for(i)), i as u64))
                .collect();
            exact.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let truth: HashSet<u64> = exact.iter().take(10).map(|&(_, id)| id).collect();
            let found = idx.search(&query, 10, 100).unwrap();
            hits += found.iter().filter(|(id, _)| truth.contains(id)).count();
        }
        let recall = hits as f64 / (queries * 10) as f64;
        assert!(recall >= 0.95, "recall@10 {recall:.3} < 0.95");
        for i in (0..n).step_by(97) {
            let found = idx.search(&vector_for(i), 1, 64).unwrap();
            assert_eq!(
                found[0].0, i as u64,
                "node {i} not found as its own nearest"
            );
        }
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
                assert_eq!(view.vector(slot), slab.vectors.get(slot));
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
    fn streamed_save_writes_legacy_bytes_and_round_trips() {
        let index = HnswIndex::new(HnswParams::new(8, DistanceMetric::Cosine));
        for id in 0..600u64 {
            let v: Vec<f32> = (0..8)
                .map(|d| ((id * 31 + d * 7) % 97) as f32 + 1.0)
                .collect();
            index.insert(id + (1 << 40), v).unwrap();
        }
        for id in (0..600u64).step_by(7) {
            index.mark_deleted(id + (1 << 40));
        }
        let expected = {
            let slab = index.inner.read().unwrap();
            legacy_encode_graph_bytes(&index.params, &slab)
        };
        // Bigger than the 1 MiB write buffer would be ideal, but the CRC and
        // byte layout are what can break; 600 nodes span many buffer refills
        // of the BufWriter's partial-write path through CountingWriter.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        index.save_to(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        let loaded = HnswIndex::load_from(&path).unwrap();
        assert_eq!(loaded.len(), index.len());
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "temp file left behind");
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
        let candidates = beam(
            &graph,
            &query,
            inverse,
            graph.entry_slot().unwrap(),
            8,
            0,
            &|slot| !graph.tombstoned(slot),
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

    /// The AVX2 compilation of the beam-search dot product must return the
    /// same bits as the scalar compilation. Lengths cover the 8-wide chunk,
    /// the remainder, and an unaligned window.
    #[test]
    fn avx2_dot_matches_scalar_bits() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("fma") {
                return;
            }
            let mut state = 0xC0FFEE_u64;
            let mut next = || {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let unit = ((state >> 33) as u32 as f32) / 2147483648.0;
                unit * 4.0 - 2.0
            };
            let mut data = Vec::with_capacity(2048);
            for _ in 0..2048 {
                data.push(next());
            }
            data[3] = 0.0;
            data[4] = -0.0;
            data[10] = 1.0e20;
            data[11] = -1.0e-20;
            for &len in &[0usize, 1, 7, 8, 9, 15, 16, 17, 31, 32, 63, 64, 768] {
                for &off in &[0usize, 1] {
                    if off + len * 2 > data.len() {
                        continue;
                    }
                    let a = &data[off..off + len];
                    let b = &data[off + len..off + len * 2];
                    let scalar = dot_scalar(a, b);
                    // SAFETY: the feature check above succeeded.
                    let avx = unsafe { dot_avx2(a, b) };
                    assert_eq!(
                        scalar.to_bits(),
                        avx.to_bits(),
                        "len {len} off {off}: scalar {scalar} avx2 {avx}"
                    );
                }
            }
        }
    }

    /// Production search (AVX2 kernel on this host, scalar otherwise) must
    /// agree bit-for-bit with the scalar eight-accumulator formula.
    #[test]
    fn search_cosine_distance_matches_scalar_dot() {
        let dim = 32usize;
        let idx = make_index(dim);
        let mut state = 0xA11CE_u64;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            (((state >> 33) as u32) as f32) / 2147483648.0 * 2.0 - 1.0
        };
        let mut vecs = Vec::new();
        for i in 0..40u64 {
            let v: Vec<f32> = (0..dim).map(|_| next()).collect();
            idx.insert(i, v.clone()).unwrap();
            vecs.push(v);
        }
        let q = vecs[0].clone();
        let q_norm = q.iter().map(|x| x * x).sum::<f32>().sqrt();
        let q_inv = if q_norm > 0.0 { 1.0 / q_norm } else { 0.0 };
        let hits = idx.search(&q, 8, 64).unwrap();
        assert!(!hits.is_empty());
        for (id, dist) in hits {
            let v = &vecs[id as usize];
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            let inv = if n > 0.0 { 1.0 / n } else { 0.0 };
            let expect = 1.0 - dot_scalar(&q, v) * q_inv * inv;
            assert_eq!(
                dist.to_bits(),
                expect.to_bits(),
                "id {id}: search {dist} scalar {expect}"
            );
        }
    }

    fn spread_vec(seed: u64, dim: usize) -> Vec<f32> {
        (0..dim as u64)
            .map(|d| {
                let h = (seed * dim as u64 + d + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                ((h >> 40) % 2001) as f32 / 1000.0 - 1.0
            })
            .collect()
    }

    /// #2796: overwriting docs the way the engine does (a fresh node id
    /// per write, the old node tombstoned) left every old vector in the
    /// graph and in `graph.bin` forever. `compact` must drop them and keep
    /// every live node findable under its id.
    #[test]
    fn compact_drops_dead_nodes_and_keeps_live_ones_2796() {
        const DIM: usize = 16;
        const LIVE: usize = 200;
        const ROUNDS: u64 = 5;
        let idx = make_index(DIM);
        let mut next_id = 0u64;
        let mut current: Vec<(u64, Vec<f32>)> = Vec::new();
        for doc in 0..LIVE {
            let v = spread_vec(doc as u64, DIM);
            idx.insert(next_id, v.clone()).unwrap();
            current.push((next_id, v));
            next_id += 1;
        }
        let mut superseded = Vec::new();
        for round in 1..=ROUNDS {
            for (doc, slot) in current.iter_mut().enumerate() {
                let v = spread_vec(round * 10_000 + doc as u64, DIM);
                idx.mark_deleted(slot.0);
                superseded.push((slot.0, slot.1.clone()));
                idx.insert(next_id, v.clone()).unwrap();
                *slot = (next_id, v);
                next_id += 1;
            }
        }
        // A same-id re-insert orphans the earlier slot, which is dead too.
        idx.insert(current[0].0, current[0].1.clone()).unwrap();

        let dead_before = LIVE * ROUNDS as usize + 1;
        assert_eq!(idx.live_and_dead_counts(), (LIVE, dead_before));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.bin");
        idx.save_to(&path).unwrap();
        let bytes_before = std::fs::metadata(&path).unwrap().len();

        let stats = idx.compact().unwrap().expect("no concurrent writer");
        assert_eq!(stats.nodes_before, LIVE + dead_before);
        assert_eq!(stats.nodes_after, LIVE);
        assert_eq!(idx.live_and_dead_counts(), (LIVE, 0));
        assert_eq!(idx.len(), LIVE);
        assert_eq!(idx.tombstone_count(), 0);

        idx.save_to(&path).unwrap();
        let bytes_after = std::fs::metadata(&path).unwrap().len();
        assert!(
            bytes_after * 5 < bytes_before,
            "graph.bin {bytes_before} -> {bytes_after} bytes for {LIVE} live of {} nodes",
            LIVE + dead_before
        );

        for (id, v) in &current {
            assert!(idx.vector_matches(*id, v));
            assert_eq!(idx.search(v, 1, 64).unwrap()[0].0, *id);
        }
        for (id, v) in &superseded {
            assert!(!idx.vector_matches(*id, v), "dead node {id} survived");
        }
        let reloaded = HnswIndex::load_from(&path).unwrap();
        assert_eq!(reloaded.live_and_dead_counts(), (LIVE, 0));
        let (id, v) = &current[LIVE / 2];
        assert_eq!(reloaded.search(v, 1, 64).unwrap()[0].0, *id);
    }

    /// #2796: writes landing while `compact` rebuilds must not be lost.
    /// More than `COMPACT_FINAL_ROUND_MAX` of them force an unlocked
    /// catch-up round before the locked final one.
    #[test]
    fn compact_replays_writes_made_during_the_rebuild_2796() {
        const DIM: usize = 16;
        let idx = make_index(DIM);
        for id in 0..300u64 {
            idx.insert(id, spread_vec(id, DIM)).unwrap();
        }
        idx.mark_deleted(3);

        let (nodes_before, items) = idx.compact_begin().expect("not running");
        assert_eq!(items.len(), 299, "the snapshot skips the dead node");
        // Writes made between the snapshot and the swap.
        let added = 300..300 + COMPACT_FINAL_ROUND_MAX as u64 * 2;
        for id in added.clone() {
            idx.insert(id, spread_vec(id, DIM)).unwrap();
        }
        idx.mark_deleted(5);
        idx.mark_deleted(400);
        let moved = spread_vec(99_999, DIM);
        idx.insert(7, moved.clone()).unwrap();
        assert!(idx.compact().unwrap().is_none(), "one compaction at a time");

        let fresh = HnswIndex::new(idx.params.clone());
        let stats = idx
            .compact_into(&fresh, items, nodes_before)
            .unwrap()
            .expect("catch-up converges without further writes");
        assert_eq!(stats.nodes_before, 300);
        assert!(idx.inner.read().unwrap().journal.is_none());

        let live = 300 - 1 + added.clone().count() - 2;
        assert_eq!(idx.live_and_dead_counts().0, live);
        assert!(!idx.is_deleted(3), "dropped node's tombstone is gone");
        assert!(idx.is_deleted(5) && idx.is_deleted(400));
        assert!(idx.vector_matches(7, &moved));
        assert_eq!(idx.search(&moved, 1, 64).unwrap()[0].0, 7);
        for id in [0u64, 299, 300, 555, added.end - 1] {
            assert_eq!(idx.search(&spread_vec(id, DIM), 1, 64).unwrap()[0].0, id);
        }
        for (id, _) in idx.search(&spread_vec(5, DIM), 10, 64).unwrap() {
            assert!(id != 5 && id != 400, "tombstoned node {id} returned");
        }
    }
}
