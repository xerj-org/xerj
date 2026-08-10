//! Issue #206: a failed index is a **state**, not a dead entry.
//!
//! Before this, an index whose directory refused to open at boot was recorded
//! in a private map that only `Engine::health` ever read. Live-reproduced at
//! `12d5cc32` with a corrupt `snapshot.json` (the storage layer's documented
//! "present but unparseable manifest" refusal):
//!
//! ```text
//! failed_indices = [("broken", "storage error: … could not be parsed …")]
//! indices        = []                                   ← invisible
//! health         = "red"
//! delete         = Some(IndexNotFound { name: "broken" }) ← undeletable
//! ```
//!
//! So the index could not be listed, could not be deleted, and could not be
//! retried: the only lever was stopping the server and editing the data
//! directory by hand. Every assertion below fails on that build.
//!
//! The corruption used here is deliberate and realistic — `IndexStore::open`
//! refuses a manifest it cannot parse rather than treating it as empty,
//! because treating it as empty would orphan and then delete every segment.
//! That refusal is correct; having no way to act on it afterwards was not.

use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_common::XerjError;
use xerj_engine::{Engine, EngineError};

fn config_for(dir: &TempDir) -> Config {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    config
}

/// Build a data dir holding `names`, each with one flushed document, then
/// corrupt `break_me`'s manifest so the next boot refuses to open it.
/// Returns the good manifest bytes so a test can "fix the cause" and retry.
async fn data_dir_with_one_broken_index(names: &[&str], break_me: &str) -> (TempDir, Vec<u8>) {
    let dir = TempDir::new().unwrap();
    {
        let engine = Engine::new(config_for(&dir)).expect("first boot");
        for name in names {
            engine.create_index(name, Schema::empty()).unwrap();
            let idx = engine.get_index(name).unwrap();
            idx.index_document(Some("1".to_string()), serde_json::json!({"t": "hello"}))
                .await
                .unwrap();
            engine.flush_index(name).await.unwrap();
        }
    }
    let manifest = dir.path().join(break_me).join("snapshot.json");
    let good = std::fs::read(&manifest).expect("a flushed index must have a manifest");
    std::fs::write(&manifest, b"{not json").unwrap();
    (dir, good)
}

fn xerj_error(e: EngineError) -> XerjError {
    e.into()
}

