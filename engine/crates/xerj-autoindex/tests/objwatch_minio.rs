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
    // panicking cleanup would hide the real assertion failure. Every page, so
    // the 10,000-object test leaves nothing behind either.
    let mut token: Option<String> = None;
    loop {
        let Ok(page) = src.list(&objwatch::ListRequest {
            prefix,
            continuation_token: token.as_deref(),
            start_after: None,
            delimiter: None,
            max_keys: 1000,
        }) else {
            return;
        };
        let keys: Vec<String> = page.objects.into_iter().map(|o| o.key).collect();
        std::thread::scope(|s| {
            for chunk in keys.chunks(keys.len().div_ceil(16).max(1)) {
                s.spawn(move || {
                    for k in chunk {
                        let _ = src.delete_object(k);
                    }
                });
            }
        });
        match page.next_token {
            Some(t) => token = Some(t),
            None => return,
        }
    }
}

/// PUT `n` objects with 16 writers, so seeding a 10,000-object prefix takes
/// seconds rather than minutes.
fn seed(src: &S3Source, keys: &[String], body: &[u8]) -> Result<()> {
    let failures = AtomicU64::new(0);
    std::thread::scope(|s| {
        for chunk in keys.chunks(keys.len().div_ceil(16).max(1)) {
            let failures = &failures;
            s.spawn(move || {
                for k in chunk {
                    if src.put_object(k, body).is_err() {
                        failures.fetch_add(1, Ordering::SeqCst);
                    }
                }
            });
        }
    });
    let f = failures.load(Ordering::SeqCst);
    anyhow::ensure!(f == 0, "{f} PUT(s) failed while seeding");
    Ok(())
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
        max_monthly_class_b: u64::MAX,
        allow_cost: false,
        page_size: 1000,
        status_path: None,
        dry_run: false,
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

// ---------------------------------------------------------------------------
// Regression tests for the #968 adversarial review, against a real server.
// Ported from the reviewer's repro files (rv_objwatch_attack.rs,
// rv_trailing_key.rs, rv_xml_keys.rs, rv_transient.rs); each failed on
// 0f785c19.
// ---------------------------------------------------------------------------

fn journal_for(env: &MinioEnv, dir: &std::path::Path, prefix: &str, append: bool) -> WatchJournal {
    WatchJournal::open(dir, &env.endpoint, &env.bucket, prefix, append, false).unwrap()
}

/// F1, end to end through the CLI: `--dry-run` then a real `--once` on the same
/// state dir. The real run must emit every object.
#[test]
fn f1_a_dry_run_does_not_poison_the_next_real_run() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "dryrun")?;
    for i in 0..3 {
        src.put_object(&format!("{prefix}d{i}.txt"), b"content")?;
    }
    // The CLI reads credentials from the environment only.
    std::env::set_var("AWS_ACCESS_KEY_ID", &env.creds.access_key_id);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", &env.creds.secret_access_key);
    let dir = tempfile::tempdir()?;
    let state = dir.path().join("state");
    let run = |extra: &[&str], feed: &std::path::Path| -> i32 {
        let mut args: Vec<String> = vec![
            format!("s3://{}/{prefix}", env.bucket),
            "--watch".into(),
            "--endpoint-url".into(),
            env.endpoint.clone(),
            "--state-dir".into(),
            state.display().to_string(),
            "--events-out".into(),
            feed.display().to_string(),
            "--quiet".into(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        match xerj_autoindex::cli::parse(args) {
            Ok(xerj_autoindex::cli::Cmd::Watch(cfg)) => {
                xerj_autoindex::objwatch::run::run(*cfg).expect("watch run")
            }
            other => panic!("expected a watch, got {other:?}"),
        }
    };
    let lines = |p: &std::path::Path| {
        std::fs::read_to_string(p)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    };
    let feed1 = dir.path().join("dry.jsonl");
    assert_eq!(run(&["--dry-run"], &feed1), 0);
    assert_eq!(lines(&feed1), 0, "a dry run emits nothing");
    assert!(
        !state.join("objwatch.json").exists(),
        "a dry run writes no journal"
    );
    let feed2 = dir.path().join("real.jsonl");
    assert_eq!(run(&["--once"], &feed2), 0);
    assert_eq!(lines(&feed2), 3, "the real run after a dry run emits all 3");
    cleanup(&src, &prefix);
    Ok(())
}

/// F2 on the wire: the projection prices what MinIO actually served.
#[test]
fn f2_the_projection_matches_the_calls_the_server_served() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "pagesize")?;
    let keys: Vec<String> = (0..26).map(|i| format!("{prefix}k{i:03}.txt")).collect();
    seed(&src, &keys, b"hello")?;
    for page in [1u64, 10, 1000] {
        let dir = tempfile::tempdir()?;
        let mut j = journal_for(&env, dir.path(), &prefix, false);
        let mut o = opts(300_000);
        o.page_size = page;
        o.fetch = false;
        let r = objwatch::poll_once(
            &src,
            &mut j,
            &o,
            &mut Recorder::default(),
            0,
            &mut CostTotals::default(),
            &|| false,
        )?;
        // 26 objects at `page` keys a page, plus the empty page a store serves
        // when the count divides exactly.
        let floor = 26u64.div_ceil(page);
        assert!(
            (floor..=floor + 1).contains(&r.list_calls),
            "page {page}: {} call(s) for 26 keys",
            r.list_calls
        );
        assert_eq!(
            r.projection.list_calls_per_cycle,
            r.list_calls,
            "page {page}: the projection must price what the server actually served: {}",
            r.line()
        );
    }
    cleanup(&src, &prefix);
    Ok(())
}

