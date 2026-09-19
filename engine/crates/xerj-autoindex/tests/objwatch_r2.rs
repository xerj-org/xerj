//! One bounded run of the watcher against Cloudflare R2, through the CLI route.
//!
//! WHY THIS IS SEPARATE FROM `objwatch_minio.rs`, AND DELIBERATELY SMALL
//!
//! R2's free tier is 1,000,000 Class A operations a month for a whole account,
//! and `PutObject` is Class A just like `ListObjectsV2`. So every object this
//! file creates is spent from the same allowance the watcher is designed to
//! protect. It therefore does the minimum that R2 can tell us and MinIO cannot:
//! that SigV4 signs correctly against R2's endpoint, that R2's `ListObjectsV2`
//! XML and continuation tokens parse, and that R2's ETag and `LastModified`
//! drive the change decision the same way MinIO's do.
//!
//! Everything else — load, tight poll intervals, multipart, thousands of
//! objects — belongs in `objwatch_minio.rs`. Running those here would spend a
//! metered allowance for no extra information.
//!
//! HARD CAPS, enforced below and not merely documented:
//! at most [`MAX_OBJECTS`] objects, each under [`MAX_OBJECT_BYTES`], and the
//! test deletes everything it created before it returns (`DeleteObject` is
//! free). The ledger it prints is the number to quote.
//!
//! Skipped, with a printed reason, unless all of these are set:
//!
//! ```sh
//! XERJ_R2_ENDPOINT=https://ACCOUNT.r2.cloudflarestorage.com \
//! XERJ_R2_ACCESS_KEY=… XERJ_R2_SECRET_KEY=… \
//! XERJ_R2_BUCKET=my-test-bucket XERJ_R2_PREFIX=my-prefix/ \
//! XERJ_R2_SPEND_ACK=1 \
//!   cargo test -p xerj-autoindex --test objwatch_r2 -- --test-threads 1 --nocapture
//! ```
//!
//! `XERJ_R2_SPEND_ACK=1` is required because the other five can plausibly be
//! set by a CI environment; this one cannot be set by accident.

use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use xerj_autoindex::objwatch::journal::WatchJournal;
use xerj_autoindex::objwatch::s3::{Credentials, S3Location, S3Source};
use xerj_autoindex::objwatch::{
    self, ChangeEvent, ChangeSink, CostTotals, ObjectSource, WatchOptions,
};

/// Objects this file may create. 24 is two pages at `--page-size 10` plus a
/// remainder, which is what proves R2's continuation tokens without writing the
/// 1,001 objects a natural-page-size test would need.
const MAX_OBJECTS: usize = 24;
const MAX_OBJECT_BYTES: usize = 512;

struct R2 {
    endpoint: String,
    creds: Credentials,
    bucket: String,
    prefix: String,
}

fn r2() -> Option<R2> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    match (
        get("XERJ_R2_ENDPOINT"),
        get("XERJ_R2_ACCESS_KEY"),
        get("XERJ_R2_SECRET_KEY"),
        get("XERJ_R2_BUCKET"),
        get("XERJ_R2_SPEND_ACK"),
    ) {
        (Some(endpoint), Some(key), Some(secret), Some(bucket), Some(ack)) if ack == "1" => {
            Some(R2 {
                endpoint,
                creds: Credentials {
                    access_key_id: key,
                    secret_access_key: secret,
                    session_token: get("XERJ_R2_SESSION_TOKEN"),
                },
                bucket,
                prefix: get("XERJ_R2_PREFIX").unwrap_or_else(|| "objwatch-r2-test/".into()),
            })
        }
        _ => {
            eprintln!(
                "SKIP: no R2 target acknowledged. This test spends Class A operations from a \
                 real monthly allowance, so it needs XERJ_R2_ENDPOINT, XERJ_R2_ACCESS_KEY, \
                 XERJ_R2_SECRET_KEY, XERJ_R2_BUCKET and XERJ_R2_SPEND_ACK=1. Load and \
                 tight-interval testing belongs in objwatch_minio.rs, never here."
            );
            None
        }
    }
}

/// Every billable call this test makes, so the run can be quoted honestly.
#[derive(Default)]
struct Ledger {
    puts: AtomicU64,
    deletes: AtomicU64,
    bytes_written: AtomicU64,
}

impl Ledger {
    fn report(&self, list_calls: u64, gets: u64, bytes_read: u64) {
        println!(
            "R2 LEDGER: Class A = {} PutObject + {} ListObjectsV2 = {}; \
             Class B = {} GetObject; free = {} DeleteObject; \
             bytes written {} read {}",
            self.puts.load(Ordering::SeqCst),
            list_calls,
            self.puts.load(Ordering::SeqCst) + list_calls,
            gets,
            self.deletes.load(Ordering::SeqCst),
            self.bytes_written.load(Ordering::SeqCst),
            bytes_read,
        );
    }
}

