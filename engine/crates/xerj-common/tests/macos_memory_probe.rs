//! On macOS the memory breaker must see the process's physical footprint, not
//! its resident set.
//!
//! The two diverge in both directions, and both hurt the breaker:
//!
//! * Pages the compressor has taken (or the pager swapped) leave the resident
//!   set but are still this process's memory. Under a 1 GiB cap a node read
//!   0.8 GB resident while it held 2.2 GB, so the breaker let it keep growing.
//! * Clean file-backed pages (mmapped segment files) count as resident but
//!   the kernel can drop them at any time, so they are not memory the process
//!   can be asked to give back.
//!
//! The first case cannot be forced from a test. The second can, and it tells
//! the two probes apart: map a file, read every page, and the resident set
//! grows by the file's size while the footprint does not move.
//!
//! One test binary of its own, so no other test allocates while it measures.

#![cfg(target_os = "macos")]

use std::io::Write as _;
use std::os::fd::AsRawFd as _;

use xerj_common::resource::current_rss_bytes;

const MIB: u64 = 1024 * 1024;
const SIZE: usize = 128 * 1024 * 1024;

fn probe() -> u64 {
    current_rss_bytes().expect("the macOS memory probe must work")
}

#[test]
fn the_memory_probe_counts_owned_memory_not_mapped_file_pages() {
    // Mapped, read, clean file pages.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("mapped.bin");
    {
        let mut f = std::fs::File::create(&path).expect("create");
        let chunk = vec![0x5au8; 1024 * 1024];
        for _ in 0..(SIZE / chunk.len()) {
            f.write_all(&chunk).expect("write");
        }
        f.sync_all().expect("sync");
    }
    let file = std::fs::File::open(&path).expect("open");
    let before_map = probe();
    // SAFETY: read-only shared mapping of a file we own, unmapped below.
    let addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            SIZE,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(addr, libc::MAP_FAILED, "mmap failed");
    let bytes = addr as *const u8;
    let mut sum = 0u64;
    for off in (0..SIZE).step_by(4096) {
        // SAFETY: `off < SIZE`, inside the mapping.
        sum += u64::from(unsafe { std::ptr::read_volatile(bytes.add(off)) });
    }
    std::hint::black_box(sum);

    // The premise: the pages really are resident, or this test proves nothing.
    let page = 16 * 1024usize;
    let mut vec = vec![0 as libc::c_char; SIZE / page];
    // SAFETY: `vec` holds one byte per page of the mapping.
    let rc = unsafe { libc::mincore(addr, SIZE, vec.as_mut_ptr()) };
    assert_eq!(rc, 0, "mincore failed");
    let resident = vec.iter().filter(|&&b| b & 1 != 0).count();
    assert!(
        resident * 10 >= vec.len() * 9,
        "only {resident} of {} mapped pages are resident",
        vec.len()
    );

    let mapped_growth = probe().saturating_sub(before_map);
    // SAFETY: the mapping created above, with its length.
    unsafe { libc::munmap(addr, SIZE) };
    assert!(
        mapped_growth < 32 * MIB,
        "probe grew {} MiB for 128 MiB of clean mapped file pages",
        mapped_growth / MIB
    );

    // Positive control: memory the process owns does move the probe.
    let before_heap = probe();
    let heap: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    std::hint::black_box(&heap);
    let heap_growth = probe().saturating_sub(before_heap);
    assert!(
        heap_growth >= 96 * MIB,
        "probe grew only {} MiB for 128 MiB of touched heap",
        heap_growth / MIB
    );
}
