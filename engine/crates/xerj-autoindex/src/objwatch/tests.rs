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

/// The hook one test uses to mutate the store DURING a fetch, which is how the
/// "replaced while the cycle was reading it" case is reproduced deterministically.
type AfterGet = Box<dyn Fn(&FakeStore, &str) + Send>;

/// An S3-shaped store: sorted keys, `max-keys` paging, continuation tokens,
/// ETag on GET. Counts every call, which is the point.
struct FakeStore {
    objects: Mutex<BTreeMap<String, StoredObject>>,
    list_calls: AtomicU64,
    gets: AtomicU64,
    /// Run after each GET, so a test can replace an object while a cycle is in
    /// flight.
    after_get: Mutex<Option<AfterGet>>,
    /// Drop the ETag from GET responses, the way some gateways and proxies do.
    no_etag_on_get: AtomicU64,
    /// Fail the list call with this 0-based number, the way a 503 SlowDown
    /// does. `u64::MAX` = never.
    fail_list_call: AtomicU64,
    /// Run before each list call is served, with its 0-based number. Lets a
    /// test observe the world MID-SCAN — which is how the crash-safety of the
    /// spend ledger is checked without killing the test process.
    #[allow(clippy::type_complexity)]
    on_list: Mutex<Option<Box<dyn Fn(u64) + Send>>>,
}

impl FakeStore {
    fn new() -> FakeStore {
        FakeStore {
            objects: Mutex::new(BTreeMap::new()),
            list_calls: AtomicU64::new(0),
            gets: AtomicU64::new(0),
            after_get: Mutex::new(None),
            no_etag_on_get: AtomicU64::new(0),
            fail_list_call: AtomicU64::new(u64::MAX),
            on_list: Mutex::new(None),
        }
    }

    fn put(&self, key: &str, body: &str) {
        self.put_with_etag(
            key,
            body,
            &format!("etag-{:x}", xxhash_rust::xxh3::xxh3_64(body.as_bytes())),
        );
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
        let n = self.list_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(f) = self.on_list.lock().unwrap().as_ref() {
            f(n);
        }
        if n == self.fail_list_call.load(Ordering::SeqCst) {
            anyhow::bail!("ListObjectsV2 returned 503 Service Unavailable: SlowDown");
        }
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
        max_monthly_class_b: cost::DEFAULT_MAX_MONTHLY_CLASS_B,
        allow_cost: false,
        page_size: 1_000,
        status_path: None,
        dry_run: false,
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

    let first = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
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
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.added, 0);
    assert_eq!(second.changed, 0);
    assert_eq!(second.deleted, 0);
    assert_eq!(second.unchanged, 2);
    assert_eq!(second.gets, 0, "an unchanged object must not be fetched");
    assert_eq!(store.gets(), gets_before);
    assert_eq!(second.list_calls, 1, "one list call for a 2-object bucket");
    assert!(sink.events.is_empty());

    // The reported cost must be the cost the STORE actually served, not a number
    // the accounting produced on its own. Every billing claim in the docs rests
    // on this count, so it is asserted against the other side of the call.
    assert_eq!(
        store.list_calls(),
        2,
        "two cycles served two ListObjectsV2 calls"
    );
    assert_eq!(store.list_calls(), totals.list_calls);
    assert_eq!(store.gets(), totals.gets);
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
    assert!(
        j.get("gone.md").is_none(),
        "a deleted key leaves the journal"
    );
    assert!(j.get("edit.md").unwrap().digest.is_some());
}

