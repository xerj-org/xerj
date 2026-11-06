//! End-to-end against a real Postgres. Skipped unless `XERJ_TEST_PG_URL`
//! points at a database with `wal_level=logical`, a role that may replicate,
//! and its password in `XERJ_TEST_PG_PASSWORD`, for example:
//!
//! ```text
//! docker run -d -p 127.0.0.1:55432:5432 -e POSTGRES_USER=app -e POSTGRES_PASSWORD=pw \
//!   -e POSTGRES_DB=shop postgres:16-alpine -c wal_level=logical
//! XERJ_TEST_PG_URL=postgres://app@127.0.0.1:55432/shop?sslmode=disable \
//! XERJ_TEST_PG_PASSWORD=pw cargo test -p xerj-sources --test pg_live
//! ```
//!
//! The sink is an in-memory index, so the test checks exactly what the node
//! would receive: snapshot rows, then inserts, updates (including a primary
//! key change and an unchanged TOAST column), deletes and a truncate, then a
//! restart that resumes from the acknowledged LSN without loss or duplicates.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use xerj_sources::pg::wire::PgConn;
use xerj_sources::{Op, Sink, SourceSpec, State, Store, Worker};

/// index -> id -> doc
type Docs = BTreeMap<String, BTreeMap<String, Map<String, Value>>>;

#[derive(Default)]
struct MemSink {
    docs: Mutex<Docs>,
    applied: Mutex<u64>,
}

impl Sink for MemSink {
    fn apply(&self, ops: &[Op]) -> anyhow::Result<()> {
        let mut d = self.docs.lock().unwrap();
        for op in ops {
            match op {
                Op::Index { index, id, doc } => {
                    d.entry(index.clone())
                        .or_default()
                        .insert(id.clone(), doc.clone());
                }
                Op::Update { index, id, doc } => {
                    let e = d
                        .entry(index.clone())
                        .or_default()
                        .entry(id.clone())
                        .or_default();
                    for (k, v) in doc {
                        e.insert(k.clone(), v.clone());
                    }
                }
                Op::Delete { index, id } => {
                    if let Some(m) = d.get_mut(index) {
                        m.remove(id);
                    }
                }
            }
        }
        *self.applied.lock().unwrap() += ops.len() as u64;
        Ok(())
    }
    fn clear_index(&self, index: &str) -> anyhow::Result<()> {
        self.docs.lock().unwrap().remove(index);
        Ok(())
    }
}

