# Object storage

XERJ meets object storage in two independent places, and conflating them is the
mistake this page exists to prevent:

- **As a SOURCE of documents** — `xerj autoindex s3://bucket/prefix`, shipped.
  Part 1, "Indexing object storage".
- **As a HOME for the index itself** — segments and WAL living in a bucket. NOT
  wired: `storage.backend = "s3"` still refuses to start, and what exists is the
  client and the read-through cache underneath it. Part 2, "Object storage".

---

# Part 1 — Indexing object storage

`xerj autoindex` takes a bucket where it used to take a folder:

```sh
xerj autoindex s3://acme-docs/handbook
```

Everything after that is the discovery pipeline you already know — format
sniffing, the code and document extractors, the plan, the resume journal, the
incremental reconcile. The bucket is a *source*, not a different product.

- [What it needs](#what-it-needs)
- [What change detection uses](#what-change-detection-uses)
- [What it costs, in requests](#what-it-costs-in-requests)
- [Where the bytes go](#where-the-bytes-go)
- [Which keys are skipped](#which-keys-are-skipped)
- [Deletions](#deletions)
- [What is not implemented](#what-is-not-implemented)
- [Measured runs](#measured-runs)

---

## What it needs

**Credentials** come from the environment and from nowhere else:
`AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, plus `AWS_SESSION_TOKEN` for
temporary credentials. **Profile files (`AWS_PROFILE`, `~/.aws/credentials`),
instance metadata (IMDS) and SSO are not read** — `aws-config` is deliberately
not a dependency of this crate, for two reasons: a chain that reaches the
network has failure modes an indexing run should not inherit (IMDS on a machine
that is not an EC2 instance hangs until its timeout), and taking it re-enables
the SDK's default HTTPS client, which pulls in the `aws-lc-rs` C/assembly crypto
backend that XERJ's cross-compile matrix excludes. `xerj-storage`'s index-side
client takes the same position. If your keys live in a profile, export them for
the run:

```sh
env AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=… xerj autoindex s3://acme-docs/handbook
```

Credentials are never taken from the URL — a URL of the form
`s3://key:secret@bucket/p` is refused by name, because that shape puts a
credential in every request line and every log that records one.

**An endpoint** is needed for anything that is not Amazon S3:

```sh
# MinIO, Ceph, SeaweedFS, localstack …
xerj autoindex s3://mybucket/docs --endpoint-url http://127.0.0.1:9000

# Cloudflare R2 — the account host has no default, so it is required
xerj autoindex r2://mybucket/docs \
  --endpoint-url https://<account-id>.r2.cloudflarestorage.com
```

`--endpoint-url` falls back to `AWS_ENDPOINT_URL_S3`, then `AWS_ENDPOINT_URL`,
which is the precedence the AWS CLI uses. When an endpoint is given, the bucket
is addressed in the path (`/bucket/key`) rather than as a subdomain, because
virtual-host addressing on a private store needs DNS nobody set up.

Passing `--endpoint-url` with a *folder* is an error rather than a no-op. A flag
that is accepted and ignored is the failure shape this project refuses on
purpose.

**The prefix is a folder.** `s3://b/docs` lists `docs/` and *not* `docs-old/`,
even though plain S3 prefix matching is a string prefix. The effective prefix is
printed at the start of the run so the choice is visible.

**Errors name the fix.** Missing credentials, a missing bucket, access denied, a
bad signature and an unreachable endpoint each produce a different message
naming the thing to change — and none of them echoes a credential.

## What change detection uses

The **ETag plus the size**, treated as an opaque token.

```
etag:"9bb58f26192e4ba00f01e2e7b136bbd8"|size:11
etag:"ab484de63d3380a7a74265e4771c85fd-2"|size:16777216      ← multipart, used as-is
```

This is the decision that makes an incremental run cheap. The alternative —
downloading every object on every run to hash it — is the cost this feature
exists to remove.

What is deliberately *not* assumed:

- **An ETag is not an MD5.** A multipart upload's ETag is a digest of digests
  with a `-N` part-count suffix, and server-side encryption changes it again.
  XERJ never compares an ETag to a locally computed checksum; it only asks
  whether the string changed.
- **A `-N` ETag is not second-class.** It is stored verbatim and compared
  verbatim.
- **No ETag at all** (a store that omits it) falls back to last-modified plus
  size, and the run says how many objects that applied to — that pair can miss a
  same-size rewrite inside the timestamp's resolution.
- **Neither** available means the object is fetched every run. Correct and
  expensive beats cheap and wrong.

The token is recorded per object in `<state-dir>/object-source.json`, beside the
resume journal:

```json
{
  "version": 1,
  "source": "s3://acme-docs/handbook/",
  "last_run": {
    "finished": "2026-09-19T10:04:11.899Z",
    "objects_listed": 6,
    "objects_admitted": 5,
    "objects_downloaded": 0,
    "bytes_downloaded": 0,
    "objects_unchanged_not_downloaded": 5,
    "objects_removed_locally": 0,
    "list_requests_class_a": 1,
    "get_requests_class_b": 0,
    "transfer_ms": 1
  },
  "objects": {
    "intro.md": {
      "change_token": "etag:\"9bb58f26…\"|size:842",
      "size": 842,
      "etag": "\"9bb58f26…\"",
      "last_modified": "2026-09-19T10:00:00Z",
      "fetched": "2026-09-19T10:04:11.771Z"
    }
  }
}
```

It is a transfer cache, not an index record: losing it costs a re-download and
nothing else, which is why it is a sidecar rather than a journal entry.

`last_run` beside it records what the most recent materialisation cost — the two
request counts, what was downloaded, what was skipped as unchanged — so "what did
this run charge me" is answerable after the terminal has scrolled, and on the
unchanged re-run path where no fresh run document is written.

A second condition has to hold as well as the token matching: the mirrored file
must be present at the size the store reports. That is what makes a run killed
half-way through heal itself instead of indexing a truncated file.

## What it costs, in requests

Object stores bill per request, and the two classes are priced very differently.
On Cloudflare R2's free tier: **1,000,000 class-A operations a month**
(LIST/PUT/COPY — the scarce ones) and **10,000,000 class-B** (GET/HEAD), with
egress free.

**An autoindex run over a bucket performs no writes at all.** No PutObject, no
CopyObject, no multipart upload, no bucket creation. Its only class-A cost is
listing:

```
class-A per run = ceil(objects_under_the_prefix / 1000)
class-B per run = one GET per object whose ETag or size changed
```

Both lines assume every request succeeds first time. **The printed counts are
wire attempts, not logical calls**: the client's own retry loop charges the
counter before each attempt, so a LIST that the store throttles twice before
answering is reported as three requests, which is what the store bills. The
AWS SDK's own retry layer is switched off for exactly this reason — an
SDK-internal retry is invisible to a counter wrapped around the call, and the
undercount would be worst precisely when throttling makes the number matter. A
run that hits throttling also prints how many of its requests were retries.

**A failed run is not free, and is not paid for twice.** If a download fails
(anything other than "the object is gone", which is a normal LIST/GET race), the
run aborts rather than presenting the object as deleted. Every object whose
bytes had already landed is recorded in the manifest before the error is
returned — and during a long transfer the manifest is checkpointed every ten
seconds — so the re-run pays one GET per object it had *not* already fetched,
not N again. Its LIST cost is paid again in full: a resumed run re-lists, so
`ceil(objects / 1000)` class-A requests are spent every time. The partial
manifest records the run as `aborted`, with what it spent.
The number that empties a free tier is never a single run — it is a schedule. At
1,000,000 class-A a month you have about **23 operations a minute for the whole
account**:

| Objects under the prefix | LIST per run | Daily | Hourly | Every 5 min | Every minute |
| --- | --- | --- | --- | --- | --- |
| 1,000 | 1 | 30 (0.003%) | 720 (0.07%) | 8,640 (0.9%) | 43,200 (4.3%) |
| 10,000 | 10 | 300 (0.03%) | 7,200 (0.7%) | 86,400 (8.6%) | 432,000 (43%) |
| 100,000 | 100 | 3,000 (0.3%) | 72,000 (7.2%) | 864,000 (86%) | **4,320,000 (432%)** |
| 1,000,000 | 1,000 | 30,000 (3%) | 720,000 (72%) | **8,640,000 (864%)** | **43,200,000 (4,320%)** |

Percentages are of one month's free class-A allowance, and they are for **this
prefix alone** — the same allowance covers everything else the account does.

So: a daily or hourly re-index of a normal corpus is free-tier work. A
minute-interval watcher over a six-figure bucket is not, and no default in XERJ
creates one. `xerj autoindex` is a one-shot command; it does not poll, it does
not run a watcher, and it has no background listing loop. If you put it in cron,
the interval is the cost.

Every object-store run prints both halves of this so you do not have to compute
it:

```
autoindex: object store requests this run: 2 LIST (class A) + 1200 GET (class B), 0 MB transferred
autoindex: re-running this at the same size costs 2 class-A request(s) each time:
  60/month daily (0.0%), 1440/month hourly (0.1%), 17280/month every 5 minutes (1.7%)
  of a 1,000,000/month free allowance
```

Those lines are emitted on the warning channel, not the chatty one, so `--quiet`
does not hide them. The same numbers are in the run document under
`object_source` (`list_requests_class_a`, `get_requests_class_b`,
`objects_unchanged_not_downloaded`, …), so a run can be priced after the fact.

Three more things that keep the count down:

- **1,000 keys per LIST page**, the protocol maximum. Asking for fewer would
  multiply the class-A cost of every run.
- **Skipped keys cost nothing.** `node_modules/`, `.git/`, dotfiles and unsafe
  keys are filtered from the listing, so they are never fetched.
- **`--dry-run` never downloads.** It lists, prices the transfer and stops. Once
  the mirror is current there is nothing to transfer, so a dry run then gives the
  full discovery-plan projection for the cost of the listing alone.

A single run will refuse to list past 10,000 pages (10 million keys) and tell you
to narrow the prefix. That is 1% of a monthly allowance in one command, which is
the point at which an operator should be asked rather than billed.

## Where the bytes go

Objects are streamed into a **local mirror** under the state directory:

```
<state-dir>/object-cache/<bucket>-<prefix>/…
```

and the ordinary walk runs over that mirror. The consequences, plainly:

- **Extraction sees exactly the bytes it sees for a folder.** Nothing about
  sniffing, the tree-sitter extractors, PDF/DOCX/SQLite handling or the
  content-identity contract changed.
- **You need local disk for the prefix you index.** `--state-dir` moves the
  mirror; the path is printed on every run, with the byte count before the
  transfer starts.
- **Nothing is held in memory.** Transfer is `io::copy` through a 256 KiB buffer
  into a `.part` file that is then atomically renamed, at most 16 objects in
  flight. Measured: a 1 GiB object moves with about 4 MB of RSS growth.

Why a mirror rather than piping the network stream into the extractors: several
of them are not sequential readers. A PDF is read from its trailer backwards, a
DOCX is a ZIP whose central directory is at the *end*, SQLite seeks by page, and
the content-identity contract reads each file twice (hash, then extract) plus a
third time on verify. Over a network stream each of those becomes either a
re-GET — more requests, more money — or a whole-object copy in memory, which is
the one thing that must not happen.

**The index still lives on local disk**, in the XERJ node `--url` points at.
Nothing in this path writes to the bucket. Keeping the *index* in object storage
is separate work and is not part of this feature.

## Which keys are skipped

An object key is an arbitrary string, not a path. Keys that cannot become a safe
and portable relative path are refused by name, and the run reports each rule
with a count:

| Rule | What it catches |
| --- | --- |
| `object:folder-marker` | the key equals the prefix, or ends in `/` (every console makes these) |
| `object:unsafe-key` | `..`, `.`, an empty component (`a//b`), a leading `/`, a backslash, a control character, a component over 255 bytes, a trailing dot or space, a Windows reserved basename (`CON`, `NUL`, `COM1`…) |
| `object:case-collision` | two keys that differ only in case, which would be one file on macOS or Windows |
| `hidden:dotfile` | any dot-prefixed component — a bucket's `.env`, `.git/config` and `.ssh/id_rsa` stay out of a queryable index, exactly as in a folder |
| `default:build-output` | `node_modules/`, `vendor/`, `target/`, `dist/`, `build/`, `.venv/`, `__pycache__/` — the same built-in list the walk uses |

Two exceptions to the dotfile rule: `.gitignore` and `.xerjignore` **are**
fetched, so a bucket's own ignore rules still apply to it. The walker then
declines to index them, under the same hidden-name rule.

The unportable-key rules are applied on **every** platform, not only on Windows,
so one bucket produces one corpus whatever machine indexes it. A key that is
refused is named in the report rather than silently missing.

## Deletions

An object that disappears from the bucket stops appearing in search results on
the next run, and it does so through the reconcile machinery that already handles
a deleted file — there is no second mechanism. **That machinery is the generated
`--no-graph` journal**, which reconciles add, change, delete, rename and no-op
runs. A bucket indexed on the default graph-enabled path inherits the same limit
a *folder* has there: the run refuses, makes no remote mutation, and prints which
content groups vanished and the three recovery routes. This is not an
object-store restriction — it is where corpus-generation reconcile has landed so
far — but it decides which flag you want when the source is a live bucket:

```sh
# a bucket whose objects get deleted: use the generated journal
xerj autoindex s3://acme-docs/handbook --no-graph
```

The mechanism: after listing, the mirror is walked and every file the store no
longer lists is deleted from it (along with stale `.part` files and any directory
that empties). The walk then sees a smaller corpus, and the existing incremental
reconcile removes those documents.

The mirror is walked rather than the manifest diffed, on purpose: a lost or
corrupt manifest must not be able to leave a deleted object searchable forever.

Two failure directions are handled asymmetrically, because they are not
symmetric:

- An object that is **listed and then 404s** was deleted between the LIST and the
  GET. That race is normal on a live bucket, so the run treats it as deleted and
  continues.
- Any **other** GET failure aborts the run before anything is indexed. Skipping a
  failed download would present the object to the reconcile step as deleted, and
  a transient 500 would then delete live documents.

## What is not implemented

Said plainly, because the absence of a feature is a fact about the product:

- **The index is not in the bucket.** Only the *source* documents are.
- **No watch mode.** There is no polling, no event subscription, no S3 event
  notification / SQS integration. A re-run is how you pick up changes, and the
  run tells you what a schedule would cost.
- **No server-side filtering.** There is no `--include` / `--exclude` glob for
  keys yet; filtering is the built-in rule set above plus the prefix.
- **No versioned-object support.** The current version of each key is indexed;
  object versions and delete markers are not enumerated.
- **No requester-pays, no SSE-C.** A bucket that needs
  `x-amz-request-payer` or a customer-provided encryption key is not supported.
- **The bucket's ignore files are honoured only where they were fetched.** Only
  `.gitignore` / `.xerjignore` objects are mirrored; a rule file kept outside the
  prefix has no effect.
- **The embedder is lexical by default.** Indexing a bucket does not change that
  (see `docs/EXPERIMENTAL_ONNX.md`).

## Measured runs

Against MinIO (`quay.io/minio/minio:latest`, loopback), from the suite in
`engine/crates/xerj-autoindex/src/objsource_minio_tests.rs`:

| Case | Result |
| --- | --- |
| first run, 4 keys in the prefix | 3 admitted (`.env` skipped), 3 downloaded, **1 LIST + 3 GET**, 4 ms |
| unchanged re-run | 0 downloaded, 0 bytes, **1 LIST + 0 GET**, 1 ms |
| one changed, one new, one deleted | 2 downloaded, 1 unchanged, 1 removed, 2 GET, 4 ms |
| 1,200 keys | **2 LIST pages**, 1,200 GET, 252 ms; re-run 1,200 unchanged, 2 LIST, **0 GET** |
| real multipart upload | ETag `"ab484de63d3380a7a74265e4771c85fd-2"`; re-run 0 GET |
| 1 GiB object | 1,024 MB, **RSS 22 MB → 26 MB**, 281–318 ms over four runs; not re-downloaded on the second run |

The 1 GiB row is the one worth reading carefully. The RSS figure is the claim —
a gigabyte moves through a 256 KiB copy buffer, so memory does not track object
size — and the millisecond figure is not: it is a loopback transfer from a
container on the same host with the object in page cache, on an idle 32-core /
119 GiB box (load average ~2.4), and it varies by ±10% run to run. Four runs on
2026-09-20 gave 281, 294, 300 and 318 ms. Do not read it as a throughput
benchmark; nothing here is one.

The suite is skipped with a printed reason when `XERJ_MINIO_ENDPOINT` is unset,
so it never needs an account and never touches a paid store. The big-object and
many-object cases exist so they do not have to.

### End to end, against a running node

The suite above stops at the mirror. These runs go all the way to search results,
against a real `xerj` node (v1.0.0-rc.74, auth on, throwaway data dir) and a
loopback MinIO holding six keys under `handbook/` — four small text/code objects,
a 12 MiB log uploaded as a **real 3-part multipart** (`…-3` ETag), plus `.env`
and a sibling `handbook-old/` prefix that must not be indexed:

| Run | Requests | Result |
| --- | --- | --- |
| first index | **1 LIST + 5 GET** | 5 objects, 9,997 documents live in 11.3 s; `.env` skipped, `handbook-old/` never listed; searches for terms in each object return that object |
| unchanged re-run | **1 LIST + 0 GET** | 0 records submitted, 0.1 s; the multipart object is not re-fetched |
| one object changed | **1 LIST + 1 GET** | only the changed key is fetched, and its new text is searchable on that run |
| one object deleted (`--no-graph`) | **1 LIST + 0 GET** | the deleted object's documents are gone from the index; every other document survives |

Two facts worth stating because they were checked rather than assumed:

- The mirrored 12 MiB multipart object is **byte-identical** to what was
  uploaded (same MD5), and the same corpus indexed from a plain folder produces
  the same document count as indexing it from the bucket — the object path is
  not a second, weaker extraction path.
- The deletion row needs `--no-graph`, for the reason in [Deletions](#deletions).

Everything measured here is MinIO, which is S3-compatible but is not Amazon S3
and is not R2. The `r2://` scheme, the account-host endpoint and the R2 pricing
arithmetic above are implemented and reasoned from Cloudflare's published
pricing; the numbers in these tables were not taken from a paid store, and this
page does not claim they were.

---

# Part 2 — Object storage

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