/// An object replaced by a multipart upload keeps its size and can keep its
/// last-modified second, but its ETag stops being an MD5 and becomes
/// `<hex>-<parts>`. The watcher must notice on the ETag alone.
/// An object replaced by a multipart upload. The ETag stops being an MD5 and
/// becomes `<hex>-<parts>`, which must be compared as an opaque string and never
/// read as a content hash.
///
/// Both halves matter, and they used to be conflated. Replacing the CONTENT via
/// multipart is a change. Re-uploading the SAME bytes via multipart is only ETag
/// churn, and re-indexing it would cost a bulk request, a merge and a refresh for
/// nothing — so it is deliberately not a change. The original version of this
/// test asserted a change for identical bytes and was wrong about the intended
/// behaviour, not about the code.
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

    // New content, delivered multipart: the ETag's SHAPE changes as well as its
    // value, and a size comparison alone would still catch this one.
    store.put_with_etag(
        "big.bin",
        "0123456789abc",
        "9a0364b9e99bb480dd25e1f0284c8555-2",
    );
    let r = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(
        r.changed, 1,
        "multipart content replacement must be a change"
    );
    assert_eq!(r.content_identical, 0);
    assert_eq!(sink.keys("changed"), vec!["big.bin"]);
    let recorded = j.get("big.bin").unwrap().etag.clone();
    assert!(
        recorded.ends_with("-2"),
        "the part-count suffix is kept verbatim, never parsed as a hash: {recorded}"
    );
    sink.clear();

    // Same bytes re-uploaded multipart under yet another ETag: churn, not a
    // change. The journal still catches up so the NEXT cycle is free again.
    store.put_with_etag(
        "big.bin",
        "0123456789abc",
        "ffffffffffffffffffffffffffffffff-3",
    );
    let r = poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
    assert_eq!(
        r.changed, 0,
        "identical bytes under a new ETag is not a change"
    );
    assert_eq!(r.content_identical, 1);
    assert!(sink.keys("changed").is_empty());
    assert!(j.get("big.bin").unwrap().etag.ends_with("-3"));
    let r = poll_once(&store, &mut j, &o, &mut sink, 3, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.gets, 0, "and the cycle after the churn costs nothing");
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

    let first = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(first.added, 1);
    // What the journal remembers is what was read, so the newer bytes are still
    // outstanding.
    let recorded = j.get("live.md").unwrap().clone();
    assert_eq!(
        recorded.digest.unwrap(),
        format!("{:016x}", xxhash_rust::xxh3::xxh3_64(b"v1"))
    );

    *store.after_get.lock().unwrap() = None;
    sink.clear();
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.changed, 1, "the update made mid-cycle is not lost");
    assert_eq!(sink.keys("changed"), vec!["live.md"]);

    sink.clear();
    let third = poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
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
    let first = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(first.added, 1);
    assert_eq!(first.errors.len(), 1, "{:?}", first.errors);
    assert!(j.get("bad.md").is_none());

    sink.fail_on = None;
    sink.clear();
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
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
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(second.gets, 1);
    assert_eq!(second.changed, 0);
    assert_eq!(second.content_identical, 1);
    assert!(sink.events.is_empty());
    assert!(
        !j.get("a.md").unwrap().etag_unverified,
        "one verification is enough — a store with no ETags must not cost a GET every cycle"
    );
    // And now it is free.
    let third = poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
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
    assert_eq!(
        j.get("a.md").unwrap().etag,
        "etag-2",
        "the new ETag is recorded"
    );
    // Settled: the next cycle is free.
    let third = poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
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
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
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
    let r = poll_once(
        &store,
        &mut j,
        &opts(),
        &mut sink,
        0,
        &mut totals,
        &never_stop(),
    )
    .unwrap();
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
    assert!(
        j.get("log/00001").is_some(),
        "the deleted key stays recorded"
    );
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
    assert!(
        sink.events.is_empty(),
        "nothing was indexed before the refusal"
    );
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
        // A 10 ms poll really is ~259,200,000 Class A operations a month, and the
        // guard is right to refuse it. This test is about the loop, not the
        // guard, so it accepts the cost explicitly rather than hiding it.
        allow_cost: true,
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
        allow_cost: true,
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
    let second = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
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

