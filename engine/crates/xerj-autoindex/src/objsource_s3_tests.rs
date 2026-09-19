//! End-to-end coverage of the object-storage source against a real HTTP
//! endpoint that speaks S3, in-process and hermetic.
//!
//! Why a stub as well as the MinIO suite next door: this one runs in CI on every
//! commit, costs nothing, and can be made to do things a real store will not do
//! on request — return a 500 for one key, lose an object between the LIST and the
//! GET, hand back a multipart ETag, paginate past 1,000 keys. The MinIO suite
//! proves the same paths against a real S3 implementation (SigV4, chunked
//! transfer, genuine multipart uploads) and is skipped with a printed reason when
//! MinIO is not running.
//!
//! The stub does not verify signatures. That is what MinIO is for; here the
//! subject under test is XERJ's listing, change detection, mirror and reconcile
//! logic, and every request the SDK makes is signed regardless.

use crate::objsource::{
    materialize_from, ObjectManifest, ObjectRun, ObjectStoreSource, MANIFEST_FILE,
};
use crate::source::{DocSource, ObjectScheme, ObjectSpec};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Credentials for the stub. Fake, and set once: the SDK refuses to sign
/// without them, and the stub never looks.
fn ensure_credentials() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        if std::env::var("AWS_ACCESS_KEY_ID").is_err() {
            std::env::set_var("AWS_ACCESS_KEY_ID", "stub-access-key");
        }
        if std::env::var("AWS_SECRET_ACCESS_KEY").is_err() {
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "stub-secret-key");
        }
    });
}

#[derive(Default)]
struct StubState {
    /// key → (bytes, ETag exactly as the store would report it)
    ///
    /// The bytes are behind an `Arc` and are never copied to answer a request.
    /// This stub runs INSIDE the test process, and
    /// `a_large_object_streams_rather_than_buffering` asserts on the process's
    /// own RSS: a stub that cloned a 64 MiB body to write it would add 64 MiB
    /// of RSS itself and fail the test with the subject under test innocent.
    /// That is exactly what happened on CI (2026-09-19, run 35470348250) while
    /// the same test passed locally, because whether the clone reuses the freed
    /// arena or maps fresh pages is an allocator's choice, not a fact about
    /// XERJ.
    objects: BTreeMap<String, (std::sync::Arc<Vec<u8>>, String)>,
    list_requests: u64,
    get_requests: u64,
    /// Keys the store answers with a 500.
    fail_get: BTreeSet<String>,
    /// Keys that are listed and then not there — the LIST/GET race.
    missing_get: BTreeSet<String>,
    /// Answer this many more LIST requests with a 503 SlowDown before serving
    /// one. Every one of them is a billed request the caller must see.
    throttle_list: u64,
    /// Never stop throttling: the "retries exhausted" path.
    throttle_list_forever: bool,
}

struct S3Stub {
    url: String,
    state: Arc<Mutex<StubState>>,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl S3Stub {
    fn start() -> Self {
        ensure_credentials();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(StubState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let state = Arc::clone(&state);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for incoming in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = incoming else { continue };
                    let state = Arc::clone(&state);
                    // One connection per request: the stub answers with
                    // `Connection: close`, which keeps the parser trivial.
                    std::thread::spawn(move || {
                        let _ = serve(stream, &state);
                    });
                }
            })
        };
        Self {
            url: format!("http://{address}"),
            state,
            stop,
            address,
            thread: Some(thread),
        }
    }

    fn put(&self, key: &str, body: &[u8]) {
        let etag = format!("\"{:032x}\"", xxhash_rust::xxh3::xxh3_128(body));
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(key.to_string(), (std::sync::Arc::new(body.to_vec()), etag));
    }

    /// Store an object whose ETag is the multipart form (`…-N`), which is NOT an
    /// MD5 of the body and must still work as a change token.
    fn put_multipart(&self, key: &str, body: &[u8], parts: u32) {
        let etag = format!("\"{:032x}-{parts}\"", xxhash_rust::xxh3::xxh3_128(body));
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(key.to_string(), (std::sync::Arc::new(body.to_vec()), etag));
    }

    fn delete(&self, key: &str) {
        self.state.lock().unwrap().objects.remove(key);
    }

    /// Make the next `n` LIST requests answer 503 SlowDown (the code R2 and S3
    /// both use to throttle), or every one of them when `forever`.
    fn throttle_list(&self, n: u64, forever: bool) {
        let mut state = self.state.lock().unwrap();
        state.throttle_list = n;
        state.throttle_list_forever = forever;
    }

    fn fail_get(&self, key: &str) {
        self.state.lock().unwrap().fail_get.insert(key.to_string());
    }

    fn stop_failing_get(&self, key: &str) {
        self.state.lock().unwrap().fail_get.remove(key);
    }

    fn counters(&self) -> (u64, u64) {
        let state = self.state.lock().unwrap();
        (state.list_requests, state.get_requests)
    }

    fn reset_counters(&self) {
        let mut state = self.state.lock().unwrap();
        state.list_requests = 0;
        state.get_requests = 0;
    }
}

