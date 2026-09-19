//! Integration tests for the real S3-compatible backend.
//!
//! These need an endpoint. They are **skipped with a printed reason** when one
//! is not configured, because a silently-passing integration test is worse than
//! no integration test.
//!
//! ```sh
//! docker run -d --name xerj-minio -p 9000:9000 \
//!   -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
//!   quay.io/minio/minio:latest server /data
//! export XERJ_S3_TEST_ENDPOINT=http://127.0.0.1:9000
//! export XERJ_S3_TEST_BUCKET=xerj-test          # must already exist
//! export AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin
//! cargo test -p xerj-storage --test s3_object_store -- --nocapture
//! ```
//!
//! Point `XERJ_S3_TEST_ENDPOINT` at Cloudflare R2 and they pass there too —
//! but read the operation counts each test prints first. R2's free tier allows
//! 1,000,000 Class A operations per month per account, and
//! `list_pagination_crosses_the_1000_key_page_boundary` alone writes 1,200
//! objects (1,200 Class A). Run that one against MinIO, where requests are
//! free, and keep R2 for a hand-checked pass over the cheap tests.
//!
//! Every test uses its own key prefix and deletes what it wrote
//! (`DeleteObject` is free on R2), so a run leaves the bucket as it found it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use xerj_storage::backend::{OpBudget, StorageBackend};
use xerj_storage::cache::SegmentCache;
use xerj_storage::s3::{RetryPolicy, S3Backend, S3Config};

/// Build a backend for a test, or `None` with a printed reason.
///
/// `tag` becomes part of the key prefix so tests running concurrently cannot
/// collide, and so anything left behind by a crashed run is identifiable.
fn backend(tag: &str) -> Option<S3Backend> {
    let endpoint = match std::env::var("XERJ_S3_TEST_ENDPOINT") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            println!(
                "SKIP: XERJ_S3_TEST_ENDPOINT is not set. \
                 Start MinIO and set it to http://127.0.0.1:9000 to run this test."
            );
            return None;
        }
    };
    let bucket = match std::env::var("XERJ_S3_TEST_BUCKET") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            println!("SKIP: XERJ_S3_TEST_BUCKET is not set (the bucket must already exist).");
            return None;
        }
    };
    if std::env::var("AWS_ACCESS_KEY_ID").is_err()
        || std::env::var("AWS_SECRET_ACCESS_KEY").is_err()
    {
        println!(
            "SKIP: AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY are not in the environment. \
             XERJ reads object-store credentials from the environment only."
        );
        return None;
    }

    let region = std::env::var("XERJ_S3_TEST_REGION").unwrap_or_else(|_| "auto".into());
    let prefix = format!(
        "{}{tag}",
        std::env::var("XERJ_S3_TEST_PREFIX").unwrap_or_else(|_| "xerj-it/".into())
    );
    match S3Backend::connect(
        S3Config::new(bucket)
            .with_prefix(prefix)
            .with_endpoint(endpoint)
            .with_region(region),
    ) {
        Ok(b) => Some(b),
        Err(e) => {
            println!("SKIP: could not build the S3 client: {e}");
            None
        }
    }
}

/// Remove every key under the backend's prefix. Deletes are free on R2, so a
/// test has no excuse for leaving objects behind.
///
/// Concurrent in batches of 16 because a serial sweep of 1,200 keys over a WAN
/// takes about ten minutes — long enough that a test would be killed before it
/// finished cleaning up, which is the one failure mode that leaves objects
/// behind in a metered bucket.
async fn clean(b: &S3Backend) {
    let Ok(keys) = b.list("").await else { return };
    for chunk in keys.chunks(16) {
        let mut handles = Vec::with_capacity(chunk.len());
        for key in chunk {
            let b = b.clone();
            let key = key.clone();
            handles.push(tokio::spawn(async move { b.delete(&key).await }));
        }
        for handle in handles {
            let _ = handle.await;
        }
    }
}