/// F4 on the wire: rewrite only the tail of a 2 MB object past a 1 MB cap.
#[test]
fn f4_an_edit_past_the_byte_cap_is_emitted_on_a_real_server() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "trunc")?;
    let key = format!("{prefix}big.bin");
    let mut v1 = vec![b'A'; 2 << 20];
    v1.extend_from_slice(&[b'X'; 1024]);
    src.put_object(&key, &v1)?;
    let dir = tempfile::tempdir()?;
    let mut j = journal_for(&env, dir.path(), &prefix, false);
    let mut o = opts(300_000);
    o.max_object_bytes = 1 << 20;
    let mut sink = Recorder::default();
    let seen = sink.shared();
    let mut totals = CostTotals::default();
    let r0 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 0, &mut totals, &|| false)?;
    assert_eq!((r0.added, r0.truncated), (1, 1));
    let mut v2 = vec![b'A'; 2 << 20];
    v2.extend_from_slice(&[b'Z'; 1024]);
    src.put_object(&key, &v2)?;
    let r1 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 1, &mut totals, &|| false)?;
    assert_eq!(r1.changed, 1, "{}", r1.line());
    assert_eq!(r1.content_identical, 0);
    let ev = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(ev.kind.as_str(), "changed");
    assert!(
        ev.truncated,
        "the event must say its digest covers a prefix"
    );
    cleanup(&src, &prefix);
    Ok(())
}

