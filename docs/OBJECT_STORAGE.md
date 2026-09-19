# Object storage

XERJ can talk to S3-compatible object storage — Cloudflare R2, MinIO, AWS S3 —
through `StorageBackend`. This page says exactly what that means today, because
the honest answer has two halves and only one of them is finished.

**Implemented and tested against real endpoints:** the client
(`engine/crates/xerj-storage/src/s3.rs`) and the read-through segment cache
(`engine/crates/xerj-storage/src/cache.rs`).

**Not implemented:** an index that lives in a bucket. `storage.backend = "s3"`
still refuses to start, on purpose. Jump to
[What is not wired](#what-is-not-wired) for why, and for the measurement that
settles it.

## Contents

- [The client](#the-client)
- [Request cost is a design constraint](#request-cost-is-a-design-constraint)
- [Free-tier arithmetic](#free-tier-arithmetic)
- [The operation budget](#the-operation-budget)
- [The read-through cache](#the-read-through-cache)
- [Measurements](#measurements)
- [Retry and timeout policy](#retry-and-timeout-policy)
- [Credentials](#credentials)
- [What is not wired](#what-is-not-wired)
- [Running the tests](#running-the-tests)

## The client

`xerj_storage::s3::S3Backend` implements the `StorageBackend` trait over
`aws-sdk-s3`:

| Trait method | Request | Billing class |
|---|---|---|
| `read_range` | `GetObject` with `Range: bytes=<start>-<end>` | Class B |
| `write` | `PutObject` | **Class A** |
| `list` | `ListObjectsV2`, paginated | **Class A, once per page** |
| `exists` | `HeadObject` | Class B |
| `metadata` | `HeadObject` (size, last-modified, ETag) | Class B |
| `delete` | `DeleteObject` | free |

Configuration is `S3Config`: bucket, key prefix, region, endpoint override, and
path-style addressing. One type covers all three providers.

```rust
use xerj_storage::s3::{S3Backend, S3Config};

// Cloudflare R2
let r2 = S3Backend::connect(
    S3Config::new("my-bucket")
        .with_prefix("xerj/")
        .with_region("auto")
        .with_endpoint("https://<account-id>.r2.cloudflarestorage.com"),
)?;

// MinIO
let minio = S3Backend::connect(
    S3Config::new("my-bucket").with_endpoint("http://127.0.0.1:9000"),
)?;

// AWS S3 — no endpoint override, and a real region
let s3 = S3Backend::connect(S3Config::new("my-bucket").with_region("eu-west-1"))?;
```

Defaults worth knowing:

- **`region` is `"auto"`.** R2 requires that literal value and MinIO ignores
  the field. AWS S3 is the case you must configure.
- **`force_path_style` is `true`** — `<endpoint>/<bucket>/<key>`. Path style
  works on R2, MinIO and AWS S3; virtual-host style works on two of the three,
  so the default is the one that works everywhere.
- **`connect` does no I/O.** It does not verify the bucket or the credentials,
  because a `HeadBucket` probe on every startup is a billed request spent on
  something the first real operation reports anyway.

`FileMetadata::created` is always `None` for an object store. Object stores
record one last-modified time, and a `PutObject` that replaces an object resets
it; reporting that as a creation time would be a fabrication.

## Request cost is a design constraint

Object stores bill per request, and the numbers are small enough that a polling
interval is an architectural decision rather than a tuning knob.

Cloudflare R2's free tier, per **account**, per month:

| | Allowance | What counts |
|---|---|---|
| Storage | 10 GB-month | |
| Class A | **1,000,000** | `PutObject`, `ListObjectsV2`, `CopyObject`, `CreateMultipartUpload`, `UploadPart`, `CompleteMultipartUpload` |
| Class B | 10,000,000 | `GetObject`, `HeadObject` |
| Free | unlimited | `DeleteObject`, `AbortMultipartUpload` |
| Egress | free | |

1,000,000 Class A operations a month is **about 23 per minute for everything
the account does** — every bucket, every client, not just XERJ.

A `ListObjectsV2` page is a Class A operation, and a page holds at most 1,000
keys. So the cost of watching a bucket is proportional to how many objects are
in it, not to how often you find something.

## Free-tier arithmetic

Class A operations per month for one poller doing `ListObjectsV2`, as a
percentage of R2's 1,000,000 free Class A allowance. A 30-day month is
2,592,000 seconds; a bucket of *n* objects needs `ceil(n / 1000)` pages per
cycle.

| Poll interval | 1 object | 10,000 objects | 100,000 objects | 1,000,000 objects |
|---|---|---|---|---|
| 5 s | 518,400 (52%) | 5,184,000 (518%) | 51,840,000 (5,184%) | 518,400,000 (51,840%) |
| 30 s | 86,400 (9%) | 864,000 (86%) | 8,640,000 (864%) | 86,400,000 (8,640%) |
| 60 s | 43,200 (4%) | 432,000 (43%) | 4,320,000 (432%) | 43,200,000 (4,320%) |
| 5 min | 8,640 (0.9%) | 86,400 (9%) | 864,000 (86%) | 8,640,000 (864%) |
| 1 hour | 720 (0.07%) | 7,200 (0.7%) | 72,000 (7%) | 720,000 (72%) |
| 1 day | 30 (0.003%) | 300 (0.03%) | 3,000 (0.3%) | 30,000 (3%) |

Read the row you are actually in:

- A watcher polling **every 5 seconds spends half the free tier on an empty
  bucket**, finding nothing.
- Polling a **100,000-object bucket every 60 seconds costs over four times the
  entire free allowance** — 100 pages a cycle, 43,200 cycles a month.
- Anything at or above 10,000 objects wants an interval of **5 minutes or
  more**, and a million-object bucket wants **hourly at the very fastest**.

The same arithmetic on the write side, because `PutObject` is Class A too. One
object per flushed segment, flushing every 30 seconds, is 86,400 Class A
operations a month — 9% of the tier, fine. But a segment is not one file:

| Flush interval | 1 object per segment | 104 objects per segment (one per file) |
|---|---|---|
| 30 s | 86,400 (9%) | 8,985,600 (**899%**) |
| 5 min | 8,640 (0.9%) | 898,560 (90%) |

104 is a hand count of one real 25-field index in this repository — `.seg`,
`.sidx`, `.dv`, `.ids`, and `.fst`/`.meta`/`.norms`/`.post` per indexed field —
not a figure any test asserts. `object_store_mode_does_not_yet_make_an_index_stateless`
asserts only the part the conclusion rests on: the local directory holds many
more files than the bucket does. The conclusion does not depend on the exact
number. Uploading a segment as one object per file would cost nine times the
free tier at a 30-second flush interval, and it would still be several times
over at a tenth of 104. Any future wiring of the index path has to bundle a segment into one
object; see [What is not wired](#what-is-not-wired).

## The operation budget

Every billed attempt is counted, by class, and the counters are readable:

```rust
use xerj_storage::backend::StorageBackend;

let ops = backend.ops().expect("object stores have counters; local disk does not");
println!("Class A: {}  Class B: {}  retried: {}", ops.class_a, ops.class_b, ops.retried);
```

Counted per **wire attempt**, not per logical call — a `PutObject` that fails
once and succeeds on retry is billed twice and counted twice. This is why the
AWS SDK's own retry layer is switched off and retries happen in XERJ's code: an
SDK-internal retry is invisible to a counter wrapped around the call, and an
undercount is the number that produces a surprise invoice.

A listing is bounded twice, because a store that keeps answering "truncated"
bills a Class A request per page for as long as it likes. A continuation token
that has already been seen ends the listing with an error (any cycle length, not
just an immediately repeated token), and `S3Config::max_list_pages` (default
10,000 pages, i.e. 10 million keys and 1% of R2's monthly free allowance) ends
it even when the tokens never repeat.

`OpBudget` puts a ceiling on it:

```rust
use xerj_storage::backend::OpBudget;

let backend = S3Backend::connect(
    S3Config::new("my-bucket").with_budget(OpBudget::new(50_000, 500_000)),
)?;
```

Past the ceiling, requests are refused before they leave the process, with an
error naming the limit. `DeleteObject` is never refused, so cleanup always
works.

**What the budget is not:** a monthly quota. The counters live in one process
and reset when it restarts, and they know nothing about other clients on the
same bucket. It is a circuit breaker that stops one runaway loop from spending
a month's allowance in an afternoon. Staying inside a monthly free tier is the
arithmetic above; the budget only bounds the blast radius of a bug.

The default is **no ceiling**, because a ceiling that fires in the middle of a
legitimate bulk ingest is worse than no ceiling — the number has to match your
own traffic.

For an operator-facing feature that would otherwise have none, there is a
preset:

```rust
OpBudget::r2_free_tier_slice()   // 200,000 Class A, 2,000,000 Class B
```

A fifth of R2's monthly free allowance, so an account that also serves other
traffic is not spent dry by one process. Its own doc comment says what it cannot
do: the counters are process-lifetime, so it approximates a monthly bound only
for a process that lives about a month. A CLI run that exits in a minute will
never reach it, and a server restarted daily gets thirty times what the name
suggests. Use the table above for the monthly arithmetic; use the preset to stop
a bug.

## The read-through cache

`SegmentCache` keeps a local copy of fetched objects, so on a per-request-billed
store the hit rate *is* the cost model: every miss is a `GetObject`.

```rust
let cache = SegmentCache::new("/var/lib/xerj/cache", 8 * 1024 * 1024 * 1024, backend);

let footer = cache.get_range("segments/seg-0001.seg", offset, len).await?;
let stats = cache.stats();
println!("hit rate {:?}, fetched {} bytes", stats.hit_rate(), stats.bytes_fetched);
```

Two read paths, and the difference matters:

- **`get_range`** — serves the range from the cached object; on a miss fetches
  the **whole object**, caches it, then slices. Right when the object will be
  read again, which is the engine's normal pattern (segment reads are `mmap`
  over a whole file).
- **`get_range_uncached`** — fetches **only the range** and caches nothing.
  Right for a cold one-off probe: a footer, a header, one skip-list block. Same
  single billed request either way, so the saving is bandwidth and latency, not
  operations.

`maybe_evict()` brings the directory under `max_size_bytes`, removing
oldest-by-mtime first — **when something calls it**. Nothing does yet outside
tests: there is no background evictor, so whatever drives the cache has to drive
eviction, or the directory grows until the disk does. Cache entries are written
temp-file-plus-rename, because a half-written entry `exists()` and would serve
short reads forever.

The cache also never revalidates: a cached key is served from disk forever, with
no `ETag`/`If-None-Match` check. That is correct for the only thing it is used
for — segment files, which are immutable once written and replaced by a new id
rather than edited — and wrong for anything mutable. Before caching a key whose
bytes can change under the same name, add revalidation; do not assume the cache
will notice.

## Measurements

All from runs in this repository, on 2026-09-19. A 4 MiB object standing in for
a small segment, a 64 KiB range read from offset 1,000,000, five runs, medians.

**MinIO on loopback** (`quay.io/minio/minio:latest`, 127.0.0.1) — these isolate
the software path, with effectively no network:

| | Median | Range |
|---|---|---|
| Cold range read (whole-object fetch, 4 MiB) | 5.98 ms | 5.49–7.96 ms |
| Uncached ranged read (64 KiB only) | 1.67 ms | 1.41–2.01 ms |
| Warm range read (from local cache) | 0.068 ms | 0.065–0.109 ms |

**Cloudflare R2** over the internet from a sandboxed CI-style box, one run on
2026-09-19 that has not been repeated (R2 traffic from this repository is
currently frozen, so treat the row as a recorded observation rather than a
reproducible measurement):

| | Measured |
|---|---|
| Cold range read (whole-object fetch, 4 MiB) | 3.72 s |
| Uncached ranged read (64 KiB only) | 0.67 s |
| Warm range read (from local cache) | 0.00012 s |

The R2 figures are dominated by that box's ~1 MB/s effective throughput. They
are **not** a claim about R2's performance, and nothing here is a
throughput benchmark. What they do show, and what the loopback numbers hide, is
the shape: when bandwidth is the constraint, fetching a whole 4 MiB object to
serve a 64 KiB range costs 5.6× more than fetching the range. That is the whole
reason `get_range_uncached` exists.

Cache accounting, asserted rather than observed (from
`read_through_cache_measured_cold_and_warm`): ten range reads of one object
produce **1 miss and 9 hits** — one billed `GetObject` for ten reads, hit rate
0.900. `get_range` transferred 4,194,304 bytes; `get_range_uncached` of the same
range transferred 65,536.

Neither measurement involves the index path, because the index path does not
use this backend. See below.

## Retry and timeout policy

- Up to **4 attempts** (3 retries). Exponential backoff from 100 ms, doubling,
  capped at 5 s, full-jittered into the lower half of each interval so a fleet
  does not resynchronise.
- **Retried:** connect/dispatch failures, response-parse failures, per-attempt
  timeouts, any HTTP 5xx, HTTP 429, and the S3 codes `SlowDown`,
  `RequestTimeout`, `InternalError`, `ServiceUnavailable`,
  `RequestTimeTooSkewed`, `ThrottlingException`, `TooManyRequests`.
- **Not retried:** any other 4xx — `AccessDenied`, `NoSuchBucket`,
  `InvalidAccessKeyId`, `SignatureDoesNotMatch`. Retrying a rejected signature
  spends billed requests and never succeeds.
- **5 s** connect timeout, **30 s** per-attempt timeout, and **no**
  whole-operation timeout: a 4 GiB upload and a 4 KiB footer read have no
  shared deadline, so the caller owns that.
- A missing object is an `io::ErrorKind::NotFound`, the same as a missing local
  file, rather than a generic backend error. A missing **bucket** is not: it
  carries the provider's `NoSuchBucket` code and is a permanent error, because a
  mistyped bucket reported as "the object is absent" is a configuration mistake
  hidden behind an empty store.
  The `HeadObject` path (`exists`, `metadata`) is the one exception and cannot
  be fixed here: a HEAD response has no body, so there is no error code to read
  and a missing bucket is indistinguishable from a missing key — `exists()`
  answers `false`. Probe a bucket once with `list()` at startup if that
  distinction matters to you. (Pinned by
  `a_missing_bucket_is_an_error_not_an_absent_object`.)

All of it is `RetryPolicy` / `S3Config`, and `RetryPolicy::none()` makes one
logical call cost exactly one billed request.

## Credentials

From the environment, and nowhere else:

```sh
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
export AWS_SESSION_TOKEN=...   # optional
```

`aws-config` is deliberately not a dependency, so there is **no** IMDS probe,
no SSO, no profile files, no web identity. Two reasons: on a box with no
instance metadata service the IMDS probe is a multi-second startup stall for a
lookup that cannot succeed, and a credential chain that silently picks up an
unrelated profile is how data ends up in the wrong bucket.

A missing or half-set credential is an error that names the variables and
echoes no values, so it is safe in a log.

## What is not wired

**`storage.backend = "s3"` refuses to start, and should.** The guard is in
`engine/crates/xerj-common/src/config.rs`.

The client works. The index does not use it. Concretely:

1. **Nothing constructs the object-store mode.** `StorageMode::ObjectStore`
   exists in `engine/crates/xerj-storage/src/index_store.rs` and has both a
   flush-upload path and a read-through open path. The only place outside tests
   that builds a `StorageMode` is `engine/crates/xerj-engine/src/index.rs`, and
   it hardcodes `StorageMode::Local`.
2. **The upload path sends 1 file of 104.** It uploads the `.seg` and nothing
   else — not the `.sidx` skip index, not `.dv` doc values, not `.ids`, and not
   the four FTS files per indexed field.
3. **`snapshot.json` never leaves local disk.** That file lists which segments
   exist, so without it a node has nothing to look for.

Point 3 is what makes an index in a bucket unrecoverable, and it is measured,
not assumed. The test
`object_store_mode_does_not_yet_make_an_index_stateless` (in `index_store.rs`)
runs the experiment: index two documents into a bucket-backed store, drop the
node, open a fresh store on an empty data directory pointed at the same bucket.
The fresh node sees **zero segments**.

The same test found the half that does work, and it is the useful half: asked
for a segment **by id**, a fresh node with an empty disk fetches it from the
bucket, caches it locally, and reads back the correct document count. The bytes
survive losing the node. What is missing is the catalogue that lets a node know
what to ask for.

That test asserts the gap deliberately. When the wiring lands it will fail, and
the failure is the reminder to update this page and to lift the config guard.

### What the remaining work needs

- **Bundle a segment into one object.** Not an optimisation — see the
  arithmetic above: one object per file is 899% of the free tier at a 30-second
  flush interval, and one object per segment is 9%. quickwit solved this with
  its `.split` format: concatenate every file of a split into one object and
  append a footer holding each file's `[start, end)` byte offsets, so an
  individual file is recovered with a ranged `GetObject`
  (`quickwit/docs/internals/split-format.md`, Apache-2.0 — approach cited, no
  code taken).
- **Publish the snapshot to the bucket atomically**, with a scheme that
  survives two nodes writing at once. This is the hard part, and it is why the
  work is not in the same change as the client.
- **Crash-consistency tests against a real endpoint** — including a killed
  process mid-upload — before the config guard is relaxed.
- **Multipart upload**, which this client does not implement, so an object is
  capped at the provider's 5 GiB single-PUT limit.

Also not implemented: bulk delete, server-side copy, object tagging,
versioning, SSE-C.

## Running the tests

Unit tests need nothing. The integration tests need an endpoint and **skip with
a printed reason** when one is absent — verified by running them with the
environment unset, which prints eight explicit skips rather than eight silent
passes.

```sh
docker run -d --name xerj-minio -p 9000:9000 \
  -e MINIO_ROOT_USER=minioadmin -e MINIO_ROOT_PASSWORD=minioadmin \
  quay.io/minio/minio:latest server /data
# create the bucket first — XERJ never calls CreateBucket

export XERJ_S3_TEST_ENDPOINT=http://127.0.0.1:9000
export XERJ_S3_TEST_BUCKET=xerj-test
export XERJ_S3_TEST_REGION=us-east-1
export AWS_ACCESS_KEY_ID=minioadmin AWS_SECRET_ACCESS_KEY=minioadmin

cd engine
cargo test -p xerj-storage --test s3_object_store -- --nocapture
```

Every test writes under its own key prefix and deletes what it wrote.

`XERJ_S3_TEST_PAGINATION=1` adds the test that crosses `ListObjectsV2`'s
1,000-key page boundary. It writes 1,200 objects, which is **1,200 Class A
operations** — free on MinIO, metered on R2. That is why it is opt-in.

The same suite ran green against R2 once, on 2026-09-19, by pointing
`XERJ_S3_TEST_ENDPOINT` at `https://<account>.r2.cloudflarestorage.com` with
`XERJ_S3_TEST_REGION=auto`. That run is not repeated on demand and no CI job
does it: every number this repository re-verifies comes from MinIO. Read the
operation counts each test prints before you point it at a metered bucket —
the pagination test alone is 1,200 Class A operations.
