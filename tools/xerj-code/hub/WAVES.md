# Wave reports — PROGRAM-100

One section per wave, appended in order. The template below is the contract: the
misses are part of the report, a kill is a normal outcome, and reviewer-hours
keep the program honest about what 100 corpora actually costs.

```
## Wave N — <category>, <date opened>..<date closed>

| corpus | G1..G6 | G7 (relevant/5 queries) | status | notes |
|--------|--------|--------------------------|--------|-------|

- kills: <slug> — <gate that failed, one line of evidence>
- W-B drift: <manifests re-pinned / withdrawn, from profiler.py --check>
- reviewer-hours: <estimate>
- consume proof: <one xerj code line per live corpus, pasted>
```

## Wave 0 (bulk, all categories) — 2026-10-02

The program's first land is a bulk intake rather than a per-category wave: 73
backlog rows entered G1–G3 already judged (task #63), the profiler v2
(trees-only clones) pinned and gathered licence evidence for 70 of them, and
one session applied G4 across all of them. Recorded here because the amendment
in PROGRAM-100.md §1 was written for exactly this session and its conditions
were met in order: the stratified sample (2 per category, 50 pre-registered
queries in /root/g7/queries.json, graded manually) ran BEFORE any status
flipped to live.

**In: 68 manifests reviewed (G4) + 5 launch entries. Out: 71 live, 18 candidate,
7 deferred, 3 killed, 1 planned.**

Stratified G7 sample (10 corpora, 5 queries each, pre-registered, top-5 graded):

| corpus | G7 median | verdict |
|--------|-----------|---------|
| etcd-docs | 5.0 | pass |
| leveldb | 4.0 | pass |
| quinn | 4.0 | pass |
| google-eng-practices | 4.0 | pass |
| osquery-config-examples | 4.0 | pass |
| lmdb | 3.5 | pass |
| json-schema-spec | 3.5 | pass |
| boringtun | 3.0 | pass |
| ecma262 | 0.0 | **FAIL → demoted** |
| school-of-sre | 2.5 | **FAIL → demoted** |

Sample lessons (systemic, applied to the pending list's priorities):

- **Single-file giant HTML specs fail G2 outright.** ecma262's entire spec text
  is one 3MB spec.html; the indexer skips it and retrieval surfaces FAQ and
  CONTRIBUTING instead. Risk class on the pending list: whatwg-specs (html
  source), kafka-protocol (single-page protocol.html) — try those FIRST next
  cycle; the fix is section-split intake, not re-indexing.
- **Query vocabulary must be the corpus's vocabulary.** leveldb Q5 asked about
  "tombstones" — leveldb calls them deletions (1/5); school-of-sre queries that
  presumed an on-call chapter retrieved nothing because there is none. Rescope
  G1 domain sentences to actual content before re-running G7 — no query-shopping.
- **Whole-file md chunking is fine for spec repos** (json-schema-spec, etcd-docs
  medians 3.5–5): clause-level questions hit the right file every time.
- Duplicate-passage hits (same file ×5, e.g. school-of-sre replication.md) are
  redundant but not wrong; a dedup-by-file knob would improve precision margins.

G4 (licence review, 72 repos): all permissive families verified from the
licence text at the pin (profiler's git-show evidence retained in each
manifest's review.note; 21 ambiguous/restricted-suspect files hand-opened,
g4read.py output quoted). **Two detector false-positives found — both hinted
GPL-2.0, both actually MPL-2.0** (mozilla-ssl-configs, openbao: the MPL 2.0
text discusses GPL compatibility and the regex bit). Three hint corrections
where evidence over hint: badger is Apache-2.0 not MIT, google-eng-practices
is CC-BY-3.0 not 4.0, raft-rs/dragonboat Apache-2.0 not BSD. Verdicts:
66 adapt-with-attribution, 1 approach-only (valkey-docs CC-BY-SA-4.0),
2 mixed (lz4, zstd — BSD lib / GPL-2.0 programs; snippets outside lib/ are
approach-only).

Kills and defers this wave:

- killed: postgres-docs, sqlite-docs, prometheus-docs — same repository as
  their -src row; a second corpus over one repo is padding.
- deferred: ietf-rfc (no maintained full-text git mirror; needs an http
  tar.gz recipe), mqtt-spec (mqtt/mqtt.org is site chrome — the repo does NOT
  contain the spec text, verified at pin faab2e8a05; OASIS hosts it),
  postmortems (no licence at pin 630562e5 AND it is a link index, not the
  postmortem text — Lane B curation with per-post licence review).

W-B drift: profiler --check not re-run post-land (pins are hours old); first
scheduled pass with the next wave.

Pending G7s (58, the amendment's open list — deadline 2026-10-09, after which
each demotes to candidate automatically). Order: STD single-file-HTML risk
class first, then DATA/NET reference code, then OPS/GOOD prose. By category:
DATA 19, GOOD 7, NET 19, OPS 5, STD 8.

| corpus | cat | refresh | G7 due |
|--------|-----|---------|--------|
| whatwg-specs | STD | pinned | 2026-10-09 |
| w3c-css | STD | pinned | 2026-10-09 |
| unicode-cldr | STD | pinned | 2026-10-09 |
| openapi-spec | STD | pinned | 2026-10-09 |
| graphql-spec | STD | pinned | 2026-10-09 |
| nats-protocol | STD | pinned | 2026-10-09 |
| kafka-protocol | STD | pinned | 2026-10-09 |
| valkey-docs | STD | pinned | 2026-10-09 |
| gitlab-runbooks | OPS | pinned | 2026-10-09 |
| otel-proto | OPS | pinned | 2026-10-09 |
| freebsd-handbook | OPS | pinned | 2026-10-09 |
| openbsd-faq | OPS | pinned | 2026-10-09 |
| tldr-pages | OPS | weekly | 2026-10-09 |
| rust-api-guidelines | GOOD | pinned | 2026-10-09 |
| msft-api-guidelines | GOOD | pinned | 2026-10-09 |
| zalando-restful-api-guidelines | GOOD | pinned | 2026-10-09 |
| github-api-description | GOOD | weekly | 2026-10-09 |
| mozilla-ssl-configs | GOOD | pinned | 2026-10-09 |
| govuk-design-system | GOOD | pinned | 2026-10-09 |
| twelve-factor | GOOD | pinned | 2026-10-09 |
| duckdb | DATA | pinned | 2026-10-09 |
| rocksdb | DATA | pinned | 2026-10-09 |
| badger | DATA | pinned | 2026-10-09 |
| faiss | DATA | pinned | 2026-10-09 |
| annoy | DATA | pinned | 2026-10-09 |
| sqlite-src | DATA | pinned | 2026-10-09 |
| postgres-src | DATA | pinned | 2026-10-09 |
| parquet-format | DATA | pinned | 2026-10-09 |
| arrow-rs | DATA | pinned | 2026-10-09 |
| iceberg | DATA | pinned | 2026-10-09 |
| kafka-src | DATA | pinned | 2026-10-09 |
| rabbitmq | DATA | pinned | 2026-10-09 |
| nats-server | DATA | pinned | 2026-10-09 |
| etcd-src | DATA | pinned | 2026-10-09 |
| raft-rs | DATA | pinned | 2026-10-09 |
| dragonboat | DATA | pinned | 2026-10-09 |
| influxdb | DATA | pinned | 2026-10-09 |
| prometheus-src | DATA | pinned | 2026-10-09 |
| valkey-src | DATA | pinned | 2026-10-09 |
| rustls | NET | pinned | 2026-10-09 |
| openssl | NET | pinned | 2026-10-09 |
| s2n-tls | NET | pinned | 2026-10-09 |
| nghttp2 | NET | pinned | 2026-10-09 |
| curl-src | NET | pinned | 2026-10-09 |
| libuv | NET | pinned | 2026-10-09 |
| zstd | NET | pinned | 2026-10-09 |
| lz4 | NET | pinned | 2026-10-09 |
| brotli | NET | pinned | 2026-10-09 |
| zlib-ng | NET | pinned | 2026-10-09 |
| snappy | NET | pinned | 2026-10-09 |
| protobuf | NET | pinned | 2026-10-09 |
| flatbuffers | NET | pinned | 2026-10-09 |
| capnproto | NET | pinned | 2026-10-09 |
| msgpack-c | NET | pinned | 2026-10-09 |
| openbao | NET | pinned | 2026-10-09 |
| caddy | NET | pinned | 2026-10-09 |
| envoy | NET | pinned | 2026-10-09 |
| nginx | NET | pinned | 2026-10-09 |

Reviewer-hours: ~6 (bulk profiling 0.5, G4 pass 2, sample G7 incl. grading 2,
diagnoses and report 1.5).

Consume proof (sampled corpora, pasted from the run):

```
$ xerj corpus add --from tools/xerj-code/hub/leveldb.json && xerj corpus index leveldb --url http://localhost:9200
corpus 'leveldb' searchable: 2047 records
$ xerj code leveldb "where is the manifest file written and what does a version edit contain" --meatl
@ok f=leveldb/db/version_set.cc score=24.18 why=BSD
$ xerj code lmdb "how does the reader lock table let readers avoid blocking the writer" --meatl
@ok f=lmdb/libraries/liblmdb/mdb.c score=54.23 why=NONE-FOUND
$ xerj code etcd-docs "how do you defragment and compact etcd to reclaim database space" --meatl
@ok f=etcd-docs/content/en/blog/2023/how_to_debug_large_db_size_issue.md score=24.57 why=Apache-2.0
$ xerj code google-eng-practices "what should a reviewer look for in the first pass of a changelist" --meatl
@ok f=google-eng-practices/review/reviewer/looking-for.md score=8.93 why=UNKNOWN
```

The `why=NONE-FOUND` / `why=UNKNOWN` lines are the engine's licence detector,
not the G4 verdict — lmdb's licence lives in libraries/liblmdb/LICENSE and
google-eng-practices' is CC-BY-3.0, both recorded in the manifests' review
blocks. The detector disagreeing with the reviewed manifest is expected; the
manifest is the record.

## Wave N1 — non-IT lane, demand-ranked — 2026-10-02

First land of the non-IT lane (backlog/backlog-nonit.md), scope chosen by the
demand study (backlog/demand-2026-10.md), not by intuition: the top Ask-HN/Reddit
demand domains with no hub coverage were consumer-finance (71), employment-HR
(57), legal (46), tax (40) — all four map onto government regulation/statute
text, which is also the cleanest-licence material there is (US PD, UK OGL).

| corpus | G1..G6 | G7 (relevant/5) | status | notes |
|--------|--------|------------------|--------|-------|
| ecfr-title-12 | pass (Reg Z clause-level; demand 71) | **5/5 — pass** (graded 2026-10-02) | live | mirror corpus-ecfr-title-12 @4dc0246c; expected § in top-3 on all 5 queries, per-query grades in backlog/g7-nonit-2026-10-graded.json |
| ecfr-title-26 | pass (58MB part-1 split per §; demand 40) | **1/4 relevant — FAIL → demoted** (graded 2026-10-02; 1 suite defect excluded) | candidate | mirror corpus-ecfr-title-26 @3f3e58c7; see notes below |
| ecfr-title-29 | pass (OSHA/FMLA/FLSA; demand 57) | **5/5 — pass** (graded 2026-10-02) | live | mirror corpus-ecfr-title-29 @bf0c9b74; every clause grep-verified in a retrieved section (Q3 graded on clause presence in 778.311/553.28/794.141 — the canonical 778.107 itself was not retrieved, noted in the graded file) |
| uk-legislation | pass (418 acts 2015–26 current, OGL ack in README) | **5/5 — pass** (graded 2026-10-02) | live | mirror corpus-uk-legislation @0575db1c; three of five answers came from post-2024 acts (DMCCA 2024, DUAA 2025, BSA 2022) — the currency case in one result |
| uscode | G5 blocked | not run | deferred | OLRC download centre under maintenance; mirrors stale; COMPS is per-act — retry |

- kills: none. deferrals: uscode (source outage, one line above).
- G7 demotion, ecfr-title-26 (1 relevant / 1 partial / 2 miss / 1 excluded):
  two failure causes, both recorded because they generalise. (1) One
  pre-registered expect was unattainable — §1.280A-2 exists only as
  PROPOSED regs (45 FR 52399, 1980), never finalized; the corpus correctly
  omits it (verified against the govinfo bulk XML and the raw §-list;
  eCFR carries only 1.280A-1/-3). Suite-author error, query excluded.
  (2) The real misses are paraphrase gaps: tax questions phrased the way
  people ask them ("prior-year safe harbor", "home office") share no
  vocabulary with reg section titles, and the lexical default ranked
  1.1446-*/1.6655-2T (obsolete temporary) above sec-1.6654-2 which holds
  the answer. Corpus shape is fine — sec-1.263(a)-1 retrieved + verified
  when the query carried verbatim terms. Re-test pre-registered: same
  suite with --embed-mode neural, plus query-expansion, before any
  reconsideration for live. NOT a corpus kill: the G2 shape and G4/G5
  pins stand; this is a retrieval-mode finding on the tax domain.
- G7: 5 queries per corpus pre-registered in backlog/g7-nonit-2026-10.json
  BEFORE retrieval ran; per the §1 bulk amendment they are due within one full
  wave cycle — results recorded in this file when graded.
- shape lesson (G2): GPO ships each eCFR title as ONE XML; title 26's part 1
  alone is 58MB — the ecma262 single-giant-file failure at corpus scale. Both
  splitters (hub/nonit/split_ecfr.py, split_uk.py) therefore write one file per
  §/parliamentary section with breadcrumb headers; max file <2MB everywhere.
- demand evidence: backlog/demand-2026-10.md (1,000-title Ask-HN sample,
  44 exact-phrase queries, Reddit spot-checks).
- Wave N1 G7 close (2026-10-02, all grades on fresh verified indexes):
  **3 pass (title-12 5/5, title-29 5/5, uk-legislation 5/5), 1 fail
  demoted (title-26 1/4)**. Method notes that generalise: (1) every
  expect is validated against the clone BEFORE grading — this caught the
  §1.280A-2 proposed-only citation (excluded as suite defect) and
  confirmed the Building Safety Act is c.30, not c.34; (2) two expect
  CITATIONS were wrong without making the query unanswerable (DPA SAR
  fee is s.53 not s.45/46; CRA Sch 2 today is unfair terms — the current
  distance-contract pre-contract info lives in DMCCA 2024 Sch 23) — the
  corpus out-performed the expects, which is the un-memorisation case:
  consolidated current law has moved past the grader's priors; (3)
  title-26's failure is retrieval-mode (lexical paraphrase gap), with
  the neural re-test pre-registered in this file.

## Wave 1 (protocol/standards lane) — first close 2026-10-03: 15 graded, 10 pass, 5 demoted

G7 suites pre-registered in `backlog/g7-wave1-2026-10.json` (commit a23f1c1e,
BEFORE retrieval ran). These 20 corpora had gone live in the wave-0 bulk flip
on the 8/10 sample basis; this wave is their per-corpus audit. Graded
artifact: `backlog/g7-wave1-2026-10-graded.json`. Live count 74 → **69** —
the audit took back more than the sample promised, which is the system
working, not backsliding.

Pass (10): openapi-spec 5/5, graphql-spec 5/5, twelve-factor 5/5,
rust-api-guidelines 5/5, w3c-css 3/5 (+2 same-family), msft-api-guidelines
4/5, nats-protocol 4/5, valkey-docs 4/5, gitlab-runbooks 3/5,
mozilla-ssl-configs 3/5 (probation — see graded notes).

Demoted to candidate, manifests removed (5) — each with a pre-registered
fix, none a content kill: whatwg-specs 0/5 (7.9MB url.bs monolith + 20×
FAQ.md — section-split mirror), govuk-design-system 0/5 (component YAML
never retrieved — per-component mirror), otel-proto 0/5 (docs prose
outranks .proto — proto-only re-scope), zalando 1/5 (compat.adoc crowding
— guidelines-scoped mirror), tldr-pages 2/5 (30k multilingual dupes —
pages/common-only mirror).

Pending (5, index chain still running when this closed): freebsd-handbook,
openbsd-faq, unicode-cldr, kafka-protocol, github-api-description — graded
against the same pre-registered suite at wave-1 final close; they stay live
on the wave-0 sample basis until then.

Method notes: (1) a filtered re-run of the retrieval driver once clobbered
the results file — the driver now merges, and the artifact records that all
15 results here are from the post-fix single pass; (2) grading is clause
presence (whitespace-normalised expect in any top-5 hit), then a manual
audit of every FAIL and borderline — tldr-pages Q4 held at miss after
reading cron.md (it carries the etymology sentence, not the `crontab -e`
clause, and crontab.md never surfaced); (3) nats-protocol's pass certifies
the PINNED repo (ADR collection) — the re-pin to a wire-protocol mirror is
queued and will require a fresh G7, recorded in the graded notes.

## 2026-10-29: proposed author-candidate promotion — `wysking--quantitera`

- Agent-prepared Lane A application from `354b4e44890822bed192344bc0c50bf4c2446c8a`: 66 files / 343402 bytes; 320 locally indexed records on verified official `v1.0.0-rc.100` (Windows x64).
- G1–G3: specific niche research-engine precedent and runnable synthetic code/tests retained.
- G4: agent opened pinned licence/notices and documented scope in `review`; **human licence sign-off pending**. Original attribution ledger remains unchanged.
- G5: built from the merged public source commit; independent clean rebuild reproduced pin, files, bytes and per-file SHA-256 inventory.
- G6: local registry gate run; official Rust manifest test evidence is linked in the application PR. Engine code unchanged.
- G7: five queries written before retrieval; agent relevant/top-5 counts `[4, 5, 3, 2, 3]`. **Human grading/sign-off pending**; these are agent judgments, not a human pass.
- Limits: excluded `quantitera_calendars` implementation and source-derived schedules; reference subset is not an installable distribution.
- `live` in the proposed backlog row is required by the validator for a root manifest. It does not mean this PR is merged, human gates are passed, or the official Hub has published it.

  Q1: `evidence known_at available_at source` — 4/5 relevant (agent).
  Q2: `expectations surprise consensus revision` — 5/5 relevant (agent).
  Q3: `review due feedback objective outcome` — 3/5 relevant (agent).
  Q4: `knowledge supersedes normalize evidence` — 2/5 relevant (agent).
  Q5: `performance cash flows fees benchmark` — 3/5 relevant (agent).

## 2026-10-29: proposed author-candidate promotion — `wysking--postgrad-study-engine`

- Agent-prepared Lane A application from `94c4b00b025a9968c8a91279a86a30a321f7354e`: 112 files / 794099 bytes; 649 locally indexed records on verified official `v1.0.0-rc.100` (Windows x64).
- G1–G3: specific niche study-support precedent, deterministic contracts/code and fictional tests.
- G4: agent opened pinned licence/notices and documented scope in `review`; **human licence sign-off pending**. Original attribution ledger remains unchanged.
- G5: built from the merged public source commit; independent clean rebuild reproduced pin, files, bytes and per-file SHA-256 inventory.
- G6: local registry gate run; official Rust manifest test evidence is linked in the application PR. Engine code unchanged.
- G7: five queries written before retrieval; agent relevant/top-5 counts `[4, 2, 5, 4, 3]`. **Human grading/sign-off pending**; these are agent judgments, not a human pass.
- Limits: MIT Python and CC BY documents are allocated by file; third-party source works remain outside the grant. Generated JPEG is inventoried but skipped by indexing.
- `live` in the proposed backlog row is required by the validator for a root manifest. It does not mean this PR is merged, human gates are passed, or the official Hub has published it.

  Q1: `request idempotency receipt inbox` — 4/5 relevant (agent).
  Q2: `diagnosis hypothesized evidence gate` — 2/5 relevant (agent).
  Q3: `half_life fsrs recall calibration` — 5/5 relevant (agent).
  Q4: `scheduler prerequisite budget due` — 4/5 relevant (agent).
  Q5: `study_record assessment event fictional` — 3/5 relevant (agent).
