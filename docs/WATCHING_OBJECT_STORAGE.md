# Keeping a bucket-backed index current

A bucket has no inotify. There are exactly two ways to learn that an object
changed, and choosing between them is a cost decision before it is a latency
decision.

This page documents what XERJ implements today — **polling**, in
`xerj autoindex s3://<bucket>/<prefix> --watch` — the arithmetic that decides
the poll interval, and what the event-driven alternative would take. Everything
below was read out of `engine/crates/xerj-autoindex/src/objwatch/`.

## What this does and does not do

It produces a **change feed**. One JSON object per changed object on stdout (or
to `--events-out`), with the kind (`added`, `changed`, `deleted`), the key, the
size, the ETag, the store's `LastModified`, and an xxh3 digest of the bytes it
read:

```json
{"xerj":"objwatch-change","kind":"changed","key":"docs/alpha.md","size":184,
 "etag":"9f2c…","last_modified":"2026-09-19T15:04:11.000Z","digest":"1a7c…",
 "bytes_fetched":184,"reason":"etag","truncated":false}
```

It does **not** index, and it is not the same command as
`xerj autoindex s3://bucket/prefix`. That one — see
[docs/OBJECT_STORAGE.md](./OBJECT_STORAGE.md) — is a **one-shot** index: it
mirrors the prefix to local disk and runs the ordinary discovery pipeline over
it, once. `--watch` is the other half: it tells you *what changed*, as a feed,
and writes to no node. Wiring the feed into the indexer so a bucket stays
current without a cron is still a separate change; `ObjectSource` and
`ChangeSink` in `objwatch/mod.rs` are the two seams it plugs into. It also
does not implement the event-driven path, retries with backoff, virtual-host
addressing, IMDS or profile credential chains, or `ListObjectVersions` — a
versioned bucket is watched by current version only.