#[tokio::test]
async fn round_trip_range_read_metadata_and_delete() {
    let Some(b) = backend("roundtrip/") else {
        return;
    };
    clean(&b).await;

    // "hello object storage": bytes 6..=11 are "object", the same range the R2
    // probe verified by hand (Range: bytes=6-11 -> 206 + Content-Range).
    let body = b"hello object storage";
    let key = "docs/alpha.txt";

    b.write(key, body).await.expect("write");
    assert!(b.exists(key).await.expect("exists"));

    let meta = b.metadata(key).await.expect("metadata");
    assert_eq!(meta.size, body.len() as u64, "HeadObject size");
    assert!(
        meta.etag.as_deref().is_some_and(|e| !e.contains('"')),
        "ETag must be present and unquoted, got {:?}",
        meta.etag
    );
    assert!(
        meta.created.is_none(),
        "object stores have no creation time; reporting one would be a fabrication"
    );

    let ranged = b.read_range(key, 6, 6).await.expect("ranged read");
    assert_eq!(&ranged[..], b"object", "ranged GetObject must be exact");

    let whole = b.read_range(key, 0, u64::MAX).await.expect("whole read");
    assert_eq!(&whole[..], body);

    let tail = b.read_range(key, 6, u64::MAX).await.expect("open-ended");
    assert_eq!(&tail[..], b"object storage");

    // A zero-length read must not cost a request.
    let before = b.ops().unwrap().class_b;
    assert!(b.read_range(key, 0, 0).await.unwrap().is_empty());
    assert_eq!(
        b.ops().unwrap().class_b,
        before,
        "a zero-length read must not send a billed GetObject"
    );

    b.delete(key).await.expect("delete");
    assert!(!b.exists(key).await.expect("exists after delete"));
    // Deleting an absent key is not an error, matching S3 itself.
    b.delete(key).await.expect("idempotent delete");

    println!("round_trip ops: {:?}", b.ops().unwrap());
    clean(&b).await;
}

#[tokio::test]
async fn missing_key_reads_as_not_found() {
    let Some(b) = backend("missing/") else { return };

    let err = b
        .read_range("definitely/absent.bin", 0, 16)
        .await
        .expect_err("a missing key must be an error");
    match err {
        xerj_storage::StorageError::Io(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::NotFound,
            "a missing object must surface as NotFound, the same as a missing local file"
        ),
        other => panic!("expected Io(NotFound), got {other:?}"),
    }

    let err = b
        .metadata("definitely/absent.bin")
        .await
        .expect_err("HeadObject on a missing key");
    assert!(matches!(err, xerj_storage::StorageError::Io(ref e)
        if e.kind() == std::io::ErrorKind::NotFound));

    assert!(!b.exists("definitely/absent.bin").await.unwrap());
}

#[tokio::test]
async fn list_strips_the_prefix_so_its_output_round_trips() {
    let Some(b) = backend("listprefix/") else {
        return;
    };
    clean(&b).await;

    b.write("segments/a.seg", b"A").await.unwrap();
    b.write("segments/b.seg", b"B").await.unwrap();
    b.write("other/c.seg", b"C").await.unwrap();

    let mut scoped = b.list("segments/").await.unwrap();
    scoped.sort();
    assert_eq!(scoped, vec!["segments/a.seg", "segments/b.seg"]);

    // The contract that matters: a key from list() is accepted by read_range()
    // unchanged. Returning prefixed keys here is the bug this asserts against.
    for key in &scoped {
        let got = b.read_range(key, 0, u64::MAX).await.unwrap_or_else(|e| {
            panic!("list() output must feed straight into read_range(): {key}: {e}")
        });
        assert_eq!(got.len(), 1);
    }

    assert_eq!(b.list("").await.unwrap().len(), 3);
    clean(&b).await;
}

