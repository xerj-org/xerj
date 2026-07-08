//! HNSW (Hierarchical Navigable Small World) graph index.
//!
//! Reference: Malkov & Yashunin 2018, "Efficient and Robust Approximate Nearest
//! Neighbor Search Using Hierarchical Navigable Small World Graphs."
//!
//! Design decisions:
//! - M=16 bidirectional connections per layer (good accuracy/memory trade-off)
//! - ef_construction=200 (high recall during build)
//! - Level selection uses the standard ln-based formula
//! - Graph is entirely in-memory; persistence is handled by the storage crate

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, RwLock};

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
// Internal node
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Node {
    /// The node's own id. Redundant with its key in the `nodes` map, so it is
    /// never read back out — kept for `Debug` output and crash diagnostics.
    #[allow(dead_code)]
    id: u64,
    vector: Vec<f32>,
    /// Neighbor lists per layer. `neighbors[layer]` holds neighbor node IDs.
    neighbors: Vec<Vec<u64>>,
}

impl Node {
    fn new(id: u64, vector: Vec<f32>, max_layer: usize, m: usize) -> Self {
        let neighbors = (0..=max_layer)
            .map(|l| Vec::with_capacity(if l == 0 { 2 * m } else { m }))
            .collect();
        Self {
            id,
            vector,
            neighbors,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// HNSW index
// ─────────────────────────────────────────────────────────────────────────────

/// A thread-safe HNSW approximate nearest-neighbor index.
pub struct HnswIndex {
    params: HnswParams,
    nodes: RwLock<HashMap<u64, Arc<RwLock<Node>>>>,
    entry_point: RwLock<Option<u64>>,
    entry_layer: RwLock<usize>,
    /// Soft-delete bitmap. v0.6.2: deleting a doc removes its
    /// entry from `Index.hnsw_id_map` but the node and its
    /// neighbour edges stay in the graph until a graph compaction
    /// (planned v0.7+). Adding to this set causes search to skip
    /// the node — both as a result and as a traversal target.
    /// Persisted alongside the graph (format version 2+).
    tombstones: RwLock<HashSet<u64>>,
}

impl HnswIndex {
    /// Create a new empty index.
    pub fn new(params: HnswParams) -> Self {
        Self {
            params,
            nodes: RwLock::new(HashMap::new()),
            entry_point: RwLock::new(None),
            entry_layer: RwLock::new(0),
            tombstones: RwLock::new(HashSet::new()),
        }
    }

    /// Mark `id` as deleted. Search will skip the node and never
    /// return it as a hit. The node and its neighbour edges remain
    /// until a graph compaction (planned v0.7+). Idempotent.
    pub fn mark_deleted(&self, id: u64) {
        self.tombstones.write().unwrap().insert(id);
    }

    /// Reverse of `mark_deleted` — used in tests and on un-delete /
    /// reindex paths.
    pub fn unmark_deleted(&self, id: u64) {
        self.tombstones.write().unwrap().remove(&id);
    }

    /// True if `id` has been tombstoned via `mark_deleted`.
    pub fn is_deleted(&self, id: u64) -> bool {
        self.tombstones.read().unwrap().contains(&id)
    }

    /// Number of tombstones currently held. Operators can monitor
    /// this to decide when to schedule a compaction.
    pub fn tombstone_count(&self) -> usize {
        self.tombstones.read().unwrap().len()
    }

    pub fn params(&self) -> &HnswParams {
        &self.params
    }

    /// Number of indexed vectors.
    pub fn len(&self) -> usize {
        self.nodes.read().unwrap().len()
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

    // ── Distance helper ───────────────────────────────────────────────────

    fn dist(&self, a: &[f32], b: &[f32]) -> f32 {
        compute_distance(self.params.metric, a, b)
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

        // Build the node (not yet inserted into the map)
        let node = Arc::new(RwLock::new(Node::new(id, vector.clone(), node_level, m)));

        // Handle first insertion
        {
            let mut ep = self.entry_point.write().unwrap();
            if ep.is_none() {
                *ep = Some(id);
                *self.entry_layer.write().unwrap() = node_level;
                self.nodes.write().unwrap().insert(id, node);
                debug!("HNSW: first node id={id} level={node_level}");
                return Ok(());
            }
        }

        // Greedy descent from top layer to node_level+1
        let ep_id = self.entry_point.read().unwrap().unwrap();
        let ep_layer = *self.entry_layer.read().unwrap();

        // Greedy descent tracks only the current entry point; the distance to
        // it is recomputed by `search_layer`/`greedy_search_layer` at each step,
        // so we don't carry it between layers.
        let mut curr_ep = ep_id;

        for layer in (node_level + 1..=ep_layer).rev() {
            curr_ep = self.greedy_search_layer(&vector, curr_ep, layer, 1).0;
        }

        // Search and connect from node_level down to 0
        for layer in (0..=node_level.min(ep_layer)).rev() {
            let m_layer = if layer == 0 { 2 * m } else { m };
            let candidates = self.search_layer(&vector, curr_ep, ef, layer);

            // Select M nearest
            let selected = self.select_neighbors(&vector, &candidates, m_layer);

            // Connect new node → selected
            {
                let mut n = node.write().unwrap();
                if layer < n.neighbors.len() {
                    n.neighbors[layer] = selected.iter().map(|&(_, nid)| nid).collect();
                }
            }

            // Update existing nodes: add back-link to new node, prune to m_layer
            {
                let nodes = self.nodes.read().unwrap();
                for &(_, nid) in &selected {
                    if let Some(existing) = nodes.get(&nid) {
                        let mut en = existing.write().unwrap();
                        if layer < en.neighbors.len() {
                            en.neighbors[layer].push(id);
                            if en.neighbors[layer].len() > m_layer {
                                // Re-select neighbors to keep M_layer best
                                let ev = en.vector.clone();
                                let neighbor_dists: Vec<(f32, u64)> = en.neighbors[layer]
                                    .iter()
                                    .filter_map(|&nid2| {
                                        nodes.get(&nid2).map(|nn| {
                                            let nv = nn.read().unwrap();
                                            (self.dist(&ev, &nv.vector), nid2)
                                        })
                                    })
                                    .collect();
                                let pruned = self.select_neighbors(&ev, &neighbor_dists, m_layer);
                                en.neighbors[layer] =
                                    pruned.iter().map(|&(_, nid2)| nid2).collect();
                            }
                        }
                    }
                }
            }

            if !candidates.is_empty() {
                curr_ep = candidates[0].1;
            }
        }

        // Insert the node
        self.nodes.write().unwrap().insert(id, node);

        // Update entry point if new node is on a higher layer
        {
            let mut ep = self.entry_point.write().unwrap();
            let mut ep_layer = self.entry_layer.write().unwrap();
            if node_level > *ep_layer {
                *ep = Some(id);
                *ep_layer = node_level;
            }
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

        // Snapshot tombstones once and clone-into-the-closure so the
        // beam search sees a consistent set even if mark_deleted runs
        // mid-query. Cloning a u64 HashSet is cheap and avoids holding
        // the read lock across the whole traversal.
        let tombstoned: HashSet<u64> = self.tombstones.read().unwrap().clone();
        let combined_filter = move |id: u64| -> bool { !tombstoned.contains(&id) && filter(id) };

        let ep_opt = *self.entry_point.read().unwrap();
        let ep_id = match ep_opt {
            Some(id) => id,
            None => return Ok(vec![]),
        };
        let ep_layer = *self.entry_layer.read().unwrap();

        let mut curr_ep = ep_id;

        // Greedy descent from top to layer 1
        for layer in (1..=ep_layer).rev() {
            let (new_ep, _) = self.greedy_search_layer(query, curr_ep, layer, 1);
            curr_ep = new_ep;
        }

        // Beam search at layer 0 with ef. Tombstoned ids are kept as
        // *traversal* candidates (otherwise we lose access to their
        // neighbours and recall collapses) but excluded from results
        // by the post-take filter below.
        let candidates = self.search_layer_filtered(query, curr_ep, ef.max(k), 0, &combined_filter);

        let results: Vec<(u64, f32)> = candidates
            .into_iter()
            .filter(|&(_, id)| combined_filter(id))
            .take(k)
            .map(|(dist, id)| (id, dist))
            .collect();

        Ok(results)
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
    /// method is not transactional / all-or-nothing).
    pub fn insert_batch(&self, items: Vec<(u64, Vec<f32>)>) -> Result<()> {
        for (id, vec) in items {
            self.insert(id, vec)?;
        }
        Ok(())
    }

    // ── Persistence ───────────────────────────────────────────────────────
    //
    // Wire format (little-endian, single file, written atomically via
    // tmp + rename):
    //
    //   magic:        8 bytes  = "XHNS0001"
    //   format_ver:   u32      = HNSW_FORMAT_VERSION
    //   m:            u32
    //   ef_construction:u32
    //   metric:       u8       = DistanceMetric repr (0=cosine,1=l2,2=dot,3=mip)
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

        let ep = *self.entry_point.read().unwrap();
        let el = *self.entry_layer.read().unwrap();
        buf.write_u64::<LittleEndian>(ep.unwrap_or(u64::MAX))
            .map_err(io_to_xerj)?;
        buf.write_u32::<LittleEndian>(el as u32)
            .map_err(io_to_xerj)?;

        let nodes = self.nodes.read().unwrap();
        buf.write_u32::<LittleEndian>(nodes.len() as u32)
            .map_err(io_to_xerj)?;
        for (id, node_arc) in nodes.iter() {
            let n = node_arc.read().unwrap();
            buf.write_u64::<LittleEndian>(*id).map_err(io_to_xerj)?;
            let max_layer = n.neighbors.len().saturating_sub(1) as u32;
            buf.write_u32::<LittleEndian>(max_layer)
                .map_err(io_to_xerj)?;
            for &v in &n.vector {
                buf.write_f32::<LittleEndian>(v).map_err(io_to_xerj)?;
            }
            for layer_neighbors in &n.neighbors {
                buf.write_u32::<LittleEndian>(layer_neighbors.len() as u32)
                    .map_err(io_to_xerj)?;
                for &nid in layer_neighbors {
                    buf.write_u64::<LittleEndian>(nid).map_err(io_to_xerj)?;
                }
            }
        }
        drop(nodes);

        // v2 — tombstone block. Empty for indices that have never had
        // a delete; v1 readers don't know to skip past this, so the
        // format version was bumped (loader checks).
        let tombs = self.tombstones.read().unwrap();
        buf.write_u32::<LittleEndian>(tombs.len() as u32)
            .map_err(io_to_xerj)?;
        for &t in tombs.iter() {
            buf.write_u64::<LittleEndian>(t).map_err(io_to_xerj)?;
        }
        drop(tombs);

        let crc = crc32fast::hash(&buf);
        buf.write_u32::<LittleEndian>(crc).map_err(io_to_xerj)?;

        // Atomic publish.
        std::fs::write(&tmp_path, &buf).map_err(io_to_xerj)?;
        std::fs::rename(&tmp_path, path).map_err(io_to_xerj)?;
        debug!(
            path = %path.display(),
            bytes = buf.len(),
            nodes = self.nodes.read().unwrap().len(),
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
        let mut node_map: HashMap<u64, Arc<RwLock<Node>>> = HashMap::with_capacity(num_nodes);

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
            node_map.insert(
                id,
                Arc::new(RwLock::new(Node {
                    id,
                    vector,
                    neighbors,
                })),
            );
        }

        // v2+: read trailing tombstone block. v1 files have no
        // tombstones and we treat the set as empty.
        let mut tombstones: HashSet<u64> = HashSet::new();
        if format_ver >= 2 {
            let num_t = cur.read_u32::<LittleEndian>().map_err(io_to_xerj)? as usize;
            tombstones.reserve(num_t);
            for _ in 0..num_t {
                tombstones.insert(cur.read_u64::<LittleEndian>().map_err(io_to_xerj)?);
            }
        }

        Ok(Self {
            params,
            nodes: RwLock::new(node_map),
            entry_point: RwLock::new(if ep_raw == u64::MAX {
                None
            } else {
                Some(ep_raw)
            }),
            entry_layer: RwLock::new(entry_layer),
            tombstones: RwLock::new(tombstones),
        })
    }

    // ── Internal search primitives ────────────────────────────────────────

    /// Greedy search: descend to the single closest node at a given layer.
    fn greedy_search_layer(
        &self,
        query: &[f32],
        entry: u64,
        layer: usize,
        _ef: usize,
    ) -> (u64, f32) {
        let nodes = self.nodes.read().unwrap();
        let mut best_id = entry;
        let mut best_dist = {
            let n = nodes[&entry].read().unwrap();
            self.dist(query, &n.vector)
        };

        loop {
            let mut improved = false;
            let neighbors: Vec<u64> = {
                let n = nodes[&best_id].read().unwrap();
                if layer < n.neighbors.len() {
                    n.neighbors[layer].clone()
                } else {
                    vec![]
                }
            };

            for nid in neighbors {
                if let Some(nn) = nodes.get(&nid) {
                    let d = {
                        let nv = nn.read().unwrap();
                        self.dist(query, &nv.vector)
                    };
                    if d < best_dist {
                        best_dist = d;
                        best_id = nid;
                        improved = true;
                    }
                }
            }

            if !improved {
                break;
            }
        }

        (best_id, best_dist)
    }

    /// Beam search at a given layer, returning candidates sorted by distance.
    fn search_layer(&self, query: &[f32], entry: u64, ef: usize, layer: usize) -> Vec<(f32, u64)> {
        self.search_layer_filtered(query, entry, ef, layer, &|_| true)
    }

    fn search_layer_filtered(
        &self,
        query: &[f32],
        entry: u64,
        ef: usize,
        layer: usize,
        filter: &dyn Fn(u64) -> bool,
    ) -> Vec<(f32, u64)> {
        let nodes = self.nodes.read().unwrap();

        let entry_dist = {
            match nodes.get(&entry) {
                Some(n) => self.dist(query, &n.read().unwrap().vector),
                None => return vec![],
            }
        };

        // Min-heap of (dist, id) candidates to explore
        let mut candidates: BinaryHeap<Reverse<(ordered_float::OrderedFloat, u64)>> =
            BinaryHeap::new();
        // Max-heap of (dist, id) for the result set W
        let mut w: BinaryHeap<(ordered_float::OrderedFloat, u64)> = BinaryHeap::new();
        let mut visited: HashSet<u64> = HashSet::new();

        candidates.push(Reverse((ordered_float::OrderedFloat(entry_dist), entry)));
        if filter(entry) {
            w.push((ordered_float::OrderedFloat(entry_dist), entry));
        }
        visited.insert(entry);

        while let Some(Reverse((dist_c, c))) = candidates.pop() {
            // Pruning: if closest candidate is farther than the farthest result, stop
            if let Some(&(farthest, _)) = w.peek() {
                if dist_c > farthest && w.len() >= ef {
                    break;
                }
            }

            let neighbors: Vec<u64> = {
                match nodes.get(&c) {
                    Some(n) => {
                        let n = n.read().unwrap();
                        if layer < n.neighbors.len() {
                            n.neighbors[layer].clone()
                        } else {
                            vec![]
                        }
                    }
                    None => continue,
                }
            };

            for nid in neighbors {
                if visited.contains(&nid) {
                    continue;
                }
                visited.insert(nid);

                let d = match nodes.get(&nid) {
                    Some(nn) => self.dist(query, &nn.read().unwrap().vector),
                    None => continue,
                };

                let farthest_w = w.peek().map(|&(f, _)| f.0).unwrap_or(f32::MAX);

                if d < farthest_w || w.len() < ef {
                    candidates.push(Reverse((ordered_float::OrderedFloat(d), nid)));
                    if filter(nid) {
                        w.push((ordered_float::OrderedFloat(d), nid));
                        if w.len() > ef {
                            w.pop();
                        }
                    }
                }
            }
        }

        // Collect results sorted by distance ascending
        let mut results: Vec<(f32, u64)> = w.into_iter().map(|(d, id)| (d.0, id)).collect();
        results.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        results
    }

    /// Simple neighbor selection: take the M closest candidates.
    fn select_neighbors(
        &self,
        _query: &[f32],
        candidates: &[(f32, u64)],
        m: usize,
    ) -> Vec<(f32, u64)> {
        let mut sorted = candidates.to_vec();
        sorted.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        sorted.truncate(m);
        sorted
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
}
