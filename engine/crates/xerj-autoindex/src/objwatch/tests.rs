//! Watcher behaviour, against an in-memory store that counts what it was asked.
//!
//! These tests own the semantics: what counts as changed, that an unchanged
//! bucket costs zero GETs and a bounded number of list calls, that a delete
//! removes records, that an object replaced mid-cycle is not lost, and that the
//! cost guard refuses before spending. The same scenarios run against a real
//! MinIO in `tests/objwatch_minio.rs`; this file is where they are cheap enough
//! to run on every `cargo test`.

use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone)]
struct StoredObject {
    bytes: Vec<u8>,
    etag: String,
    last_modified: String,
}

/// An S3-shaped store: sorted keys, `max-keys` paging, continuation tokens,
/// ETag on GET. Counts every call, which is the point.
struct FakeStore {
    objects: Mutex<BTreeMap<String, StoredObject>>,
    list_calls: AtomicU64,
    gets: AtomicU64,
    /// Run after each GET, so a test can replace an object while a cycle is in
    /// flight.
    after_get: Mutex<Option<Box<dyn Fn(&FakeStore, &str) + Send>>>,
    /// Drop the ETag from GET responses, the way some gateways and proxies do.
    no_etag_on_get: AtomicU64,
}

impl FakeStore {
    fn new() -> FakeStore {
        FakeStore {
            objects: Mutex::new(BTreeMap::new()),
            list_calls: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            after_get: Mutex::new(None),
            no_etag_on_get: AtomicU64::new(0),
        }
    }

    fn put(&self, key: &str, body: &str) {
        self.put_with_etag(key, body, &format!("etag-{:x}", xxhash_rust::xxh3::xxh3_64(body.as_bytes())));
    }

    fn put_with_etag(&self, key: &str, body: &str, etag: &str) {
        let mut m = self.objects.lock().unwrap();
        let n = m.len();
        m.insert(
            key.to_string(),
            StoredObject {
                bytes: body.as_bytes().to_vec(),
                etag: etag.to_string(),
                last_modified: format!("2026-09-19T10:{:02}:00.000Z", n % 60),
            },
        );
    }

    fn remove(&self, key: &str) {
        self.objects.lock().unwrap().remove(key);
    }

    fn list_calls(&self) -> u64 {
        self.list_calls.load(Ordering::SeqCst)
    }

    fn gets(&self) -> u64 {
        self.gets.load(Ordering::SeqCst)
    }
}

impl ObjectSource for FakeStore {
    fn describe(&self) -> String {
        "s3://fake/ @ memory".into()
    }

    fn list(&self, req: &ListRequest) -> Result<ListPage> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        let m = self.objects.lock().unwrap();
        let after = req.continuation_token.or(req.start_after);
        let mut objects: Vec<ObjectMeta> = Vec::new();
        let mut next = None;
        for (key, o) in m.iter() {
            if !key.starts_with(req.prefix) {
                continue;
            }
            if let Some(a) = after {
                if key.as_str() <= a {
                    continue;
                }
            }
            if objects.len() as u64 >= req.max_keys {
                next = Some(objects.last().map(|o| o.key.clone()).unwrap_or_default());
                break;
            }
            objects.push(ObjectMeta {
                key: key.clone(),
                size: o.bytes.len() as u64,
                etag: o.etag.clone(),
                last_modified: o.last_modified.clone(),
            });
        }
        Ok(ListPage {
            objects,
            next_token: next,
            common_prefixes: Vec::new(),
        })
    }

    fn get(&self, key: &str, max_bytes: u64) -> Result<FetchedObject> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let (bytes, etag) = {
            let m = self.objects.lock().unwrap();
            let o = m
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("no such key {key}"))?;
            (o.bytes.clone(), o.etag.clone())
        };
        let hook = self.after_get.lock().unwrap();
        if let Some(f) = hook.as_ref() {
            f(self, key);
        }
        let truncated = bytes.len() as u64 > max_bytes;
        let bytes = if truncated {
            bytes[..max_bytes as usize].to_vec()
        } else {
            bytes
        };
        Ok(FetchedObject {
            bytes,
            etag: if self.no_etag_on_get.load(Ordering::SeqCst) == 1 {
                None
            } else {
                Some(etag)
            },
            truncated,
        })
    }
}

/// Records every event, in order, and can run a hook while it is "indexing".
#[derive(Default)]
struct RecordingSink {
    events: Vec<(String, String)>,
    fail_on: Option<String>,
    during_accept: Option<Box<dyn Fn() + Send>>,
}

