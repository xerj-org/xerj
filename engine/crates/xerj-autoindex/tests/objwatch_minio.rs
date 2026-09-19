//! `--watch` against a real S3-compatible server.
//!
//! The unit tests in `objwatch::tests` drive a `FakeStore`, which proves the
//! diff and the cost accounting but not the wire: SigV4 signing, the
//! `ListObjectsV2` XML a real gateway emits, continuation tokens, the ETag a
//! real multipart upload produces. This file is that half, and it is where all
//! the load and tight-interval testing belongs — the same scenarios against
//! Cloudflare R2 would spend Class A operations from a metered allowance for no
//! extra information.
//!
//! Skipped, with a printed reason, when no server is configured:
//!
//! ```sh
//! docker run -d --name minio -p 13370:9000 \
//!   -e MINIO_ROOT_USER=xerjtest -e MINIO_ROOT_PASSWORD=xerjtest12345 \
//!   quay.io/minio/minio:latest server /data
//! XERJ_MINIO_ENDPOINT=http://127.0.0.1:13370 \
//! XERJ_MINIO_ACCESS_KEY=xerjtest XERJ_MINIO_SECRET_KEY=xerjtest12345 \
//!   cargo test -p xerj-autoindex --test objwatch_minio
//! ```
//!
//! Credentials come from the environment, never from a literal in this file.

use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use xerj_autoindex::objwatch::journal::WatchJournal;
use xerj_autoindex::objwatch::s3::{Credentials, S3Location, S3Source};
use xerj_autoindex::objwatch::{
    self, ChangeEvent, ChangeSink, CostTotals, ObjectSource, WatchOptions,
};

/// What the tests need from the environment, or the reason they cannot run.
struct MinioEnv {
    endpoint: String,
    creds: Credentials,
    bucket: String,
}

fn minio() -> Option<MinioEnv> {
    let endpoint = std::env::var("XERJ_MINIO_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty());
    let key = std::env::var("XERJ_MINIO_ACCESS_KEY")
        .ok()
        .filter(|v| !v.is_empty());
    let secret = std::env::var("XERJ_MINIO_SECRET_KEY")
        .ok()
        .filter(|v| !v.is_empty());
    match (endpoint, key, secret) {
        (Some(endpoint), Some(access_key_id), Some(secret_access_key)) => Some(MinioEnv {
            endpoint,
            creds: Credentials {
                access_key_id,
                secret_access_key,
                session_token: None,
            },
            bucket: std::env::var("XERJ_MINIO_BUCKET")
                .unwrap_or_else(|_| "xerj-objwatch-test".to_string()),
        }),
        _ => {
            eprintln!(
                "SKIP: no S3-compatible server configured. Set XERJ_MINIO_ENDPOINT, \
                 XERJ_MINIO_ACCESS_KEY and XERJ_MINIO_SECRET_KEY (see this file's header for a \
                 one-line docker run). This test never runs against Cloudflare R2: the load and \
                 tight intervals here would spend Class A operations from a metered allowance."
            );
            None
        }
    }
}

/// A fresh key prefix per test, so the tests are independent and can run in
/// parallel against one server.
fn source(env: &MinioEnv, test: &str) -> Result<(S3Source, String)> {
    let prefix = format!(
        "objwatch-it/{test}-{}-{}/",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let src = S3Source::new(
        &env.endpoint,
        "us-east-1",
        env.creds.clone(),
        S3Location {
            bucket: env.bucket.clone(),
            prefix: prefix.clone(),
        },
        std::time::Duration::from_secs(30),
    )?;
    src.create_bucket()?;
    Ok((src, prefix))
}

fn cleanup(src: &S3Source, prefix: &str) {
    // Best effort: a leaked key in a throwaway MinIO bucket is noise, but a
    // panicking cleanup would hide the real assertion failure.
    if let Ok(page) = src.list(&objwatch::ListRequest {
        prefix,
        continuation_token: None,
        start_after: None,
        delimiter: None,
        max_keys: 1000,
    }) {
        for o in page.objects {
            let _ = src.delete_object(&o.key);
        }
    }
}

/// Records every event, in order, and can be told to be slow.
#[derive(Default)]
struct Recorder {
    events: Arc<Mutex<Vec<ChangeEvent>>>,
    delay: std::time::Duration,
}

impl Recorder {
    fn shared(&self) -> Arc<Mutex<Vec<ChangeEvent>>> {
        Arc::clone(&self.events)
    }
}

impl ChangeSink for Recorder {
    fn accept(&mut self, ev: &ChangeEvent) -> Result<()> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        self.events.lock().unwrap().push(ev.clone());
        Ok(())
    }
}

fn kinds(events: &[ChangeEvent], kind: &str) -> Vec<String> {
    let mut v: Vec<String> = events
        .iter()
        .filter(|e| e.kind.as_str() == kind)
        .map(|e| e.key.clone())
        .collect();
    v.sort();
    v
}

fn opts(interval_ms: u64) -> WatchOptions {
    WatchOptions {
        poll_interval: std::time::Duration::from_millis(interval_ms),
        max_cycles: Some(1),
        fetch: true,
        max_object_bytes: 8 << 20,
        append_only: false,
        // These tests are about correctness, not the budget, and every one of
        // them uses a sub-second interval that the production guard would (and
        // should) refuse. Raising the budget here rather than disabling the
        // guard keeps the guard itself under test in the unit suite.
        max_monthly_class_a: u64::MAX,
        allow_cost: false,
        page_size: 1000,
        status_path: None,
    }
}

fn digest_of(bytes: &[u8]) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(bytes))
}