Credentials come from the environment only: `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, optionally `AWS_SESSION_TOKEN`. There is deliberately no
flag for them, because a flag puts a secret in a shell history and in `ps`.

## The cost shape: `ListObjectsV2` is a Class A operation

Cloudflare R2 (and S3, with different prices) splits operations into classes.
`ListObjectsV2` is in the expensive one:

| class | operations | R2 free tier |
|---|---|---|
| **A** | `ListObjectsV2`, `PutObject`, `CopyObject`, `ListBuckets`, the multipart verbs | **1,000,000 / month** |
| B | `GetObject`, `HeadObject` | 10,000,000 / month |
| free | `DeleteObject`, `AbortMultipartUpload` | — |

One list call returns at most 1,000 keys — `--page-size` keys, and 1,000 is both
the default and the API maximum — so **one full scan of an N-object prefix costs
at least `ceil(N / page_size)` Class A operations, every cycle, whether anything
changed or not.** An empty prefix still costs one: you have to ask to learn it is
empty.

`ceil(N / page_size)` is the **floor, not the bill.** When a store stops paging
is implementation-defined: it may set `IsTruncated` on a page it has just filled
and only reveal that there is nothing after it when you ask again. Measured
against MinIO at page size 1,000 (`boto3`, one `list_objects_v2` paginator,
counted per call):

| objects | calls MinIO served | pages returned |
|---|---|---|
| 1,000 | 1 | `(1000, IsTruncated=false)` |
| 2,000 | 2 | `(1000, true) (1000, false)` |
| 3,000 | **4** | three full pages, then `(0, false)` |
| 5,000 | **6** | five full pages, then `(0, false)` |
| 10,000 | **11** | ten full pages, then `(0, false)` |

So this page used to give the wrong reason ("a store serves one extra empty page
when the key count divides exactly by the page size", #968 review, m1): 1,000 and
2,000 divide exactly and cost no extra page, while 3,000 and 5,000 do. Above two
pages MinIO's walker cannot tell it has reached the end without asking once more.
Another gateway will draw that line somewhere else again.

The code is not optimistic about it — the projection takes
`max(ceil(keys / page_size), calls the store actually served this cycle)`, so a
store that pages differently raises the projection rather than hiding under it.
Budget for `ceil(N / page_size) + 1` and you will not be surprised.

The page size is part of the bill, not a tuning knob: `--page-size 10` on a
26-object prefix is **three** list calls a cycle, not one. The projection is
computed at the page size the watch actually uses, and is never lower than the
number of calls the store just served.

1,000,000 Class A operations a month is about **23 a minute for the whole
account**. That is the number every default here is derived from:

| objects under the prefix | list calls / cycle (floor) | every 5 s | every 60 s | every 300 s (default) | every 3600 s |
|---|---|---|---|---|---|
| 0 (empty) | 1 | 518,400 | 43,200 | 8,640 | 720 |
| 1,000 | 1 | 518,400 | 43,200 | 8,640 | 720 |
| 10,000 | 10 | 5,184,000 | 432,000 | 86,400 | 7,200 |
| 100,000 | 100 | 51,840,000 | 4,320,000 | 864,000 | 72,000 |
| 1,000,000 | 1,000 | 518,400,000 | 43,200,000 | 8,640,000 | 720,000 |

Every cell is `ceil(N / 1000)` calls times the cycles in a 30-day month — the
**floor**, for the reason above. MinIO served **11** calls for the 10,000-object
row, not 10, which is 95,040 operations a month at the default interval rather
than 86,400: about 10% more. That row used to be published as if 10 were the
number to plan with.

Read the first row again: **a 5-second poll on an empty bucket spends half the
free tier to watch nothing.** A 60-second poll on a 100,000-object bucket spends
over four times the entire monthly allowance. A 1-second poll on that bucket is
about 259,200,000 operations a month.

Those numbers are computed by `objwatch::cost`, and
`the_published_cost_table_is_what_the_code_computes` in `objwatch/cost.rs`
asserts them, so this table cannot drift away from the code that enforces it.

### The default, and the refusal

* **Default interval: 300 s** (`DEFAULT_POLL_INTERVAL_SECS`). At 300 s a watcher
  runs 8,640 cycles a month, so it stays inside the default budget for any
  prefix up to about 23,000 objects and inside the whole free tier up to about
  115,000.
* **Default budget: 200,000 Class A operations a month**
  (`DEFAULT_MAX_MONTHLY_CLASS_A`) — 20% of the free tier, not 100%, because the
  same account's other buckets spend from the same allowance. A watcher that
  budgets for all of it leaves nothing for the product it is watching for.
* **Default GET budget: 2,000,000 Class B operations a month**
  (`DEFAULT_MAX_MONTHLY_CLASS_B`, `--max-monthly-gets`) — 20% of that free tier,
  for the same reason. A first scan reads every object once and a churning
  prefix re-reads every changed one, so GETs need a breaker of their own.
* The budget is a **circuit breaker, checked on every cycle**, not a first-cycle
  greeting. Any cycle whose projection is over budget stops the watch; so does a
  month whose Class A or Class B budget is spent. Nothing further is fetched or
  emitted; a decision-request document goes to stdout and the exit code is **4**,
  the same contract the folder-indexing gate uses:

```json
{"xerj":"objwatch-decision-request","exit_code":4,"reason":"poll_cost_over_budget",
 "location":"s3://logs/ @ https://acct.r2.cloudflarestorage.com",
 "projection":{"keys_listed":100000,"list_calls_per_cycle":100,"interval_secs":300,
   "cycles_per_month":8640,"monthly_class_a":864000,"budget_monthly_class_a":200000,
   "free_tier_monthly_class_a":1000000,"free_tier_percent":86.4,
   "min_safe_interval_secs":1296,"over_budget":true},
 "answers":["--poll-interval <secs>","--append-only","--allow-cost"]}