/// `Index` is not `Debug`, so `unwrap_err()` is unavailable on the
/// index-returning calls. Take the error explicitly instead.
fn err_of<T>(r: Result<T, EngineError>, what: &str) -> XerjError {
    match r {
        Ok(_) => panic!("{what} must not succeed"),
        Err(e) => xerj_error(e),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_index_is_listed_with_its_reason() {
    let (dir, _good) = data_dir_with_one_broken_index(&["broken", "healthy"], "broken").await;
    let engine = Engine::new(config_for(&dir)).expect("boot with one broken index");

    let failed = engine.list_failed_indices();
    assert_eq!(
        failed.len(),
        1,
        "expected exactly one failed index: {failed:?}"
    );
    let f = &failed[0];
    assert_eq!(f.name, "broken");
    assert!(
        f.reason.contains("snapshot.json") && f.reason.contains("could not be parsed"),
        "the reason must be the verbatim open error, not a summary: {}",
        f.reason
    );
    assert!(f.failed_at_ms > 0, "a failed index records when it failed");
    assert_eq!(f.retries, 0, "no retry has been attempted yet");

    // The healthy index in the same data dir is unaffected — one broken index
    // must not cost the node the others.
    assert_eq!(engine.list_indices().await.len(), 1);
    assert!(engine.get_index("healthy").is_ok());
    assert_eq!(engine.health().await.status, "red");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_and_writes_to_a_failed_index_say_unavailable_not_not_found() {
    let (dir, _good) = data_dir_with_one_broken_index(&["broken"], "broken").await;
    let engine = Engine::new(config_for(&dir)).expect("boot");

    // `index_not_found` (404) was a lie: the name is taken and the bytes are
    // on disk. It is now 503 `no_shard_available_action_exception` carrying
    // the open error.
    let e = err_of(engine.get_index("broken"), "get_index on a failed index");
    assert!(
        matches!(e, XerjError::IndexUnavailable { .. }),
        "get_index on a failed index must not report not-found: {e:?}"
    );
    assert_eq!(e.http_status(), 503);
    assert!(e.to_string().contains("snapshot.json"), "{e}");

    // Auto-create must not run over a failed index — that would either destroy
    // recoverable segments or fail deep in the store with an opaque message.
    let e = err_of(
        engine.get_or_create_index("broken"),
        "auto-create over a failed index",
    );
    assert!(
        matches!(e, XerjError::IndexUnavailable { .. }),
        "auto-create must refuse a failed index: {e:?}"
    );
    // …and the refusal must not have created anything.
    assert!(engine.list_indices().await.is_empty());
    assert_eq!(engine.list_failed_indices().len(), 1);

    // Explicit create is refused for the same reason, with the same message.
    let e = xerj_error(engine.create_index("broken", Schema::empty()).unwrap_err());
    assert!(
        matches!(e, XerjError::IndexUnavailable { .. }),
        "create over a failed index must name the real cause: {e:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_index_can_be_deleted_without_a_restart() {
    let (dir, _good) = data_dir_with_one_broken_index(&["broken", "healthy"], "broken").await;
    let engine = Engine::new(config_for(&dir)).expect("boot");
    let index_dir = dir.path().join("broken");
    assert!(index_dir.exists());

    engine
        .delete_index("broken")
        .await
        .expect("a failed index must be deletable");

    assert!(!index_dir.exists(), "delete must remove the bytes on disk");
    assert!(engine.list_failed_indices().is_empty());
    // Health recovers without a restart, and the name is reusable.
    assert_ne!(engine.health().await.status, "red");
    engine
        .create_index("broken", Schema::empty())
        .expect("the freed name must be reusable");

    // A name that was never an index is still a plain 404.
    let e = xerj_error(engine.delete_index("never-existed").await.unwrap_err());
    assert!(matches!(e, XerjError::IndexNotFound { .. }), "{e:?}");
}

/// The failed-index arm of `delete_index` drops its bookkeeping only after the
/// bytes are gone. The **open**-index arm did the opposite: the handle was
/// pulled out of `Engine::indices` before `delete_all_data`, so a removal that
/// failed left the name freed and the directory alive — no handle, no
/// `failed_indices` entry, nothing on `_cat/indices`, `DELETE` answering 404
/// and none of the three recovery levers able to name it. That is issue #206's
/// stuck state reached from the other side, and it only became
/// operator-visible once `DELETE` started propagating the error instead of
/// dropping it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_delete_that_failed_leaves_the_index_addressable() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, good) = data_dir_with_one_broken_index(&["keeper"], "keeper").await;
    // Undo the corruption — this test is about a healthy, open index whose
    // *bytes* refuse to go away, not about a failed one.
    std::fs::write(dir.path().join("keeper").join("snapshot.json"), &good).unwrap();
    let engine = Engine::new(config_for(&dir)).expect("boot");
    assert!(engine.get_index("keeper").is_ok());

    // Make the removal fail: with the index directory read-only its entries
    // cannot be unlinked, so `remove_dir_all` returns EACCES.
    let index_dir = dir.path().join("keeper");
    let original = std::fs::metadata(&index_dir).unwrap().permissions();
    std::fs::set_permissions(&index_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    // Skip rather than lie if the platform/user does not enforce the mode
    // (running as root, or an fs that ignores it) — the assertion below would
    // be vacuous there.
    let enforced = std::fs::write(index_dir.join(".perm-probe"), b"x").is_err();
    if !enforced {
        let _ = std::fs::remove_file(index_dir.join(".perm-probe"));
        std::fs::set_permissions(&index_dir, original).unwrap();
        eprintln!("skipping: directory permissions are not enforced for this user");
        return;
    }

    let err = engine
        .delete_index("keeper")
        .await
        .expect_err("a delete whose bytes cannot be removed must fail");
    assert!(
        index_dir.exists(),
        "the directory survived the failed delete"
    );

    // The point of the test: the name did not become a dead end.
    assert!(
        engine.get_index("keeper").is_ok(),
        "a delete that did not happen must leave the index addressable ({err})"
    );
    assert!(
        engine
            .list_indices()
            .await
            .iter()
            .any(|i| i.name == "keeper"),
        "and still enumerable, or no surface can show it"
    );

    // Once the cause is fixed the operator's retry works — no restart.
    std::fs::set_permissions(&index_dir, original).unwrap();
    engine
        .delete_index("keeper")
        .await
        .expect("retrying the delete after fixing the cause must work");
    assert!(!index_dir.exists());
    assert!(engine.list_indices().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retry_reports_the_live_reason_and_succeeds_once_the_cause_is_fixed() {
    let (dir, good) = data_dir_with_one_broken_index(&["broken"], "broken").await;
    let engine = Engine::new(config_for(&dir)).expect("boot");

    // Retry while still broken: the error is returned, not swallowed, and the
    // attempt is counted so a flapping directory is visible as one.
    let e = xerj_error(engine.retry_failed_index("broken").unwrap_err());
    assert!(
        matches!(e, XerjError::IndexUnavailable { .. }),
        "a retry that did not work must fail loudly: {e:?}"
    );
    let after = engine.list_failed_indices();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].retries, 1, "the retry must be counted");

    // Operator fixes the cause (here: restores the manifest from the backup
    // the storage error told them to look for) and retries again.
    std::fs::write(dir.path().join("broken").join("snapshot.json"), &good).unwrap();
    engine
        .retry_failed_index("broken")
        .expect("retry after the fix must reopen the index");

    assert!(engine.list_failed_indices().is_empty());
    assert_eq!(engine.list_indices().await.len(), 1);
    assert_ne!(engine.health().await.status, "red");
    // The index actually serves: the document survived the whole round trip.
    let idx = engine.get_index("broken").expect("reopened index");
    assert_eq!(idx.stats().await.doc_count, 1);

    // Retrying a name that is not a failed index is a 404, not a silent ok.
    let e = xerj_error(engine.retry_failed_index("broken").unwrap_err());
    assert!(matches!(e, XerjError::IndexNotFound { .. }), "{e:?}");
}
