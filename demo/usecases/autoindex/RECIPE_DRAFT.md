# Zero-config indexing: make any folder AI-searchable with `xerj autoindex`

**Goal:** You (or your AI agent) have been handed a folder. Inside: a JSONL
application log, a sensor export some tool wrote as a semicolon-separated CSV
with decimal commas and German headers, an HTML postmortem, and a binary file
nobody can identify. You need to answer real questions — *which service has
the most errors? what's the max temperature in the export? which documents
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
format by **content sniffing** (magic bytes — extensions are never trusted),
samples a bounded number of records per file, infers field types, date
formats, and keyword-vs-text semantics from the data itself, PUTs explicit
mappings, and streams the full files into the engine with parallel workers
and idempotent `_id`s. Junk is skipped and recorded with a reason, never
fatal. The same command works unchanged on medical PDFs, server logs, or a
SQL backup.

It also writes everything it learned to a catalog index, so the first
question — "what is even in this folder?" — is answered by the engine, not
by you reading files.

## 1. Index the folder — one command

```bash
xerj autoindex ./corpus
```

Real output (4 files: `logs/app-2026-03.jsonl` with 5,000 events,
`exports/sensor_export.csv` with 800 semicolon/decimal-comma rows,
`docs/postmortem.html`, and `docs/opaque.bin` — 4 KB of random bytes):

```
autoindex: 4 files (0 MB) under /tmp/xerj-autoindex/docs-demo/corpus
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
clean run; junk is *never* fatal.

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
| `ax-logs` | 5000 | 1 | ["jsonl"] | ts | 2026-03-01T00:00:00.000Z → 2026-03-06T04:58:30.000Z |
| `ax-exports` | 800 | 1 | ["csv"] | zeitpunkt | 2026-03-01T00:00:00.000Z → 2026-03-28T00:00:00.000Z |
| `ax-docs` | 1 | 1 | ["html"] | — | — |
```

```
### `ax-exports`

| field | type | semantic | cardinality | null% | examples |
|---|---|---|---|---|---|
| `geraet` | keyword | — | 40 | 0% | `dev-000`, `dev-001`, `dev-002` |
| `ort` | keyword | — | 1 | 0% | `Pune` |
| `temperatur_c` | double | — | 377 | 0% | `92.4`, `67.7`, `97.1` |
| `zeitpunkt` | date (date-only) | — | 28 | 0% | `2026-03-01`, `2026-03-02`, `2026-03-03` |
```

```
## Junk / skipped (1 files recorded, never fatal)

- `docs/opaque.bin` — junk (binary content (unknown))
```

Look at what required zero configuration: `temperatur_c` held strings like
`"92,4"` — decimal-comma values in a semicolon CSV — and was inferred
`double` with the commas coerced at index time. `zeitpunkt`'s date-only
strings became a typed `date` field (mapped
`strict_date_optional_time||epoch_millis`), so `date_histogram` and range
queries just work. Low-cardinality string columns (`level`, `service`,
`geraet`) became top-level `keyword` fields for exact filtering; the HTML
page became a `title`/`headings`/`body` document.

The map ends with a **Gotchas** section listing engine quirks verified on
this exact build — including the honesty note that XERJ's built-in embedder
is *lexical* (see limits below). `--json` gives the raw catalog documents;
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
    "temp": { "count": 800, "min": 15.0, "max": 98.9,
              "avg": 57.45875000000004, "sum": 45967.00000000003 }
  }
}
```

Max 98.9 °C over all 800 rows, in 2 ms, without anyone ever knowing the file
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
  "took": 12,
  "hits": { "total": { "value": 164, "relation": "eq" } },
  "aggregations": { "by_service": { "buckets": [
    { "key": "checkout", "doc_count": 64 },
    { "key": "billing",  "doc_count": 50 },
    { "key": "search",   "doc_count": 50 }
  ]}}
}
```

164 errors total; `checkout` owns the most.

## 5. Q3 — trace one user across structured logs *and* prose

Exact filter on the structured side, full-text on the document side:

```json
GET  /ax-logs/_count      {"query": {"term":  {"user": "u-0042"}}}
POST /ax-docs/_search     {"query": {"match": {"body": "u-0042"}}, "size": 1, "_source": ["title"]}
```

Real responses: the count returns `{"count": 23}` — 23 log events — and the
document search returns the postmortem:

```json
{
  "hits": { "total": { "value": 1, "relation": "eq" },
    "hits": [ { "_index": "ax-docs", "_score": 0.5853416919708252,
                "_source": { "title": "Postmortem: search outage" } } ] }
}
```

One entity, two very different sources (JSONL log and an HTML page), one
uniform API. On larger corpora `autoindex` also records cross-dataset key
correlations (which columns in different files share values) in the catalog,
so an agent knows *which* indices to join before it queries.