impl ChangeSink for RecordingSink {
    fn accept(&mut self, ev: &ChangeEvent) -> Result<()> {
        if self.fail_on.as_deref() == Some(ev.key.as_str()) {
            anyhow::bail!("sink refused {}", ev.key);
        }
        if let Some(f) = &self.during_accept {
            f();
        }
        self.events
            .push((ev.kind.as_str().to_string(), ev.key.clone()));
        Ok(())
    }
}

impl RecordingSink {
    fn keys(&self, kind: &str) -> Vec<String> {
        self.events
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, key)| key.clone())
            .collect()
    }
    fn clear(&mut self) {
        self.events.clear();
    }
}

fn opts() -> WatchOptions {
    WatchOptions {
        poll_interval: Duration::from_secs(300),
        max_cycles: Some(1),
        fetch: true,
        max_object_bytes: 1 << 20,
        append_only: false,
        max_monthly_class_a: cost::DEFAULT_MAX_MONTHLY_CLASS_A,
        allow_cost: false,
        page_size: 1_000,
        status_path: None,
    }
}

fn journal(dir: &std::path::Path) -> WatchJournal {
    WatchJournal::open(dir, "http://memory", "fake", "", false, false).unwrap()
}

fn never_stop() -> impl Fn() -> bool {
    || false
}

#[test]
fn the_first_poll_indexes_everything_and_the_second_indexes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("a.md", "alpha");
    store.put("b.txt", "beta");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = opts();

    let first =
        poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(first.added, 2);
    assert_eq!(first.changed, 0);
    assert_eq!(first.deleted, 0);
    assert_eq!(first.gets, 2, "the first poll fetches both objects");
    assert_eq!(first.list_calls, 1);
    assert_eq!(sink.keys("added"), vec!["a.md", "b.txt"]);

    // THE load-bearing assertion for cost: an unchanged bucket costs one list
    // call and ZERO GETs. A watcher that re-reads bytes every cycle would pass
    // every other test in this file and quietly bill for it.
    sink.clear();
    let gets_before = store.gets();
    let second =
        poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.added, 0);
    assert_eq!(second.changed, 0);
    assert_eq!(second.deleted, 0);
    assert_eq!(second.unchanged, 2);
    assert_eq!(second.gets, 0, "an unchanged object must not be fetched");
    assert_eq!(store.gets(), gets_before);
    assert_eq!(second.list_calls, 1, "one list call for a 2-object bucket");
    assert!(sink.events.is_empty());
}

#[test]
fn a_changed_object_is_re_extracted_a_new_one_is_added_and_a_deleted_one_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("keep.md", "keep");
    store.put("edit.md", "before");
    store.put("gone.md", "gone");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = opts();
    poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    sink.clear();

    store.put("edit.md", "after");
    store.put("new.md", "new");
    store.remove("gone.md");

    let r = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.changed, 1);
    assert_eq!(r.added, 1);
    assert_eq!(r.deleted, 1);
    assert_eq!(r.unchanged, 1);
    assert_eq!(sink.keys("changed"), vec!["edit.md"]);
    assert_eq!(sink.keys("added"), vec!["new.md"]);
    assert_eq!(sink.keys("deleted"), vec!["gone.md"]);
    assert_eq!(r.gets, 2, "only the changed and the new object are fetched");
    assert!(j.get("gone.md").is_none(), "a deleted key leaves the journal");
    assert_eq!(j.get("edit.md").unwrap().digest.is_some(), true);
}

/// An object replaced by a multipart upload keeps its size and can keep its
/// last-modified second, but its ETag stops being an MD5 and becomes
/// `<hex>-<parts>`. The watcher must notice on the ETag alone.
#[test]
fn a_multipart_replacement_is_detected_by_the_etag_shape_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put_with_etag("big.bin", "0123456789", "d41d8cd98f00b204e9800998ecf8427e");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = opts();
    poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    sink.clear();

    // Same bytes, same length, multipart ETag: a size/mtime comparison alone
    // would miss this.
    store.put_with_etag("big.bin", "0123456789", "9a0364b9e99bb480dd25e1f0284c8555-2");
    let r = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.changed, 1, "multipart ETag change must be a change");
    assert_eq!(sink.keys("changed"), vec!["big.bin"]);
    assert!(j.get("big.bin").unwrap().etag.ends_with("-2"));
}

