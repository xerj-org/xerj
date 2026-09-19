//! Real S3-compatible object storage: Cloudflare R2, MinIO, AWS S3.
//!
//! [`S3Backend`] is a [`StorageBackend`] that talks HTTP to an object store via
//! `aws-sdk-s3`. It replaces the local-directory simulation that used to carry
//! this name (now [`crate::backend::SimulatedObjectStore`], kept because unit
//! tests want a backend with no network and no cost).
//!
//! | trait method  | request          | billing class            |
//! |---------------|------------------|--------------------------|
//! | `read_range`  | ranged GetObject | Class B                  |
//! | `write`       | PutObject        | **Class A**              |
//! | `list`        | ListObjectsV2    | **Class A, once a page** |
//! | `exists`      | HeadObject       | Class B                  |
//! | `metadata`    | HeadObject       | Class B                  |
//! | `delete`      | DeleteObject     | free                     |
//!
//! ## Why this counts its own requests
//!
//! Object stores bill per request. Cloudflare R2's free tier allows 1,000,000
//! Class A operations per month *per account* — about 23 a minute for
//! everything the account does. A `ListObjectsV2` page is a Class A operation,
//! so a watcher polling a bucket every 5 seconds spends roughly half the
//! month's allowance doing nothing, and polling a 100,000-object bucket (100
//! pages a cycle) every 60 seconds spends over four times the entire free tier.
//! Code that cannot report its own request count cannot be run safely inside
//! that allowance, so every billed attempt goes through
//! [`ObjectStoreOps::charge`] and is visible via [`StorageBackend::ops`].
//! `docs/OBJECT_STORAGE.md` has the arithmetic table.
//!
//! ## Retry and timeout policy
//!
//! The AWS SDK's own retry layer is **disabled** (`RetryConfig::disabled()`)
//! and retries happen in [`S3Backend::run`] instead. This is not a preference:
//! an SDK-internal retry is invisible to a counter wrapped around the call, so
//! leaving it on would undercount exactly the requests an operator is billed
//! for. Retrying in our own loop means one `charge` per wire attempt.
//!
//! Policy, all of it configurable via [`RetryPolicy`]:
//!
//! - up to [`RetryPolicy::max_attempts`] attempts (default 4, i.e. 3 retries);
//! - exponential backoff from [`RetryPolicy::initial_backoff`] (default 100 ms)
//!   doubling to [`RetryPolicy::max_backoff`] (default 5 s), with full jitter
//!   in the lower half of each interval so a fleet does not resynchronise;
//! - **retryable**: connect/dispatch failures, response-parse failures,
//!   per-attempt timeouts, any HTTP 5xx, and the S3 codes `SlowDown`,
//!   `RequestTimeout`, `InternalError`, `ServiceUnavailable`,
//!   `RequestTimeTooSkewed`, `ThrottlingException`, `TooManyRequests`;
//! - **not retryable**: 4xx other than 429 — `AccessDenied`, `NoSuchBucket`,
//!   `InvalidAccessKeyId`, `SignatureDoesNotMatch`. Retrying a rejected
//!   signature burns billed requests and never succeeds.
//! - a per-attempt timeout ([`S3Config::attempt_timeout`], default 30 s) and a
//!   connect timeout ([`S3Config::connect_timeout`], default 5 s). There is no
//!   whole-operation timeout: the caller owns that, because a 4 GiB segment
//!   upload and a 4 KiB footer read do not share a sensible deadline.
//!
//! Reference-coded against quickwit (Apache-2.0), which solves the same
//! problem: `quickwit/quickwit-storage/src/object_storage/s3_compatible_storage.rs:668`
//! builds the range header as `bytes={start}-{end-1}` and counts a metric per
//! attempt because the method is re-invoked by its own retry wrapper, and
//! `quickwit/quickwit-config/src/node_config/mod.rs:634`
//! (`StorageTimeoutPolicy`) keeps timeout and retry count together as one
//! policy object. Both are approaches we arrived at independently here; no
//! quickwit code is copied.
//!
//! ## Not implemented
//!
//! - **Multipart upload.** [`write`](StorageBackend::write) is a single
//!   `PutObject`, so an object is capped at the provider's single-PUT limit
//!   (5 GiB on S3 and R2). Segments larger than that cannot be uploaded.
//! - **Bulk delete.** `delete` is one `DeleteObject` per key.
//! - **Server-side copy, object tagging, versioning, SSE-C.**
//! - **Credential providers other than the environment.** No IMDS, no SSO, no
//!   profile files, no web-identity — `aws-config` is deliberately not a
//!   dependency. See [`credentials_from_env`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use bytes::Bytes;
use tracing::{debug, instrument, warn};

use crate::backend::{
    FileMetadata, ObjectStoreOps, ObjectStoreOpsSnapshot, OpBudget, OpClass, StorageBackend,
};
use crate::{Result, StorageError};