/// The guard must not refuse `--append-only`, which is the escape hatch the guard
/// itself recommends.
///
/// The cost of an append-only cycle is the TAIL above `start-after`, not the key
/// space the journal remembers. Projecting it at journal size made a large
/// append-only watch refuse itself: 1,000,000 remembered keys reads as 1,000 list
/// calls a cycle, which is over any sane budget, while the real cost is one call.
#[test]
fn append_only_is_priced_at_the_tail_it_lists_not_at_the_journal_it_remembers() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    // A journal that already remembers more than one page's worth of keys.
    let mut j = WatchJournal::open(dir.path(), "http://memory", "fake", "", true, false).unwrap();
    for i in 0..2_500u32 {
        store.put(&format!("2026/{i:06}"), "old");
        j.record(
            &format!("2026/{i:06}"),
            WatchedObject {
                etag: "e".into(),
                size: 3,
                last_modified: "t".into(),
                digest: Some("d".into()),
                etag_unverified: false,
                seen_at: journal::now_rfc3339(),
                truncated: false,
            },
        );
    }
    // One new key above the highest one seen.
    store.put("2026/999999", "new");

    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        append_only: true,
        // A budget that 3 list calls/cycle at 300 s would blow through, but that
        // 1 call/cycle fits with room to spare: 8,640 vs 25,920 a month.
        max_monthly_class_a: 10_000,
        ..opts()
    };
    let r = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.added, 1, "only the tail key is new: {}", r.line());
    assert_eq!(r.keys_listed, 1, "the store returned only the tail");
    assert_eq!(r.list_calls, 1);
    assert_eq!(
        r.projection.list_calls_per_cycle,
        1,
        "priced at the tail, not at the 2,500-key journal: {}",
        r.line()
    );
    assert_eq!(r.projection.monthly_class_a, 8_640);
    assert!(!r.projection.over_budget(), "{}", r.line());

    // The same journal in FULL-SCAN mode is priced at the whole key space, which
    // is the other half of the same rule.
    let mut j2 = WatchJournal::open(dir.path(), "http://memory", "fake", "", false, true).unwrap();
    for i in 0..2_500u32 {
        j2.record(
            &format!("2026/{i:06}"),
            WatchedObject {
                etag: "e".into(),
                size: 3,
                last_modified: "t".into(),
                digest: Some("d".into()),
                etag_unverified: false,
                seen_at: journal::now_rfc3339(),
                truncated: false,
            },
        );
    }
    let full = WatchOptions {
        append_only: false,
        max_monthly_class_a: 10_000,
        ..opts()
    };
    let err = poll_once(
        &store,
        &mut j2,
        &full,
        &mut sink,
        0,
        &mut totals,
        &never_stop(),
    )
    .unwrap_err();
    let refused = err
        .downcast_ref::<PollCostRefused>()
        .expect("a full scan of 2,501 keys at this budget must be refused");
    assert_eq!(refused.projection.list_calls_per_cycle, 3);
    assert_eq!(refused.projection.monthly_class_a, 25_920);
    assert!(
        refused.to_string().contains("--append-only"),
        "the refusal must point at the cheaper mode: {refused}"
    );
}

// ---------------------------------------------------------------------------
// Regression tests for the #968 adversarial review (F1-F9). Each one failed on
// 0f785c19; the MinIO half of the same scenarios is in tests/objwatch_minio.rs.
// ---------------------------------------------------------------------------

/// F1: a dry run must record nothing. It used to feed a counting sink and then
/// record + save every object, so the next REAL run emitted nothing at all.
#[test]
fn f1_a_dry_run_records_nothing_and_the_real_run_still_emits_everything() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..3 {
        store.put(&format!("d{i}.txt"), "content");
    }
    let mut totals = CostTotals::default();
    let mut sink = RecordingSink::default();
    {
        let mut j = journal(dir.path());
        let dry = WatchOptions {
            dry_run: true,
            fetch: false,
            ..opts()
        };
        let r = poll_once(
            &store,
            &mut j,
            &dry,
            &mut sink,
            0,
            &mut totals,
            &never_stop(),
        )
        .unwrap();
        assert_eq!(r.added, 3, "the dry run reports what it would emit");
        assert!(sink.events.is_empty(), "and emits none of it");
        assert_eq!(store.gets(), 0, "and fetches nothing");
        assert!(j.is_empty(), "and records nothing in memory");
        assert_eq!(j.saves(), 0, "and writes no journal");
    }
    assert!(
        !WatchJournal::path_in(dir.path()).exists(),
        "no journal file may exist after a dry run on a fresh state dir"
    );
    // The spend it made IS recorded: those list calls were real.
    assert_eq!(SpendLedger::open(dir.path()).unwrap().spend.class_a, 1);

    let mut j = journal(dir.path());
    let r = poll_once(
        &store,
        &mut j,
        &opts(),
        &mut sink,
        0,
        &mut totals,
        &never_stop(),
    )
    .unwrap();
    assert_eq!(r.added, 3, "the real run after a dry run emits all 3");
    assert_eq!(sink.keys("added").len(), 3);
}