/// The real reason to have a real endpoint: `ListObjectsV2` pages at 1,000
/// keys, and the simulation does not.
///
/// 1,200 objects of ~200 bytes each. On MinIO that is free. On R2 it is 1,200
/// Class A operations for the writes plus 2 for the two list pages, which is
/// why it is gated behind its own variable.
#[tokio::test]
async fn list_pagination_crosses_the_1000_key_page_boundary() {
    if std::env::var("XERJ_S3_TEST_PAGINATION").is_err() {
        println!(
            "SKIP: set XERJ_S3_TEST_PAGINATION=1 to run the 1,200-object pagination test. \
             It costs 1,200 Class A operations, so run it against MinIO, not a metered bucket."
        );
        return;
    }
    let Some(b) = backend("pagination/") else {
        return;
    };
    clean(&b).await;

    const N: usize = 1_200;
    let payload = vec![b'x'; 200];
    // 16 at a time: enough to keep the connection pool busy, few enough that a
    // failure does not leave a thousand in-flight requests behind.
    let indices: Vec<usize> = (0..N).collect();
    for chunk in indices.chunks(16) {
        let mut set = Vec::new();
        for &i in chunk {
            let b = b.clone();
            let payload = payload.clone();
            set.push(tokio::spawn(async move {
                b.write(&format!("page/{i:05}.bin"), &payload).await
            }));
        }
        for handle in set {
            handle.await.unwrap().expect("write");
        }
    }

    let before = b.ops().unwrap();
    let keys = b.list("page/").await.expect("list");
    let after = b.ops().unwrap();

    assert_eq!(keys.len(), N, "every key must come back across both pages");
    assert_eq!(
        after.class_a - before.class_a,
        2,
        "1,200 keys is exactly two 1,000-key pages, so exactly two Class A operations"
    );
    // Keys must be unique: a mishandled continuation token repeats a page.
    let unique: std::collections::HashSet<_> = keys.iter().collect();
    assert_eq!(unique.len(), N, "continuation token must not repeat a page");
    assert!(keys.contains(&"page/00000.bin".to_string()));
    assert!(keys.contains(&"page/01199.bin".to_string()));

    println!(
        "pagination: {} keys in 2 Class A list operations",
        keys.len()
    );
    clean(&b).await;
}

/// The counters are the feature; this asserts they equal the requests made.
#[tokio::test]
async fn operation_counters_equal_the_requests_actually_sent() {
    let Some(b) = backend("counters/") else {
        return;
    };
    clean(&b).await;

    let base = b.ops().unwrap();

    b.write("k1", b"one").await.unwrap(); // Class A
    b.write("k2", b"two").await.unwrap(); // Class A
    let _ = b.read_range("k1", 0, u64::MAX).await.unwrap(); // Class B
    let _ = b.metadata("k2").await.unwrap(); // Class B
    let _ = b.exists("k1").await.unwrap(); // Class B (HeadObject)
    let _ = b.list("").await.unwrap(); // Class A, one page
    b.delete("k1").await.unwrap(); // free
    b.delete("k2").await.unwrap(); // free

    let ops = b.ops().unwrap();
    assert_eq!(ops.class_a - base.class_a, 3, "2 puts + 1 list page");
    assert_eq!(ops.class_b - base.class_b, 3, "1 get + 2 heads");
    assert_eq!(ops.free - base.free, 2, "2 deletes");
    assert_eq!(ops.bytes_written - base.bytes_written, 6, "3 + 3 bytes");
    assert_eq!(ops.bytes_read - base.bytes_read, 3);
    assert_eq!(ops.refused, 0);

    println!("counters: {ops:?}");
    clean(&b).await;
}

/// The circuit breaker: once the ceiling is reached, no further billed request
/// leaves the process.
#[tokio::test]
async fn budget_stops_spending_at_the_ceiling() {
    let Some(probe) = backend("budget/") else {
        return;
    };
    let cfg = S3Config::new(probe.bucket())
        .with_prefix(probe.prefix())
        .with_endpoint(std::env::var("XERJ_S3_TEST_ENDPOINT").unwrap())
        .with_region(std::env::var("XERJ_S3_TEST_REGION").unwrap_or_else(|_| "auto".into()))
        // Two Class A and one Class B for the whole process.
        .with_budget(OpBudget::new(2, 1));
    let b = S3Backend::connect(cfg).expect("connect");

    b.write("a", b"1")
        .await
        .expect("first put is within budget");
    b.write("b", b"2")
        .await
        .expect("second put is within budget");
    let err = b
        .write("c", b"3")
        .await
        .expect_err("third put must be refused, not sent");
    assert!(err.to_string().contains("budget exhausted"), "got: {err}");

    let _ = b.read_range("a", 0, u64::MAX).await.expect("first get");
    assert!(
        b.read_range("a", 0, u64::MAX).await.is_err(),
        "second get must be refused"
    );

    let ops = b.ops().unwrap();
    assert_eq!(
        ops.class_a, 2,
        "the refused put must not be counted as spent"
    );
    assert_eq!(ops.class_b, 1);
    assert_eq!(ops.refused, 2);

    // Deletes are free and must never be refused, so cleanup always works.
    b.delete("a").await.expect("free delete");
    b.delete("b").await.expect("free delete");
    println!("budget: {ops:?}");
    clean(&probe).await;
}

