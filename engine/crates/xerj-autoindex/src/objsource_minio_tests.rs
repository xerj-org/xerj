//! The object source against a REAL S3 implementation: MinIO.
//!
//! The hermetic endpoint next door (`objsource_s3_tests`) proves XERJ's logic.
//! This suite proves the wire: SigV4 signing, path-style addressing, a genuine
//! multipart upload's ETag, real pagination, and a 1 GB object streamed through a
//! bounded buffer.
//!
//! It is SKIPPED unless `XERJ_MINIO_ENDPOINT` names a reachable MinIO, and the
//! reason is printed (run with `-- --nocapture` to see it). Bring one up with:
//!
//! ```sh
//! docker run -d -p 127.0.0.1:13001:9000 \
//!   -e MINIO_ROOT_USER=minio -e MINIO_ROOT_PASSWORD=minio-secret \
//!   quay.io/minio/minio:latest server /data
//! export XERJ_MINIO_ENDPOINT=http://127.0.0.1:13001
//! export XERJ_MINIO_ACCESS_KEY=minio XERJ_MINIO_SECRET_KEY=minio-secret
//! cargo test --release -p xerj-autoindex --lib -- objsource_minio_tests --nocapture
//! ```
//!
//! Nothing here ever runs against a paid store: the big-object and
//! many-object cases exist precisely so they do not have to.

use crate::objsource::{
    materialize_from, MaterializeMode, MaterializeReport, ObjectManifest, ObjectRun,
    ObjectStoreSource, MANIFEST_FILE,
};
use crate::source::{DocSource, ObjectScheme, ObjectSpec};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};

/// 8 MiB: the smallest part S3 allows in a multipart upload except for the last.
const PART_BYTES: usize = 8 << 20;

struct Minio {
    endpoint: String,
    runtime: tokio::runtime::Runtime,
    client: aws_sdk_s3::Client,
}

/// `Some(Minio)` when one is reachable, `None` (with a printed reason) otherwise.
fn minio() -> Option<Minio> {
    let endpoint = match std::env::var("XERJ_MINIO_ENDPOINT") {
        Ok(endpoint) if !endpoint.trim().is_empty() => endpoint,
        _ => {
            println!(
                "SKIPPED: XERJ_MINIO_ENDPOINT is not set, so there is no S3 implementation to test \
                 against. See the module comment for the one-line docker command."
            );
            return None;
        }
    };
    let access = std::env::var("XERJ_MINIO_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".into());
    let secret = std::env::var("XERJ_MINIO_SECRET_KEY").unwrap_or_else(|_| "minioadmin".into());
    // These are the credentials for a throwaway local MinIO and they are put in
    // the process environment because that is the chain XERJ itself reads —
    // testing any other way would test something else.
    std::env::set_var("AWS_ACCESS_KEY_ID", &access);
    std::env::set_var("AWS_SECRET_ACCESS_KEY", &secret);
    std::env::set_var("AWS_REGION", "us-east-1");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    // The seeding client is built the same way the source builds its own — no
    // `aws-config`, an explicitly named rustls+ring TLS stack — because that is
    // the only client this crate is allowed to depend on (engine/Cargo.toml
    // explains why: the SDK's default HTTPS client drags in aws-lc-rs).
    let client = runtime.block_on(async {
        let http_client = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();
        aws_sdk_s3::Client::from_conf(
            aws_sdk_s3::config::Builder::new()
                .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
                .region(aws_sdk_s3::config::Region::new("us-east-1"))
                .credentials_provider(aws_sdk_s3::config::Credentials::new(
                    access.clone(),
                    secret.clone(),
                    None,
                    None,
                    "xerj-minio-test",
                ))
                .http_client(http_client)
                .endpoint_url(&endpoint)
                .force_path_style(true)
                .build(),
        )
    });
    // One cheap request to prove it is really there.
    if let Err(e) = runtime.block_on(client.list_buckets().send()) {
        println!(
            "SKIPPED: {endpoint} did not answer list_buckets ({}). Start MinIO, or unset \
             XERJ_MINIO_ENDPOINT.",
            aws_sdk_s3::error::DisplayErrorContext(&e)
        );
        return None;
    }
    Some(Minio {
        endpoint,
        runtime,
        client,
    })
}