/// The overlap case: the object is replaced WHILE the cycle is fetching and
/// indexing it. The journal must end up describing the bytes that were indexed,
/// and the next cycle must re-index exactly once — never zero times (a lost
/// update) and never twice for the same bytes (a double index).
#[test]
fn an_object_replaced_during_a_cycle_is_re_indexed_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("live.md", "v1");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = opts();

    // Replace the object right after the GET that read v1 — the window a
    // poll-then-fetch watcher actually has.
    *store.after_get.lock().unwrap() = Some(Box::new(|s: &FakeStore, key: &str| {
        if key == "live.md" {
            let already = s
                .objects
                .lock()
                .unwrap()
                .get(key)
                .map(|o| o.bytes == b"v2")
                .unwrap_or(false);
            if !already {
                s.put("live.md", "v2");
            }
        }
    }));

    let first =
        poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(first.added, 1);
    // What the journal remembers is what was read, so the newer bytes are still
    // outstanding.
    let recorded = j.get("live.md").unwrap().clone();
    assert_eq!(recorded.digest.unwrap(), format!("{:016x}", xxhash_rust::xxh3::xxh3_64(b"v1")));

    *store.after_get.lock().unwrap() = None;
    sink.clear();
    let second =
        poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.changed, 1, "the update made mid-cycle is not lost");
    assert_eq!(sink.keys("changed"), vec!["live.md"]);

    sink.clear();
    let third =
        poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
    assert_eq!(third.changed, 0, "and it is not indexed a second time");
    assert_eq!(third.gets, 0);
    assert!(sink.events.is_empty());
}

/// A sink that refuses one object must not have that object recorded as done —
/// otherwise the failure is permanent and silent.
#[test]
fn an_object_the_sink_refused_is_retried_on_the_next_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("ok.md", "fine");
    store.put("bad.md", "boom");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink {
        fail_on: Some("bad.md".into()),
        ..Default::default()
    };
    let mut totals = CostTotals::default();
    let o = opts();
    let first =
        poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(first.added, 1);
    assert_eq!(first.errors.len(), 1, "{:?}", first.errors);
    assert!(j.get("bad.md").is_none());

    sink.fail_on = None;
    sink.clear();
    let second =
        poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.added, 1);
    assert_eq!(sink.keys("added"), vec!["bad.md"]);
}

#[test]
fn a_store_that_omits_the_etag_on_get_verifies_by_digest_once_then_settles() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.no_etag_on_get.store(1, Ordering::SeqCst);
    store.put("a.md", "alpha");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = opts();
    poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert!(
        j.get("a.md").unwrap().etag_unverified,
        "the recorded ETag came from the listing, not from the bytes we read"
    );
    sink.clear();
    // Nothing changed in the bucket, but the recorded ETag describes bytes we
    // did not verify, so the next cycle re-reads once. The digest matches, so it
    // is recorded as the same bytes and NOT re-indexed.
    let second =
        poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.gets, 1);
    assert_eq!(second.changed, 0);
    assert_eq!(second.content_identical, 1);
    assert!(sink.events.is_empty());
    assert!(
        !j.get("a.md").unwrap().etag_unverified,
        "one verification is enough — a store with no ETags must not cost a GET every cycle"
    );
    // And now it is free.
    let third =
        poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
    assert_eq!(third.gets, 0);
    assert_eq!(third.unchanged, 1);
}

/// ETag churn: a copy or a lifecycle rewrite gives the same bytes a new ETag.
/// The watcher pays one GET to find out and then must NOT re-index.
#[test]
fn an_etag_rewritten_over_identical_bytes_is_not_re_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put_with_etag("a.md", "alpha", "etag-1");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = opts();
    poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    sink.clear();
    store.put_with_etag("a.md", "alpha", "etag-2");
    let r = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.gets, 1, "it must read the bytes to know");
    assert_eq!(r.changed, 0);
    assert_eq!(r.content_identical, 1);
    assert!(sink.events.is_empty());
    assert_eq!(j.get("a.md").unwrap().etag, "etag-2", "the new ETag is recorded");
    // Settled: the next cycle is free.
    let third =
        poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
    assert_eq!(third.gets, 0);
    assert_eq!(third.unchanged, 1);
}

#[test]
fn metadata_only_mode_never_issues_a_get() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("a.md", "alpha");
    store.put("b.md", "beta");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        fetch: false,
        ..opts()
    };
    let r = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.added, 2);
    assert_eq!(r.gets, 0);
    assert_eq!(store.gets(), 0);
    assert!(j.get("a.md").unwrap().digest.is_none());
}

