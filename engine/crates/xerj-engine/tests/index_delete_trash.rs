//! Issue #3274: `DELETE /<index>` returned `500 store_exception` while the
//! engine (or the filesystem) still held the index's files — after a client
//! was killed mid-bulk, and on an NFS data dir where unlinked mmap'd segments
//! become `.nfs*` files that block `rmdir`. Only a restart freed the name.
//!
//! The delete now unregisters the index, waits a bounded time for other
//! handles, renames the directory into `<data_dir>/_trash` and removes it
//! from there, retrying in the background and at boot when it cannot.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::index_trash::{trash_root, INDEX_TRASH_DIR};
use xerj_engine::Engine;

fn config_for(dir: &TempDir) -> Config {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config
}

fn trash_entries(dir: &TempDir) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(trash_root(dir.path()))
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

async fn wait_for_empty_trash(dir: &TempDir) {
    for _ in 0..400 {
        if trash_entries(dir).is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("trash never emptied: {:?}", trash_entries(dir));
}

async fn assert_recreated_empty(engine: &Engine, name: &str) {
    engine
        .create_index(name, Schema::empty())
        .expect("the deleted name must be re-creatable at once");
    let idx = engine.get_index(name).unwrap();
    assert_eq!(idx.live_doc_count(), 0, "a re-create must see no leftovers");
    engine.flush_index(name).await.unwrap();
    assert_eq!(idx.live_doc_count(), 0, "not even after a flush");
}

/// The killed-client case: a bulk is cut off mid-run, then the index is
/// deleted. 200, gone from the listing, and re-creatable with no leftovers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delete_after_a_bulk_was_cut_off_mid_run_succeeds() {
    let dir = TempDir::new().unwrap();
    let engine = Engine::new(config_for(&dir)).unwrap();
    engine.create_index("bulk", Schema::empty()).unwrap();

    let writer = {
        let engine = engine.clone();
        tokio::spawn(async move {
            let idx = engine.get_index("bulk").unwrap();
            for i in 0u64.. {
                let body =
                    serde_json::json!({"path": format!("src/f{i}.rs"), "body": "fn main() {}"});
                if idx.index_document(Some(i.to_string()), body).await.is_err() {
                    return;
                }
                if i % 500 == 499 {
                    let _ = idx.flush().await;
                }
            }
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    writer.abort(); // the client dies mid-run
    let _ = writer.await;

    engine
        .delete_index("bulk")
        .await
        .expect("delete after a cut-off bulk must succeed");
    assert!(engine.get_index("bulk").is_err(), "404 to new requests");
    assert!(engine.list_indices().await.iter().all(|i| i.name != "bulk"));
    assert!(!dir.path().join("bulk").exists());
    wait_for_empty_trash(&dir).await;
    assert_recreated_empty(&engine, "bulk").await;
}

/// A writer that is still running holds the index handle through the whole
/// delete. The delete must not wait on it forever, must not fail, and the
/// re-created index must not inherit what the straggler wrote afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_still_running_does_not_block_delete_or_leak_into_a_recreate() {
    let dir = TempDir::new().unwrap();
    let engine = Engine::new(config_for(&dir)).unwrap();
    engine.create_index("busy", Schema::empty()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let idx = engine.get_index("busy").unwrap();
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let _ = idx
                    .index_document(Some(i.to_string()), serde_json::json!({"n": i}))
                    .await;
                if i % 200 == 199 {
                    let _ = idx.flush().await;
                }
                i += 1;
            }
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    engine
        .delete_index("busy")
        .await
        .expect("a live writer must not turn the delete into a 500");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the wait for other handles is bounded: {:?}",
        started.elapsed()
    );
    assert!(engine.get_index("busy").is_err());
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();

    assert_recreated_empty(&engine, "busy").await;
}

/// A reader holding the index (and its mapped segments) open does not block
/// the delete; the index is gone at once and nothing is left once the
/// handle drops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_open_reader_does_not_block_delete() {
    let dir = TempDir::new().unwrap();
    let engine = Engine::new(config_for(&dir)).unwrap();
    engine.create_index("held", Schema::empty()).unwrap();
    let idx = engine.get_index("held").unwrap();
    for i in 0..50u32 {
        idx.index_document(Some(i.to_string()), serde_json::json!({"t": "hello world"}))
            .await
            .unwrap();
    }
    engine.flush_index("held").await.unwrap();
    let req = xerj_query::parse_request(&serde_json::json!({"query": {"match": {"t": "hello"}}}))
        .unwrap();
    assert_eq!(idx.search(&req).await.unwrap().total.value, 50);

    engine
        .delete_index("held")
        .await
        .expect("a held reader must not block the delete");
    assert!(engine.list_indices().await.iter().all(|i| i.name != "held"));
    assert!(
        !dir.path().join("held").exists(),
        "the name is free on disk"
    );
    drop(idx);
    wait_for_empty_trash(&dir).await;
    assert_recreated_empty(&engine, "held").await;
}

/// The NFS shape: a file that cannot be removed yet. The delete still
/// succeeds, the bytes wait in the trash, and the next boot sweeps them.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unremovable_file_waits_in_the_trash_and_boot_sweeps_it() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    {
        let engine = Engine::new(config_for(&dir)).unwrap();
        engine.create_index("nfs", Schema::empty()).unwrap();
        let idx = engine.get_index("nfs").unwrap();
        idx.index_document(Some("1".into()), serde_json::json!({"t": "x"}))
            .await
            .unwrap();
        engine.flush_index("nfs").await.unwrap();
        drop(idx);

        // Stand-in for a `.nfs*` silly-rename: a subdirectory whose entry
        // cannot be unlinked.
        let held = dir.path().join("nfs").join("held");
        std::fs::create_dir_all(&held).unwrap();
        std::fs::write(held.join(".nfs0000000000001"), b"mapped").unwrap();
        std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::write(held.join(".probe"), b"x").is_ok() {
            std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("skipping: directory permissions are not enforced for this user");
            return;
        }

        engine
            .delete_index("nfs")
            .await
            .expect("an unremovable file must not fail the delete");
        assert!(engine.get_index("nfs").is_err());
        assert!(!dir.path().join("nfs").exists());
        let trashed = trash_entries(&dir);
        assert_eq!(trashed.len(), 1, "the bytes wait in the trash: {trashed:?}");
        assert!(
            trashed[0].starts_with(dir.path().join(INDEX_TRASH_DIR)),
            "{trashed:?}"
        );
        assert_recreated_empty(&engine, "nfs").await;

        // The holder lets go (the last mmap is dropped).
        std::fs::set_permissions(
            trashed[0].join("held"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    // Reboot: the sweep (or the reaper, if it got there first) leaves
    // nothing behind, and the re-created index is still there.
    let engine = Engine::new(config_for(&dir)).unwrap();
    assert!(trash_entries(&dir).is_empty(), "{:?}", trash_entries(&dir));
    assert!(engine.get_index("nfs").is_ok());
}