```

  `min_safe_interval_secs` is the inverse of the projection: the smallest
  interval that fits the budget.

  A later cycle used to **warn** and keep polling when the bucket grew past the
  budget. It does not any more (#968 review, F8): a warning on stderr that nobody
  reads is how a watcher spends someone's free tier overnight, so growth past the
  budget stops the watch with the same exit 4 and `"cycle": N` in the document.
  `--allow-cost` is the way to say yes on purpose.

#### The three breakers

| reason (in the JSON) | what tripped | answers |
|---|---|---|
| `poll_cost_over_budget` | `ceil(keys / page_size) x cycles_per_month` exceeds `--max-monthly-ops`, on any cycle | `--poll-interval`, a narrower prefix, `--append-only`, `--allow-cost` |
| `monthly_class_a_budget_spent` | the Class A operations ALREADY spent this calendar month plus the next listing exceed the budget | `--max-monthly-ops`, `--allow-cost`, or wait for the new month |
| `monthly_class_b_budget_spent` | the GETs this cycle needs plus those already spent exceed `--max-monthly-gets`; checked before the first GET, so nothing is fetched | `--no-fetch`, `--max-monthly-gets`, `--allow-cost` |

The spend is counted in `<state-dir>/objwatch-spend.json`
(`{"month":"2026-09","class_a":...,"class_b":...}`, reset on the first cycle of a
new UTC month) so that it **survives restarts**. Without it, a watcher a
supervisor restarts — on every crash, every deploy — would start a fresh
process-local count each time and could rescan a large bucket all month without
ever tripping. Failed requests are counted too: whether a 503 is billed is the
provider's call, and the ledger assumes it is.

**The ledger is written before the calls it pays for.** A first scan of a large
bucket is one long run of `ListObjectsV2` calls, and the #968 verification found
that charging the cycle after the scan returned made the restart claim above
false for exactly the crash mode it names: five `kill -9`s two seconds into a
2,001-call scan served 49, 98, 146, 194 and 241 list calls between them, and
`objwatch-spend.json` was never created. A supervisor with a short start-up
timeout, an OOM killer, or a redeploy loop could therefore spend the whole
month's allowance without the ledger learning a thing.

So the scan reserves `CLASS_A_CHARGE_BATCH` (16) operations at a time and
persists the ledger *first*, then makes those calls, and refunds the unused tail
of the last reservation when it finishes. A killed scan over-records by at most
15 operations — the safe direction, out of a 200,000 default budget — and a
completed cycle records exactly what the store served, which is the invariant
the MinIO tests assert (reported `list_calls` == calls served == ledger
`class_a`). The proof is
`b1_a_watcher_killed_mid_scan_still_owes_what_it_spent` in
`tests/objwatch_minio.rs`: it SIGKILLs a child mid-scan and then shows the
restart being refused on the killed run's own spend.

A ledger stamped with a **future** month is not a new allowance either. It used
to reset to zero, so a backwards clock step across a UTC month boundary — a VM
restored from a snapshot, an NTP correction, a container with no RTC — minted a
fresh budget (#968 review, m3). The recorded spend is now carried into the
current month and the anomaly is printed. What this still cannot defend against
is **deleting** the file: the state directory is the trust boundary, and a
ledger that is absent is indistinguishable from a first run. Anything that can
unlink it can reset the budget — which is one more reason for the state
directory to be a durable path the watcher owns, not a temp directory.

The budget is per state directory. Five watchers on five prefixes are five
budgets, and 5 x 200,000 Class A is the whole free tier — run them with
`--max-monthly-ops` divided, or watch fewer prefixes.

* A **dry run records nothing**: no GET, no event, and the journal is neither
  written nor created. It reports the change set it *would* emit and prices the
  poll. (It used to record every object it saw, so the next real run believed the
  whole bucket was already indexed and emitted nothing — #968 review, F1.) The
  list calls it makes are real, so they do go into the spend ledger.

### Making the poll cheaper

| lever | what it costs | what you give up |
|---|---|---|
| a narrower prefix (`s3://bucket/2026/09/`) | `ceil(keys_under_prefix / 1000)` | changes outside the prefix are invisible |
| `--poll-interval` | linear in the interval | detection latency |
| `--append-only` | **one** list call per cycle, at any bucket size (plus one backfill scan on the first cycle, which is not priced as recurring) | no delete detection, and no detection of edits to keys below the highest one seen |
| `--no-fetch` | zero Class B operations | no content digest, so ETag churn looks like an edit |

