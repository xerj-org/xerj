# Zero-config indexing: make any folder AI-searchable with `xerj autoindex`

**Goal:** You (or your AI agent) have been handed a folder. Inside: a JSONL
application log, a sensor export some tool wrote as a semicolon-separated CSV
with decimal commas and German headers, an HTML postmortem, and a binary file
nobody can identify. You need to answer real questions — *which service has
the most errors? what's the max temperature in the export? which sources
mention user u-0042?* — **without writing a single mapping, extraction
script, or per-format parser**. This recipe indexes the whole folder with one
command and answers all three questions with plain Elasticsearch queries.

Everything below was run end-to-end against a live XERJ (2026-07-09, on a
throwaway server; the port in the captured output is `9280` because the demo
box already had something on 9200 — the default is `http://localhost:9200`
and no `--url` is needed then). The console output and JSON are the real
captured results.

## Why XERJ for this

`autoindex` is a **subcommand of the `xerj` binary you already have** — not a
sidecar tool, not a Python pipeline. It walks the folder, detects each file's
format by **content sniffing** (magic bytes and structure — extensions are
never trusted) across 13 format families (JSONL, JSON, CSV incl.
dialect-sniffed separators, logs, SQL dumps, SQLite, PDF, DOCX, HTML, XML,
YAML, plain text, gzip), samples a bounded number of records per file, infers
field types, date encodings, and keyword-vs-text semantics from the data
itself, PUTs explicit mappings, and streams the full files into the engine
with parallel workers and idempotent `_id`s. Junk is skipped and recorded
with a reason, never fatal.

It also writes everything it learned to a catalog index
(`autoindex-catalog`), so the first question — "what is even in this
folder?" — is answered by the engine, not by you reading files.

## 1. Index the folder — one command

```bash
xerj autoindex ./sample
```

Real output (4 files: a JSONL app log with 5,000 events, a sensor export CSV
with 800 semicolon/decimal-comma rows and German headers, an HTML
postmortem, and `docs/opaque.bin` — 4 KB of random bytes):

```
autoindex: 4 files (0 MB) under /tmp/xerj-pubpass/sample
phase A: sniffing + sampling 4 files…
phase A: 3 datasets inferred, 1 junk/skipped files
phase B: indexing 3 files with 8 workers → http://localhost:9280

done in 0.2s — 3 datasets, 5801 records live, 0 junk records, 1 junk/skipped files
  ax-logs                                        5000 docs
  ax-exports                                      800 docs
  ax-docs                                           1 docs

next: `xerj autoindex map --url http://localhost:9280` for the data map; search via GET /ax-*/_search
```

The exit code is `3` — "completed with junk": the run finished, and the
binary blob was recorded rather than crashing anything. `0` means a fully
clean run; junk is *never* fatal. Exit 3 also covers a dataset whose mapping
the server refused — see [section 7](#7-one-dataset-the-server-refuses-does-not-cost-you-the-run).

## 2. Ask the engine what it found

```bash
xerj autoindex map
```

The data map is a briefing rendered from the catalog: datasets, inferred
per-field types with real example values, time ranges, ready-to-send
queries, and the junk report. Excerpts from the real output:

```
| index | records | files | formats | time field | time range |
|---|---|---|---|---|---|
| `ax-logs` | 5000 | 1 | ["jsonl"] | ts | 2026-03-01T00:00:10.000Z → 2026-03-06T03:35:38.000Z |
| `ax-exports` | 800 | 1 | ["csv"] | zeitpunkt | 2026-03-01T00:00:00.000Z → 2026-03-28T00:00:00.000Z |
| `ax-docs` | 1 | 1 | ["html"] | — | — |
```

```
### `ax-exports`

| field | type | semantic | cardinality | null% | examples |
|---|---|---|---|---|---|
| `geraet` | keyword | — | 40 | 0% | `dev-035`, `dev-002`, `dev-025` |
| `ort` | keyword | — | 1 | 0% | `Pune` |
| `temperatur_c` | double | — | 394 | 0% | `71.5`, `35.7`, `52.1` |
| `zeitpunkt` | date (date-only) | — | 28 | 0% | `2026-03-01`, `2026-03-02`, `2026-03-03` |
```

```
## Junk / skipped (1 files recorded, never fatal)