/// First poll indexes everything; the second does zero GETs and exactly the
/// number of list calls the key count implies. This is the property the whole
/// cost model rests on: a quiet bucket must cost one list call per 1,000 keys
/// and nothing else.
#[test]
fn first_poll_indexes_everything_and_a_quiet_second_poll_costs_one_list_call() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "quiet")?;
    src.put_object(&format!("{prefix}a.md"), b"# alpha")?;
    src.put_object(&format!("{prefix}b.txt"), b"beta beta")?;

    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    let mut sink = Recorder::default();
    let seen = sink.shared();
    let mut totals = CostTotals::default();

    let r1 = objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        0,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(r1.added, 2, "first poll adds both objects: {}", r1.line());
    assert_eq!(r1.changed, 0);
    assert_eq!(r1.deleted, 0);
    assert_eq!(r1.list_calls, 1, "2 keys is one list page");
    assert_eq!(r1.gets, 2, "each added object is fetched once");
    assert_eq!(r1.bytes_fetched, 7 + 9);

    let r2 = objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        1,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(
        r2.gets,
        0,
        "a quiet poll must issue ZERO GETs: {}",
        r2.line()
    );
    assert_eq!(r2.list_calls, 1, "and exactly one list call");
    assert_eq!(r2.bytes_fetched, 0);
    assert_eq!(r2.unchanged, 2);
    assert_eq!(r2.added + r2.changed + r2.deleted, 0);
    assert_eq!(totals.list_calls, 2, "two cycles, two Class A operations");
    assert_eq!(totals.gets, 2, "and no Class B operation after the first");

    let events = seen.lock().unwrap().clone();
    assert_eq!(
        kinds(&events, "added"),
        vec![format!("{prefix}a.md"), format!("{prefix}b.txt")]
    );
    assert_eq!(events.len(), 2, "the quiet poll emitted nothing");
    let a = events.iter().find(|e| e.key.ends_with("a.md")).unwrap();
    assert_eq!(a.digest.as_deref(), Some(digest_of(b"# alpha").as_str()));

    cleanup(&src, &prefix);
    Ok(())
}