`--append-only` is the large one and it is the only lever that breaks the linear
relationship between bucket size and cost. It passes the highest key seen so far
as `start-after`, so the store returns only keys above it. On a
date-partitioned, append-only key space (`2026/09/19/…`) that turns a
1,000-page scan into a 1-page scan — about 8,640 Class A operations a month at
the default interval, regardless of how many objects the bucket holds.

It is opt-in because it is not a free win. A partial listing cannot tell
"absent" from "deleted", so delete detection is switched off with it, and an
edit to an older key is never seen. The journal records which mode built it and
**refuses** to be reused in the other mode: mixing them would make a full scan
report unrecorded keys as new, or make an append-only scan report the whole
bucket as deleted.

### A bucket with 1,000,000 objects

Do not poll it whole. 1,000 list pages a cycle is 8,640,000 Class A operations a
month at the default interval — over eight times the free tier — and the journal
for it is roughly 200 MB of JSON, held in memory, because one entry is about
150-250 bytes. That is a real limit of this design, not a rounding error.

What to do instead, in order of preference: scope the watch to the prefixes that
actually change and run one watcher per prefix; use `--append-only` if the key
space only grows; or wait for the event-driven path below, which is the right
answer at that size and is not implemented.

## Seeing what it is spending

Every cycle prints one line to **stderr** (stdout is the change feed and nothing
else, so `--watch | jq` works):

```
xerj-watch cycle=3 added=0 changed=2 deleted=0 unchanged=1198 same_bytes=0 \
  list_calls=2 gets=2 fetched=41.2KB wall=0.38s | this process: list_calls=8 \
  gets=5 | month-to-date 2026-09: Class A 8/200000 budget, Class B 5 \
  | projected 8.6% of the free-tier Class A if sustained
```

`--json` replaces that line with the same content as one JSON object per cycle.
Either way the running totals are also written to a status file —
`<state-dir>/objwatch-status.json` by default, `--status-file` to move it — which
an operator or an agent can read without attaching to the process:

```json
{"xerj":"objwatch-status","location":"s3://logs/2026/09/ @ …","updated_at":"…",
 "totals":{"cycles":3,"list_calls_class_a":8,"gets_class_b":5,
           "bytes_fetched":42188,"keys_listed":3600,"events":2,
           "skipped_deadlines":0,"errors":0,"failed_cycles":0},
 "projection":{"monthly_class_a":8640,"free_tier_percent":0.9,…}}
```

`--dry-run` runs exactly one cycle, fetches nothing, emits nothing and records
nothing: it exists to price a poll before you commit to it, and leaves the state
directory as it found it (bar the spend ledger, which counts the listing it did
make).

`skipped_deadlines` counts poll deadlines that passed while a cycle was still
running. It matters because it means the **effective** interval is longer than
the one the budget was computed from. Cycles never overlap and never queue: the
schedule is fixed at `t0 + k × interval`, a long cycle's missed slots are
counted, the schedule resyncs rather than chasing a backlog, and the next cycle
starts at once.

## How change is decided

The journal (`<state-dir>/objwatch.json`) holds `key → (etag, size,
last_modified, digest)`. Each cycle lists the prefix and compares:

1. **ETag first.** It is the store's own content identity, and the only one of
   the three that changes when the content changes but the size and the
   modification time do not.
2. **Size**, then **`LastModified`**, because some gateways reuse an ETag across
   a rewrite.
3. Absent from a **complete** listing means deleted. Absent from a partial one
   (append-only) means nothing, and delete detection is off there.