/// F2: the projection is priced at the page size the watch actually lists
/// with. 25 keys at --page-size 10 is 3 calls a cycle, not 1.
#[test]
fn f2_the_projection_uses_the_actual_page_size() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..25 {
        store.put(&format!("k{i:03}"), "x");
    }
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    for (page, calls) in [(10u64, 3u64), (1, 25), (1_000, 1)] {
        let mut j2 =
            WatchJournal::open(dir.path(), "http://memory", "fake", "", false, true).unwrap();
        let o = WatchOptions {
            page_size: page,
            fetch: false,
            allow_cost: true,
            ..opts()
        };
        let r = poll_once(
            &store,
            &mut j2,
            &o,
            &mut sink,
            0,
            &mut totals,
            &never_stop(),
        )
        .unwrap();
        assert_eq!(r.list_calls, calls, "page {page}: real calls");
        assert_eq!(
            r.projection.list_calls_per_cycle, r.list_calls,
            "page {page}: the projection must price what the store actually served"
        );
        assert_eq!(r.projection.monthly_class_a, calls * 8_640);
    }
    // And the guard acts on it: page size 1 at a budget the 1,000-key page fits.
    let o = WatchOptions {
        page_size: 1,
        fetch: false,
        max_monthly_class_a: 100_000,
        ..opts()
    };
    let err = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap_err();
    let refused = err.downcast_ref::<PollCostRefused>().expect("refusal");
    assert_eq!(refused.projection.list_calls_per_cycle, 25);
    assert_eq!(refused.projection.monthly_class_a, 216_000);
}

/// F4: an edit past --max-object-mb changes the ETag but not the fetched
/// prefix. It used to hash "identical" and be dropped without a trace.
#[test]
fn f4_a_change_past_the_byte_cap_is_emitted_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("big.bin", "AAAAAAAAAAXXXX");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        max_object_bytes: 10,
        ..opts()
    };
    let r0 = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r0.added, 1);
    assert_eq!(r0.truncated, 1);
    assert!(r0.warnings.iter().any(|w| w.contains("--max-object-mb")));
    // Same size, same first 10 bytes, different tail.
    store.put("big.bin", "AAAAAAAAAAZZZZ");
    sink.clear();
    let r1 = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(
        r1.changed,
        1,
        "the tail edit must be emitted: {}",
        r1.line()
    );
    assert_eq!(r1.content_identical, 0);
    assert_eq!(r1.truncated, 1);
    assert_eq!(sink.keys("changed"), vec!["big.bin"]);
    // And an untouched capped object still costs nothing on the next cycle.
    sink.clear();
    let r2 = poll_once(&store, &mut j, &o, &mut sink, 2, &mut totals, &never_stop()).unwrap();
    assert_eq!((r2.changed, r2.gets), (0, 0));
}

/// F6: --append-only's first cycle lists the whole space once (the backfill).
/// Pricing that one-time scan as the recurring cost refused the mode the
/// refusal itself recommends.
#[test]
fn f6_append_only_backfill_is_not_refused_on_cycle_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..1_100 {
        store.put(&format!("log/{i:06}"), "x");
    }
    let mut j =
        WatchJournal::open(dir.path(), "http://memory", "fake", "log/", true, false).unwrap();
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    // 1 call x 8,640 cycles fits 10,000; the 2-page backfill projected as
    // recurring (17,280) does not.
    let o = WatchOptions {
        append_only: true,
        fetch: false,
        max_monthly_class_a: 10_000,
        ..opts()
    };
    let r0 = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop())
        .expect("the append-only backfill must not be refused");
    assert_eq!(r0.added, 1_100);
    assert_eq!(r0.list_calls, 2);
    assert_eq!(r0.projection.list_calls_per_cycle, 1);
    assert_eq!(r0.projection.monthly_class_a, 8_640);
    assert!(r0.warnings.iter().any(|w| w.contains("backfill")));
    let r1 = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap();
    assert_eq!(r1.list_calls, 1);
    assert_eq!(r1.projection.list_calls_per_cycle, 1);
}