- `docs/opaque.bin` — junk (binary content (unknown))
```

Look at what required zero configuration: `temperatur_c` held strings like
`"71,5"` — decimal-comma values in a semicolon CSV — and was inferred
`double` with the commas coerced at index time. `zeitpunkt`'s date-only
strings became a typed `date` field (mapped
`strict_date_optional_time||epoch_millis`), so `date_histogram` and range
queries just work. Low-cardinality string columns (`level`, `service`,
`geraet`) became top-level `keyword` fields for exact filtering; the HTML
page became a `title`/`headings`/`body` document with `body` as a
`semantic_text` field.

Each dataset section ends with ready-to-send example queries, and the map
closes with a **Gotchas** section listing engine behaviors verified on the
running build — including the honesty note that XERJ's built-in embedder is
*lexical* (see limits below). `--json` gives the raw catalog documents;
agents can also query the `autoindex-catalog` index directly.

## 3. Q1 — max temperature in the hostile CSV

`stats` over the coerced decimal-comma column:

```json
POST /ax-exports/_search
{ "size": 0, "aggs": { "temp": { "stats": { "field": "temperatur_c" } } } }
```

Real response:

```json
{
  "took": 2,
  "aggregations": {
    "temp": { "count": 800, "min": 15.2, "max": 99.0,
              "avg": 56.57737499999999, "sum": 45261.899999999994 }
  }
}
```

Max 99.0 °C over all 800 rows, in 2 ms, without anyone ever knowing the file
was semicolon-separated with decimal commas.

## 4. Q2 — which service throws the most errors?

`level` and `service` were inferred `keyword`, so terms/filters are exact:

```json
POST /ax-logs/_search
{
  "size": 0,
  "query": { "term": { "level": "ERROR" } },
  "aggs": { "by_service": { "terms": { "field": "service" } } }
}
```

```json
{
  "took": 16,
  "hits": { "total": { "value": 159, "relation": "eq" } },
  "aggregations": { "by_service": { "buckets": [
    { "key": "checkout", "doc_count": 68 },
    { "key": "billing",  "doc_count": 48 },
    { "key": "search",   "doc_count": 37 },
    { "key": "auth",     "doc_count": 6 }
  ]}}
}
```

159 errors total; `checkout` owns the most.

## 5. Q3 — trace one user across structured logs *and* prose

Exact filter plus a breakdown on the structured side, full-text on the
document side:

```json
POST /ax-logs/_search
{
  "size": 0,
  "query": { "term": { "user": "u-0042" } },
  "aggs": { "by_service": { "terms": { "field": "service" } } }
}