/// F5: every awkward key survives list -> journal -> GET, and a second cycle
/// is quiet (zero GETs, zero errors). Trailing spaces and spaces around `&`
/// used to be trimmed out of the listing, so the GET 404ed on every cycle.
#[test]
fn f5_awkward_keys_round_trip_list_and_get() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "keys")?;
    // 200, not 400: MinIO's filesystem backend refuses a path component longer
    // than the 255-byte filename limit, which is the server's rule, not ours.
    let long = "l".repeat(200);
    let keys = vec![
        format!("{prefix} leading.txt"),
        format!("{prefix}trailing .txt"),
        format!("{prefix}ends "),
        format!("{prefix}a & b.txt"),
        format!("{prefix}x & y"),
        format!("{prefix}a &amp; b"),
        format!("{prefix}amp&nospace.txt"),
        format!("{prefix}hash#frag.txt"),
        format!("{prefix}query?x=1.txt"),
        format!("{prefix}ünïcodé-文字.txt"),
        format!("{prefix}plus+sign.txt"),
        format!("{prefix}pct%20literal.txt"),
        format!("{prefix}lt<gt>.txt"),
        format!("{prefix}{long}.txt"),
        format!("{prefix}double//slash.txt"),
        format!("{prefix}zero.bin"),
    ];
    // A gateway may refuse a key outright (MinIO rejects some characters its
    // backend cannot store). That is its rule; the test asserts on the keys it
    // accepted, and insists on the ones this defect was about.
    let mut stored: Vec<String> = Vec::new();
    for k in &keys {
        let body: &[u8] = if k.ends_with("zero.bin") {
            b""
        } else {
            b"payload"
        };
        match src.put_object(k, body) {
            Ok(_) => stored.push(k.clone()),
            Err(e) => eprintln!("F5: this server refuses the key {k:?}: {e}"),
        }
    }
    for must in [
        "trailing .txt",
        "ends ",
        "a & b.txt",
        "x & y",
        " leading.txt",
    ] {
        assert!(
            stored.iter().any(|k| k == &format!("{prefix}{must}")),
            "the server must accept {must:?} for this regression to mean anything"
        );
    }
    let dir = tempfile::tempdir()?;
    let mut j = journal_for(&env, dir.path(), &prefix, false);
    let mut sink = Recorder::default();
    let mut totals = CostTotals::default();
    let o = opts(300_000);
    let r0 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 0, &mut totals, &|| false)?;
    assert!(r0.errors.is_empty(), "{:?}", r0.errors);
    assert_eq!(r0.added as usize, stored.len());
    let mut want = stored.clone();
    want.sort();
    let got: Vec<String> = j.objects.keys().cloned().collect();
    assert_eq!(
        got, want,
        "the journal holds exactly the keys that were PUT"
    );
    let r1 = objwatch::poll_once(&src, &mut j, &o, &mut sink, 1, &mut totals, &|| false)?;
    assert!(r1.errors.is_empty(), "{:?}", r1.errors);
    assert_eq!((r1.gets, r1.added, r1.changed, r1.deleted), (0, 0, 0, 0));

    // A dot-segment key cannot be addressed over HTTP. It is refused before a
    // request is sent, with a message that says why, not 404ed forever.
    let dots = format!("{prefix}dots/../up.txt");
    let err = src.get(&dots, 10).err().expect("refused").to_string();
    assert!(err.contains("cannot be addressed"), "{err}");
    cleanup(&src, &prefix);
    Ok(())
}

/// F6 on the wire: --append-only's 2-page backfill is not refused.
#[test]
fn f6_append_only_backfill_is_accepted_on_a_real_server() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "append")?;
    let keys: Vec<String> = (0..1_100).map(|i| format!("{prefix}a{i:06}")).collect();
    seed(&src, &keys, b"x")?;
    let dir = tempfile::tempdir()?;
    let mut j = journal_for(&env, dir.path(), &prefix, true);
    let mut o = opts(300_000);
    o.append_only = true;
    o.fetch = false;
    o.max_monthly_class_a = 10_000;
    let r = objwatch::poll_once(
        &src,
        &mut j,
        &o,
        &mut Recorder::default(),
        0,
        &mut CostTotals::default(),
        &|| false,
    )?;
    assert_eq!(r.added, 1_100);
    assert_eq!(r.list_calls, 2);
    assert_eq!(r.projection.monthly_class_a, 8_640);
    cleanup(&src, &prefix);
    Ok(())
}