/// Pages one `list()` may fetch before it refuses to continue.
///
/// A money guard, not a performance one. Each page is a billed Class A request
/// covering at most 1,000 keys, so this caps one listing at 10,000 requests —
/// already 1% of Cloudflare R2's free monthly Class A allowance, and 10 million
/// keys. A prefix that large is a configuration mistake far more often than it
/// is a real prefix, and a broken or hostile endpoint that keeps answering
/// "truncated" with a fresh token has no other bound at all: the continuation
/// token check below cannot see a cycle that never repeats a token, and
/// `OpBudget` defaults to unlimited. `xerj-autoindex`'s object source uses the
/// same figure for the same reason (`objsource.rs`, `MAX_LIST_PAGES`).
const MAX_LIST_PAGES: u32 = 10_000;

/// S3 error codes worth another billed attempt.
const RETRYABLE_CODES: &[&str] = &[
    "SlowDown",
    "RequestTimeout",
    "RequestTimeoutException",
    "InternalError",
    "ServiceUnavailable",
    "RequestTimeTooSkewed",
    "ThrottlingException",
    "TooManyRequests",
    "503",
    "500",
];

// ── RetryPolicy ──────────────────────────────────────────────────────────────

/// How hard to try again when a request fails.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts including the first. `1` disables retrying.
    pub max_attempts: u32,
    /// Backoff before the second attempt; doubles from there.
    pub initial_backoff: Duration,
    /// Ceiling on the backoff interval.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
        }
    }
}

impl RetryPolicy {
    /// No retrying: one attempt, and a failure is a failure.
    ///
    /// Useful when the caller is doing its own accounting of a hard request
    /// budget and wants each logical call to cost exactly one billed request.
    pub const fn none() -> Self {
        Self {
            max_attempts: 1,
            initial_backoff: Duration::from_millis(0),
            max_backoff: Duration::from_millis(0),
        }
    }
}

// ── S3Config ─────────────────────────────────────────────────────────────────

/// Everything [`S3Backend::connect`] needs, apart from the credentials (which
/// come from the environment and never from a config file — see
/// [`credentials_from_env`]).
#[derive(Debug, Clone)]
pub struct S3Config {
    /// Bucket name. Must already exist: XERJ never calls `CreateBucket`,
    /// because a bucket created by accident outlives the process that made it
    /// and quietly accrues storage cost.
    pub bucket: String,
    /// Key prefix prepended to every object, e.g. `"xerj/"`. May be empty.
    pub prefix: String,
    /// Region. Cloudflare R2 wants the literal `"auto"`; MinIO ignores it;
    /// AWS S3 needs the bucket's real region.
    pub region: String,
    /// Endpoint override. `Some("https://<account>.r2.cloudflarestorage.com")`
    /// for R2, `Some("http://127.0.0.1:9000")` for MinIO, `None` for AWS S3.
    pub endpoint_url: Option<String>,
    /// Address the bucket as a path segment (`<endpoint>/<bucket>/<key>`)
    /// rather than a subdomain. Required by MinIO and by any endpoint without
    /// a wildcard TLS certificate; accepted by R2 and by AWS S3.
    pub force_path_style: bool,
    /// TCP + TLS handshake deadline.
    pub connect_timeout: Duration,
    /// Deadline for one attempt, handshake included.
    pub attempt_timeout: Duration,
    /// Retry policy — see the module docs for what is and is not retried.
    pub retry: RetryPolicy,
    /// Process-lifetime ceiling on billed requests. Defaults to no ceiling;
    /// read [`OpBudget`] before relying on it, because it is a circuit breaker
    /// and not a monthly quota.
    pub budget: OpBudget,
    /// Pages one [`StorageBackend::list`] may fetch before refusing to
    /// continue. Defaults to [`MAX_LIST_PAGES`]; configurable so a test can
    /// reach the bound without sending ten thousand requests.
    pub max_list_pages: u32,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: String::new(),
            prefix: String::new(),
            // "auto" is R2's required value and harmless on MinIO. It is wrong
            // for AWS S3, which is the case that must be configured explicitly.
            region: "auto".into(),
            endpoint_url: None,
            // Path-style works on R2, MinIO and AWS S3; virtual-host style
            // works on two of the three. The default is the one that works.
            force_path_style: true,
            connect_timeout: Duration::from_secs(5),
            attempt_timeout: Duration::from_secs(30),
            retry: RetryPolicy::default(),
            budget: OpBudget::unlimited(),
            max_list_pages: MAX_LIST_PAGES,
        }
    }
}