/// Changed, new and deleted, in one cycle, against a real gateway's ETags and
/// `LastModified` formatting.
#[test]
fn changed_new_and_deleted_objects_are_each_reported_once() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "cnd")?;
    src.put_object(&format!("{prefix}keep.txt"), b"unchanged")?;
    src.put_object(&format!("{prefix}edit.txt"), b"before")?;
    src.put_object(&format!("{prefix}gone.txt"), b"doomed")?;

    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    let mut sink = Recorder::default();
    let seen = sink.shared();
    let mut totals = CostTotals::default();
    let r1 = objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        0,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(r1.added, 3);
    seen.lock().unwrap().clear();

    src.put_object(&format!("{prefix}edit.txt"), b"after, and longer")?;
    src.put_object(&format!("{prefix}new.txt"), b"brand new")?;
    src.delete_object(&format!("{prefix}gone.txt"))?;

    let r2 = objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        1,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(r2.added, 1, "{}", r2.line());
    assert_eq!(r2.changed, 1, "{}", r2.line());
    assert_eq!(r2.deleted, 1, "{}", r2.line());
    assert_eq!(r2.unchanged, 1, "keep.txt was not re-read: {}", r2.line());
    assert_eq!(
        r2.gets, 2,
        "only the changed and the new object are fetched"
    );

    let events = seen.lock().unwrap().clone();
    assert_eq!(kinds(&events, "added"), vec![format!("{prefix}new.txt")]);
    assert_eq!(kinds(&events, "changed"), vec![format!("{prefix}edit.txt")]);
    assert_eq!(kinds(&events, "deleted"), vec![format!("{prefix}gone.txt")]);
    let edited = events.iter().find(|e| e.key.ends_with("edit.txt")).unwrap();
    assert_eq!(
        edited.reason, "etag",
        "a real gateway changes the ETag on a rewrite"
    );
    assert_eq!(
        edited.digest.as_deref(),
        Some(digest_of(b"after, and longer").as_str())
    );
    // The delete removed the journal entry, so a third poll is quiet again.
    assert!(j.get(&format!("{prefix}gone.txt")).is_none());
    let r3 = objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        2,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(r3.gets, 0, "{}", r3.line());
    assert_eq!(r3.added + r3.changed + r3.deleted, 0);

    cleanup(&src, &prefix);
    Ok(())
}

/// An object replaced by a real multipart upload. The ETag stops being an MD5
/// and becomes `<hex>-<parts>`; the watcher must notice the change and must not
/// try to read the ETag as a content hash.
#[test]
fn a_multipart_replacement_changes_the_etag_shape_and_is_re_extracted() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "multipart")?;
    let key = format!("{prefix}big.bin");
    // Single-part first, so the journal holds an MD5-shaped ETag.
    src.put_object(&key, b"small")?;

    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    let mut sink = Recorder::default();
    let seen = sink.shared();
    let mut totals = CostTotals::default();
    objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        0,
        &mut totals,
        &|| false,
    )?;
    let single = j.get(&key).unwrap().etag.clone();
    assert!(
        !single.contains('-'),
        "a single-part ETag has no part suffix: {single}"
    );
    seen.lock().unwrap().clear();

    // 5 MiB is S3's minimum non-final part size, and MinIO enforces it.
    let part = vec![b'x'; 5 * 1024 * 1024];
    let tail = b"tail".to_vec();
    let mut whole = part.clone();
    whole.extend_from_slice(&tail);
    src.put_object_multipart(&key, &[part, tail])?;

    let r = objwatch::poll_once(
        &src,
        &mut j,
        &opts(1000),
        &mut sink,
        1,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(r.changed, 1, "{}", r.line());
    assert_eq!(r.gets, 1);
    let after = j.get(&key).unwrap().etag.clone();
    assert!(
        after.contains('-'),
        "a multipart ETag carries its part count, and the watcher keeps it verbatim: {after}"
    );
    assert_ne!(after, single);
    let events = seen.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason, "etag");
    // The digest is of the bytes, NOT of the ETag — which for a multipart
    // object is not an MD5 of the content at all.
    assert_eq!(
        events[0].digest.as_deref(),
        Some(digest_of(&whole).as_str())
    );
    assert_eq!(events[0].size as usize, whole.len());

    cleanup(&src, &prefix);
    Ok(())
}