Two details that are easy to get wrong, and what this does about them:

* **A multipart object's ETag is not an MD5 of its content.** It is
  `<hex>-<parts>`. The watcher compares ETags as opaque strings and never reads
  one as a content hash, so an object replaced by a multipart upload is detected
  by the change in the string. `tests/objwatch_minio.rs` proves this with a real
  multipart upload against MinIO.
* **An ETag can change while the bytes do not** — a copy, a lifecycle rewrite, a
  gateway that re-stamps. When the fetched bytes hash identical to what the
  journal already holds, the watcher updates the journal and emits **nothing**:
  re-indexing an unchanged document costs a bulk request, a merge and a refresh
  for no gain. Those objects appear as `same_bytes` on the cycle line.

An object longer than `--max-object-mb` is read up to the cap, and its digest
therefore covers a **prefix**. A prefix digest can never prove "the bytes are the
same", so an edit past the cap used to hash identical and be dropped without a
trace (#968 review, F4). A metadata change on a capped object is now always
emitted, with `truncated: true` on the event, counted as `truncated=` on the
cycle line, and with a warning naming the cap.

**Keys are opaque bytes and are handed back exactly as they were stored.** The
`ListObjectsV2` XML is parsed without whitespace trimming and with entity
references resolved, because trimming turned `a &amp; b` into `a&b` and dropped
a trailing space — and the watcher then asked for a key that does not exist and
got a 404 on every cycle, forever (#968 review, F5). A key whose path the URL
parser would rewrite (a `.` or `..` segment) cannot be addressed over HTTP at
all; it is refused with that explanation before a request is sent, rather than
signed for one path and served as another.

Changed objects are fetched with at most **8 GETs in flight**
(`FETCH_CONCURRENCY`), in batches bounded by object count and by the bytes a
batch may hold, and the events are still emitted in listing order. Serially, a
10,000-object first scan over a 50 ms link is over eight minutes of waiting.

A cycle that **fails outright** — a 503, a timeout, a dropped connection — after
the first one is reported as a failed cycle (`failed_cycles` in the totals) and
retried at the next scheduled poll, never immediately: retrying at once would
hammer a store that is already asking for less traffic. Five failures in a row
end the watch. The first cycle's failure is returned immediately, because on a
fresh start it is almost always a wrong endpoint, key or bucket, which waiting
does not fix.

The bytes the watcher reads are what the digest covers. It records the ETag from
the **GET response**, not the one the listing showed, so an object replaced
between the list and the read is noticed on the next cycle instead of being
silently missed. A store that sends no ETag on GET is flagged
(`etag_unverified`) and re-read once to confirm by digest, and the flag clears as
soon as it does — one extra read per change, not one per cycle forever.

The journal is saved every 256 accepted objects or every 2 seconds, whichever
comes first, and at the end of every cycle, so a watcher killed mid-cycle
re-emits at most that window and never loses an update — an object is recorded
only after the sink accepted its event, and the feed is at-least-once by design.
It used to save after *every* object, which fsynced a rewrite of the whole,
growing file once per object: a quadratic first scan.

**Re-measured for the #968 remediation, because the number this page used to
carry did not reproduce.** It said 310.91 s before and 0.16 s after; the "after"
had been taken with the journal in `tempfile::tempdir()`, which is **tmpfs** on
this kind of box, where an fsync is nearly free, and the "before" almost
certainly had not. Neither half named its filesystem. Both halves have now been
run on one host (32 cores, ext4 on NVMe for `/`, tmpfs on `/tmp`, load1 ~3),
against one MinIO container, over the same 10,000 objects, `--no-fetch`, and the
filesystem is named:

| 10,000-object `--no-fetch` first scan | journal on ext4 | journal on tmpfs |
|---|---|---|
| before (`0f785c19`, save per object) | **17.95 s** | **6.08 s** |
| after (`f9_a_ten_thousand_object_first_scan_is_linear`) | **0.19 s** | **0.19 s** |
| speed-up | **94x** | **32x** |
| journal saves | 10,000 -> **40** | 10,000 -> **40** |

So the honest headline is **94x on a disk**, not the ~1,900x the old pair
implied — and the saves count, 10,000 down to 40, is the part that is exact,
provable and asserted by the test. The same scan WITH fetching (10,000 GETs at 8
in flight) is 0.84 s on ext4 and 0.68 s on tmpfs. Both scans served **11**
`ListObjectsV2` calls for 10,000 keys, which is the paging behaviour described at
the top of this page, counted by the watcher itself.

The journal is keyed by endpoint, bucket and prefix, and refuses to be reused for
a different location. One watcher per state directory is enforced with a lock
file: two sharing a journal would each see the other's writes as bucket changes
and re-index them in a loop.

## Polling versus events

| | polling (`ListObjectsV2`) | event notifications |
|---|---|---|
| works on | S3, R2, MinIO, Ceph RGW — any S3 gateway | only where configured, per-vendor plumbing |
| latency | one poll interval | seconds |
| cost | Class A per 1,000 keys **per cycle**, forever | per message, on the queue's bill; nothing when nothing changes |
| cost at 1,000,000 objects | 1,000 Class A per cycle | unchanged — it scales with the change rate, not the bucket |
| completeness | authoritative: the listing **is** the truth | at-least-once, and a missed event is invisible |
| setup | credentials and a prefix | bucket configuration, a queue, a consumer, IAM |
| failure mode | costs money on a quiet bucket | silently stale if the subscription breaks |

Polling is implemented first because it is the portable one, and because it is
**self-correcting**: every cycle re-derives the truth from the bucket, so a
dropped notification cannot leave the index permanently wrong. Events are the
right answer above roughly 100,000 objects, where a full scan stops being
affordable at any useful interval.

### What the event-driven path would take — NOT IMPLEMENTED

Written down so the next person does not have to re-derive it.

**On Cloudflare R2.** R2 event notifications are configured per bucket with a
rule (prefix, suffix, and the actions `PutObject`, `CompleteMultipartUpload`,
`CopyObject`, `DeleteObject`, `LifecycleDeletion`) and deliver to a **Cloudflare
Queue**. Consuming a queue means either a Worker consumer or the queues HTTP
pull API with a Cloudflare API token. That is a second credential kind and a
second API surface — not SigV4, not the S3 endpoint — which is most of the work.
Queue operations are billed on the Queues plan, not against R2's Class A
allowance.

**On AWS S3.** `PutBucketNotificationConfiguration` sends `s3:ObjectCreated:*`
and `s3:ObjectRemoved:*` to SNS, SQS, Lambda or EventBridge. An SQS consumer is
the least plumbing: `ReceiveMessage` long-poll, parse the S3 event JSON, delete
the message on success, and let the redrive policy handle what fails. SQS is
billed per request, and a 20-second long poll is about 130,000 receives a month
per consumer.

**On MinIO.** `mc event add` publishes to a webhook, NATS, Kafka, AMQP,
PostgreSQL and others. A webhook endpoint is the smallest testable target, which
makes MinIO the place to write the integration test for an event consumer.

**What XERJ would have to add, concretely:**

1. A `ChangeSource` trait alongside the current poll loop, with two
   implementations: the existing poll, and a queue consumer. The `ChangeSink`
   side does not change at all.
2. Per-vendor event parsing. The three payload shapes differ, and R2's is not
   S3's. Each has to map to the existing `ChangeEvent`.
3. Credentials for the queue, separate from the S3 credentials, and a decision
   about where they come from (environment only, as here, is the safe default).
4. **A reconciling poll on a long interval anyway.** At-least-once delivery
   means duplicates, which the digest comparison already absorbs, but it also
   means a missed event is invisible: the index goes quietly stale and nothing
   reports it. A daily or hourly full scan is what turns "probably current" into
   "known current", and it is the piece that makes the event path safe rather
   than merely cheap. Its cost is one full scan per reconcile — the same
   arithmetic as the table above, at a much longer interval.
5. Startup reconciliation. A queue holds nothing from before the consumer
   existed, so the first run still needs one full scan to establish the
   baseline.

The honest summary: the poll is about 900 lines including tests. The event path
is a second credential system, three payload parsers, a queue client, and a
reconciling poll — and it still needs the poll. That is why it is not in this
change.

## Flags

| flag | default | what it does |
|---|---|---|
| `--watch` | — | required; polls instead of indexing |
| `--poll-interval <secs>` | 300 | seconds between cycles, 1 to 86,400 |
| `--once` / `--max-cycles <N>` | run until stopped | stop after one / N cycles |
| `--dry-run` | off | one cycle, no reads, nothing emitted, nothing recorded: price the poll |
| `--no-fetch` | off | metadata only: compare ETag, size and modification time, never read bytes |
| `--append-only` | off | `start-after` the highest key seen; one list call per cycle, no deletes |
| `--endpoint-url <URL>` | `AWS_ENDPOINT_URL` | R2: `https://<account>.r2.cloudflarestorage.com` |
| `--region <R>` | `AWS_REGION`, else `auto` | SigV4 signing region; R2 wants `auto` |
| `--max-object-mb <N>` | 64 | byte cap per object read; a longer object is recorded as `truncated` |
| `--max-monthly-ops <N>` | 200,000 | the Class A budget, enforced on every cycle and across restarts |
| `--max-monthly-gets <N>` | 2,000,000 | the Class B (GET) budget, same enforcement |
| `--allow-cost` | off | proceed although a budget says no — the only way past any of the three breakers |
| `--page-size <N>` | 1000 | keys per list call; 1,000 is the API maximum, and the page is the billing unit — a smaller page costs proportionally more |
| `--events-out <PATH>` | stdout | where the JSONL change feed goes |
| `--status-file <PATH>` | `<state-dir>/objwatch-status.json` | the running counts |
| `--state-dir <PATH>` | `~/.xerj/autoindex/<hash>/` | the journal and the lock |
| `--fresh` | off | ignore an existing journal and treat every object as new |

`r2://bucket/prefix` is accepted as a spelling of `s3://bucket/prefix`: R2 *is*
the S3 API, and what differs is the endpoint.

Every flag that belongs to folder indexing is **refused** with `--watch` rather
than accepted and ignored — `--url`, `--max-minutes`, `--approve`, the ignore
flags, the progress flags. A flag that parses and then does nothing is the defect
class this repository refuses on purpose.

## Examples

Price a poll before running one:

```sh
export AWS_ACCESS_KEY_ID=… AWS_SECRET_ACCESS_KEY=…
xerj autoindex s3://logs/2026/09/ --watch --dry-run \
  --endpoint-url https://<account>.r2.cloudflarestorage.com
```

Watch a prefix at a cost you have chosen, with the feed on disk:

```sh
xerj autoindex s3://logs/2026/09/ --watch \
  --poll-interval 900 --events-out /var/lib/xerj/logs.jsonl \
  --endpoint-url https://<account>.r2.cloudflarestorage.com
```

An append-only, date-partitioned key space, at one list call per cycle:

```sh
xerj autoindex s3://logs/ --watch --append-only --poll-interval 60 \
  --endpoint-url https://<account>.r2.cloudflarestorage.com
```

Read what it has spent, from anywhere:

```sh
jq '.totals' ~/.xerj/autoindex/*/objwatch-status.json
```

## Tests

`objwatch`'s unit tests drive an in-process store and cover the diff, the three
cost breakers, the spend ledger across restarts, the journal, ETag churn,
metadata-only mode, pagination, append-only, dry runs, truncated objects,
transient failures and the status file. `engine/crates/xerj-autoindex/tests/objwatch_minio.rs` covers
the wire against a real S3-compatible server: SigV4, a real gateway's
`ListObjectsV2` XML and continuation tokens, a real multipart upload's ETag, a
quiet poll that must do zero reads and exactly one list call, a cycle that
outruns its interval, journal resume, and every defect the #968 review
reproduced: the dry run through the CLI, the page-size projection, an edit past
the byte cap, awkward keys (trailing spaces, spaces around `&`, Unicode, `%`,
`+`, `#`, `?`), the append-only backfill, growth past the budget, a transient
503, and a 10,000-object first scan. It is environment-gated and skips with
a printed reason:

```sh
docker run -d --name minio -p 9000:9000 \
  -e MINIO_ROOT_USER=… -e MINIO_ROOT_PASSWORD=… \
  quay.io/minio/minio:latest server /data
XERJ_MINIO_ENDPOINT=http://127.0.0.1:9000 \
XERJ_MINIO_ACCESS_KEY=… XERJ_MINIO_SECRET_KEY=… \
  cargo test -p xerj-autoindex --test objwatch_minio
```

All load testing and every sub-second interval belongs there. Running them
against a metered bucket would spend Class A operations from a real allowance
for no extra information.

`tests/objwatch_r2.rs` is the other half: one bounded run against Cloudflare R2,
because only R2 can prove R2. It is capped in the code at 24 objects of at most
512 bytes, deletes what it created through `catch_unwind` so a failed assertion
cannot leave objects in a metered bucket, and needs `XERJ_R2_SPEND_ACK=1` on top
of the endpoint and credentials — the other variables could plausibly be set by a
CI environment; that one cannot be set by accident.

### The measured R2 run (2026-09-19)

Measured on commit `0f785c19`, **before** the remediation of the #968 review.
The operation counts are unaffected by it — the same cycles issue the same list
calls and the same GETs — but the wall times predate the bounded-concurrency
fetch and have deliberately NOT been re-measured: re-running against a metered
bucket to learn a number MinIO can give is exactly what this file says not to
do. The `f9_a_ten_thousand_object_first_scan_is_linear` MinIO figures above are
the current timing evidence.

24 objects of 100-124 bytes under one prefix, `--page-size 10` so three pages
exercise R2's continuation tokens:

| cycle | what changed | list calls | GETs | bytes read | wall |
|---|---|---|---|---|---|
| 0 | first poll, 24 added | 3 | 24 | 3.2 KB | 19.91 s |
| 1 | nothing | 3 | **0** | 0 B | 1.64 s |
| 2 | 1 edit, 1 new, 1 delete | 3 | 2 | 53 B | 3.01 s |
| 3 | nothing | 3 | 0 | 0 B | 1.84 s |
| 4 | `--append-only`, fresh journal (a full pass by definition) | 3 | 24 | 2.9 KB | 14.88 s |
| 5 | `--append-only`, tail only | **1** | 0 | 0 B | 0.51 s |

The whole run cost **42 Class A operations** (26 `PutObject` to build and mutate
the corpus, 16 `ListObjectsV2`), 50 Class B `GetObject`, and 25 free
`DeleteObject`. The prefix was deleted afterwards and verified empty.

Two things it confirmed rather than assumed:

* **The quiet poll issues zero GETs against R2.** Every cost figure on this page
  rests on that, and cycles 1 and 3 measured it.
* **R2 changes the ETag on a rewrite**, so `reason: "etag"` is what drove the
  re-read. The size and last-modified fallbacks exist for gateways that do not,
  and they were not needed here.

The first cycle's 19.91 s was 24 **sequential** HTTPS round trips from this host
to R2, not throughput. That is what made the review's 10,000-object case take
minutes; fetches now run 8 at a time, bounded, so a first poll is faster without
becoming a thundering herd against the store it is watching.

The 1,000-key page boundary is not re-tested against R2: `--page-size 10`
establishes the continuation-token path for 24 objects, and proving it at the
natural page size would cost ~1,200 more Class A operations to learn the same
fact.