impl S3Config {
    /// A config for `bucket` with everything else defaulted.
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            ..Default::default()
        }
    }

    /// Set the key prefix.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Set the endpoint override (R2, MinIO, or any other S3-compatible API).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint_url = Some(endpoint.into());
        self
    }

    /// Set the region.
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    /// Set the process-lifetime billed-request ceiling.
    pub fn with_budget(mut self, budget: OpBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Set the retry policy.
    /// Override the page cap on one listing. See [`MAX_LIST_PAGES`].
    pub fn with_max_list_pages(mut self, pages: u32) -> Self {
        self.max_list_pages = pages;
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

// ── Credentials ──────────────────────────────────────────────────────────────

/// Read static credentials from `AWS_ACCESS_KEY_ID` /
/// `AWS_SECRET_ACCESS_KEY` (and optionally `AWS_SESSION_TOKEN`).
///
/// This is the only credential source. `aws-config` — which would also try
/// IMDS, SSO, profile files and web identity — is deliberately not a
/// dependency: on a box with no instance metadata service the IMDS probe is a
/// multi-second startup stall for a lookup that cannot succeed, and a
/// credential chain that silently picks up an unrelated profile is how data
/// ends up in the wrong bucket.
///
/// The error names the variables and never echoes a value, so it is safe in a
/// log.
pub fn credentials_from_env() -> Result<Credentials> {
    fn non_empty(var: &str) -> Option<String> {
        std::env::var(var).ok().filter(|v| !v.trim().is_empty())
    }
    let key_id = non_empty("AWS_ACCESS_KEY_ID");
    let secret = non_empty("AWS_SECRET_ACCESS_KEY");
    match (key_id, secret) {
        (Some(key_id), Some(secret)) => Ok(Credentials::new(
            key_id,
            secret,
            non_empty("AWS_SESSION_TOKEN"),
            None,
            "xerj-env",
        )),
        (None, Some(_)) => Err(StorageError::Backend(
            "object storage credentials incomplete: AWS_SECRET_ACCESS_KEY is set but \
             AWS_ACCESS_KEY_ID is not"
                .into(),
        )),
        (Some(_), None) => Err(StorageError::Backend(
            "object storage credentials incomplete: AWS_ACCESS_KEY_ID is set but \
             AWS_SECRET_ACCESS_KEY is not"
                .into(),
        )),
        (None, None) => Err(StorageError::Backend(
            "object storage credentials missing: set AWS_ACCESS_KEY_ID and \
             AWS_SECRET_ACCESS_KEY in the environment (XERJ reads credentials from the \
             environment only — no profile files, no instance metadata, no SSO)"
                .into(),
        )),
    }
}

// ── Error classification ─────────────────────────────────────────────────────

/// What to do about a failed attempt.
enum OpError {
    /// Worth another billed attempt.
    Retryable(String),
    /// Will fail the same way forever; retrying only spends money.
    Permanent(String),
    /// The key is not there. Surfaced to callers as an
    /// `io::ErrorKind::NotFound`, so a missing object behaves the same whether
    /// it is missing from a bucket or from a local directory.
    NotFound,
}

/// Map an SDK error onto a retry decision.
///
/// 404 detection is by error code (`NoSuchKey` from `GetObject`, `NotFound`
/// from `HeadObject`) rather than by status, because the typed errors are what
/// the SDK guarantees. 5xx detection falls back to the raw status so an
/// unmapped upstream error (R2 occasionally returns a bare 500) is still
/// retried instead of being treated as permanent.
fn classify<E>(err: &SdkError<E>) -> OpError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let code = err.code().unwrap_or_default().to_owned();
    if matches!(code.as_str(), "NoSuchKey" | "NotFound") {
        return OpError::NotFound;
    }
    // A missing BUCKET is also an HTTP 404, and mapping it to NotFound would
    // make `exists()` answer "that object is not there" for every key in a
    // misconfigured bucket — a permanently empty store and no error. It is a
    // configuration failure, so it is permanent, not absent.
    if code == "NoSuchBucket" {
        return OpError::Permanent(format!("{}", DisplayErrorContext(err)));
    }
    // `DisplayErrorContext` walks the whole source chain; the bare Display of
    // an SdkError is usually just "service error" with the cause hidden.
    let msg = format!("{}", DisplayErrorContext(err));
    match err {
        SdkError::TimeoutError(_) | SdkError::DispatchFailure(_) | SdkError::ResponseError(_) => {
            OpError::Retryable(msg)
        }
        SdkError::ServiceError(ctx) => {
            let status = ctx.raw().status().as_u16();
            if status == 404 {
                // Code-based checks above have already taken NoSuchBucket out
                // of this branch.
                OpError::NotFound
            } else if status >= 500 || status == 429 || RETRYABLE_CODES.contains(&code.as_str()) {
                OpError::Retryable(msg)
            } else {
                OpError::Permanent(msg)
            }
        }
        _ => OpError::Permanent(msg),
    }
}

// ── S3Backend ────────────────────────────────────────────────────────────────

/// An S3-compatible object store, reached over HTTP.
///
/// Cheap to clone: the inner `aws_sdk_s3::Client` is a handle over a shared
/// connection pool, and clones share one set of operation counters.
#[derive(Debug, Clone)]
pub struct S3Backend {
    client: Client,
    cfg: Arc<S3Config>,
    ops: Arc<ObjectStoreOps>,
    /// Monotonic counter mixed into the backoff jitter, so two attempts in the
    /// same nanosecond still get different sleeps.
    jitter_seq: Arc<AtomicU64>,
}

impl S3Backend {
    /// Build a client for `cfg`, taking credentials from the environment.
    ///
    /// This performs **no** network I/O and no billed request: it does not
    /// verify that the bucket exists or that the credentials work. A
    /// `HeadBucket`/`ListBuckets` probe here would be a Class A operation on
    /// every startup, which is the wrong default when the whole point is to fit
    /// inside a per-month request allowance. The first real operation reports
    /// an authentication or missing-bucket failure with the provider's own
    /// error code.
    pub fn connect(cfg: S3Config) -> Result<Self> {
        if cfg.bucket.trim().is_empty() {
            return Err(StorageError::Backend(
                "object storage misconfigured: bucket name is empty".into(),
            ));
        }
        let credentials = credentials_from_env()?;

        // TLS is pinned to rustls+ring rather than left to the SDK default,
        // which would pull in aws-lc-rs and break the musl / aarch64 / windows
        // cross-compile matrix. See engine/Cargo.toml for the full reasoning.
        let http_client = aws_smithy_http_client::Builder::new()
            .tls_provider(aws_smithy_http_client::tls::Provider::rustls(
                aws_smithy_http_client::tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();

        let mut builder = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(cfg.region.clone()))
            .credentials_provider(credentials)
            .http_client(http_client)
            .force_path_style(cfg.force_path_style)
            // Retries are ours, not the SDK's — see the module docs.
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
            .timeout_config(
                aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                    .connect_timeout(cfg.connect_timeout)
                    .operation_attempt_timeout(cfg.attempt_timeout)
                    .build(),
            );
        if let Some(endpoint) = &cfg.endpoint_url {
            builder = builder.endpoint_url(endpoint.clone());
        }

        Ok(Self {
            client: Client::from_conf(builder.build()),
            cfg: Arc::new(cfg),
            ops: Arc::new(ObjectStoreOps::default()),
            jitter_seq: Arc::new(AtomicU64::new(0)),
        })
    }

    /// The bucket this backend writes to.
    pub fn bucket(&self) -> &str {
        &self.cfg.bucket
    }

    /// The key prefix applied to every path.
    pub fn prefix(&self) -> &str {
        &self.cfg.prefix
    }

    /// A shared handle on the live operation counters, for an operator-facing
    /// endpoint that wants to read them without holding the backend.
    pub fn ops_handle(&self) -> Arc<ObjectStoreOps> {
        Arc::clone(&self.ops)
    }

    /// Full object key for `path`: the configured prefix, then `path`.
    pub fn object_key(&self, path: &str) -> String {
        if self.cfg.prefix.is_empty() {
            path.trim_start_matches('/').to_owned()
        } else {
            format!(
                "{}/{}",
                self.cfg.prefix.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        }
    }

    /// Inverse of [`object_key`](Self::object_key): strip the configured prefix
    /// off a key as the service returned it. `None` when the key is outside the
    /// prefix, which should not happen for a prefixed `ListObjectsV2` but is
    /// not worth a panic if it does.
    fn strip_object_prefix(&self, key: &str) -> Option<String> {
        if self.cfg.prefix.is_empty() {
            return Some(key.to_owned());
        }
        let head = format!("{}/", self.cfg.prefix.trim_end_matches('/'));
        key.strip_prefix(&head).map(|k| k.to_owned())
    }

    /// Backoff before attempt number `attempt + 1`.
    ///
    /// Exponential from `initial_backoff`, capped at `max_backoff`, then
    /// full-jittered into the lower half of the interval. Jitter is derived
    /// from the wall clock and an internal counter rather than from an RNG
    /// crate: this is sleep scheduling, not cryptography, and it keeps the
    /// dependency list of the storage crate unchanged.
    fn backoff(&self, attempt: u32) -> Duration {
        let policy = &self.cfg.retry;
        let base = policy
            .initial_backoff
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16))
            .min(policy.max_backoff);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let seq = self.jitter_seq.fetch_add(1, Ordering::Relaxed);
        // frac in [0, 1): scaled so the sleep lands in [base/2, base).
        let frac = ((nanos ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) % 1000) as f64 / 1000.0;
        base.mul_f64(0.5 + 0.5 * frac)
    }

    /// Run one logical operation, charging and retrying per wire attempt.
    ///
    /// `f` is called once per attempt and must map its failure to an
    /// [`OpError`]. Every call to `f` is preceded by a [`ObjectStoreOps::charge`],
    /// so the counters and the provider's invoice see the same number of
    /// requests.
    async fn run<T, F, Fut>(&self, class: OpClass, op: &'static str, key: &str, f: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, OpError>>,
    {
        let mut attempt: u32 = 1;
        loop {
            self.ops.charge(class, &self.cfg.budget)?;
            match f().await {
                Ok(value) => return Ok(value),
                Err(OpError::NotFound) => {
                    return Err(StorageError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("no such object: s3://{}/{key}", self.cfg.bucket),
                    )));
                }
                Err(OpError::Permanent(msg)) => {
                    return Err(StorageError::Backend(format!(
                        "{op} s3://{}/{key} failed (not retryable): {msg}",
                        self.cfg.bucket
                    )));
                }
                Err(OpError::Retryable(msg)) => {
                    if attempt >= self.cfg.retry.max_attempts {
                        return Err(StorageError::Backend(format!(
                            "{op} s3://{}/{key} failed after {attempt} attempt(s): {msg}",
                            self.cfg.bucket
                        )));
                    }
                    let delay = self.backoff(attempt);
                    warn!(
                        op,
                        key,
                        attempt,
                        max_attempts = self.cfg.retry.max_attempts,
                        backoff_ms = delay.as_millis() as u64,
                        error = %msg,
                        "object-store request failed, retrying"
                    );
                    self.ops.note_retry();
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }

    /// The `Range` header value for a read, or `None` for "whole object".
    ///
    /// `length == u64::MAX` means "to the end", matching
    /// [`StorageBackend::read_range`]: at offset 0 that is a plain `GetObject`
    /// with no header, and past offset 0 it is an open-ended
    /// `bytes={offset}-`.
    fn range_header(offset: u64, length: u64) -> Option<String> {
        if length == u64::MAX {
            if offset == 0 {
                None
            } else {
                Some(format!("bytes={offset}-"))
            }
        } else {
            // Inclusive end, hence the -1 (quickwit builds the same header at
            // s3_compatible_storage.rs:668). Saturating, because `offset +
            // length` on absurd inputs would panic in a debug build and wrap
            // into a nonsense header in a release one.
            Some(format!(
                "bytes={}-{}",
                offset,
                offset.saturating_add(length).saturating_sub(1)
            ))
        }
    }
}

#[async_trait]
impl StorageBackend for S3Backend {
    /// Ranged `GetObject` — one Class B operation per attempt.
    ///
    /// A short body is an error, not a truncated success: the local backend
    /// uses `read_exact` and callers decode fixed-width structures out of the
    /// result, so silently returning fewer bytes would turn a missing tail into
    /// a checksum mismatch several layers away.
    #[instrument(skip(self), fields(path, offset, length))]
    async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<Bytes> {
        if length == 0 {
            // Serving this from the bucket would cost a billed request for
            // zero bytes, and S3 rejects a zero-length range anyway.
            return Ok(Bytes::new());
        }
        let key = self.object_key(path);
        let range = Self::range_header(offset, length);

        let body = self
            .run(OpClass::ClassB, "GetObject", &key, || {
                let range = range.clone();
                let key = key.clone();
                async move {
                    let out = self
                        .client
                        .get_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .set_range(range)
                        .send()
                        .await
                        .map_err(|e| classify(&e))?;
                    // Collected inside the attempt so a body that dies
                    // mid-stream is retried rather than surfacing as a short
                    // read.
                    out.body
                        .collect()
                        .await
                        .map(|b| b.into_bytes())
                        .map_err(|e| OpError::Retryable(format!("reading response body: {e}")))
                }
            })
            .await?;

        self.ops.add_bytes_read(body.len() as u64);
        if length != u64::MAX && body.len() as u64 != length {
            return Err(StorageError::Backend(format!(
                "GetObject s3://{}/{key} range {}..+{length} returned {} bytes, expected {length}",
                self.cfg.bucket,
                offset,
                body.len()
            )));
        }
        debug!(key, offset, bytes = body.len(), "s3 read_range");
        Ok(body)
    }

    /// `PutObject` — one **Class A** operation per attempt.
    ///
    /// Whole-object and atomic: readers see the old object or the new one,
    /// never a splice. No multipart upload, so the provider's single-PUT
    /// ceiling (5 GiB on S3 and R2) is this method's ceiling too.
    #[instrument(skip(self, data), fields(path, bytes = data.len()))]
    async fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let key = self.object_key(path);
        self.run(OpClass::ClassA, "PutObject", &key, || {
            let key = key.clone();
            // A fresh body per attempt: a ByteStream is consumed by the send.
            let body = ByteStream::from(data.to_vec());
            async move {
                self.client
                    .put_object()
                    .bucket(&self.cfg.bucket)
                    .key(&key)
                    .body(body)
                    .send()
                    .await
                    .map(|_| ())
                    .map_err(|e| classify(&e))
            }
        })
        .await?;
        self.ops.add_bytes_written(data.len() as u64);
        debug!(key, bytes = data.len(), "s3 write");
        Ok(())
    }

    /// `DeleteObject` — free on R2. Deleting an absent key is not an error,
    /// which is also what S3 itself reports.
    async fn delete(&self, path: &str) -> Result<()> {
        let key = self.object_key(path);
        match self
            .run(OpClass::Free, "DeleteObject", &key, || {
                let key = key.clone();
                async move {
                    self.client
                        .delete_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .send()
                        .await
                        .map(|_| ())
                        .map_err(|e| classify(&e))
                }
            })
            .await
        {
            Ok(()) => Ok(()),
            Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// `HeadObject` — one Class B operation per attempt.
    async fn exists(&self, path: &str) -> Result<bool> {
        match self.metadata(path).await {
            Ok(_) => Ok(true),
            Err(StorageError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// `ListObjectsV2` — one **Class A** operation per *page*, and a page holds
    /// at most 1,000 keys.
    ///
    /// This is the expensive method, and the cost is proportional to the number
    /// of objects, not to the number of calls: listing a 100,000-object prefix
    /// is 100 Class A operations every time. Anything that polls should either
    /// list a narrow prefix or not poll — `docs/OBJECT_STORAGE.md` works the
    /// arithmetic through.
    ///
    /// Keys come back with the configured prefix stripped, so the output feeds
    /// straight into `read_range`. Zero-byte "directory marker" keys ending in
    /// `/`, which some tools create, are skipped: they are not objects anyone
    /// can read.
    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let full_prefix = self.object_key(prefix);
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        let mut pages: u32 = 0;
        // Every token the store has handed back. A server that cycles through
        // two or more tokens is not caught by comparing against the previous
        // one, and each turn of that loop is a billed Class A request — see
        // MAX_LIST_PAGES.
        let mut seen_tokens: std::collections::HashSet<String> = std::collections::HashSet::new();

        loop {
            let this_token = token.clone();
            let prefix_for_page = full_prefix.clone();
            let page = self
                .run(OpClass::ClassA, "ListObjectsV2", &full_prefix, || {
                    let this_token = this_token.clone();
                    let prefix_for_page = prefix_for_page.clone();
                    async move {
                        self.client
                            .list_objects_v2()
                            .bucket(&self.cfg.bucket)
                            .prefix(prefix_for_page)
                            .set_continuation_token(this_token)
                            .max_keys(1000)
                            .send()
                            .await
                            .map_err(|e| classify(&e))
                    }
                })
                .await?;
            pages += 1;

            for object in page.contents() {
                match object.key() {
                    Some(key) if key.ends_with('/') => continue,
                    Some(key) => {
                        if let Some(rel) = self.strip_object_prefix(key) {
                            keys.push(rel);
                        }
                    }
                    None => continue,
                }
            }

            let next = page
                .next_continuation_token()
                .filter(|_| page.is_truncated().unwrap_or(false))
                .map(|t| t.to_owned());

            // A server that keeps replying "truncated" would spin here forever,
            // and every turn of that loop is a billed Class A operation.
            // Refusing is the only safe response: an unbounded list is how a bug
            // turns into an invoice. (An `OpBudget` would eventually stop it
            // too, but the budget is optional and this is not.)
            //
            // Two bounds, because one is not enough. The token check catches a
            // cycle of ANY length — the previous version compared only against
            // the immediately preceding token, so an endpoint alternating
            // A/B/A/B walked straight past it and billed a Class A request per
            // page. The page cap catches what no token check can: a store that
            // hands back a fresh token every time.
            if let Some(next) = next.as_deref() {
                if !seen_tokens.insert(next.to_owned()) {
                    return Err(StorageError::Backend(format!(
                        "ListObjectsV2 s3://{}/{full_prefix} returned a continuation token it had \
                         already used after {pages} page(s) ({pages} billed Class A request(s)); \
                         the listing is cycling, so refusing to keep paying for it",
                        self.cfg.bucket
                    )));
                }
                let cap = self.cfg.max_list_pages;
                if pages >= cap {
                    return Err(StorageError::Backend(format!(
                        "ListObjectsV2 s3://{}/{full_prefix} is still truncated after {cap} \
                         page(s) — {cap} billed Class A request(s), up to {} keys. Refusing to \
                         keep listing: list a narrower prefix, or raise S3Config::max_list_pages \
                         if a prefix really is this large",
                        self.cfg.bucket,
                        cap as u64 * 1000
                    )));
                }
            }
            token = next;
            if token.is_none() {
                break;
            }
        }

        debug!(
            prefix = %full_prefix,
            keys = keys.len(),
            class_a_ops = pages,
            "s3 list complete"
        );
        Ok(keys)
    }

    /// `HeadObject` — one Class B operation per attempt.
    ///
    /// `created` is always `None`: object stores record a single
    /// last-modified time and a `PutObject` that replaces an object resets it,
    /// so there is no creation time to report. Reporting last-modified as
    /// creation time would be a fabrication.
    async fn metadata(&self, path: &str) -> Result<FileMetadata> {
        let key = self.object_key(path);
        let head = self
            .run(OpClass::ClassB, "HeadObject", &key, || {
                let key = key.clone();
                async move {
                    self.client
                        .head_object()
                        .bucket(&self.cfg.bucket)
                        .key(&key)
                        .send()
                        .await
                        .map_err(|e| classify(&e))
                }
            })
            .await?;

        let modified = head
            .last_modified()
            .and_then(|t| {
                let secs = t.secs();
                if secs < 0 {
                    None
                } else {
                    UNIX_EPOCH.checked_add(Duration::new(
                        secs as u64,
                        t.subsec_nanos().min(999_999_999),
                    ))
                }
            })
            .unwrap_or(UNIX_EPOCH);

        Ok(FileMetadata {
            size: head.content_length().unwrap_or(0).max(0) as u64,
            modified,
            created: None,
            // S3 quotes the ETag; the quotes are part of the header, not of the
            // tag, and leaving them in makes every comparison against a value
            // from another tool fail.
            etag: head
                .e_tag()
                .map(|e| e.trim_matches('"').to_owned())
                .filter(|e| !e.is_empty()),
        })
    }

    fn ops(&self) -> Option<ObjectStoreOpsSnapshot> {
        Some(self.ops.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialise env-var mutation: `std::env::set_var` is process-global and
    /// cargo runs tests in threads.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn range_header_inclusive_end() {
        // bytes 6..=11 is the six-byte range the R2 probe verified by hand.
        assert_eq!(S3Backend::range_header(6, 6).as_deref(), Some("bytes=6-11"));
        assert_eq!(S3Backend::range_header(0, 1).as_deref(), Some("bytes=0-0"));
    }

    #[test]
    fn range_header_saturates_instead_of_overflowing() {
        // Absurd, but it must not panic in debug or wrap in release. The end
        // saturates at u64::MAX and then loses the inclusive -1, so start and
        // end coincide — a one-byte range, which is nonsense the service will
        // reject cleanly rather than a wrapped range that asks for something
        // else entirely.
        assert_eq!(
            S3Backend::range_header(u64::MAX - 1, 10).as_deref(),
            Some("bytes=18446744073709551614-18446744073709551614")
        );
        // The ordinary case is unaffected.
        assert_eq!(
            S3Backend::range_header(10, 5).as_deref(),
            Some("bytes=10-14")
        );
    }

    #[test]
    fn range_header_whole_object_has_no_header() {
        assert_eq!(S3Backend::range_header(0, u64::MAX), None);
    }

    #[test]
    fn range_header_open_ended_from_offset() {
        assert_eq!(
            S3Backend::range_header(128, u64::MAX).as_deref(),
            Some("bytes=128-")
        );
    }

    #[test]
    fn missing_credentials_are_a_clear_error() {
        let _guard = ENV_LOCK.lock().unwrap();
        let saved = (
            std::env::var("AWS_ACCESS_KEY_ID").ok(),
            std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
        );
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");

        let err = S3Backend::connect(S3Config::new("some-bucket")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("AWS_ACCESS_KEY_ID") && msg.contains("AWS_SECRET_ACCESS_KEY"),
            "error should name both variables, got: {msg}"
        );

        if let Some(v) = saved.0 {
            std::env::set_var("AWS_ACCESS_KEY_ID", v);
        }
        if let Some(v) = saved.1 {
            std::env::set_var("AWS_SECRET_ACCESS_KEY", v);
        }
    }

    #[test]
    fn empty_bucket_is_rejected_before_credentials_are_read() {
        let err = S3Backend::connect(S3Config::new("   ")).unwrap_err();
        assert!(
            err.to_string().contains("bucket name is empty"),
            "got: {err}"
        );
    }

    #[test]
    fn object_key_applies_and_strips_the_prefix() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("AWS_ACCESS_KEY_ID", "test-key-id");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test-secret");
        let backend = S3Backend::connect(
            S3Config::new("b")
                .with_prefix("xerj/v1")
                .with_endpoint("http://127.0.0.1:1"),
        )
        .unwrap();

        assert_eq!(
            backend.object_key("segments/a.seg"),
            "xerj/v1/segments/a.seg"
        );
        // A leading slash on the path must not produce a double slash.
        assert!(!backend.object_key("/segments/a.seg").contains("//"));
        // Round-trip: what list() hands back is what read_range() accepts.
        assert_eq!(
            backend.strip_object_prefix("xerj/v1/segments/a.seg"),
            Some("segments/a.seg".to_owned())
        );
        assert_eq!(backend.strip_object_prefix("other/a.seg"), None);
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
    }

    #[test]
    fn budget_refuses_over_the_ceiling_and_counts_the_refusal() {
        let ops = ObjectStoreOps::default();
        let budget = OpBudget::new(2, 0);

        assert!(ops.charge(OpClass::ClassA, &budget).is_ok());
        assert!(ops.charge(OpClass::ClassA, &budget).is_ok());
        let err = ops.charge(OpClass::ClassA, &budget).unwrap_err();
        assert!(err.to_string().contains("budget exhausted"), "got: {err}");

        // Class B ceiling of 0 refuses the first request.
        assert!(ops.charge(OpClass::ClassB, &budget).is_err());
        // Free operations are never refused.
        assert!(ops.charge(OpClass::Free, &budget).is_ok());

        let snap = ops.snapshot();
        assert_eq!(snap.class_a, 2, "rolled-back charge must not be counted");
        assert_eq!(snap.class_b, 0);
        assert_eq!(snap.free, 1);
        assert_eq!(snap.refused, 2);
    }

    #[test]
    fn r2_free_tier_slice_is_a_fifth_of_the_monthly_allowance() {
        let b = OpBudget::r2_free_tier_slice();
        // A fifth of 1,000,000 Class A and 10,000,000 Class B, so an account
        // serving other traffic is not spent dry by one process.
        assert_eq!(b.max_class_a, Some(200_000));
        assert_eq!(b.max_class_b, Some(2_000_000));
        // And the default really is no ceiling, because a ceiling that fires
        // mid-ingest is worse than none.
        assert_eq!(OpBudget::default().max_class_a, None);
        assert_eq!(OpBudget::unlimited().max_class_b, None);
    }

    #[test]
    fn backoff_grows_and_stays_within_the_ceiling() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("AWS_ACCESS_KEY_ID", "test-key-id");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test-secret");
        let backend = S3Backend::connect(
            S3Config::new("b")
                .with_endpoint("http://127.0.0.1:1")
                .with_retry(RetryPolicy {
                    max_attempts: 6,
                    initial_backoff: Duration::from_millis(100),
                    max_backoff: Duration::from_millis(800),
                }),
        )
        .unwrap();

        // Full jitter means only bounds can be asserted, never equality.
        for attempt in 1..=3u32 {
            let base = 100u64 << (attempt - 1);
            let d = backend.backoff(attempt).as_millis() as u64;
            assert!(
                d >= base / 2 && d < base,
                "attempt {attempt}: {d}ms outside [{}, {})",
                base / 2,
                base
            );
        }
        // Capped, jitter included.
        for attempt in 4..=12u32 {
            assert!(backend.backoff(attempt) < Duration::from_millis(800));
        }
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
    }

    /// An endpoint that answers every `ListObjectsV2` with "truncated", handing
    /// back continuation tokens from `tokens` in a loop.
    ///
    /// Hermetic, so the bound can be tested without a bucket and without
    /// spending anything: the failure being tested is one where a real store
    /// would bill per page.
    struct ForeverTruncatedStub {
        url: String,
        pages: Arc<AtomicU64>,
        stop: Arc<std::sync::atomic::AtomicBool>,
        address: std::net::SocketAddr,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl ForeverTruncatedStub {
        /// `tokens` is cycled; `None` means "a fresh token every page".
        fn start(tokens: Option<Vec<&'static str>>) -> Self {
            use std::io::{BufRead, BufReader, Write};
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let pages = Arc::new(AtomicU64::new(0));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let thread = {
                let pages = Arc::clone(&pages);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    for incoming in listener.incoming() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(mut stream) = incoming else { continue };
                        let n = pages.fetch_add(1, Ordering::Relaxed);
                        let token = match &tokens {
                            Some(cycle) => cycle[(n as usize) % cycle.len()].to_string(),
                            None => format!("fresh-token-{n}"),
                        };
                        // Drain the request.
                        {
                            let mut reader = BufReader::new(stream.try_clone().unwrap());
                            loop {
                                let mut line = String::new();
                                match reader.read_line(&mut line) {
                                    Ok(0) => break,
                                    Ok(_) if line == "\r\n" || line == "\n" => break,
                                    Ok(_) => continue,
                                    Err(_) => break,
                                }
                            }
                        }
                        let body = format!(
                            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult \
                             xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name>\
                             <KeyCount>0</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>true\
                             </IsTruncated><NextContinuationToken>{token}</NextContinuationToken>\
                             </ListBucketResult>"
                        );
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: \
                             {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                        let _ = stream.flush();
                    }
                })
            };
            Self {
                url: format!("http://{address}"),
                pages,
                stop,
                address,
                thread: Some(thread),
            }
        }

        fn wire_pages(&self) -> u64 {
            self.pages.load(Ordering::Relaxed)
        }
    }

    impl Drop for ForeverTruncatedStub {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = std::net::TcpStream::connect(self.address);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn stub_backend(stub: &ForeverTruncatedStub, max_pages: u32) -> S3Backend {
        std::env::set_var("AWS_ACCESS_KEY_ID", "test-key-id");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test-secret");
        let backend = S3Backend::connect(
            S3Config::new("bucket")
                .with_endpoint(&stub.url)
                .with_retry(RetryPolicy::none())
                .with_max_list_pages(max_pages),
        )
        .unwrap();
        std::env::remove_var("AWS_ACCESS_KEY_ID");
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
        backend
    }

    /// 966-M1: the old guard compared the new continuation token with the
    /// PREVIOUS one only, so an endpoint alternating two tokens listed forever
    /// — 201 billed Class A requests on an empty bucket in the reviewer's
    /// repro. A token that has been seen at any point is now the bound.
    #[tokio::test]
    async fn an_alternating_continuation_token_stops_the_listing() {
        let stub = ForeverTruncatedStub::start(Some(vec!["TOKEN-A", "TOKEN-B"]));
        // The guard covers only the env-var window inside `stub_backend`; it is
        // dropped before the await, because holding a std Mutex across one is
        // how a test deadlocks a runtime.
        let backend = {
            let _guard = ENV_LOCK.lock().unwrap();
            stub_backend(&stub, MAX_LIST_PAGES)
        };

        let err = backend.list("").await.unwrap_err().to_string();

        assert!(
            err.contains("already used"),
            "the cycle must be named: {err}"
        );
        // Page 1 -> TOKEN-A (new), page 2 -> TOKEN-B (new), page 3 -> TOKEN-A
        // (seen). Three billed requests, and no more.
        assert_eq!(stub.wire_pages(), 3, "{err}");
        assert_eq!(backend.ops().unwrap().class_a, 3, "{err}");
    }

    /// 966-M1, the half a token check cannot catch: an endpoint whose tokens
    /// never repeat. Only a page cap bounds that, and the cap is what the
    /// operator is billed at worst.
    #[tokio::test]
    async fn a_never_repeating_continuation_token_is_capped() {
        let stub = ForeverTruncatedStub::start(None);
        // Three rather than MAX_LIST_PAGES so the test costs three requests
        // instead of ten thousand; the code path is the same one.
        let backend = {
            let _guard = ENV_LOCK.lock().unwrap();
            stub_backend(&stub, 3)
        };

        let err = backend.list("").await.unwrap_err().to_string();

        assert!(
            err.contains("still truncated after 3 page(s)"),
            "the cap must say what it cost: {err}"
        );
        assert_eq!(stub.wire_pages(), 3, "{err}");
        assert_eq!(backend.ops().unwrap().class_a, 3, "{err}");
    }
}