impl Drop for S3Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock `accept`.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn serve(stream: TcpStream, state: &Arc<Mutex<StubState>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    // Drain the headers; the stub needs none of them (it does not verify
    // signatures — see the module comment).
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default().to_string();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (target.as_str(), ""),
    };
    let mut writer = stream;
    let params: BTreeMap<String, String> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), percent_decode(v)),
            None => (kv.to_string(), String::new()),
        })
        .collect();
    // Path-style addressing: /<bucket>[/<key>]
    let trimmed = path.trim_start_matches('/');
    let (_bucket, key) = match trimmed.split_once('/') {
        Some((bucket, key)) => (bucket.to_string(), percent_decode(key)),
        None => (trimmed.to_string(), String::new()),
    };

    if params.get("list-type").map(String::as_str) == Some("2") {
        let prefix = params.get("prefix").cloned().unwrap_or_default();
        let max_keys: usize = params
            .get("max-keys")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let after = params
            .get("continuation-token")
            .cloned()
            .unwrap_or_default();
        let throttled = {
            let mut state = state.lock().unwrap();
            // Counted first: a throttled request is a request the store
            // answered, and the store bills for it.
            state.list_requests += 1;
            if state.throttle_list_forever {
                true
            } else if state.throttle_list > 0 {
                state.throttle_list -= 1;
                true
            } else {
                false
            }
        };
        if throttled {
            let body = "<?xml version=\"1.0\"?><Error><Code>SlowDown</Code><Message>Please reduce \
                        your request rate.</Message></Error>";
            return write_response(&mut writer, 503, "application/xml", body.as_bytes(), None);
        }
        let (page, truncated, next) = {
            let state = state.lock().unwrap();
            let matching: Vec<(String, std::sync::Arc<Vec<u8>>, String)> = state
                .objects
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix) && **k > after)
                .map(|(k, (body, etag))| (k.clone(), body.clone(), etag.clone()))
                .collect();
            let truncated = matching.len() > max_keys;
            let page: Vec<_> = matching.into_iter().take(max_keys).collect();
            let next = page.last().map(|(k, _, _)| k.clone()).unwrap_or_default();
            (page, truncated, next)
        };
        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult \
             xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
        );
        body.push_str(&format!(
            "<Name>bucket</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{}</MaxKeys>\
             <IsTruncated>{}</IsTruncated>",
            xml_escape(&prefix),
            page.len(),
            max_keys,
            truncated
        ));
        if truncated {
            body.push_str(&format!(
                "<NextContinuationToken>{}</NextContinuationToken>",
                xml_escape(&next)
            ));
        }
        for (key, bytes, etag) in &page {
            body.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>2026-09-19T10:00:00.000Z</LastModified>\
                 <ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                xml_escape(key),
                xml_escape(etag),
                bytes.len()
            ));
        }
        body.push_str("</ListBucketResult>");
        write_response(&mut writer, 200, "application/xml", body.as_bytes(), None)?;
        return Ok(());
    }

    // GetObject
    let (body, etag, fail, missing) = {
        let mut state = state.lock().unwrap();
        state.get_requests += 1;
        let fail = state.fail_get.contains(&key);
        let missing = state.missing_get.contains(&key);
        let found = state.objects.get(&key).cloned();
        (
            found.as_ref().map(|(b, _)| b.clone()),
            found.map(|(_, e)| e),
            fail,
            missing,
        )
    };
    if fail {
        let body = "<?xml version=\"1.0\"?><Error><Code>InternalError</Code><Message>injected \
                    failure</Message></Error>";
        return write_response(&mut writer, 500, "application/xml", body.as_bytes(), None);
    }
    match (body, etag) {
        (Some(body), Some(etag)) if !missing => write_response(
            &mut writer,
            200,
            "binary/octet-stream",
            &body[..],
            Some(&etag),
        ),
        _ => {
            let body = "<?xml version=\"1.0\"?><Error><Code>NoSuchKey</Code><Message>The \
                        specified key does not exist.</Message></Error>";
            write_response(&mut writer, 404, "application/xml", body.as_bytes(), None)
        }
    }
}