impl Minio {
    /// An empty bucket named for the test, recreated from scratch.
    fn bucket(&self, name: &str) -> String {
        self.runtime.block_on(async {
            // Drain and drop any bucket left by an earlier run: these tests
            // assert on exact counts, so a stale object is a false failure.
            if self.client.head_bucket().bucket(name).send().await.is_ok() {
                let mut token: Option<String> = None;
                loop {
                    let page = self
                        .client
                        .list_objects_v2()
                        .bucket(name)
                        .set_continuation_token(token.clone())
                        .send()
                        .await
                        .unwrap();
                    for object in page.contents() {
                        if let Some(key) = object.key() {
                            let _ = self
                                .client
                                .delete_object()
                                .bucket(name)
                                .key(key)
                                .send()
                                .await;
                        }
                    }
                    if !page.is_truncated().unwrap_or(false) {
                        break;
                    }
                    token = page.next_continuation_token().map(str::to_string);
                }
            } else {
                self.client
                    .create_bucket()
                    .bucket(name)
                    .send()
                    .await
                    .unwrap();
            }
        });
        name.to_string()
    }

    fn put(&self, bucket: &str, key: &str, body: &[u8]) {
        self.runtime
            .block_on(
                self.client
                    .put_object()
                    .bucket(bucket)
                    .key(key)
                    .body(ByteStream::from(body.to_vec()))
                    .send(),
            )
            .unwrap();
    }

    fn delete(&self, bucket: &str, key: &str) {
        self.runtime
            .block_on(self.client.delete_object().bucket(bucket).key(key).send())
            .unwrap();
    }

    /// A real multipart upload of `parts × PART_BYTES` bytes, without ever
    /// holding more than one part in memory or writing a local file. The
    /// resulting ETag carries the `-N` suffix, which is the case that must not
    /// be mistaken for an MD5.
    fn put_multipart(&self, bucket: &str, key: &str, parts: usize) -> u64 {
        let chunk: Vec<u8> = (0..PART_BYTES).map(|i| (i % 251) as u8).collect();
        self.runtime.block_on(async {
            let created = self
                .client
                .create_multipart_upload()
                .bucket(bucket)
                .key(key)
                .send()
                .await
                .unwrap();
            let upload_id = created.upload_id().unwrap().to_string();
            let mut completed = Vec::with_capacity(parts);
            for part in 1..=parts {
                let uploaded = self
                    .client
                    .upload_part()
                    .bucket(bucket)
                    .key(key)
                    .upload_id(&upload_id)
                    .part_number(part as i32)
                    .body(ByteStream::from(chunk.clone()))
                    .send()
                    .await
                    .unwrap();
                completed.push(
                    CompletedPart::builder()
                        .part_number(part as i32)
                        .set_e_tag(uploaded.e_tag().map(str::to_string))
                        .build(),
                );
            }
            self.client
                .complete_multipart_upload()
                .bucket(bucket)
                .key(key)
                .upload_id(&upload_id)
                .multipart_upload(
                    CompletedMultipartUpload::builder()
                        .set_parts(Some(completed))
                        .build(),
                )
                .send()
                .await
                .unwrap();
        });
        (parts * PART_BYTES) as u64
    }
}

struct Harness {
    _state: tempfile::TempDir,
    run: ObjectRun,
    progress: std::sync::Arc<crate::progress::Progress>,
}

impl Harness {
    fn new(minio: &Minio, bucket: &str, prefix: &str) -> Self {
        let state = tempfile::tempdir().unwrap();
        let spec = ObjectSpec {
            scheme: ObjectScheme::S3,
            bucket: bucket.to_string(),
            prefix: prefix.to_string(),
            endpoint: Some(minio.endpoint.clone()),
        };
        let mirror = state.path().join("object-cache").join(spec.slug());
        std::fs::create_dir_all(&mirror).unwrap();
        Self {
            run: ObjectRun {
                identity: spec.identity(),
                spec,
                mirror,
                manifest_path: state.path().join(MANIFEST_FILE),
            },
            _state: state,
            progress: crate::progress::Progress::silent(),
        }
    }