POST /ax-docs/_search
{ "query": { "match": { "body": "u-0042" } }, "size": 1, "_source": ["title"] }
```

Real responses: the log query returns `total: 28` — 28 events for that user
(`auth` 11, `billing` 6, `checkout` 6, `search` 5) — and the document search
returns the postmortem:

```json
{
  "hits": { "total": { "value": 1, "relation": "eq" },
    "hits": [ { "_index": "ax-docs", "_score": 0.3646431863307953,
                "_source": { "title": "Postmortem: search outage" } } ] }
}
```

One entity, two very different sources (a JSONL log and an HTML page), one
uniform API. On larger corpora `autoindex` also records cross-dataset key
correlations (which columns in different files share values) in the catalog,
so an agent knows *which* indices to join before it queries.

## 6. Re-run any time — it resumes, it never duplicates

`autoindex` journals progress per file (`--state-dir`, default
`~/.xerj/autoindex/<hash>/`) and derives deterministic `_id`s, so re-running
is safe and cheap. Real output of running the exact same command again:

```
autoindex: 4 files (0 MB) under /tmp/xerj-pubpass/sample
resuming from journal /tmp/xerj-pubpass/state/journal.ndjson (3 files already done)
phase B: indexing 0 files with 8 workers → http://localhost:9280
(resumed run: key-overlap correlations kept from the original run's catalog)

done in 0.0s — 3 datasets, 5801 records live, 0 junk records, 1 junk/skipped files
```

`GET /ax-*/_count` still says exactly `5801`. This holds under violence, not
just polite re-runs: in the robustness evaluation, a `kill -9` mid-ingest
followed by a re-run resumed from the journal and converged to identical
final counts — no duplicates. `xerj autoindex status` shows the journal and
live index counts (real output):

```
journal /tmp/xerj-pubpass/state/journal.ndjson — root /tmp/xerj-pubpass/sample — 3 files done, 5801 records, FINISHED

live indices at http://localhost:9280:
  ax-logs                                        5000 docs
  ax-docs                                           1 docs
  ax-exports                                      800 docs
  autoindex-catalog                                 9 docs
```

`--fresh` ignores the resume journal and restarts (ids stay idempotent), which on a
graph-enabled or pre-generation state directory is how a rerun picks up files added
since the last one. It never removes stale records from the destination, and it is
refused once the state directory holds a durable corpus generation — re-run without it
and the generated `--no-graph` path reconciles additions, changes, deletions, renames
and no-op reruns incrementally.

On a graph-enabled or pre-generation journal, a rerun that resumes an existing plan
indexes changed files and reports added ones as skipped. Deleting a file is refused
there: nothing on that path removes the documents it already published, so the rerun
stops before touching the destination and names what is gone — the first ten, then an
`… and N more` tail, with `--json` carrying every entry. Restore the file and rerun, or
rebuild — in place by deleting the named indices and the state directory, or isolated
under a new `--state-dir`, `--prefix` and `--brain` (or `--no-graph`), validated before
you switch readers. The shared `autoindex-catalog` and the old target require explicit
cleanup. A `--no-graph` state directory written before the generation format cannot be
adopted in place and must be rebuilt the same way.

## 7. One dataset the server refuses does not cost you the run

`autoindex` infers a mapping per dataset and asks the server to install it. A
server can refuse one: a field shape it does not support, a field-count limit,
a conflict with a mapping an earlier release left behind. That is a statement
about **one** dataset, so it costs one dataset
([#929](https://github.com/xerj-org/xerj/issues/929)):

- every other dataset is indexed as usual;
- the refused dataset's files are recorded in `autoindex-catalog` as junk, each
  carrying the server's own refusal text as its reason;
- the run announces it while it is still running (`dataset … REFUSED by the
  server — N file(s) recorded as junk and NOT indexed; every other dataset
  continues`), and `--quiet` does not silence that line;
- the run exits **3**, and the terminal line carries the counts:

```text
xerj-done ok=true exit=3 reason=completed-with-junk … datasets_refused=1 files_refused=2
```

- the catalog's run document carries `datasets_refused`, `files_refused` and
  `refused_datasets_json` (index, file count and the server's reason per
  dataset), and `xerj autoindex map` prints a **Refused datasets — NOT
  indexed** section *above* the dataset table, so a corpus that lacks a dataset
  cannot read as a whole one.

Only an HTTP **400** on create-index or put-mapping is treated this way. A 401
or 403 is your credentials, a 404 a vanished index, and 408, 429 and 5xx are the
endpoint: none of those says anything about the dataset being installed, so all
of them still abort the run with exit 1. Routing around a 503 would publish a
corpus that silently lacks data for a transient reason.

Three limits, stated plainly:

- **A refusal is decided once, when the corpus is first built.** On the
  `--no-graph` path the refused dataset is frozen into the committed generation
  along with the rest of the plan. A later run keeps reporting it (still exit 3)
  and treats a new or changed file of the same shape as refused too. If you
  upgrade the server so that it would now accept the mapping, rebuild under a
  new `--state-dir` and a new `--prefix` to pick the dataset up.
- **A file is dropped whole.** If one file feeds several datasets (a SQL dump
  with many tables) and one of them is refused, the whole file is recorded as
  refused, not just that table's rows.
- **The refused dataset's index may be left behind empty.** The create call can
  succeed before the mapping update is refused. `autoindex` does not delete an
  index it cannot prove it created, so an empty `<prefix>-<dataset>` index may
  remain. It holds no documents and does not affect search or counts.

If the server starts refusing a dataset it accepted when the generation was
built, that *is* fatal — a sealed generation cannot drop a dataset without its
manifest, snapshot and catalog disagreeing — but the error names every refused
dataset, not just the first, and says to rebuild under a new `--state-dir` and
`--prefix`.

## 8. Back-pressure from the node is waited out, not fatal

The engine measures its own resident memory and, above a watermark, answers
writes with HTTP 429 until memory drops back — a parent memory circuit breaker
that engages and releases within seconds. A bulk can be answered two ways while
it is engaged: the whole request comes back 429, or the request comes back 200
with some *items* marked `status: 429`. Both used to end the run. The second
ended a 48,533-file run at 60.4% of its `index` phase, after 85 minutes, on the
first bulk that came back with 747 items rejected
([#944](https://github.com/xerj-org/xerj/issues/944)); the breaker had released
about a second later. The first ended the resumed run 1039.6 s in: a
whole-request 429 was handed to the transport retry — six attempts, about 8 s
of backoff — and then aborted with `error: _bulk: HTTP 429 Too Many Requests`,
while the same rejection carried per item would have been waited out.

Now, when a bulk comes back 429 as a whole, or when every failed item in it is
a 429, the run:

- lowers its bulk concurrency once per congestion event, as before;
- cuts exactly the rejected actions out of the body it sent — the response is
  positional, and a `delete` (no document line) keeps its place — and re-sends
  only those after a backoff of 250 ms doubling to 8 s;
- keeps doing so until the node takes them;
- gives up 600 s after that bulk was first offered. Only then is it exit 1,
  with an error line that begins `the server kept rejecting` and says that
  nothing from that bulk was journaled and the same command resumes the run.

A 429 beside a different failure (a 400 for a record the node cannot parse) is
never re-sent: that bulk carries a bad record you need to see. A whole-request
429 whose body is a full bulk response (the engine echoes one) is read item by
item, so an action the node says it accepted is not sent again; a bare
`{"error": …}` body means every action was rejected and the whole body goes
out again.

Patience is finite for a reason. A node whose resident memory stays pinned
above its watermark never accepts again, and no client-side wait fixes that:
after the full-corpus run above, the node still held 14.8 GB of anonymous
memory for 1.2 GB on disk 2.5 hours after the last write, and every write was
429 until it was restarted
([#950](https://github.com/xerj-org/xerj/issues/950)). The run then exits 1
with `the server kept rejecting`; restart the node, or raise
`XERJ_MAX_PROCESS_MEMORY_MB` / `limits.max_process_memory_mb`, and rerun the
same command to resume. On the `--no-graph` path the terminal line names that
stop instead of calling it an abort, and says how much is left:

```text
autoindex: stopped by server back-pressure while applying <file>: N operation(s) are journaled applied, M are not (this one first) — the same command resumes from here once the node accepts writes again
xerj-done ok=false exit=1 reason=server-backpressure wall=… ops_applied=N ops_remaining=M
```

It stays exit 1, not 3: exit 3 means "a finished run, retry nothing", and this
generation is not finished. Forced on a real node with a 64 MiB memory cap, the
line read `xerj-done ok=false exit=1 reason=server-backpressure wall=609.1s
ops_applied=0 ops_remaining=231`; after a restart on the default cap, the same
command committed the generation with the control run's 1,663 records.

The stream says what is happening at most once every 30 s per waiting bulk
(`autoindex: server is shedding load — <the node's own reason>; re-offering N
rejected record(s) (waited Ws, giving up after 600s)`), and
the terminal line of a run that met back-pressure carries `bulk_retries=N`,
present only when it happened. The `raising bulk concurrency` line after
recovery is printed at most once every 10 s (the motivating capture held 117 of
them for 11 shrinks).

## 9. A request the node calls too large is split, not fatal

The engine refuses a `_bulk` request above two operator limits:
`limits.max_body_bytes` (HTTP 413 on the request, 100 MiB by default) and
`limits.max_actions_per_bulk` (HTTP 200 with one item answered 413, 50,000
actions by default). Until [#955](https://github.com/xerj-org/xerj/issues/955)
the catalog write — one document per file, per dataset, per run — went out as
ONE request on both paths, so it grew with the corpus. On the 48,533-file
corpus that was 51,129 actions in 31.9 MB, and a `--no-graph` run that had
applied all 47,444 operations ended in `finalize-catalog`, 10,336 s in:

```text
xerj-done ok=false exit=1 reason=aborted wall=10336.0s
error: prepared bulk contained 1 rejected items: {"type":"engine_exception","reason":"bulk request contains 102258 lines (~51129 actions); exceeds max_actions_per_bulk of 50000","status":413}
```

The default (graph) path counted the same answer as one ignorable item error
and exited 0 with an empty catalog; a test against a stub that answers the
engine's literal 413 shows it.

Now every `_bulk` body goes out in windows of at most 10,000 actions, and the
catalog write also stays under `--bulk-mb`. A request the node still refuses as
too large is cut in two by actions and re-sent, and the run keeps the
smallest bound it has learned for every later request, so later bodies are cut
before they are sent instead of being refused again. A
refusal of size is not congestion: the bulk concurrency does not drop. The
terminal line carries `bulk_splits=N` when the node refused a request for its
size; cutting a body before it is sent is not counted. One action the node
calls too large cannot be cut and still ends the run with an error that names
`limits.max_body_bytes`.

Resuming the generation above with this change committed it, on the same node
data: `xerj-done ok=true exit=3 reason=completed-with-junk wall=415.0s
files=47444 records=821840 generation=1`, with 51,129 catalog documents and the
node's largest request body at 8,388,241 bytes (was 31,910,392). Captures:
`benchmarks/autoindex-resilience/before-955.*` and `after-955.*`.

Both refusal shapes were also run against real nodes on the `sonic`
repository (`limits-real-node.txt`): with `max_actions_per_bulk = 64` and with
`max_body_bytes = 98304`, each run halved one request and ended
`ok=true exit=3 records=1663 bulk_splits=1`, the same records and catalog as a
control run on default limits. With `max_body_bytes = 65536` one 70,471-byte
record could not be cut, and the run ended exit 1 with the error that names
`limits.max_body_bytes`.

## 10. Reading progress on the `--no-graph` path

Every long step is a phase of its own, in this order: `walk`, `hash`, `scan`,
`prepare` (install mappings, counted in datasets), `snapshot` (seal and extract
every file, counted in source bytes), `index` (send the sealed bulk bytes, the
in-flight file named in `waiting_on`), `finalize-catalog`, `finalize-refresh`
(one refresh per dataset), `finalize-verify` (one read-back per file). A
resumed run starts at `replay`, and its `index` phase counts only the
operations still to apply, so it starts at 0% of what remains rather than
crediting this run with an earlier one's writes.

Before [#931](https://github.com/xerj-org/xerj/issues/931) this path reported
none of those steps: the stream kept printing `phase=scan … pct=100.0 …
eta_quality=stalled` with a climbing `since_progress_s` for as long as documents
were landing, which is byte for byte what a real hang at the end of the scan
prints. A line that says `scan` at 100% now means scan. The before and after
streams from the same 48,533-file corpus are in
[`benchmarks/autoindex-resilience/`](../../benchmarks/autoindex-resilience/).

## Reproduce it yourself

```bash
# 1. Start XERJ (any endpoint works; autoindex expects localhost:9200 by default)
./engine/target/release/xerj --insecure --data-dir /tmp/xerj-auto &

# 2. Index any folder you like — no flags needed
./engine/target/release/xerj autoindex ~/some-folder

# 3. Orient, then query
./engine/target/release/xerj autoindex map
curl localhost:9200/ax-*/_search -H 'Content-Type: application/json' \
     -d '{"query":{"match_all":{}},"size":3}'
```

Useful knobs (all optional): `--url` for a non-default endpoint, `--workers N`
(the *default* is every core, reduced when the memory safe zone cannot pay for
that many in-flight bulk buffers; a value you pass is honoured as typed and the
disagreement is printed, not applied; bounds both the scan and the indexing
phase), `--prefix` to namespace the indices (default `ax`),
`--no-semantic` for pure BM25+keyword without vector fields, `--dry-run` to
print the inferred plan without indexing anything, `--follow-symlinks`
(loop-safe — and a followed link is judged by what it resolves to: a target
outside the indexed folder is refused, and one inside a hidden directory is
skipped like any dotfile, however visible the link's own name;
`--follow-symlinks-outside-root` waives the folder boundary and nothing else),
`--sample N`
records per file for inference (default 500).

Build output never reaches the index in the first place. The walk honours
`.xerjignore`, `.gitignore` (nested files and `!negation` included) and
`.git/info/exclude`, plus a built-in list — `node_modules/ vendor/ target/
dist/ build/ .venv/ __pycache__/` — in that order of precedence; an excluded
directory is never descended, so nothing inside it is stat-ed, hashed or sent.
The two git-owned kinds stop at a repository boundary the way git does: a
`.gitignore` above a nested checkout has no authority over the files inside it,
so a vendored or submoduled tree keeps its own rules. `.xerjignore` and the
built-in list are XERJ's rather than git's and apply throughout the folder you
named. Hidden files (`.env`, `.git/`) are skipped by a separate, non-optional
rule. Every run prints what was dropped and by which rule (`ignore rules: …` on
stderr), and `--dry-run` also counts the non-hidden files inside each pruned
directory, so "where did my file go?" always has an answer. `--no-ignore` turns
the rules off; `--no-default-ignores` keeps your ignore files and drops only the
built-in list. Both apply to an indexing run only — `autoindex map` and
`autoindex status` never walk a folder and refuse them rather than accepting a
flag that would do nothing. The folder you point at is never excluded: if it is
itself ignored — say you run this on `~/proj/target` — it is indexed anyway and
the run says which rule it would have matched.

`--workers` is a ceiling, not a promise: when the server answers `429 Too Many
Requests` the run halves its own bulk concurrency on the spot and probes back
up one worker at a time only after a clean streak, so a busy server is not
handed the same load six times over. The run's summary records where it ended
up (`bulk_concurrency_final`, `bulk_congestion_events`).

Your dataset names, field types, and counts will reflect *your* folder — that
is the point. The behaviors shown here (decimal-comma → `double`, date
strings → typed `date`, keyword inference, junk recording, resume-to-identical
counts) are the invariants.

## Verified claims

Every number in this recipe traces to a run:

| Claim | Source |
|---|---|
| Console output in §1, §2, §6; JSON in §3–§5 | Live captured run, 2026-07-09, 4-file demo corpus, throwaway server on port 9280 |
| Zero-config correctness at corpus scale | 80/81 itemized ground-truth checks on a 518 MB / 1,995-file manifest corpus (one miss: a Shift-JIS note indexed as mojibake) |
| Corpus-scale indexing | the same 518 MB corpus → 31 datasets / 2,018,398 records live in 38.1 s (`demo/usecases/autoindex/AGENT_SIM_SCORECARD.md`) |
| Whole-pipeline throughput | 33.7k records/s end-to-end on a 923 MB corpus — server-bound, not extractor-bound; client memory stays flat (~250 MB) at 5× input growth |
| `kill -9` → resume to identical final counts | Robustness evaluation, journal-based resume (`demo/usecases/autoindex/README.md`) |
| Agent evaluation | XERJ-backed agent 9 correct + 1 partial of 10 vs a fair grep baseline's 10/10 on the same questions — details in `demo/usecases/autoindex/AGENT_SIM_SCORECARD.md` |

## Notes and honest limits

- **The built-in embedder is lexical, not neural.** Prose fields get
  `semantic_text` (hash-bucket 384-dim vectors) unless `--no-semantic`. That
  buys **hybrid lexical+vector** retrieval — useful ranking, not semantic
  *understanding*. Don't expect synonym-level recall; for that, configure an
  external embedding model. The `map` output repeats this warning so agents
  see it too.
- **Inference is sampled** (default 500 records/file). A type that only
  appears past the sample horizon can be missed; raise `--sample` for
  pathological files. Values that fail coercion at index time are dropped
  from the record and counted; the record itself still indexes — never
  fatal.
- **Legacy encodings:** UTF-8 and Latin-1 text decode correctly (verified);
  a planted Shift-JIS file was indexed as mojibake — the one miss in the
  81-check ground-truth verification.
- **Archives are recorded, not extracted:** a `tar.gz` shows up in the junk
  report with a reason instead of being unpacked.
- **Exotic CSV headers** (BOM + Japanese + emoji) are handled but may become
  positional field names (`field`, `field_2`, …) while the *values* still
  type correctly.
- **Big-ingest server caveat:** the client streams with flat memory and the
  pipeline is resumable — multi-GB corpora are verified — but the current
  *server* retains ~7–10 KB of heap per indexed doc, so multi-GB / >5M-doc
  ingests can OOM the server until the ticketed cache-budget fix lands
  (`demo/usecases/autoindex/scale/TICKET_server-unbounded-ingest-heap.md`).
  Do not point it at TB-scale folders yet.
- **At small local scale, grep is a worthy rival.** In a controlled exam on
  a 518 MB folder, an agent with raw file access and grep/python matched the
  XERJ-backed agent on accuracy (10/10 vs 9 + 1 partial of 10). `autoindex`
  earns its keep on orientation speed (a full corpus inventory in 4 API
  calls), sub-second aggregations over millions of rows, format-hostile
  sources (SQLite, DOCX, gzip, dialect CSV through one API), and
  remote/API-only access — not on beating grep at reading one file.