/// Paging is where a change detector silently loses a bucket: stop early and
/// every unlisted object looks deleted.
#[test]
fn a_bucket_larger_than_one_page_is_listed_completely_and_costs_one_call_per_page() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..2_500 {
        store.put(&format!("k/{i:05}"), "x");
    }
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        fetch: false,
        ..opts()
    };
    let r = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.keys_listed, 2_500);
    assert_eq!(r.added, 2_500);
    // 2,500 keys at 1,000 per call: 3 calls with keys, plus the call that
    // returns the empty tail. That is the real cost shape, and the docs quote
    // ceil(keys/1000) as the floor.
    assert!(
        (3..=4).contains(&r.list_calls),
        "expected 3-4 list calls, got {}",
        r.list_calls
    );
    assert_eq!(r.deleted, 0);

    sink.clear();
    let second =
        poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.unchanged, 2_500);
    assert_eq!(second.deleted, 0, "nothing may look deleted across pages");
    assert_eq!(second.gets, 0);
}

#[test]
fn a_prefix_scoped_watch_ignores_everything_outside_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("docs/a.md", "a");
    store.put("other/b.md", "b");
    let mut j =
        WatchJournal::open(dir.path(), "http://memory", "fake", "docs/", false, false).unwrap();
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let r = poll_once(&store, &mut j, &opts(), &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.added, 1);
    assert_eq!(sink.keys("added"), vec!["docs/a.md"]);
    // And the objects outside the prefix are not deletions.
    assert_eq!(r.deleted, 0);
}

/// `--append-only` is the cheap poll: one list call whatever the bucket size.
/// The price is that it cannot see deletes or edits to older keys, and it must
/// never pretend otherwise.
#[test]
fn append_only_mode_lists_only_the_tail_and_reports_no_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..2_500 {
        store.put(&format!("log/{i:05}"), "x");
    }
    let mut j =
        WatchJournal::open(dir.path(), "http://memory", "fake", "log/", true, false).unwrap();
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        fetch: false,
        append_only: true,
        ..opts()
    };
    let first = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(first.added, 2_500);
    let calls_first = first.list_calls;

    // One new key at the top of the key space, and one old key deleted.
    store.put("log/09999", "new");
    store.remove("log/00001");
    sink.clear();
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.added, 1);
    assert_eq!(sink.keys("added"), vec!["log/09999"]);
    assert_eq!(
        second.deleted, 0,
        "append-only never saw the whole key space, so it must not report deletes"
    );
    assert_eq!(
        second.list_calls, 1,
        "the tail is one page: {calls_first} calls for the backfill, 1 afterwards"
    );
    assert!(j.get("log/00001").is_some(), "the deleted key stays recorded");
}

#[test]
fn the_cost_guard_refuses_the_first_cycle_and_names_a_safe_interval() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    // 1,200 keys = 2 list pages. At a 1-second interval that is 5,184,000
    // Class A operations a month: five times the whole free tier.
    for i in 0..1_200 {
        store.put(&format!("k/{i:05}"), "x");
    }
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        poll_interval: Duration::from_secs(1),
        fetch: false,
        ..opts()
    };
    let err = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap_err();
    let refused = err
        .downcast_ref::<PollCostRefused>()
        .expect("a cost refusal, not a generic error");
    assert!(refused.projection.over_budget());
    assert_eq!(refused.projection.list_calls_per_cycle, 2);
    assert_eq!(refused.projection.monthly_class_a, 5_184_000);
    assert!(refused.projection.min_safe_interval_secs >= 26);
    assert!(sink.events.is_empty(), "nothing was indexed before the refusal");
    assert_eq!(store.gets(), 0, "and nothing was fetched");
    let payload = refused.to_json();
    assert_eq!(payload["exit_code"], 4);
    assert_eq!(payload["reason"], "poll_cost_over_budget");

    // --allow-cost is the documented way through, and it still reports the
    // number rather than going quiet.
    let o = WatchOptions {
        allow_cost: true,
        ..o
    };
    let r = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.added, 1_200);
    assert!(
        r.warnings.iter().any(|w| w.contains("over budget")),
        "{:?}",
        r.warnings
    );
}

#[test]
fn run_watch_stops_at_max_cycles_and_reports_every_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("a.md", "alpha");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let o = WatchOptions {
        poll_interval: Duration::from_millis(10),
        max_cycles: Some(3),
        ..opts()
    };
    let mut seen: Vec<u64> = Vec::new();
    let outcome = run_watch(&store, &mut j, &o, &mut sink, &|| false, &mut |r| {
        seen.push(r.cycle)
    })
    .unwrap();
    assert_eq!(seen, vec![0, 1, 2]);
    assert_eq!(outcome.totals.cycles, 3);
    assert_eq!(outcome.totals.gets, 1, "only the first cycle fetched");
    assert_eq!(outcome.totals.list_calls, 3);
    assert_eq!(j.cycles, 3);
}