/// A failed attempt must be charged, because the provider charges it.
#[tokio::test]
async fn a_retried_attempt_is_charged_twice() {
    // No endpoint needed: nothing is listening on port 1, so every attempt is a
    // dispatch failure — retryable, and billed by a real provider each time it
    // reaches them.
    if std::env::var("AWS_ACCESS_KEY_ID").is_err() {
        println!("SKIP: AWS_ACCESS_KEY_ID is not set (any value will do for this test).");
        return;
    }
    let b = S3Backend::connect(
        S3Config::new("no-such-bucket")
            .with_endpoint("http://127.0.0.1:1")
            .with_retry(RetryPolicy {
                max_attempts: 3,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(2),
            }),
    )
    .expect("connect performs no I/O");

    let err = b.write("k", b"v").await.expect_err("port 1 refuses");
    assert!(
        err.to_string().contains("after 3 attempt(s)"),
        "the error must say how many attempts were paid for, got: {err}"
    );
    let ops = b.ops().unwrap();
    assert_eq!(ops.class_a, 3, "three wire attempts are three billed puts");
    assert_eq!(ops.retried, 2, "two of the three were retries");
}

/// Step 3 of the task: serve a segment byte range from the bucket through a
/// bounded local cache, and measure it.
#[tokio::test]
async fn read_through_cache_measured_cold_and_warm() {
    let Some(b) = backend("cache/") else { return };
    clean(&b).await;

    // 4 MiB stands in for a small flushed segment. Deliberately under the 8 MiB
    // per-object cap the R2 test budget sets.
    const SIZE: usize = 4 * 1024 * 1024;
    let segment: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    b.write("segments/seg-0001.seg", &segment).await.unwrap();

    let cache_dir = tempfile::tempdir().unwrap();
    let cache = SegmentCache::new(
        cache_dir.path(),
        // 16 MiB ceiling: bounded, and four times the object, so this test
        // measures fetching rather than eviction.
        16 * 1024 * 1024,
        Arc::new(b.clone()) as Arc<dyn StorageBackend>,
    );

    // A 64 KiB range from the middle — the shape of a real skip-list/doc-values
    // read, not a whole-segment scan.
    let (offset, length) = (1_000_000u64, 65_536u64);
    let expected = &segment[offset as usize..(offset + length) as usize];

    let t0 = Instant::now();
    let cold = cache
        .get_range("segments/seg-0001.seg", offset, length)
        .await
        .unwrap();
    let cold_us = t0.elapsed().as_micros();
    assert_eq!(&cold[..], expected, "cold range read must be byte-exact");

    let t1 = Instant::now();
    let warm = cache
        .get_range("segments/seg-0001.seg", offset, length)
        .await
        .unwrap();
    let warm_us = t1.elapsed().as_micros();
    assert_eq!(
        &warm[..],
        expected,
        "warm range read must be byte-identical"
    );
    assert_eq!(&cold[..], &warm[..], "cold and warm must agree exactly");

    // Eight more warm reads at different offsets, all from the cached copy.
    for i in 0..8u64 {
        let off = i * 400_000;
        let got = cache
            .get_range("segments/seg-0001.seg", off, 4096)
            .await
            .unwrap();
        assert_eq!(&got[..], &segment[off as usize..off as usize + 4096]);
    }

    let stats = cache.stats();
    assert_eq!(stats.misses, 1, "only the first read may reach the bucket");
    assert_eq!(stats.hits, 9);
    assert_eq!(
        stats.bytes_fetched, SIZE as u64,
        "a miss fetches the whole object — the cache is whole-object granular"
    );
    assert_eq!(stats.cache_write_failures, 0);
    assert!((stats.hit_rate().unwrap() - 0.9).abs() < 1e-9);

    // The uncached path transfers only the range, at the same request cost.
    let before = b.ops().unwrap();
    let t2 = Instant::now();
    let probe = cache
        .get_range_uncached("segments/seg-0001.seg", offset, length)
        .await
        .unwrap();
    let uncached_us = t2.elapsed().as_micros();
    let after = b.ops().unwrap();
    assert_eq!(&probe[..], expected);
    assert_eq!(after.class_b - before.class_b, 1, "one ranged GetObject");
    assert_eq!(
        after.bytes_read - before.bytes_read,
        length,
        "an uncached range read must transfer the range, not the object"
    );

    println!("--- read-through cache, 4 MiB object, 64 KiB range ---");
    println!("cold range read (whole-object fetch): {cold_us} us");
    println!("warm range read (local):              {warm_us} us");
    println!("uncached ranged read (range only):    {uncached_us} us");
    println!(
        "hit rate: {:.3}  hits: {}  misses: {}",
        stats.hit_rate().unwrap(),
        stats.hits,
        stats.misses
    );
    println!(
        "bytes fetched from bucket: {}  bytes served local: {}",
        stats.bytes_fetched, stats.bytes_served_local
    );
    println!("backend ops: {:?}", cache.backend_ops().unwrap());

    // Eviction: the cache must stay under its ceiling.
    let small = SegmentCache::new(
        cache_dir.path().join("bounded"),
        1024 * 1024, // 1 MiB ceiling, 4 MiB object
        Arc::new(b.clone()) as Arc<dyn StorageBackend>,
    );
    let _ = small.get("segments/seg-0001.seg").await.unwrap();
    small.maybe_evict().await.unwrap();
    assert!(
        small.cache_size_bytes().await.unwrap() <= 1024 * 1024,
        "the cache must respect its byte ceiling after eviction"
    );

    clean(&b).await;
}

