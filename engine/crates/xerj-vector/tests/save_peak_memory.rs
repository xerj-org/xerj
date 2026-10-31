//! #2427: saving an HNSW graph must not hold a copy of the whole file in
//! memory. Before the fix `save_to` encoded into a `Vec` that grew by
//! doubling, so every flush (including the memory-pressure drain) added one
//! to two graph sizes to RSS.

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
fn save_to_does_not_buffer_the_whole_graph() {
    let dim = 128;
    let index = HnswIndex::new(HnswParams::new(dim, DistanceMetric::Cosine));
    for id in 0..12_000u64 {
        let v: Vec<f32> = (0..dim as u64)
            .map(|d| ((id.wrapping_mul(2654435761) ^ (d * 40503)) % 1009) as f32)
            .collect();
        index.insert(id, v).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("graph.bin");

    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    index.save_to(&path).unwrap();
    let extra = PEAK.load(Ordering::Relaxed) - base;

    let file_len = std::fs::metadata(&path).unwrap().len() as isize;
    assert!(file_len > 6 << 20, "graph too small to tell: {file_len} B");
    // Streaming needs a fixed ~1 MiB write buffer; the old encoder needed
    // at least the file size (and up to twice it while the Vec doubled).
    assert!(
        extra < file_len / 4,
        "save_to allocated {extra} B transient for a {file_len} B graph"
    );
}