fn put(src: &S3Source, led: &Ledger, key: &str, body: &[u8]) -> Result<()> {
    assert!(
        body.len() <= MAX_OBJECT_BYTES,
        "this test must not write an object over {MAX_OBJECT_BYTES} bytes to a metered bucket"
    );
    assert!(
        led.puts.load(Ordering::SeqCst) < (MAX_OBJECTS * 2) as u64,
        "put cap reached; refusing to spend more Class A operations"
    );
    src.put_object(key, body)?;
    led.puts.fetch_add(1, Ordering::SeqCst);
    led.bytes_written
        .fetch_add(body.len() as u64, Ordering::SeqCst);
    Ok(())
}

#[derive(Default)]
struct Recorder {
    events: Vec<ChangeEvent>,
}

impl ChangeSink for Recorder {
    fn accept(&mut self, ev: &ChangeEvent) -> Result<()> {
        self.events.push(ev.clone());
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

/// The whole R2 run, in one test so the ledger is one number and the cleanup
/// cannot be skipped by a second test failing first.
#[test]
fn a_bounded_watch_against_r2_detects_added_changed_and_deleted_across_pages() -> Result<()> {
    let Some(env) = r2() else { return Ok(()) };
    let led = Ledger::default();
    let src = S3Source::new(
        &env.endpoint,
        // R2 wants "auto"; this also proves the region an S3-only client would
        // have guessed wrong.
        "auto",
        env.creds.clone(),
        S3Location {
            bucket: env.bucket.clone(),
            prefix: env.prefix.clone(),
        },
        std::time::Duration::from_secs(30),
    )?;

    // Always clean up, even on a panic: leaving objects behind in a metered
    // bucket is the one failure mode this file must not have.
    let cleanup = || {
        let mut removed = 0u64;
        let mut token: Option<String> = None;
        loop {
            let page = match src.list(&objwatch::ListRequest {
                prefix: &env.prefix,
                continuation_token: token.as_deref(),
                start_after: None,
                delimiter: None,
                max_keys: 1000,
            }) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("R2 cleanup listing failed: {e}");
                    break;
                }
            };
            for o in &page.objects {
                if src.delete_object(&o.key).is_ok() {
                    removed += 1;
                    led.deletes.fetch_add(1, Ordering::SeqCst);
                }
            }
            match page.next_token {
                Some(t) => token = Some(t),
                None => break,
            }
        }
        println!(
            "R2 CLEANUP: deleted {removed} object(s) under {}",
            env.prefix
        );
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        run_scenario(&src, &env, &led)
    }));
    cleanup();
    match result {
        Ok(r) => r,
        Err(p) => std::panic::resume_unwind(p),
    }
}