/// More than one page, against a real gateway's continuation tokens: the scan
/// must be complete (a short scan would report the tail as deleted) and cost
/// exactly one list call per page.
#[test]
fn a_multi_page_listing_is_complete_and_costs_one_call_per_page() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "pages")?;
    for i in 0..25 {
        src.put_object(
            &format!("{prefix}k{i:03}.txt"),
            format!("body {i}").as_bytes(),
        )?;
    }
    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    let mut sink = Recorder::default();
    let mut totals = CostTotals::default();
    let mut o = opts(1000);
    // Small pages so the pagination path runs without writing 1,001 objects.
    o.page_size = 10;
    let r = objwatch::poll_once(&src, &mut j, &o, &mut sink, 0, &mut totals, &|| false)?;
    assert_eq!(r.keys_listed, 25, "{}", r.line());
    assert_eq!(r.added, 25);
    assert_eq!(
        r.list_calls,
        3,
        "25 keys at 10 per page is 3 calls: {}",
        r.line()
    );
    // And nothing is reported deleted on the next pass, which is what a short
    // scan would have caused.
    let r2 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 1, &mut totals, &|| false)?;
    assert_eq!(r2.deleted, 0, "{}", r2.line());
    assert_eq!(r2.unchanged, 25);
    assert_eq!(r2.gets, 0);

    cleanup(&src, &prefix);
    Ok(())
}

/// `--no-fetch`: metadata-only change detection issues no GETs at all, on a
/// real server, including for a change.
#[test]
fn metadata_only_mode_never_gets_even_when_an_object_changes() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "nofetch")?;
    let key = format!("{prefix}x.txt");
    src.put_object(&key, b"one")?;
    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    let mut o = opts(1000);
    o.fetch = false;
    let mut sink = Recorder::default();
    let seen = sink.shared();
    let mut totals = CostTotals::default();
    let r1 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 0, &mut totals, &|| false)?;
    assert_eq!(r1.added, 1);
    assert_eq!(r1.gets, 0, "{}", r1.line());
    src.put_object(&key, b"two")?;
    let r2 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 1, &mut totals, &|| false)?;
    assert_eq!(r2.changed, 1, "{}", r2.line());
    assert_eq!(r2.gets, 0, "still zero Class B operations: {}", r2.line());
    assert_eq!(totals.bytes_fetched, 0);
    let events = seen.lock().unwrap().clone();
    assert!(
        events.iter().all(|e| e.digest.is_none()),
        "no bytes means no digest"
    );

    cleanup(&src, &prefix);
    Ok(())
}