fn write_response(
    writer: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    etag: Option<&str>,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\nx-amz-request-id: stub\r\n",
        body.len()
    );
    if let Some(etag) = etag {
        head.push_str(&format!("ETag: {etag}\r\n"));
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

/// A run rooted on a temp dir, pointed at the stub.
struct Harness {
    stub: S3Stub,
    _state: tempfile::TempDir,
    run: ObjectRun,
    progress: std::sync::Arc<crate::progress::Progress>,
}

impl Harness {
    fn new(prefix: &str) -> Self {
        let stub = S3Stub::start();
        let state = tempfile::tempdir().unwrap();
        let spec = ObjectSpec {
            scheme: ObjectScheme::S3,
            bucket: "bucket".into(),
            prefix: prefix.to_string(),
            endpoint: Some(stub.url.clone()),
        };
        let mirror = state.path().join("object-cache").join(spec.slug());
        std::fs::create_dir_all(&mirror).unwrap();
        let run = ObjectRun {
            identity: spec.identity(),
            spec,
            mirror,
            manifest_path: state.path().join(MANIFEST_FILE),
        };
        Self {
            stub,
            _state: state,
            run,
            progress: crate::progress::Progress::silent(),
        }
    }

    fn source(&self) -> ObjectStoreSource {
        ObjectStoreSource::connect(&self.run.spec).unwrap()
    }

    fn materialize(&self) -> anyhow::Result<crate::objsource::MaterializeReport> {
        let source = self.source();
        materialize_from(
            &self.run,
            &source,
            &self.progress,
            4,
            crate::objsource::MaterializeMode::Fetch,
        )
    }

    fn mirror_text(&self, rel: &str) -> String {
        std::fs::read_to_string(self.run.mirror.join(rel)).unwrap()
    }

    fn mirror_rels(&self) -> Vec<String> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.run.mirror)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                out.push(
                    entry
                        .path()
                        .strip_prefix(&self.run.mirror)
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
        out.sort();
        out
    }
}