fn run_scenario(src: &S3Source, env: &R2, led: &Ledger) -> Result<()> {
    // A prefix that is empty at the start, so the run is self-contained.
    for o in src
        .list(&objwatch::ListRequest {
            prefix: &env.prefix,
            continuation_token: None,
            start_after: None,
            delimiter: None,
            max_keys: 1000,
        })?
        .objects
    {
        src.delete_object(&o.key)?;
        led.deletes.fetch_add(1, Ordering::SeqCst);
    }

    for i in 0..MAX_OBJECTS {
        let body = format!("watchs3 r2 object {i:03}\n{}\n", "x".repeat(100 + i));
        put(
            src,
            led,
            &format!("{}k{i:03}.txt", env.prefix),
            body.as_bytes(),
        )?;
    }

    let dir = tempfile::tempdir()?;
    let mut j = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &env.prefix,
        false,
        false,
    )?;
    let mut sink = Recorder::default();
    let mut totals = CostTotals::default();
    let mut opts = WatchOptions {
        // The production default. The projection for 24 objects at 300 s is
        // 8,640 Class A operations a month, well inside the default budget, so
        // the guard is exercised in its accepting direction rather than switched
        // off.
        poll_interval: std::time::Duration::from_secs(300),
        max_cycles: Some(1),
        max_object_bytes: 1 << 20,
        // 10 keys a page: three pages over 24 objects, which is R2's
        // continuation-token path without writing 1,001 objects for it.
        page_size: 10,
        ..WatchOptions::default()
    };

    let first = objwatch::poll_once(src, &mut j, &opts, &mut sink, 0, &mut totals, &|| false)?;
    println!("R2 cycle 0: {}", first.line());
    assert_eq!(first.added as usize, MAX_OBJECTS, "{}", first.line());
    assert_eq!(first.keys_listed as usize, MAX_OBJECTS);
    assert_eq!(first.list_calls, 3, "24 keys at 10 a page is 3 calls on R2");
    assert_eq!(first.gets as usize, MAX_OBJECTS);
    assert!(
        !first.projection.over_budget(),
        "{}",
        first.projection.line()
    );

    // A quiet poll: the load-bearing cost property, now against R2's own ETags.
    sink.events.clear();
    let quiet = objwatch::poll_once(src, &mut j, &opts, &mut sink, 1, &mut totals, &|| false)?;
    println!("R2 cycle 1 (quiet): {}", quiet.line());
    assert_eq!(
        quiet.gets,
        0,
        "a quiet poll must issue ZERO GETs: {}",
        quiet.line()
    );
    assert_eq!(quiet.list_calls, 3);
    assert_eq!(quiet.unchanged as usize, MAX_OBJECTS);
    assert!(sink.events.is_empty());

    // One changed, one new, one deleted.
    put(
        src,
        led,
        &format!("{}k005.txt", env.prefix),
        b"EDITED on R2, and longer than before\n",
    )?;
    put(
        src,
        led,
        &format!("{}new999.txt", env.prefix),
        b"brand new on R2\n",
    )?;
    src.delete_object(&format!("{}k017.txt", env.prefix))?;
    led.deletes.fetch_add(1, Ordering::SeqCst);

    let changed = objwatch::poll_once(src, &mut j, &opts, &mut sink, 2, &mut totals, &|| false)?;
    println!("R2 cycle 2 (changed): {}", changed.line());
    assert_eq!(changed.added, 1, "{}", changed.line());
    assert_eq!(changed.changed, 1, "{}", changed.line());
    assert_eq!(changed.deleted, 1, "{}", changed.line());
    assert_eq!(
        changed.gets, 2,
        "only the new and the changed object are read"
    );
    assert_eq!(
        kinds(&sink.events, "added"),
        vec![format!("{}new999.txt", env.prefix)]
    );
    assert_eq!(
        kinds(&sink.events, "changed"),
        vec![format!("{}k005.txt", env.prefix)]
    );
    assert_eq!(
        kinds(&sink.events, "deleted"),
        vec![format!("{}k017.txt", env.prefix)]
    );
    let edited = sink
        .events
        .iter()
        .find(|e| e.key.ends_with("k005.txt"))
        .unwrap();
    assert_eq!(edited.reason, "etag", "R2 changes the ETag on a rewrite");

    // And the poll after that is quiet again.
    sink.events.clear();
    let settled = objwatch::poll_once(src, &mut j, &opts, &mut sink, 3, &mut totals, &|| false)?;
    println!("R2 cycle 3 (settled): {}", settled.line());
    assert_eq!(settled.gets, 0, "{}", settled.line());
    assert_eq!(settled.added + settled.changed + settled.deleted, 0);

    // --append-only on R2: one list call, whatever the prefix holds. This is the
    // lever the docs recommend, so it is proved on the metered store too.
    let mut j2 = WatchJournal::open(
        dir.path(),
        &env.endpoint,
        &env.bucket,
        &env.prefix,
        true,
        true,
    )?;
    opts.append_only = true;
    let tail = objwatch::poll_once(src, &mut j2, &opts, &mut sink, 0, &mut totals, &|| false)?;
    println!("R2 cycle 4 (append-only, fresh journal): {}", tail.line());
    // A fresh append-only journal has no `start-after`, so this first pass is a
    // full scan; what it proves is that the SECOND pass is one call.
    sink.events.clear();
    let tail2 = objwatch::poll_once(src, &mut j2, &opts, &mut sink, 1, &mut totals, &|| false)?;
    println!("R2 cycle 5 (append-only, tail only): {}", tail2.line());
    assert_eq!(
        tail2.list_calls,
        1,
        "append-only asks only for keys above the highest one seen: {}",
        tail2.line()
    );
    assert_eq!(
        tail2.keys_listed,
        0,
        "nothing was appended: {}",
        tail2.line()
    );
    assert_eq!(tail2.deleted, 0, "append-only never reports a delete");
    assert_eq!(
        tail2.projection.list_calls_per_cycle,
        1,
        "and it is PRICED at one call, not at the journal size: {}",
        tail2.projection.line()
    );

    led.report(totals.list_calls, totals.gets, totals.bytes_fetched);
    println!(
        "R2 watcher totals: {} list calls (Class A), {} GETs (Class B), {} bytes read",
        totals.list_calls, totals.gets, totals.bytes_fetched
    );
    Ok(())
}