/// F8 on the wire: a watched prefix that grows past the budget stops the watch
/// at the next cycle, through `run_watch`, with a decision request.
#[test]
fn f8_growth_past_the_budget_stops_a_running_watch() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "grow")?;
    src.put_object(&format!("{prefix}seed"), b"x")?;
    let dir = tempfile::tempdir()?;
    let mut j = journal_for(&env, dir.path(), &prefix, false);
    // 100 ms interval x 1 call = 25,920,000/month; the budget allows 1 page a
    // cycle and not 2.
    let mut o = opts(100);
    o.fetch = false;
    o.max_cycles = Some(20);
    o.max_monthly_class_a = 26_000_000;
    let keys: Vec<String> = (0..1_100).map(|i| format!("{prefix}g{i:06}")).collect();
    let grown = std::sync::atomic::AtomicBool::new(false);
    let mut cycles = 0u64;
    let res = objwatch::run_watch(
        &src,
        &mut j,
        &o,
        &mut Recorder::default(),
        &|| false,
        &mut |r| {
            cycles += 1;
            if r.cycle == 0 && !grown.swap(true, Ordering::SeqCst) {
                seed(&src, &keys, b"x").unwrap();
            }
        },
    );
    let err = res.expect_err("the breaker must stop the watch, not warn");
    let refused = err
        .downcast_ref::<objwatch::PollCostRefused>()
        .expect("a cost refusal");
    assert_eq!(refused.cycle, 1);
    assert_eq!(refused.projection.list_calls_per_cycle, 2);
    assert_eq!(cycles, 1, "no cycle ran after the trip");
    cleanup(&src, &prefix);
    Ok(())
}

/// One 503 on a later cycle's listing does not end a watch against a real
/// server (the reviewer's rv_transient.rs).
#[test]
fn a_transient_list_failure_is_survived_on_a_real_server() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let (src, prefix) = source(&env, "flaky")?;
    for i in 0..3 {
        src.put_object(&format!("{prefix}f{i}.txt"), b"body")?;
    }
    struct Flaky<'a> {
        inner: &'a S3Source,
        calls: AtomicU64,
    }
    impl ObjectSource for Flaky<'_> {
        fn describe(&self) -> String {
            self.inner.describe()
        }
        fn list(&self, req: &objwatch::ListRequest) -> Result<objwatch::ListPage> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                anyhow::bail!("ListObjectsV2 returned 503 Service Unavailable: SlowDown");
            }
            self.inner.list(req)
        }
        fn get(&self, key: &str, max: u64) -> Result<objwatch::FetchedObject> {
            self.inner.get(key, max)
        }
    }
    let flaky = Flaky {
        inner: &src,
        calls: AtomicU64::new(0),
    };
    let dir = tempfile::tempdir()?;
    let mut j = journal_for(&env, dir.path(), &prefix, false);
    let mut o = opts(200);
    o.max_cycles = Some(4);
    let out = objwatch::run_watch(
        &flaky,
        &mut j,
        &o,
        &mut Recorder::default(),
        &|| false,
        &mut |_| {},
    )?;
    assert_eq!(out.totals.failed_cycles, 1);
    assert_eq!(out.totals.cycles, 3);
    cleanup(&src, &prefix);
    Ok(())
}

