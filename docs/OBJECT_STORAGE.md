# Indexing object storage

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