/// A cycle that outlasts its own poll interval — the "poll overlaps a
/// still-running reindex" case. Cycles never overlap by construction, so what
/// this proves on a real server is the two observable properties: nothing is
/// indexed twice, and an object mutated while the slow cycle was running ends up
/// recorded at its NEWEST bytes rather than being lost.
#[test]
fn a_cycle_that_outruns_its_interval_neither_double_indexes_nor_loses_an_update() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "overrun")?;
    let a = format!("{prefix}a.txt");
    let b = format!("{prefix}b.txt");
    src.put_object(&a, b"a-v1")?;
    src.put_object(&b, b"b-v1")?;

    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    let mut o = opts(200);
    o.max_cycles = Some(3);
    // Each accepted object takes longer than the whole poll interval, so cycle
    // 0 cannot finish inside its slot.
    let mut sink = Recorder {
        events: Arc::new(Mutex::new(Vec::new())),
        delay: std::time::Duration::from_millis(400),
    };
    let seen = sink.shared();

    // Mutate `a` while the first cycle is still inside the sink.
    let mutator = {
        let endpoint = env.endpoint.clone();
        let creds = env.creds.clone();
        let bucket = env.bucket.clone();
        let prefix2 = prefix.clone();
        let a2 = a.clone();
        std::thread::spawn(move || -> Result<()> {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let s = S3Source::new(
                &endpoint,
                "us-east-1",
                creds,
                S3Location {
                    bucket,
                    prefix: prefix2,
                },
                std::time::Duration::from_secs(30),
            )?;
            s.put_object(&a2, b"a-v2-longer")?;
            Ok(())
        })
    };

    let cycles = AtomicU64::new(0);
    let outcome = objwatch::run_watch(&src, &mut j, &o, &mut sink, &|| false, &mut |_r| {
        cycles.fetch_add(1, Ordering::SeqCst);
    })?;
    mutator.join().expect("mutator thread")?;

    assert_eq!(cycles.load(Ordering::SeqCst), 3, "every cycle reported");
    assert!(
        outcome.totals.skipped_deadlines >= 1,
        "a cycle that takes 800ms with a 200ms interval must report the deadlines it passed \
         through, not silently stretch the interval: {:?}",
        outcome.totals
    );

    let events = seen.lock().unwrap().clone();
    let for_a: Vec<&ChangeEvent> = events.iter().filter(|e| e.key == a).collect();
    let for_b: Vec<&ChangeEvent> = events.iter().filter(|e| e.key == b).collect();
    assert_eq!(
        for_b.len(),
        1,
        "an untouched object is indexed exactly once: {for_b:?}"
    );
    assert!(
        (1..=2).contains(&for_a.len()),
        "the mutated object is indexed once for the version each cycle saw, never more: {for_a:?}"
    );
    // No lost update: whatever the interleaving was, the newest bytes are what
    // the last event and the journal hold.
    let newest = digest_of(b"a-v2-longer");
    assert_eq!(
        for_a.last().unwrap().digest.as_deref(),
        Some(newest.as_str()),
        "the last event for the mutated object must carry the NEWEST bytes"
    );
    assert_eq!(j.get(&a).unwrap().digest.as_deref(), Some(newest.as_str()));
    // And the third cycle, after everything settled, found nothing to do.
    let last = outcome.last.expect("a last cycle report");
    assert_eq!(
        last.added + last.changed + last.deleted,
        0,
        "{}",
        last.line()
    );
    assert_eq!(last.gets, 0, "{}", last.line());

    cleanup(&src, &prefix);
    Ok(())
}

/// A killed watcher must not re-index what it had already finished. The journal
/// is saved per accepted object, so reopening it from disk resumes rather than
/// restarting.
#[test]
fn a_journal_reopened_from_disk_does_not_re_index_what_was_already_accepted() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "resume")?;
    for i in 0..4 {
        src.put_object(&format!("{prefix}r{i}.txt"), format!("r{i}").as_bytes())?;
    }
    let dir = tempfile::tempdir()?;
    {
        let mut j = WatchJournal::open(
            dir.path(),
            &env.endpoint,
            &env.bucket,
            &prefix,
            false,
            false,
        )?;
        let mut sink = Recorder::default();
        let mut totals = CostTotals::default();
        let r = objwatch::poll_once(
            &src,
            &mut j,
            &opts(1000),
            &mut sink,
            0,
            &mut totals,
            &|| false,
        )?;
        assert_eq!(r.added, 4);
    }
    // A new process, same state dir.
    let mut j2 = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &prefix,
        false,
        false,
    )?;
    assert_eq!(j2.len(), 4, "the journal survived the process");
    let mut sink = Recorder::default();
    let mut totals = CostTotals::default();
    let r = objwatch::poll_once(
        &src,
        &mut j2,
        &opts(1000),
        &mut sink,
        0,
        &mut totals,
        &|| false,
    )?;
    assert_eq!(r.added, 0, "{}", r.line());
    assert_eq!(
        r.gets,
        0,
        "resuming costs zero Class B operations: {}",
        r.line()
    );
    assert_eq!(r.unchanged, 4);

    cleanup(&src, &prefix);
    Ok(())
}