/// F8, projection breaker: a bucket that grows past the budget while it is
/// being watched STOPS the watch. It used to warn and keep spending.
#[test]
fn f8_a_bucket_that_grows_past_the_budget_trips_the_breaker_after_cycle_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("g/seed", "x");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        fetch: false,
        max_monthly_class_a: 10_000,
        ..opts()
    };
    let r0 = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert!(!r0.projection.over_budget());
    for i in 0..1_100 {
        store.put(&format!("g/{i:06}"), "x");
    }
    let calls_before = store.list_calls();
    let err = poll_once(&store, &mut j, &o, &mut sink, 1, &mut totals, &never_stop()).unwrap_err();
    let refused = err
        .downcast_ref::<PollCostRefused>()
        .expect("a breaker trip, not a warning");
    assert_eq!(refused.reason, RefusalReason::ProjectedOverBudget);
    assert_eq!(refused.cycle, 1);
    assert_eq!(refused.projection.list_calls_per_cycle, 2);
    assert!(refused.to_string().contains("circuit breaker"), "{refused}");
    assert_eq!(refused.to_json()["exit_code"], 4);
    assert_eq!(
        sink.events.len(),
        1,
        "nothing from the grown bucket was emitted"
    );
    assert_eq!(
        store.list_calls() - calls_before,
        2,
        "the listing that measured it"
    );

    // run_watch propagates it instead of sleeping and polling again.
    let o = WatchOptions {
        poll_interval: Duration::from_millis(1),
        max_cycles: Some(10),
        ..o
    };
    let dir2 = tempfile::tempdir().unwrap();
    let mut j = journal(dir2.path());
    let before = store.list_calls();
    let err = run_watch(&store, &mut j, &o, &mut sink, &|| false, &mut |_| {}).unwrap_err();
    assert!(err.downcast_ref::<PollCostRefused>().is_some());
    assert!(
        store.list_calls() - before <= 2,
        "one cycle's listing, then stop"
    );
}

/// F8, spend breaker: the month's budget is counted across restarts in the
/// state dir. A watcher restarted by a supervisor must not get a fresh budget
/// every time it starts.
#[test]
fn f8_the_monthly_spend_is_capped_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..30 {
        store.put(&format!("k{i:03}"), "x");
    }
    let mut sink = RecordingSink::default();
    // 30 keys at page size 10 = 3 calls a cycle. At one cycle a month the
    // projection (3) fits a budget of 10, so what stops the restart loop below
    // can only be the ledger of real spend.
    let o = WatchOptions {
        fetch: false,
        page_size: 10,
        max_monthly_class_a: 10,
        poll_interval: Duration::from_secs(cost::SECONDS_PER_MONTH),
        ..opts()
    };
    let mut spent_cycles = 0;
    let mut tripped = None;
    for restart in 0..10u64 {
        // A fresh process each time: new journal handle, new totals.
        let mut j = journal(dir.path());
        let mut totals = CostTotals::default();
        match poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()) {
            Ok(_) => spent_cycles += 1,
            Err(e) => {
                tripped = Some((restart, e));
                break;
            }
        }
    }
    let (restart, e) = tripped.expect("the ledger must stop a restart loop");
    let refused = e.downcast_ref::<PollCostRefused>().unwrap();
    assert_eq!(refused.reason, RefusalReason::ClassABudgetSpent);
    assert_eq!(
        spent_cycles, 3,
        "3 cycles x 3 calls = 9 of 10; the 4th needs 3 more"
    );
    assert_eq!(restart, 3);
    assert_eq!(refused.spent.class_a, 9);
    assert_eq!(store.list_calls(), 9, "not one call past the budget");
    assert_eq!(refused.to_json()["reason"], "monthly_class_a_budget_spent");
}

/// F8, Class B: GETs have their own breaker, checked before any fetch.
#[test]
fn f8_the_class_b_budget_refuses_before_fetching() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..5 {
        store.put(&format!("k{i}"), "x");
    }
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        max_monthly_class_b: 4,
        ..opts()
    };
    let err = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap_err();
    let refused = err.downcast_ref::<PollCostRefused>().unwrap();
    assert_eq!(refused.reason, RefusalReason::ClassBBudgetSpent);
    assert_eq!(refused.needed, 5);
    assert_eq!(store.gets(), 0);
    assert!(sink.events.is_empty());
    assert!(refused.to_string().contains("--no-fetch"));
}

/// F9: the first scan's journal writes are O(n), not one full rewrite per
/// object. 10,000 objects used to mean 10,000 fsynced rewrites — 17.95 s for
/// that scan against MinIO with the journal on ext4, 6.08 s on tmpfs, against
/// 0.19 s either way now. The saves count is the exact guard; the times are in
/// docs/WATCHING_OBJECT_STORAGE.md with their filesystems named.
#[test]
fn f9_a_large_first_scan_saves_the_journal_in_batches() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..10_000 {
        store.put(&format!("bulk/{i:06}"), "x");
    }
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let o = WatchOptions {
        fetch: false,
        ..opts()
    };
    let r = poll_once(&store, &mut j, &o, &mut sink, 0, &mut totals, &never_stop()).unwrap();
    assert_eq!(r.added, 10_000);
    // 10,000 / 256 batch saves + the time-based ones + the end-of-cycle one.
    assert!(
        j.saves() <= 10_000 / 256 + 1 + r.wall.as_secs() / 2 + 2,
        "{} journal saves for 10,000 objects",
        j.saves()
    );
    // Nothing lost: every object is in the saved file.
    let re = WatchJournal::open(dir.path(), "http://memory", "fake", "", false, false).unwrap();
    assert_eq!(re.len(), 10_000);
}