/// 966-m4: a missing BUCKET is an HTTP 404 too, and treating it as a missing
/// OBJECT hides a configuration mistake behind an empty store.
///
/// `list()` carries the provider's `NoSuchBucket` code in its response body and
/// is now classified as permanent rather than "not found" — so it errors, and
/// it is not retried (a bucket does not appear because we asked four times).
///
/// The `HeadObject` path (`exists`/`metadata`) genuinely cannot tell the two
/// apart: a HEAD response has no body, so there is no error code to read and
/// every 404 looks identical. That limitation is asserted here too, so it
/// cannot be discovered by surprise, and `docs/OBJECT_STORAGE.md` says to probe
/// with a `list()` at startup when it matters.
///
/// Costs two Class A/B requests and writes nothing.
#[tokio::test]
async fn a_missing_bucket_is_an_error_not_an_absent_object() {
    let Some(reference) = backend("missing-bucket") else {
        return;
    };
    // A bucket name that cannot exist on the endpoint under test, built from
    // the real one so the credentials and endpoint are unchanged.
    let absent = format!("{}-absent-xerj-test", reference.bucket());
    let Ok(backend) = S3Backend::connect(
        S3Config::new(absent)
            .with_prefix("xerj-it/missing-bucket/")
            .with_endpoint(std::env::var("XERJ_S3_TEST_ENDPOINT").unwrap())
            .with_region(std::env::var("XERJ_S3_TEST_REGION").unwrap_or_else(|_| "auto".into()))
            .with_retry(RetryPolicy::none()),
    ) else {
        println!("SKIP: could not build a client for the absent bucket");
        return;
    };

    let err = backend
        .list("")
        .await
        .expect_err("listing a bucket that does not exist must fail");
    println!("missing bucket: {err}");
    assert!(
        err.to_string().contains("NoSuchBucket"),
        "the provider's own code must survive: {err}"
    );
    assert!(
        err.to_string().contains("not retryable"),
        "a missing bucket must not be retried: {err}"
    );

    // The documented limitation, pinned: HEAD cannot distinguish.
    assert_eq!(
        backend.exists("anything.bin").await.ok(),
        Some(false),
        "if this ever starts erroring, HeadObject gained a distinguishable \
         response and docs/OBJECT_STORAGE.md's note should be removed"
    );

    let ops = backend.ops().expect("an S3 backend has counters");
    assert_eq!(ops.class_a, 1, "{ops:?}");
    assert_eq!(ops.class_b, 1, "{ops:?}");
    assert_eq!(ops.retried, 0, "{ops:?}");
}
