//! Issue #199 regression: an ILM retention policy is **executed**, not just
//! stored.
//!
//! Before the fix, `Engine.ilm_policies` was a write-only `DashMap`: the API
//! accepted a policy, `GET` echoed it back, and no code path anywhere read it.
//! A user who configured 30-day retention got an index that grew forever, with
//! no signal that nothing was running.
//!
//! These tests would all have failed at that commit — `run_ilm_once` did not
//! exist, and no amount of waiting deleted anything.
//!
//! The clock is injected (`run_ilm_once(now_ms)`) rather than slept on, so a
//! 30-day phase transition is exercised deterministically in microseconds.

use serde_json::json;
use tempfile::TempDir;
use xerj_common::config::Config;
use xerj_common::types::Schema;
use xerj_engine::engine::IndexTemplate;
use xerj_engine::ilm::now_ms;
use xerj_engine::Engine;

const DAY_MS: i64 = 86_400_000;

fn make_engine(dir: &TempDir) -> Engine {
    let mut config = Config::default();
    config.server.data_dir = dir.path().to_str().unwrap().to_string();
    Engine::new(config).expect("engine::new")
}

fn thirty_day_delete_policy() -> serde_json::Value {
    json!({
        "phases": {
            "delete": { "min_age": "30d", "actions": { "delete": {} } }
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_phase_deletes_the_index_once_min_age_elapses() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "logs-000001",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");

    let created = now_ms();

    // Day 0 and day 29: the policy is live, and it must NOT fire. Deleting
    // early is the one failure worse than not deleting at all.
    for age_days in [0, 29] {
        let report = engine.run_ilm_once(created + age_days * DAY_MS).await;
        assert_eq!(
            report.evaluated, 1,
            "the index is managed at day {age_days}"
        );
        assert!(
            report.deleted.is_empty(),
            "nothing may be deleted at day {age_days}: {report:?}"
        );
        assert!(
            engine.get_index("logs-000001").is_ok(),
            "index still exists at day {age_days}"
        );
    }

    // Day 31: past min_age — the delete phase fires.
    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec!["logs-000001".to_string()],
        "delete phase deletes the index: {report:?}"
    );
    assert!(
        engine.get_index("logs-000001").is_err(),
        "index is gone from the engine"
    );
    assert!(
        !dir.path().join("logs-000001").exists(),
        "the index's data directory is gone from disk, not just its handle"
    );

    // A second pass is a no-op, not an error.
    let report = engine.run_ilm_once(created + 32 * DAY_MS).await;
    assert_eq!(report.evaluated, 0);
    assert!(report.deleted.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unmanaged_index_is_never_touched() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index("no-policy-here", Schema::empty())
        .expect("create index");

    let report = engine.run_ilm_once(now_ms() + 3650 * DAY_MS).await;
    assert_eq!(report.evaluated, 0, "no policy attached → not evaluated");
    assert!(report.deleted.is_empty(), "{report:?}");
    assert!(engine.get_index("no-policy-here").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readonly_phase_sets_the_write_block_and_stops_there() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy(
        "warm-7d",
        json!({
            "phases": { "warm": { "min_age": "7d", "actions": { "readonly": {} } } }
        }),
    );
    engine
        .create_index_with_settings(
            "app-logs",
            Schema::empty(),
            json!({ "index": { "lifecycle": { "name": "warm-7d" } } }),
        )
        .expect("create index");
    let created = now_ms();

    let idx = engine.get_index("app-logs").expect("index");
    assert!(!idx.is_write_blocked().await, "writable before the phase");

    let report = engine.run_ilm_once(created + 8 * DAY_MS).await;
    assert_eq!(report.read_only, vec!["app-logs".to_string()], "{report:?}");
    assert!(
        engine
            .get_index("app-logs")
            .expect("index")
            .is_write_blocked()
            .await,
        "readonly action actually set the write block"
    );
    // No delete phase in this policy → the index survives forever.
    let report = engine.run_ilm_once(created + 3650 * DAY_MS).await;
    assert!(report.deleted.is_empty(), "{report:?}");
    assert!(engine.get_index("app-logs").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_ilm_stops_deletion() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "logs-000002",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");
    let created = now_ms();

    engine.set_ilm_running(false);
    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert!(report.deleted.is_empty(), "stopped ILM deletes nothing");
    assert!(engine.get_index("logs-000002").is_ok());
    assert_eq!(engine.ilm_status()["operation_mode"], "STOPPED");

    engine.set_ilm_running(true);
    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert_eq!(report.deleted, vec!["logs-000002".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn internal_indices_are_never_deleted_by_a_wildcard_policy() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("everything-7d", {
        json!({ "phases": { "delete": { "min_age": "7d", "actions": { "delete": {} } } } })
    });
    // A second brain, swept up by a policy attached to it directly — the
    // worst case, since a wildcard index template would do exactly this.
    engine
        .create_index_with_settings(
            ".xerj-memory-alice-edges",
            Schema::empty(),
            json!({ "index.lifecycle.name": "everything-7d" }),
        )
        .expect("create index");

    let report = engine.run_ilm_once(now_ms() + 30 * DAY_MS).await;
    assert!(
        report.deleted.is_empty(),
        "a dot-prefixed internal index is never ILM-deleted: {report:?}"
    );
    assert!(engine.get_index(".xerj-memory-alice-edges").is_ok());
    assert!(
        report
            .skipped
            .iter()
            .any(|(i, r)| i == ".xerj-memory-alice-edges" && r.contains("dot-prefixed")),
        "and the refusal is reported, not silent: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_template_manages_indices_created_after_it_and_only_those() {
    // The one way a retention executor can destroy data ES would have kept:
    // resolving the policy from an index *template* at evaluation time rather
    // than at creation time. Write `logs-*` with a 7-day delete phase today
    // and every `logs-*` index created last year is suddenly past its min_age.
    // ES applies template settings when the index is created and never
    // retroactively, so the pre-existing index must stay untouched.
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy(
        "logs-7d",
        json!({ "phases": { "delete": { "min_age": "7d", "actions": { "delete": {} } } } }),
    );
    engine
        .create_index("logs-from-last-year", Schema::empty())
        .expect("create index");

    // Only now does the template appear.
    engine.templates.insert(
        "logs".to_string(),
        IndexTemplate {
            index_patterns: vec!["logs-*".to_string()],
            settings: json!({ "index.lifecycle.name": "logs-7d" }),
            mappings: json!({}),
            priority: 100,
        },
    );
    engine
        .create_index("logs-from-today", Schema::empty())
        .expect("create index");
    let created = now_ms();

    let report = engine.run_ilm_once(created + 30 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec!["logs-from-today".to_string()],
        "the index created under the template is managed, the older one is not: {report:?}"
    );
    assert!(
        engine.get_index("logs-from-last-year").is_ok(),
        "a template written today must not retroactively delete yesterday's index"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retention_survives_a_restart() {
    // The half of #199 that only shows up the next morning: policies and
    // lifecycle attachments lived in memory, so even a working executor would
    // have silently stopped managing every index after a reboot.
    let dir = TempDir::new().unwrap();
    let created;
    {
        let engine = make_engine(&dir);
        engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
        engine
            .create_index_with_settings(
                "logs-000003",
                Schema::empty(),
                json!({ "index.lifecycle.name": "logs-30d" }),
            )
            .expect("create index");
        created = now_ms();
        engine.flush_index("logs-000003").await.expect("flush");
    } // engine dropped → node lock released

    let engine = make_engine(&dir);
    assert!(
        engine.ilm_policies.get("logs-30d").is_some(),
        "the policy came back from ilm_state.json"
    );
    assert_eq!(
        engine.ilm_policy_for_index("logs-000003").await.as_deref(),
        Some("logs-30d"),
        "the attachment came back too"
    );

    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec!["logs-000003".to_string()],
        "retention resumes after a restart: {report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_reports_the_next_phase_before_it_fires() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "logs-000004",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");
    let created = now_ms();

    let explained = engine
        .ilm_explain("logs-000004", created + 10 * DAY_MS)
        .await;
    assert_eq!(explained["managed"], true);
    assert_eq!(explained["policy"], "logs-30d");
    assert_eq!(explained["xerj"]["executable"], true);
    assert_eq!(explained["xerj"]["next_phase"], "delete");
    let due = explained["xerj"]["next_phase_due_at_millis"]
        .as_i64()
        .expect("due date");
    assert!(
        (due - (created + 30 * DAY_MS)).abs() < 5_000,
        "delete is due 30 days after creation, got {due}"
    );

    let unmanaged = engine.ilm_explain("nope", created).await;
    assert_eq!(unmanaged["managed"], false);
}

// ─────────────────────────────────────────────────────────────────────────────
// Repairs found by adversarial review of the first cut of this change.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_detached_index_is_never_deleted() {
    // The defect this test exists for: `PUT /{index}/_settings
    // {"index.lifecycle.name": null}` — ES's documented way to stop managing
    // an index, and the only one this engine offers — was acknowledged and
    // then ignored. Detach cleared the in-memory `ilm_index_state` entry, but
    // the resolver fell back to the index's persisted `settings.json`, which
    // still carried the create-time `index.lifecycle.name`. `_ilm/explain`
    // kept reporting `managed: true`, and the next pass deleted the index.
    //
    // An acknowledged "stop deleting my data" that deletes the data anyway is
    // the exact failure class this whole module exists to remove.
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "logs-000009",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");
    let created = now_ms();
    assert_eq!(
        engine.ilm_policy_for_index("logs-000009").await.as_deref(),
        Some("logs-30d"),
        "managed before the detach"
    );

    engine.set_index_lifecycle_policy("logs-000009", None);

    assert_eq!(
        engine.ilm_policy_for_index("logs-000009").await,
        None,
        "the detach is authoritative — settings.json must not resurrect the policy"
    );
    assert_eq!(
        engine.ilm_explain("logs-000009", created).await["managed"],
        false,
        "and _ilm/explain agrees, so the operator is not told a comforting lie"
    );

    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert_eq!(report.evaluated, 0, "a detached index is not evaluated");
    assert!(
        report.deleted.is_empty(),
        "a detached index is never deleted: {report:?}"
    );
    assert!(
        engine.get_index("logs-000009").is_ok(),
        "the index is still here 31 days later"
    );
    assert!(
        dir.path().join("logs-000009").exists(),
        "and so is its data on disk"
    );

    // Re-attaching still works, and ages from the original creation time
    // rather than from the moment of re-attach.
    engine.set_index_lifecycle_policy("logs-000009", Some("logs-30d"));
    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec!["logs-000009".to_string()],
        "{report:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detaching_an_index_with_no_recorded_state_is_honoured() {
    // The upgrade shape, and the second half of the same defect: an index
    // whose only record of its policy is the persisted `settings.json` (no
    // `ilm_state.json` entry, because it predates this feature). Detaching it
    // used to be a *completely silent no-op* — there was no entry to remove,
    // so nothing was written, the settings fallback still resolved the policy,
    // and the next pass deleted it.
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "legacy-logs",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");
    let created = now_ms();
    // Simulate the pre-#199 node: settings.json says logs-30d, nothing else does.
    engine.ilm_index_state.remove("legacy-logs");
    assert_eq!(
        engine.ilm_policy_for_index("legacy-logs").await.as_deref(),
        Some("logs-30d"),
        "the settings fallback is what makes an upgraded index managed at all"
    );

    engine.set_index_lifecycle_policy("legacy-logs", None);

    assert_eq!(
        engine.ilm_policy_for_index("legacy-logs").await,
        None,
        "the detach records a tombstone even with nothing to remove"
    );
    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert!(report.deleted.is_empty(), "{report:?}");
    assert!(engine.get_index("legacy-logs").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_detach_survives_a_restart() {
    // A tombstone that evaporates on reboot is not a detach, it is a delay.
    let dir = TempDir::new().unwrap();
    let created;
    {
        let engine = make_engine(&dir);
        engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
        engine
            .create_index_with_settings(
                "logs-000010",
                Schema::empty(),
                json!({ "index.lifecycle.name": "logs-30d" }),
            )
            .expect("create index");
        created = now_ms();
        engine.set_index_lifecycle_policy("logs-000010", None);
        engine.flush_index("logs-000010").await.expect("flush");
    } // engine dropped → node lock released

    let engine = make_engine(&dir);
    assert_eq!(
        engine.ilm_policy_for_index("logs-000010").await,
        None,
        "the detach came back from ilm_state.json"
    );
    let report = engine.run_ilm_once(created + 31 * DAY_MS).await;
    assert!(
        report.deleted.is_empty(),
        "still detached after a restart: {report:?}"
    );
    assert!(engine.get_index("logs-000010").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_detached_index_does_not_block_deleting_its_former_policy() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "logs-000011",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");

    assert_eq!(
        engine.ilm_indices_using_policy("logs-30d").await,
        vec!["logs-000011".to_string()],
        "in use while attached"
    );
    engine.set_index_lifecycle_policy("logs-000011", None);
    assert!(
        engine.ilm_indices_using_policy("logs-30d").await.is_empty(),
        "and not in use once detached"
    );
    assert_eq!(
        engine.ilm_status()["xerj"]["managed_indices"],
        0,
        "a tombstone is not a managed index"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upgraded_index_still_counts_as_using_its_policy() {
    // The in-use check must ask the same resolver the executor asks. An index
    // that points at the policy only through its persisted settings is
    // genuinely managed, so deleting the policy out from under it would leave
    // the executor skipping it every pass with "policy not found" — retention
    // silently stopped, which is the state this work exists to remove.
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);
    engine.put_ilm_policy("logs-30d", thirty_day_delete_policy());
    engine
        .create_index_with_settings(
            "legacy-logs-2",
            Schema::empty(),
            json!({ "index.lifecycle.name": "logs-30d" }),
        )
        .expect("create index");
    engine.ilm_index_state.remove("legacy-logs-2");

    assert_eq!(
        engine.ilm_indices_using_policy("logs-30d").await,
        vec!["legacy-logs-2".to_string()],
        "resolved through settings, exactly as the executor resolves it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_data_stream_created_under_an_ilm_template_is_actually_managed() {
    // Backing indices are named `.ds-<stream>-NNNNNN`, so matching templates
    // against the literal backing-index name never matches the `applogs*`
    // pattern the user wrote. The stream came out silently unmanaged:
    // `evaluated: 0`, no error, no warning, retention simply never happened —
    // in the shape ES users reach ILM through most often.
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy(
        "stream-7d",
        json!({ "phases": { "delete": { "min_age": "7d", "actions": { "delete": {} } } } }),
    );
    engine.templates.insert(
        "applogs".to_string(),
        IndexTemplate {
            index_patterns: vec!["applogs*".to_string()],
            settings: json!({ "index.lifecycle.name": "stream-7d" }),
            mappings: json!({}),
            priority: 100,
        },
    );

    engine.create_data_stream("applogs").expect("create stream");
    let created = now_ms();
    assert_eq!(
        engine
            .ilm_policy_for_index(".ds-applogs-000001")
            .await
            .as_deref(),
        Some("stream-7d"),
        "the template is resolved from the stream name, not the backing name"
    );

    // The sole backing index is the stream's *write* index, so the safety rail
    // keeps it — retention must not delete the index documents land in.
    let report = engine.run_ilm_once(created + 30 * DAY_MS).await;
    assert_eq!(report.evaluated, 1, "it is evaluated now: {report:?}");
    assert!(report.deleted.is_empty(), "{report:?}");
    assert!(
        report
            .skipped
            .iter()
            .any(|(i, r)| i == ".ds-applogs-000001" && r.contains("write index")),
        "and the refusal names the reason: {report:?}"
    );

    // After a rollover the old generation is no longer the write index, so it
    // ages out — which is the entire point of ILM on a data stream.
    engine.rollover_data_stream("applogs").expect("rollover");
    assert_eq!(
        engine
            .ilm_policy_for_index(".ds-applogs-000002")
            .await
            .as_deref(),
        Some("stream-7d"),
        "the new generation is managed too"
    );
    let report = engine.run_ilm_once(created + 30 * DAY_MS).await;
    assert_eq!(
        report.deleted,
        vec![".ds-applogs-000001".to_string()],
        "the rolled-over generation is deleted, the write index is not: {report:?}"
    );
    assert!(engine.get_index(".ds-applogs-000002").is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_data_stream_forgets_its_ilm_bookkeeping() {
    // `delete_data_stream` bypasses `delete_index`, so it has to drop the ILM
    // state itself. It did not, and the state outlived the index: status kept
    // reporting a managed index that no longer existed, and the policy became
    // permanently undeletable, refused in the name of a phantom.
    let dir = TempDir::new().unwrap();
    let engine = make_engine(&dir);

    engine.put_ilm_policy(
        "stream-7d",
        json!({ "phases": { "delete": { "min_age": "7d", "actions": { "delete": {} } } } }),
    );
    engine.templates.insert(
        "applogs".to_string(),
        IndexTemplate {
            index_patterns: vec!["applogs*".to_string()],
            settings: json!({ "index.lifecycle.name": "stream-7d" }),
            mappings: json!({}),
            priority: 100,
        },
    );
    engine.create_data_stream("applogs").expect("create stream");
    assert_eq!(engine.ilm_status()["xerj"]["managed_indices"], 1);

    engine.delete_data_stream("applogs").await.expect("delete");

    assert_eq!(
        engine.ilm_status()["xerj"]["managed_indices"],
        0,
        "no ILM state survives an index that no longer exists"
    );
    assert!(
        engine
            .ilm_indices_using_policy("stream-7d")
            .await
            .is_empty(),
        "so the policy is deletable again rather than pinned by a phantom"
    );
}