/// F9: GETs run with bounded concurrency, and events still come out in listing
/// order with the right digest per key.
#[test]
fn f9_parallel_fetches_keep_listing_order_and_per_key_digests() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..100 {
        store.put(&format!("p/{i:03}"), &format!("body-{i}"));
    }
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let mut totals = CostTotals::default();
    let r = poll_once(
        &store,
        &mut j,
        &opts(),
        &mut sink,
        0,
        &mut totals,
        &never_stop(),
    )
    .unwrap();
    assert_eq!(r.added, 100);
    assert_eq!(r.gets, 100);
    assert_eq!(store.gets(), 100);
    let keys = sink.keys("added");
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "events in listing order");
    for i in 0..100 {
        let want = format!(
            "{:016x}",
            xxhash_rust::xxh3::xxh3_64(format!("body-{i}").as_bytes())
        );
        assert_eq!(
            j.get(&format!("p/{i:03}")).unwrap().digest.as_deref(),
            Some(want.as_str())
        );
    }
}

/// One transient list failure (a 503 SlowDown) after the first cycle is
/// reported and retried at the next poll; it used to end a long-lived watch.
/// The failed call is still charged to the ledger.
#[test]
fn a_transient_list_failure_after_cycle_zero_is_retried_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    store.put("a", "x");
    store.fail_list_call.store(1, Ordering::SeqCst);
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let o = WatchOptions {
        poll_interval: Duration::from_millis(5),
        max_cycles: Some(4),
        allow_cost: true,
        ..opts()
    };
    let mut seen: Vec<(u64, usize)> = Vec::new();
    let out = run_watch(&store, &mut j, &o, &mut sink, &|| false, &mut |r| {
        seen.push((r.cycle, r.errors.len()))
    })
    .expect("one 503 must not end the watch");
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[1].1, 1, "cycle 1 is reported as failed: {seen:?}");
    assert_eq!(out.totals.failed_cycles, 1);
    assert_eq!(out.totals.cycles, 3, "three cycles completed");
    assert_eq!(SpendLedger::open(dir.path()).unwrap().spend.class_a, 4);

    // Persistent failure still ends it.
    let store = FakeStore::new();
    store.put("a", "x");
    let dir2 = tempfile::tempdir().unwrap();
    let mut j = journal(dir2.path());
    let o = WatchOptions {
        max_cycles: Some(50),
        ..o
    };
    struct AlwaysFailAfterFirst<'a>(&'a FakeStore);
    impl ObjectSource for AlwaysFailAfterFirst<'_> {
        fn describe(&self) -> String {
            self.0.describe()
        }
        fn list(&self, req: &ListRequest) -> Result<ListPage> {
            if self.0.list_calls() >= 1 {
                self.0.list_calls.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("503");
            }
            self.0.list(req)
        }
        fn get(&self, key: &str, max: u64) -> Result<FetchedObject> {
            self.0.get(key, max)
        }
    }
    let err = run_watch(
        &AlwaysFailAfterFirst(&store),
        &mut j,
        &o,
        &mut sink,
        &|| false,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("in a row"), "{err:#}");
    assert_eq!(store.list_calls(), 1 + MAX_CONSECUTIVE_FAILED_CYCLES);
}