#[test]
fn the_status_file_is_what_an_operator_reads_without_attaching_to_the_process() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("a.md", "alpha");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let status = dir.path().join("status.json");
    let o = WatchOptions {
        poll_interval: Duration::from_millis(5),
        max_cycles: Some(2),
        status_path: Some(status.clone()),
        ..opts()
    };
    run_watch(&store, &mut j, &o, &mut sink, &|| false, &mut |_| {}).unwrap();
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&status).unwrap()).unwrap();
    assert_eq!(body["xerj"], "objwatch-status");
    assert_eq!(body["totals"]["cycles"], 2);
    assert_eq!(body["totals"]["list_calls_class_a"], 2);
    assert_eq!(body["totals"]["gets_class_b"], 1);
    assert!(body["projection"]["monthly_class_a"].as_u64().unwrap() > 0);
    assert_eq!(body["location"], "s3://fake/ @ memory");
}

#[test]
fn the_jsonl_sink_writes_one_line_per_event() {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut sink = JsonlSink::new(&mut buf);
        sink.accept(&ChangeEvent {
            kind: ChangeKind::Added,
            key: "a.md".into(),
            size: 3,
            etag: "e".into(),
            last_modified: "lm".into(),
            digest: Some("d".into()),
            bytes_fetched: 3,
            reason: "new_key",
            truncated: false,
        })
        .unwrap();
        sink.accept(&ChangeEvent {
            kind: ChangeKind::Deleted,
            key: "b.md".into(),
            size: 0,
            etag: String::new(),
            last_modified: String::new(),
            digest: None,
            bytes_fetched: 0,
            reason: "absent_from_listing",
            truncated: false,
        })
        .unwrap();
    }
    let text = String::from_utf8(buf).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["kind"], "added");
    assert_eq!(first["key"], "a.md");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["kind"], "deleted");
    assert!(second["digest"].is_null());
}

#[test]
fn a_fetch_capped_by_max_object_bytes_is_recorded_as_partial_and_not_re_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("big.txt", "0123456789");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        max_object_bytes: 4,
        ..opts()
    };
    let r = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.added, 1);
    let rec = j.get("big.txt").unwrap();
    assert!(rec.truncated, "the record must say the bytes are a prefix");
    assert_eq!(rec.size, 10, "the record keeps the object's real size");
    sink.clear();
    // A capped fetch is a decision the operator made with --max-object-mb, not
    // an unfinished job: the watcher records it, says so, and does not re-read
    // the same prefix every cycle.
    let second =
        poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.changed, 0);
    assert_eq!(second.gets, 0);
    assert_eq!(second.unchanged, 1);
}

#[test]
fn diff_is_pure_and_reports_a_reason_for_every_change() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = journal(dir.path());
    j.record(
        "same",
        WatchedObject {
            etag: "e".into(),
            size: 1,
            last_modified: "lm".into(),
            digest: None,
            etag_unverified: false,
            seen_at: "t".into(),
            truncated: false,
        },
    );
    j.record(
        "size-only",
        WatchedObject {
            etag: "e".into(),
            size: 1,
            last_modified: "lm".into(),
            digest: None,
            etag_unverified: false,
            seen_at: "t".into(),
            truncated: false,
        },
    );
    let listed = vec![
        ObjectMeta {
            key: "same".into(),
            size: 1,
            etag: "e".into(),
            last_modified: "lm".into(),
        },
        ObjectMeta {
            key: "size-only".into(),
            size: 2,
            etag: "e".into(),
            last_modified: "lm".into(),
        },
        ObjectMeta {
            key: "brand-new".into(),
            size: 9,
            etag: "x".into(),
            last_modified: "lm".into(),
        },
    ];
    let plan = diff(&j, &listed, true);
    assert_eq!(plan.unchanged, 1);
    assert_eq!(plan.added.len(), 1);
    assert_eq!(plan.changed.len(), 1);
    assert_eq!(plan.changed[0].1, "size");
    assert!(plan.deleted.is_empty());

    // The same listing with delete detection off, and one journal key missing
    // from it.
    j.forget("size-only");
    let plan = diff(&j, &[], true);
    assert_eq!(plan.deleted, vec!["same".to_string()]);
    let plan = diff(&j, &[], false);
    assert!(plan.deleted.is_empty());
}