impl MemSink {
    fn get(&self, index: &str, id: &str) -> Option<Map<String, Value>> {
        self.docs.lock().unwrap().get(index)?.get(id).cloned()
    }
    fn count(&self, index: &str) -> usize {
        self.docs
            .lock()
            .unwrap()
            .get(index)
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

fn env() -> Option<(String, String)> {
    Some((
        std::env::var("XERJ_TEST_PG_URL").ok()?,
        std::env::var("XERJ_TEST_PG_PASSWORD").ok()?,
    ))
}

fn sql(url: &str, pw: &str, q: &str) {
    let cp = xerj_sources::spec::parse_url(url).unwrap();
    let mut c = PgConn::connect(&cp, Some(pw), false).unwrap();
    c.simple_query(q).unwrap();
    c.close();
}

fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(
            t.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn snapshot_then_stream_then_restart_without_loss() {
    let Some((url, pw)) = env() else {
        eprintln!("skipped: XERJ_TEST_PG_URL / XERJ_TEST_PG_PASSWORD not set");
        return;
    };
    std::env::set_var("XERJ_TEST_PG_PW_VAR", &pw);
    sql(&url, &pw, "DROP TABLE IF EXISTS items, nokey CASCADE; \
        SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = 'xerj_t1'; \
        DROP PUBLICATION IF EXISTS xerj_t1; \
        CREATE TABLE items (id int PRIMARY KEY, name text, price numeric, tags jsonb, big text, updated_at timestamptz DEFAULT now()); \
        ALTER TABLE items ALTER COLUMN big SET STORAGE EXTERNAL; \
        CREATE TABLE nokey (v text); \
        INSERT INTO items (id, name, price, tags) SELECT g, 'item ' || g, g * 1.5, '{\"k\": 1}' FROM generate_series(1, 1000) g;");
    let dir = tempfile::tempdir().unwrap();
    let spec: SourceSpec = serde_json::from_value(json!({
        "name": "t1", "url": url, "password_env": "XERJ_TEST_PG_PW_VAR"
    }))
    .unwrap();
    spec.validate().unwrap();
    let store = Store::new(dir.path(), "t1");
    store.save_spec(&spec).unwrap();
    let sink = Arc::new(MemSink::default());
    let idx = "src-t1-public-items";

    let w = Worker::spawn(spec.clone(), store.clone(), sink.clone(), None);
    wait("streaming", 60, || w.status().state == State::Streaming);
    assert_eq!(sink.count(idx), 1000, "the snapshot indexed every row");
    let st = w.status();
    assert!(
        st.skipped.iter().any(|s| s.contains("nokey")),
        "{:?}",
        st.skipped
    );
    let doc = sink.get(idx, "7").unwrap();
    assert_eq!(doc["name"], json!("item 7"));
    assert_eq!(doc["price"], json!(10.5));
    assert_eq!(doc["tags"], json!({"k": 1}));

    // Changes stream in.
    let big = "x".repeat(10_000);
    sql(
        &url,
        &pw,
        &format!(
            "INSERT INTO items (id, name, big) VALUES (2001, 'new', '{big}'); \
        UPDATE items SET name = 'renamed' WHERE id = 7; \
        DELETE FROM items WHERE id = 8; \
        UPDATE items SET id = 9009 WHERE id = 9;"
        ),
    );
    wait("streamed changes", 20, || sink.get(idx, "9009").is_some());
    assert_eq!(sink.get(idx, "2001").unwrap()["name"], json!("new"));
    assert_eq!(sink.get(idx, "7").unwrap()["name"], json!("renamed"));
    assert!(sink.get(idx, "8").is_none(), "delete applied");
    assert!(
        sink.get(idx, "9").is_none(),
        "a primary-key change removes the old id"
    );
    // TOAST: an update that does not touch `big` must keep it.
    sql(
        &url,
        &pw,
        "UPDATE items SET name = 'still big' WHERE id = 2001;",
    );
    wait("toast update", 20, || {
        sink.get(idx, "2001")
            .map(|d| d["name"] == json!("still big"))
            .unwrap_or(false)
    });
    assert_eq!(
        sink.get(idx, "2001").unwrap()["big"]
            .as_str()
            .unwrap()
            .len(),
        10_000
    );
    // A new column appears as a new field.
    sql(
        &url,
        &pw,
        "ALTER TABLE items ADD COLUMN color text; UPDATE items SET color = 'red' WHERE id = 1;",
    );
    wait("new column", 20, || {
        sink.get(idx, "1")
            .map(|d| d.get("color") == Some(&json!("red")))
            .unwrap_or(false)
    });

    // Stop, write while stopped, restart: resumes from the acknowledged LSN.
    assert!(w.stop(Duration::from_secs(10)));
    let before = sink.count(idx);
    sql(&url, &pw, "INSERT INTO items (id, name) SELECT g, 'while down' FROM generate_series(5001, 5100) g; DELETE FROM items WHERE id = 10;");
    let w = Worker::spawn(spec.clone(), store.clone(), sink.clone(), None);
    wait("resume", 30, || {
        sink.get(idx, "5100").is_some() && sink.get(idx, "10").is_none()
    });
    assert_eq!(
        sink.count(idx),
        before + 100 - 1,
        "no loss, no duplicates after restart"
    );
    assert_eq!(
        w.status().state,
        State::Streaming,
        "a restart does not re-snapshot"
    );

    // TRUNCATE clears the index.
    sql(&url, &pw, "TRUNCATE items;");
    wait("truncate", 20, || sink.count(idx) == 0);
    sql(
        &url,
        &pw,
        "INSERT INTO items (id, name) VALUES (1, 'after truncate');",
    );
    wait("insert after truncate", 20, || sink.count(idx) == 1);
    let s = w.status();
    assert!(s.stream.apply_latency_ms_p50.is_some());
    assert!(w.stop(Duration::from_secs(10)));

    // Cleanup drops the slot and the publication XERJ created.
    let done = xerj_sources::pg::connector::cleanup(&spec, &store.load_state()).unwrap();
    assert_eq!(done.len(), 2, "{done:?}");
    let cp = xerj_sources::spec::parse_url(&url).unwrap();
    let mut c = PgConn::connect(&cp, Some(&pw), false).unwrap();
    let r = c
        .simple_query("SELECT count(*) FROM pg_replication_slots WHERE slot_name = 'xerj_t1'")
        .unwrap();
    assert_eq!(
        r.rows[0][0].as_deref(),
        Some("0"),
        "no slot left behind to retain WAL"
    );
}