/// B1 of the #968 verification: **the ledger must be on disk before the calls
/// it pays for.**
///
/// The blocker as reproduced: `scan()` may issue up to the whole remaining
/// monthly allowance in one cycle, and the spend was charged only after it
/// returned. Five `kill -9`s two seconds into a 2,001-call scan served 241 list
/// calls and never created `objwatch-spend.json`, so a supervisor restarting a
/// crashing watcher did get the fresh budget every time that the ledger is
/// there to deny.
///
/// This is the deterministic half: a hook on the store reads the ledger FILE
/// while the scan is still running, which is exactly what a crash at that
/// instant would leave behind. `tests/objwatch_minio.rs` has the other half,
/// with a real `kill -9`.
#[test]
fn a_scan_charges_the_ledger_before_the_calls_and_not_after() {
    let dir = tempfile::tempdir().unwrap();
    let store = FakeStore::new();
    for i in 0..200 {
        store.put(&format!("k{i:04}.txt"), "x");
    }
    // What a crash at list call N would have left in the ledger file.
    let seen: std::sync::Arc<Mutex<Vec<(u64, u64)>>> = Default::default();
    {
        let state = dir.path().to_path_buf();
        let seen = std::sync::Arc::clone(&seen);
        *store.on_list.lock().unwrap() = Some(Box::new(move |n| {
            let on_disk = std::fs::read_to_string(state.join(SpendLedger::FILE))
                .ok()
                .and_then(|raw| serde_json::from_str::<MonthSpend>(&raw).ok())
                .map(|s| s.class_a)
                .unwrap_or(0);
            seen.lock().unwrap().push((n, on_disk));
        }));
    }

    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    // One key per page: 200 objects is 201 list calls, the shape the blocker
    // was reproduced in.
    let o = WatchOptions {
        page_size: 1,
        fetch: false,
        allow_cost: true,
        ..opts()
    };
    let r = poll_once(
        &store,
        &mut j,
        &o,
        &mut sink,
        0,
        &mut CostTotals::default(),
        &never_stop(),
    )
    .unwrap();
    assert_eq!(r.list_calls, 200, "200 objects at one key per page");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 200);
    // The first call is paid for before it is made, so even a crash on call 0
    // owes something.
    assert!(
        seen[0].1 >= 1,
        "the ledger was empty when the first list call went out: {:?}",
        &seen[..1]
    );
    for (n, on_disk) in &seen {
        assert!(
            *on_disk > *n,
            "list call {n} went out with only {on_disk} operation(s) recorded on disk — a crash \
             here would lose {} of them",
            n + 1 - on_disk
        );
        assert!(
            *on_disk <= n + cost::CLASS_A_CHARGE_BATCH,
            "list call {n} had {on_disk} recorded: over-charged by more than one batch"
        );
    }
    // And a scan that finishes records EXACTLY what the store served: the
    // unused tail of the last reservation is refunded.
    assert_eq!(store.list_calls(), 200);
    assert_eq!(
        SpendLedger::open(dir.path()).unwrap().spend.class_a,
        200,
        "a completed cycle records what the store served, not the reservation"
    );
}

/// m3 of the #968 verification: a ledger stamped with a FUTURE month is a
/// backwards clock step, not a new allowance. It used to reset the spend to
/// zero, so a VM restored from a snapshot across a UTC month boundary minted a
/// fresh 200,000-operation budget.
#[test]
fn a_future_dated_ledger_does_not_mint_a_fresh_allowance() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SpendLedger::FILE),
        r#"{"month":"2099-01","class_a":200000,"class_b":7}"#,
    )
    .unwrap();
    let l = SpendLedger::open(dir.path()).unwrap();
    assert_eq!(l.spend.month, cost::current_month());
    assert_eq!(
        (l.spend.class_a, l.spend.class_b),
        (200_000, 7),
        "the recorded spend must be carried into the current month"
    );

    // And the watcher then refuses on it, which is the behaviour the reset
    // defeated.
    let store = FakeStore::new();
    store.put("a.md", "alpha");
    let mut j = journal(dir.path());
    let mut sink = RecordingSink::default();
    let o = WatchOptions {
        max_monthly_class_a: 200_000,
        ..opts()
    };
    let err = poll_once(
        &store,
        &mut j,
        &o,
        &mut sink,
        0,
        &mut CostTotals::default(),
        &never_stop(),
    )
    .unwrap_err();
    let refused = err.downcast_ref::<PollCostRefused>().expect("refused");
    assert_eq!(refused.reason, RefusalReason::ClassABudgetSpent);
    assert_eq!(store.list_calls(), 0, "refused before any call");

    // A PAST month is still a new allowance — that is the whole point of a
    // monthly budget.
    std::fs::write(
        dir.path().join(SpendLedger::FILE),
        r#"{"month":"1999-01","class_a":200000,"class_b":7}"#,
    )
    .unwrap();
    assert_eq!(SpendLedger::open(dir.path()).unwrap().spend.class_a, 0);
}