    fn materialize(&self) -> MaterializeReport {
        let source = ObjectStoreSource::connect(&self.run.spec).unwrap();
        materialize_from(
            &self.run,
            &source,
            &self.progress,
            8,
            MaterializeMode::Fetch,
        )
        .unwrap()
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
fn minio_incremental_matrix() {
    let Some(minio) = minio() else { return };
    let bucket = minio.bucket("xerj-s3source-matrix");
    minio.put(
        &bucket,
        "docs/alpha.md",
        b"# alpha\n\nthe first document.\n",
    );
    minio.put(&bucket, "docs/beta.txt", b"beta beta beta\n");
    minio.put(&bucket, "docs/sub/gamma.md", b"# gamma\n");
    minio.put(&bucket, "outside/delta.md", b"# not in the prefix\n");
    minio.put(&bucket, "docs/.env", b"SECRET=hunter2\n");
    let h = Harness::new(&minio, &bucket, "docs/");

    let first = h.materialize();
    println!("MinIO first run: {first:?}");
    assert_eq!(first.admitted, 3, "{first:?}");
    assert_eq!(first.downloaded, 3);
    assert_eq!(first.list_requests, 1);
    assert_eq!(first.read_requests, 3);
    assert_eq!(
        h.mirror_rels(),
        vec!["alpha.md", "beta.txt", "sub/gamma.md"]
    );
    assert!(
        !h.run.mirror.join(".env").exists(),
        "a bucket's dotfiles stay out of the mirror and out of the index"
    );

    let second = h.materialize();
    println!("MinIO unchanged re-run: {second:?}");
    assert_eq!(second.unchanged, 3, "{second:?}");
    assert_eq!(
        second.downloaded, 0,
        "an unchanged bucket downloads nothing"
    );
    assert_eq!(second.read_requests, 0, "and makes no GET requests at all");
    assert_eq!(second.list_requests, 1);

    minio.put(&bucket, "docs/alpha.md", b"# alpha\n\nrevised.\n");
    minio.put(&bucket, "docs/epsilon.md", b"# epsilon\n");
    minio.delete(&bucket, "docs/beta.txt");
    let third = h.materialize();
    println!("MinIO changed+new+deleted run: {third:?}");
    assert_eq!(
        third.downloaded, 2,
        "the changed one and the new one: {third:?}"
    );
    assert_eq!(third.unchanged, 1);
    assert_eq!(third.removed, 1);
    assert_eq!(
        h.mirror_rels(),
        vec!["alpha.md", "epsilon.md", "sub/gamma.md"]
    );
    assert_eq!(
        std::fs::read_to_string(h.run.mirror.join("alpha.md")).unwrap(),
        "# alpha\n\nrevised.\n"
    );
    let (manifest, _) = ObjectManifest::load(&h.run.manifest_path, &h.run.identity);
    assert!(!manifest.objects.contains_key("beta.txt"));
}

#[test]
fn minio_real_multipart_upload_is_not_re_downloaded() {
    let Some(minio) = minio() else { return };
    let bucket = minio.bucket("xerj-s3source-multipart");
    // Two 8 MiB parts: the smallest genuine multipart upload S3 semantics allow.
    let bytes = minio.put_multipart(&bucket, "big/blob.bin", 2);
    let h = Harness::new(&minio, &bucket, "big/");

    let first = h.materialize();
    assert_eq!(first.downloaded, 1, "{first:?}");
    assert_eq!(first.bytes_downloaded, bytes);
    let (manifest, _) = ObjectManifest::load(&h.run.manifest_path, &h.run.identity);
    let etag = manifest.objects["blob.bin"].etag.clone().unwrap();
    println!("MinIO multipart ETag: {etag}");
    assert!(
        etag.contains("-2"),
        "a real multipart upload's ETag carries the part count: {etag}"
    );

    let second = h.materialize();
    assert_eq!(
        second.unchanged, 1,
        "a multipart ETag is a perfectly good change token: {second:?}"
    );
    assert_eq!(second.read_requests, 0);
}

#[test]
fn minio_pagination_past_one_thousand_keys() {
    let Some(minio) = minio() else { return };
    let bucket = minio.bucket("xerj-s3source-pages");
    for i in 0..1_200u32 {
        minio.put(
            &bucket,
            &format!("many/{i:05}.txt"),
            format!("row {i}\n").as_bytes(),
        );
    }
    let h = Harness::new(&minio, &bucket, "many/");
    let report = h.materialize();
    println!("MinIO 1,200-key run: {report:?}");
    assert_eq!(report.objects_listed, 1_200);
    assert_eq!(report.admitted, 1_200);
    assert_eq!(
        report.list_requests, 2,
        "1,200 keys is two LIST pages, so two class-A operations"
    );
    assert_eq!(h.mirror_rels().len(), 1_200);
    let second = h.materialize();
    assert_eq!(second.unchanged, 1_200);
    assert_eq!(second.read_requests, 0);
    assert_eq!(second.list_requests, 2);
}

/// A gigabyte, streamed. Only ever against MinIO: this is exactly the shape that
/// must not be run against a store that bills for it.
#[test]
fn minio_one_gigabyte_object_streams_through_a_bounded_buffer() {
    let Some(minio) = minio() else { return };
    if std::env::var("XERJ_MINIO_BIG").is_err() {
        println!(
            "SKIPPED: the 1 GB streaming case needs XERJ_MINIO_BIG=1 (it moves 2 GB of local \
             traffic and is not part of an ordinary test run)."
        );
        return;
    }
    let bucket = minio.bucket("xerj-s3source-big");
    // 128 × 8 MiB = 1 GiB, uploaded a part at a time so creating the fixture
    // costs one part of memory rather than a gigabyte of it.
    let bytes = minio.put_multipart(&bucket, "big/one-gig.bin", 128);
    assert_eq!(bytes, 1 << 30);
    let h = Harness::new(&minio, &bucket, "big/");

    let before = xerj_common::resource::current_rss_bytes();
    let report = h.materialize();
    let after = xerj_common::resource::current_rss_bytes();
    println!(
        "MinIO 1 GiB run: {} MB downloaded in {} ms; RSS {:?} -> {:?}",
        report.bytes_downloaded >> 20,
        report.elapsed_ms,
        before.map(|b| b >> 20),
        after.map(|b| b >> 20)
    );
    assert_eq!(report.downloaded, 1);
    assert_eq!(report.bytes_downloaded, 1 << 30);
    assert_eq!(
        std::fs::metadata(h.run.mirror.join("one-gig.bin"))
            .unwrap()
            .len(),
        1 << 30,
        "every byte landed"
    );
    if let (Some(before), Some(after)) = (before, after) {
        let growth = after.saturating_sub(before);
        assert!(
            growth < 128 << 20,
            "streaming 1 GiB grew RSS by {} MB — it is being buffered, not streamed",
            growth >> 20
        );
    }
    let second = h.materialize();
    assert_eq!(
        second.unchanged, 1,
        "and the gigabyte is not downloaded twice"
    );
}

#[test]
fn minio_access_denied_and_missing_bucket_say_what_to_fix() {
    let Some(minio) = minio() else { return };
    // A bucket that does not exist, through the same client the run uses.
    let spec = ObjectSpec {
        scheme: ObjectScheme::S3,
        bucket: "xerj-s3source-absent".into(),
        prefix: String::new(),
        endpoint: Some(minio.endpoint.clone()),
    };
    let source = ObjectStoreSource::connect(&spec).unwrap();
    let err = source.list().unwrap_err().to_string();
    println!("MinIO missing-bucket error: {err}");
    assert!(
        err.contains("xerj-s3source-absent"),
        "the error names the bucket: {err}"
    );
    assert!(
        !err.contains(&std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default()),
        "no credential in an error message"
    );
}