/// F9: a 10,000-object first scan. The cause of the old quadratic cost was one
/// fsynced rewrite of the whole journal per object; the exact regression guard
/// is the SAVES count, not a time.
///
/// The times are printed and the wall-clock bound is deliberately loose,
/// because a shared runner is not a benchmark — and because the journal's
/// filesystem dominates the result when it is saved per object. The published
/// before/after pair was re-measured for the #968 remediation on both, on one
/// host and one MinIO container; see docs/WATCHING_OBJECT_STORAGE.md. Set
/// `XERJ_F9_STATE_BASE` to put the journal somewhere other than the default
/// temp directory (which is tmpfs on many Linux boxes, where fsync is nearly
/// free and a per-object save looks much cheaper than it is on a disk).
#[test]
fn f9_a_ten_thousand_object_first_scan_is_linear() -> Result<()> {
    let Some(env) = minio() else { return Ok(()) };
    let n: usize = std::env::var("XERJ_MINIO_BULK_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let state_base = std::env::var("XERJ_F9_STATE_BASE").ok();
    let (src, prefix) = source(&env, "bulk")?;
    let keys: Vec<String> = (0..n).map(|i| format!("{prefix}b{i:06}.txt")).collect();
    let t = std::time::Instant::now();
    seed(&src, &keys, b"bulk object body")?;
    eprintln!("F9 seeded {n} objects in {:.2}s", t.elapsed().as_secs_f64());

    for fetch in [false, true] {
        let dir = match &state_base {
            Some(base) => tempfile::Builder::new().prefix("f9-").tempdir_in(base)?,
            None => tempfile::tempdir()?,
        };
        let mut j = journal_for(&env, dir.path(), &prefix, false);
        let mut o = opts(300_000);
        o.fetch = fetch;
        let r = objwatch::poll_once(
            &src,
            &mut j,
            &o,
            &mut Recorder::default(),
            0,
            &mut CostTotals::default(),
            &|| false,
        )?;
        eprintln!(
            "F9 first scan fetch={fetch} journal_in={}: {} objects, {} list calls, {} GETs, {} \
             journal saves, wall {:.2}s",
            dir.path().display(),
            r.added,
            r.list_calls,
            r.gets,
            j.saves(),
            r.wall.as_secs_f64()
        );
        assert_eq!(r.added as usize, n);
        assert!(
            j.saves() <= (n as u64) / 256 + 2 + r.wall.as_secs() / 2,
            "{} saves",
            j.saves()
        );
        assert!(
            r.wall.as_secs() < 120,
            "a {n}-object first scan took {:.2}s",
            r.wall.as_secs_f64()
        );
    }
    let t = std::time::Instant::now();
    cleanup(&src, &prefix);
    eprintln!("F9 cleanup {:.2}s", t.elapsed().as_secs_f64());
    Ok(())
}

/// The child half of `b1_a_watcher_killed_mid_scan_still_owes_what_it_spent`:
/// one `--page-size 1` first scan over a 2,000-object prefix, which is 2,000
/// `ListObjectsV2` calls the parent can kill in the middle of.
fn b1_child(state: &str, prefix: &str) -> Result<()> {
    let env = minio().expect("the child inherits the parent's MinIO environment");
    let src = S3Source::new(
        &env.endpoint,
        "us-east-1",
        env.creds.clone(),
        S3Location {
            bucket: env.bucket.clone(),
            prefix: prefix.to_string(),
        },
        std::time::Duration::from_secs(30),
    )?;
    let dir = std::path::Path::new(state);
    let mut j = journal_for(&env, dir, prefix, false);
    let mut o = opts(300_000);
    o.fetch = false;
    o.page_size = 1;
    let r = objwatch::poll_once(
        &src,
        &mut j,
        &o,
        &mut Recorder::default(),
        0,
        &mut CostTotals::default(),
        &|| false,
    )?;
    eprintln!("B1CHILD-COMPLETED list_calls={}", r.list_calls);
    Ok(())
}

fn b1_ledger_class_a(path: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()?
        .get("class_a")?
        .as_u64()
}

/// B1 of the #968 verification, with a real `kill -9`.
///
/// The blocker: `scan()` may issue up to the whole remaining monthly allowance
/// in one cycle, and the ledger was charged only after it returned. The
/// verifier killed a 2,001-call scan five times — the store served 49, 98, 146,
/// 194 and 241 list calls and `objwatch-spend.json` was never created — so a
/// supervisor restarting a crashing watcher got a fresh budget every attempt,
/// which is the opposite of what the docs and the published answers page
/// promise ("that file survives a restart, so a supervisor that restarts a
/// crashing watcher cannot give it a fresh budget every minute").
///
/// This test re-executes its own test binary as a child, kills it with SIGKILL
/// while the first scan is still listing, and then asserts the two things the
/// claim needs: the ledger on disk owes what was spent, and a restart against
/// that ledger is refused instead of being handed a fresh allowance.
///
/// Before the fix it fails on the `< total_calls` assertion (the only value the
/// ledger ever holds is the whole finished scan) or on "the child finished
/// before the ledger recorded anything".
#[test]
fn b1_a_watcher_killed_mid_scan_still_owes_what_it_spent() -> Result<()> {
    if let Ok(state) = std::env::var("XERJ_B1_CHILD_STATE") {
        let prefix = std::env::var("XERJ_B1_CHILD_PREFIX").expect("child prefix");
        return b1_child(&state, &prefix);
    }
    let Some(env) = minio() else { return Ok(()) };
    let n: u64 = 2_000;
    let (src, prefix) = source(&env, "b1kill")?;
    let keys: Vec<String> = (0..n).map(|i| format!("{prefix}k{i:05}.txt")).collect();
    seed(&src, &keys, b"x")?;
    // One key per page: the scan is at LEAST `n` calls long (MinIO served
    // 2,001 for 2,000 keys), which is the window the kill has to land in.
    let scan_floor_calls = n;

    let dir = tempfile::tempdir()?;
    let ledger = dir.path().join("objwatch-spend.json");
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("b1_a_watcher_killed_mid_scan_still_owes_what_it_spent")
        .arg("--exact")
        .arg("--nocapture")
        .env("XERJ_B1_CHILD_STATE", dir.path())
        .env("XERJ_B1_CHILD_PREFIX", &prefix)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let at_kill = loop {
        if let Some(c) = b1_ledger_class_a(&ledger) {
            if c >= 32 {
                break c;
            }
        }
        if let Some(status) = child.try_wait()? {
            cleanup(&src, &prefix);
            anyhow::bail!(
                "the child finished ({status}) before the ledger ever showed a partial spend: \
                 the cost of a scan in flight is not on disk, so a crash loses it"
            );
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            cleanup(&src, &prefix);
            anyhow::bail!("the spend ledger never appeared while the scan was running");
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    child.kill()?; // SIGKILL
    let status = child.wait()?;
    let after = b1_ledger_class_a(&ledger).unwrap_or(0);
    eprintln!(
        "B1 killed at class_a={at_kill}, ledger after SIGKILL={after} of >={scan_floor_calls} \
         calls, child {status}"
    );

    assert!(
        status.code().is_none(),
        "the child was supposed to die by signal, not exit ({status})"
    );
    assert!(
        after >= 32,
        "the ledger held {after} after a kill mid-scan: the spend was lost"
    );
    assert!(
        after < scan_floor_calls,
        "the ledger only ever held the FINISHED scan ({after}, and the scan is at least \
         {scan_floor_calls} calls) — the spend of a scan IN FLIGHT never reached the disk, \
         which is the case B1 is about"
    );

    // And the claim itself: a restart does not get a fresh allowance. The
    // budget is set to exactly what the killed run already owes, so the next
    // cycle must refuse before issuing a call.
    let mut j = journal_for(&env, dir.path(), &prefix, false);
    let mut o = opts(300_000);
    o.fetch = false;
    o.page_size = 1;
    o.max_monthly_class_a = after;
    let err = objwatch::poll_once(
        &src,
        &mut j,
        &o,
        &mut Recorder::default(),
        0,
        &mut CostTotals::default(),
        &|| false,
    )
    .expect_err("a restart must not be handed a fresh budget");
    let refused = err
        .downcast_ref::<objwatch::PollCostRefused>()
        .expect("a cost refusal");
    assert_eq!(refused.spent.class_a, after);

    cleanup(&src, &prefix);
    Ok(())
}