## 6. Re-run any time — it resumes, it never duplicates

`autoindex` journals progress per file (`--state-dir`, default
`~/.xerj/autoindex/<hash>/`) and derives deterministic `_id`s, so re-running
is safe and cheap. Real output of running the exact same command again:

```
autoindex: 4 files (0 MB) under /tmp/xerj-autoindex/docs-demo/corpus
resuming from journal /tmp/xerj-autoindex/docs-demo/state/journal.ndjson (3 files already done)
phase B: indexing 0 files with 8 workers → http://localhost:9280
(resumed run: key-overlap correlations kept from the original run's catalog)

done in 0.1s — 3 datasets, 5801 records live, 0 junk records, 1 junk/skipped files
```

`GET /ax-*/_count` still says exactly `5801`. This holds under violence, not
just polite re-runs: in the robustness evaluation, a `kill -9` at 18 s into a
200 MB file followed by a re-run converged to **byte-identical counts across
all 9 datasets** — no duplicates. `xerj autoindex status` shows the journal
and live index counts; `--fresh` ignores the journal and restarts (ids stay
idempotent).

## Reproduce it yourself

```bash
# 1. Start XERJ (any endpoint works; default expected at localhost:9200)
./engine/target/release/xerj --insecure --data-dir /tmp/xerj-auto &

# 2. Index any folder you like — no flags needed
./engine/target/release/xerj autoindex ~/some-folder

# 3. Orient, then query
./engine/target/release/xerj autoindex map
curl localhost:9200/ax-*/_search -H 'Content-Type: application/json' \
     -d '{"query":{"match_all":{}},"size":3}'
```

Useful knobs (all optional): `--url` for a remote endpoint, `--workers N`
(default min(cores, 8)), `--prefix` to namespace the indices (default `ax`),
`--no-semantic` for pure BM25+keyword without vector fields, `--dry-run` to
print the inferred plan without indexing anything, `--follow-symlinks`
(loop-safe), `--sample N` records per file for inference (default 500).

Your dataset names, field types, and counts will reflect *your* folder — that
is the point. The behaviors shown here (decimal-comma → `double`, date
strings → typed `date`, keyword inference, junk recording, resume-to-identical
counts) are the invariants.

## Verified claims

Every number in this recipe traces to a run:

| Claim | Source |
|---|---|
| Console output in §1, §6; map excerpts in §2; JSON in §3–§5 | Live captured run, 2026-07-09, demo corpus at `/tmp/xerj-autoindex/docs-demo` (raw captures: `autoindex-run.txt`, `map-output.txt`, `rerun-output.txt`, `q1-stats.json`, `q2a-entity-count.json`, `q2b-entity-docs.json`, `q3-errors.json`) |
| kill -9 → byte-identical counts across 9 datasets | Surprise-corpus robustness run (201 MB, 11 hostile files incl. a 200 MB JSONL of exactly 1,142,044 records) |
| Zero-config correctness at corpus scale | 80/81 itemized ground-truth checks on a 518 MB / 1,995-file / 25-format manifest corpus (one miss: a Shift-JIS note indexed as mojibake) |
| Whole-pipeline throughput | 923 MB / 2,601,752 records end-to-end in 77.3 s (33.7k records/s), client peak RSS 168 MB |
| Agent evaluation | XERJ-backed agent 9 correct + 1 partial of 10 vs a fair grep baseline's 10/10 on the same questions — details in `demo/usecases/autoindex/AGENT_SIM_SCORECARD.md` |

## Notes and honest limits

- **The built-in embedder is lexical, not neural.** Prose fields get
  `semantic_text` (hash-bucket 384-dim vectors) unless `--no-semantic`. That
  buys **hybrid lexical+vector** retrieval — useful ranking, not semantic
  *understanding*. Don't expect synonym-level recall; for that, bring an
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
- **Big-ingest server caveat:** the client streams with flat memory
  (~250 MB peak RSS on a 4.6 GB corpus), but the current *server* retains
  ~7–10 KB of heap per indexed doc — multi-GB / >5M-doc ingests can OOM the
  server until the ticketed cache-budget fix lands
  (`demo/usecases/autoindex/scale/TICKET_server-unbounded-ingest-heap.md`).
- **Engine quirk found during evaluation:** `bool` queries with only
  `should` clauses return 0 hits on this engine (ES would match). Use
  `must`/`filter` or standalone clauses until fixed.
- **At small local scale, grep is a worthy rival.** In a controlled exam on
  a 518 MB folder, an agent with raw file access and grep/python matched the
  XERJ-backed agent on accuracy. `autoindex` earns its keep on orientation
  speed, aggregations over millions of rows, format-hostile sources, and
  remote/API-only access — not on beating grep at reading one file.
