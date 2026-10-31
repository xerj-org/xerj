//! #2427: loading an HNSW graph must not hold a copy of the whole file next
//! to the graph being built. Before the fix `load_from` started with
//! `fs::read`, so a node restarting on a 0.93 GB graph peaked ~1 GB above
//! its steady state.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use xerj_vector::{DistanceMetric, HnswIndex, HnswParams};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now =
                LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed) + layout.size() as isize;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            let delta = new_size as isize - layout.size() as isize;
            let now = LIVE.fetch_add(delta, Ordering::Relaxed) + delta;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn load_from_does_not_buffer_the_whole_file() {
    let dim = 128;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.bin");
    {
        let index = HnswIndex::new(HnswParams::new(dim, DistanceMetric::Cosine));
        for id in 0..12_000u64 {
            let v: Vec<f32> = (0..dim as u64)
                .map(|d| ((id.wrapping_mul(2654435761) ^ (d * 40503)) % 1009) as f32)
                .collect();
            index.insert(id, v).unwrap();
        }
        index.save_to(&path).unwrap();
    }
    let file_len = std::fs::metadata(&path).unwrap().len() as isize;
    assert!(file_len > 6 << 20, "graph too small to tell: {file_len} B");

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let loaded = HnswIndex::load_from(&path).unwrap();
    let peak = PEAK.load(Ordering::Relaxed) - base;
    let retained = LIVE.load(Ordering::Relaxed) - base;
    assert_eq!(loaded.len(), 12_000);

    // What the load needs beyond the graph it returns: the neighbour lists
    // keyed by external id (pass 1) plus a 1 MiB read buffer. Reading the
    // file first added the whole file on top.
    let transient = peak - retained;
    assert!(
        transient < file_len / 2,
        "load_from peaked {transient} B above the {retained} B graph for a {file_len} B file"
    );
}