#[test]
fn first_run_fetches_every_object_and_a_second_run_fetches_none() {
    let h = Harness::new("docs/");
    h.stub.put("docs/alpha.md", b"# alpha\n");
    h.stub.put("docs/beta.txt", b"beta beta\n");
    h.stub.put("docs/sub/gamma.md", b"# gamma\n");
    // Outside the prefix: must never be listed, let alone fetched.
    h.stub.put("other/delta.md", b"# delta\n");

    let first = h.materialize().unwrap();
    assert_eq!(first.admitted, 3, "{first:?}");
    assert_eq!(first.downloaded, 3);
    assert_eq!(first.unchanged, 0);
    assert_eq!(first.list_requests, 1);
    assert_eq!(first.read_requests, 3);
    assert_eq!(
        h.mirror_rels(),
        ["docs/alpha.md", "docs/beta.txt", "docs/sub/gamma.md"]
            .iter()
            .map(|r| r.trim_start_matches("docs/").to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(h.mirror_text("alpha.md"), "# alpha\n");

    h.stub.reset_counters();
    let second = h.materialize().unwrap();
    assert_eq!(second.unchanged, 3, "{second:?}");
    assert_eq!(
        second.downloaded, 0,
        "an unchanged prefix downloads nothing"
    );
    assert_eq!(second.bytes_downloaded, 0);
    let (lists, gets) = h.stub.counters();
    assert_eq!(
        (lists, gets),
        (1, 0),
        "one LIST page, zero GETs — the whole point of ETag change detection"
    );
}

#[test]
fn a_changed_object_is_refetched_and_only_that_one() {
    let h = Harness::new("docs/");
    h.stub.put("docs/a.md", b"one\n");
    h.stub.put("docs/b.md", b"two\n");
    h.materialize().unwrap();

    h.stub.put("docs/a.md", b"one, revised\n");
    h.stub.reset_counters();
    let report = h.materialize().unwrap();
    assert_eq!(report.downloaded, 1, "{report:?}");
    assert_eq!(report.unchanged, 1);
    assert_eq!(h.mirror_text("a.md"), "one, revised\n");
    assert_eq!(h.stub.counters().1, 1, "exactly one GET");
}

#[test]
fn a_new_object_is_fetched_and_a_deleted_one_leaves_the_mirror() {
    let h = Harness::new("docs/");
    h.stub.put("docs/a.md", b"one\n");
    h.stub.put("docs/gone.md", b"temporary\n");
    h.materialize().unwrap();
    assert!(h.run.mirror.join("gone.md").exists());

    h.stub.delete("docs/gone.md");
    h.stub.put("docs/new.md", b"three\n");
    let report = h.materialize().unwrap();
    assert_eq!(report.downloaded, 1, "{report:?}");
    assert_eq!(report.removed, 1);
    assert_eq!(h.mirror_rels(), vec!["a.md", "new.md"]);
    // The manifest forgets it too, so the next run does not carry a ghost.
    let (manifest, _) = ObjectManifest::load(&h.run.manifest_path, &h.run.identity);
    assert!(!manifest.objects.contains_key("gone.md"));
}

#[test]
fn a_multipart_etag_is_a_change_token_like_any_other() {
    let h = Harness::new("");
    h.stub.put_multipart("big.bin", &vec![7u8; 4096], 3);
    let first = h.materialize().unwrap();
    assert_eq!(first.downloaded, 1);
    let (manifest, _) = ObjectManifest::load(&h.run.manifest_path, &h.run.identity);
    let record = &manifest.objects["big.bin"];
    assert!(
        record.etag.as_deref().unwrap().contains("-3"),
        "the multipart suffix is recorded verbatim, not parsed: {record:?}"
    );

    h.stub.reset_counters();
    let second = h.materialize().unwrap();
    assert_eq!(
        second.unchanged, 1,
        "a multipart ETag must not force a re-download"
    );
    assert_eq!(h.stub.counters().1, 0);

    // Re-upload with a different part count: same bytes, different ETag, so the
    // store says it changed and we believe the store.
    h.stub.put_multipart("big.bin", &vec![7u8; 4096], 5);
    let third = h.materialize().unwrap();
    assert_eq!(third.downloaded, 1);
}

#[test]
fn listing_pages_past_one_thousand_keys() {
    let h = Harness::new("bulk/");
    for i in 0..2_500u32 {
        h.stub
            .put(&format!("bulk/{i:05}.txt"), format!("row {i}\n").as_bytes());
    }
    let report = h.materialize().unwrap();
    assert_eq!(report.objects_listed, 2_500, "{report:?}");
    assert_eq!(report.admitted, 2_500);
    assert_eq!(report.downloaded, 2_500);
    assert_eq!(
        report.list_requests, 3,
        "2,500 keys is three LIST pages at the 1,000-key maximum — and three \
         class-A operations, which is what the cost line reports"
    );
    assert_eq!(h.mirror_rels().len(), 2_500);

    h.stub.reset_counters();
    let second = h.materialize().unwrap();
    assert_eq!(second.unchanged, 2_500);
    assert_eq!(h.stub.counters(), (3, 0));
}

#[test]
fn hidden_and_unportable_keys_cost_no_requests() {
    let h = Harness::new("");
    h.stub.put("ok.md", b"fine\n");
    h.stub.put(".env", b"SECRET=hunter2\n");
    h.stub.put(".git/config", b"[core]\n");
    h.stub
        .put("node_modules/react/index.js", b"module.exports={}\n");
    h.stub.put("docs/", b"");
    h.stub.put("a//b.txt", b"double slash\n");
    h.stub.put("weird\tname.txt", b"control char\n");
    h.stub.put("trailing.", b"dot\n");

    let report = h.materialize().unwrap();
    assert_eq!(report.admitted, 1, "{report:?}");
    assert_eq!(h.mirror_rels(), vec!["ok.md"]);
    assert_eq!(h.stub.counters().1, 1, "one GET, for the one real document");
    assert_eq!(
        report.skipped.get(crate::ignore_rules::HIDDEN_RULE),
        Some(&2),
        "{:?}",
        report.skipped
    );
    assert_eq!(
        report.skipped.get(crate::source::rules::UNSAFE_KEY),
        Some(&3),
        "{:?}",
        report.skipped
    );
    assert_eq!(
        report.skipped.get(crate::source::rules::FOLDER_MARKER),
        Some(&1)
    );
    assert_eq!(report.skipped.get("default:build-output"), Some(&1));
    // And the secret is not on local disk either.
    assert!(!h.run.mirror.join(".env").exists());
}

#[test]
fn a_truncated_mirror_file_is_fetched_again() {
    let h = Harness::new("");
    h.stub.put("a.md", b"the whole thing\n");
    h.materialize().unwrap();
    // Simulate a run killed mid-write that somehow left a short file: the size
    // check, not the ETag, is what catches this.
    std::fs::write(h.run.mirror.join("a.md"), b"the wh").unwrap();
    h.stub.reset_counters();
    let report = h.materialize().unwrap();
    assert_eq!(report.downloaded, 1, "{report:?}");
    assert_eq!(h.mirror_text("a.md"), "the whole thing\n");
}

#[test]
fn a_failed_get_aborts_the_run_rather_than_looking_like_a_deletion() {
    let h = Harness::new("");
    h.stub.put("a.md", b"one\n");
    h.stub.put("b.md", b"two\n");
    h.stub.fail_get("b.md");
    let err = h.materialize().unwrap_err().to_string();
    assert!(
        err.contains("b.md") || err.contains("failed against"),
        "the failure must name what it was doing: {err}"
    );
    // The run failed, so `b.md` was NOT recorded and was NOT treated as
    // deleted. `a.md` DID land and was paid for, so it IS recorded — see
    // `an_aborted_run_does_not_pay_twice_for_the_objects_it_already_fetched`.
    let (manifest, _) = ObjectManifest::load(&h.run.manifest_path, &h.run.identity);
    assert!(!manifest.objects.contains_key("b.md"), "{manifest:?}");
    assert!(manifest.objects.contains_key("a.md"), "{manifest:?}");
    assert!(
        manifest.last_run.as_ref().is_some_and(|r| r.aborted),
        "a partial manifest must say so: {manifest:?}"
    );
}

/// 970-B1: the SDK's own retry layer is disabled and every WIRE attempt is
/// counted, so a throttled request reports what the store will bill.
///
/// Before the fix (`aws_config::defaults`, default retry mode "standard",
/// counter incremented after a successful `send()`), this stub's three wire
/// requests were reported as one.
#[test]
fn a_throttled_request_counts_every_billed_attempt_not_just_the_one_that_worked() {
    let h = Harness::new("");
    h.stub.put("a.md", b"one\n");
    // Two 503 SlowDown answers, then the real listing.
    h.stub.throttle_list(2, false);
    let source = h.source();
    let listing = source.list().unwrap();
    assert_eq!(listing.entries.len(), 1, "the retry must still succeed");
    let (wire_lists, _) = h.stub.counters();
    let ops = source.ops();
    assert_eq!(wire_lists, 3, "the stub saw three LIST requests");
    assert_eq!(
        ops.list_requests, wire_lists,
        "every billed attempt must be counted, not only the one that succeeded"
    );
    assert_eq!(ops.retried_requests, 2, "{ops:?}");
}

/// 970-B1, the worse half: a request that exhausts its retries used to count
/// ZERO while costing four.
#[test]
fn a_request_that_exhausts_its_retries_reports_all_of_them() {
    let h = Harness::new("");
    h.stub.put("a.md", b"one\n");
    h.stub.throttle_list(0, true);
    let source = h.source();
    let err = source.list().unwrap_err().to_string();
    let (wire_lists, _) = h.stub.counters();
    let ops = source.ops();
    assert_eq!(wire_lists, 4, "attempts are bounded: {err}");
    assert_eq!(
        ops.list_requests, wire_lists,
        "a failed request still costs money and must still be reported: {err}"
    );
    assert!(
        err.contains("4 billed requests"),
        "the operator is told what the failure cost: {err}"
    );
}

/// 970-M2: an aborted run keeps the receipts for the objects whose bytes
/// landed, so the next run pays only for the rest.
///
/// Before the fix the manifest was written only after the last object, so a run
/// that failed on object N paid for N-1 downloads it then threw away — 2N GETs
/// against a documented N.
#[test]
fn an_aborted_run_does_not_pay_twice_for_the_objects_it_already_fetched() {
    let h = Harness::new("");
    for name in ["a.md", "b.md", "c.md", "d.md"] {
        h.stub.put(name, format!("contents of {name}\n").as_bytes());
    }
    h.stub.fail_get("d.md");
    h.stub.reset_counters();
    assert!(h.materialize().is_err(), "the run must still fail");
    let (_, first_gets) = h.stub.counters();
    assert!(first_gets >= 4, "every object was attempted: {first_gets}");

    h.stub.stop_failing_get("d.md");
    h.stub.reset_counters();
    let report = h.materialize().unwrap();
    let (_, second_gets) = h.stub.counters();
    assert_eq!(
        report.downloaded, 1,
        "only the object that failed is fetched again: {report:?}"
    );
    assert_eq!(report.unchanged, 3, "{report:?}");
    assert_eq!(
        second_gets, 1,
        "the re-run must not re-GET objects the aborted run already paid for"
    );
    assert_eq!(h.mirror_rels(), ["a.md", "b.md", "c.md", "d.md"]);
}

#[test]
fn an_object_that_vanishes_between_list_and_get_is_treated_as_deleted() {
    let h = Harness::new("");
    h.stub.put("a.md", b"one\n");
    h.stub.put("racing.md", b"here for now\n");
    h.stub
        .state
        .lock()
        .unwrap()
        .missing_get
        .insert("racing.md".to_string());
    let report = h.materialize().unwrap();
    assert_eq!(report.vanished, 1, "{report:?}");
    assert_eq!(report.downloaded, 1);
    assert_eq!(h.mirror_rels(), vec!["a.md"]);
    let (manifest, _) = ObjectManifest::load(&h.run.manifest_path, &h.run.identity);
    assert!(!manifest.objects.contains_key("racing.md"));
}

#[test]
fn plan_only_lists_and_prices_the_transfer_without_downloading() {
    let h = Harness::new("");
    h.stub.put("a.md", b"one\n");
    h.stub.put("b.md", b"two\n");
    let source = h.source();
    let report = materialize_from(
        &h.run,
        &source,
        &h.progress,
        4,
        crate::objsource::MaterializeMode::PlanOnly,
    )
    .unwrap();
    assert_eq!(report.pending, 2, "{report:?}");
    assert_eq!(report.pending_bytes, 8);
    assert_eq!(report.downloaded, 0);
    assert_eq!(h.stub.counters().1, 0, "a dry run spends no GET requests");
    assert!(h.mirror_rels().is_empty(), "and writes nothing locally");
    assert!(
        !h.run.manifest_path.exists(),
        "a dry run does not rewrite state either"
    );
    assert!(report.cost_lines()[0].contains("1 LIST (class A)"));
}

#[test]
fn a_large_object_streams_rather_than_buffering() {
    let h = Harness::new("");
    // 64 MiB is small enough to keep the test fast and large enough that a
    // whole-object buffer would be unmistakable in RSS.
    const SIZE: usize = 64 << 20;
    h.stub.put("big.bin", &vec![0x5au8; SIZE]);
    let source = h.source();
    let listing = source.list().unwrap();
    let entry = &listing.entries[0];
    assert_eq!(entry.size, SIZE as u64);

    let before = xerj_common::resource::current_rss_bytes();
    let mut reader = source.open(entry).unwrap();
    let mut total = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    let mut peak = before;
    loop {
        let read = reader.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        total += read as u64;
        if let (Some(peak), Some(now)) = (peak.as_mut(), xerj_common::resource::current_rss_bytes())
        {
            *peak = (*peak).max(now);
        }
    }
    assert_eq!(total, SIZE as u64, "every byte arrived");
    if let (Some(before), Some(peak)) = (before, peak) {
        let growth = peak.saturating_sub(before);
        assert!(
            growth < (SIZE as u64) / 4,
            "streaming a {} MiB object grew RSS by {} MiB — it is being buffered",
            SIZE >> 20,
            growth >> 20
        );
    }
}

#[test]
fn a_missing_bucket_says_which_bucket_and_what_to_check() {
    // The stub answers a list for any bucket, so this exercises the classifier
    // through a real transport failure instead: an endpoint nobody is serving.
    ensure_credentials();
    let spec = ObjectSpec {
        scheme: ObjectScheme::S3,
        bucket: "nope".into(),
        prefix: String::new(),
        // Port 1 is not listening, and binding it would need root.
        endpoint: Some("http://127.0.0.1:1".into()),
    };
    let source = ObjectStoreSource::connect(&spec).unwrap();
    let err = source.list().unwrap_err().to_string();
    assert!(
        err.contains("could not reach") && err.contains("127.0.0.1:1"),
        "an unreachable endpoint must name the endpoint: {err}"
    );
    assert!(
        !err.contains("stub-secret-key"),
        "no credential may appear in an error: {err}"
    );
}

#[test]
fn prepare_rewrites_the_root_for_an_object_url_and_leaves_folders_alone() {
    let state = tempfile::tempdir().unwrap();
    let mut cfg =
        crate::phase_a_grouping_tests::cfg_for(std::path::Path::new("s3://acme-docs/handbook"));
    cfg.state_dir = Some(state.path().to_path_buf());
    let run = crate::objsource::prepare(&mut cfg).unwrap().unwrap();
    assert_eq!(run.identity, "s3://acme-docs/handbook/");
    assert_eq!(cfg.root, run.mirror, "the run walks the mirror");
    assert!(run.mirror.ends_with("object-cache/acme-docs-handbook"));
    // The mirror's basename is what the default brain name derives from, so it
    // is the bucket and prefix rather than the word "objects".
    assert_eq!(crate::derive_brain_name(&cfg.root), "acme-docs-handbook");

    let folder = tempfile::tempdir().unwrap();
    let mut local = crate::phase_a_grouping_tests::cfg_for(folder.path());
    assert!(crate::objsource::prepare(&mut local).unwrap().is_none());
    assert_eq!(local.root, folder.path());
}

#[test]
fn an_endpoint_for_a_folder_and_an_r2_url_without_one_are_both_refused() {
    let folder = tempfile::tempdir().unwrap();
    let mut cfg = crate::phase_a_grouping_tests::cfg_for(folder.path());
    cfg.endpoint_url = Some("http://127.0.0.1:9000".into());
    let err = crate::objsource::prepare(&mut cfg).unwrap_err().to_string();
    assert!(err.contains("--endpoint-url applies only to"), "{err}");

    let mut r2 = crate::phase_a_grouping_tests::cfg_for(std::path::Path::new("r2://bucket/prefix"));
    r2.endpoint_url = None;
    // The environment of a developer box may carry AWS_ENDPOINT_URL; the test is
    // about the case where nothing supplies one.
    let saved = std::env::var("AWS_ENDPOINT_URL").ok();
    let saved_s3 = std::env::var("AWS_ENDPOINT_URL_S3").ok();
    std::env::remove_var("AWS_ENDPOINT_URL");
    std::env::remove_var("AWS_ENDPOINT_URL_S3");
    let err = crate::objsource::prepare(&mut r2).unwrap_err().to_string();
    if let Some(v) = saved {
        std::env::set_var("AWS_ENDPOINT_URL", v);
    }
    if let Some(v) = saved_s3 {
        std::env::set_var("AWS_ENDPOINT_URL_S3", v);
    }
    assert!(err.contains("r2.cloudflarestorage.com"), "{err}");
}
