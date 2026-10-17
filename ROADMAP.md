# XERJ Roadmap

This roadmap tracks capabilities that are **planned but not yet fully implemented**, so the project's public claims stay honest about what ships today versus what is coming. Status is verified against the actual code and by real API requests to the release binary, not aspirational.

Last reviewed: 2026-10-17 (against `v1.0.0-rc.97` and `main`). Statuses trace to issues, merged PRs, the CHANGELOG, and the conformance suite; items carried forward from the 2026-07-12 review without fresh live verification are marked as such. This review line is machine-checked: `docs_capability_lists` fails the build if a release is cut without re-reviewing this file (issue #298 — closed as abandoned 2026-09-29; the machine check, not the issue, enforces the cadence now). This pass is the rc.97 release-cut roll:
the open-defects shortlist was re-verified against the live tracker at
cut time — **five open**: #1110, #1122, #1170, #1267, #1635. #1094 closed
inside the window (low-confidence history votes escalate to the local
decide tier and the write-back keeps one cached answer per text, PR
[#1503](https://github.com/xerj-org/xerj/pull/1503); the Banking77 replay
gate was not re-measured). #1635 is new: autoindex at the default worker
count stalled a 13,765-file repo and the node stopped answering searches
(field report PR [#1636](https://github.com/xerj-org/xerj/pull/1636)). The
window since rc.96 is about 125 merged PRs: Corpus Hub v2 switched on (the
hub public key compiled in, PR
[#1505](https://github.com/xerj-org/xerj/pull/1505), and the §12 release
gate tooling), `xerj search` / `xerj def` / `xerj code` on non-code and
mixed-type corpora, autoindex dataset folding and finalize robustness,
deadline-bounded search during a merge publication, ES cluster-settings
merge semantics, three more #1122 spawn sites, and an ES-YAML runner that
enforces `catch:`; the CHANGELOG lists every PR.
The rc.96 cut's record stands as written: the open-defects shortlist was re-verified against the live tracker at
cut time — **five open**: #1094, #1110, #1122, #1170, #1267. Four
closed inside the window: #1250 (multi-index ingest against the breaker:
the live-tunable watermark, PR
[#1323](https://github.com/xerj-org/xerj/pull/1323), and the damped bulk
AIMD client, PR [#1482](https://github.com/xerj-org/xerj/pull/1482)), #1108 (the #1062 wrong-and-confident gate run
through the real `_watcher` path and published as a FAIL, PR
[#1322](https://github.com/xerj-org/xerj/pull/1322)), #1183 (the
dishonest-resume footer, PR
[#1318](https://github.com/xerj-org/xerj/pull/1318)) and #1226 (empty
shards skip the shared finalize gate, PRs
[#1319](https://github.com/xerj-org/xerj/pull/1319) and
[#1456](https://github.com/xerj-org/xerj/pull/1456)). The window since
rc.95 was a fix-and-honesty sweep of 60-odd merged PRs — total tie order
on two more `term` routes (#1359), the ES dynamic-mapping doc-values
default for inferred `text` (#1329), the memory-breaker work that closed
#1250, finalize progress and old-server detection
(partial #1267), native Windows home resolution, `xerj init` backup and
upgrade safety, and the `quinn-proto` / `calamine` security updates; the
CHANGELOG lists every PR. Corpus Hub v2 code merged (PR
[#1423](https://github.com/xerj-org/xerj/pull/1423)) but is not a
feature of rc.96: no hub signing key is compiled in, so no pack can be
verified and none is downloaded.
The rc.95 cut's record stands as written: the open-defects shortlist
was re-verified at that cut — **nine open** (unchanged from rc.94): #1094, #1108, #1110,
#1122, #1170, #1183, #1226, #1250, #1267. The window since rc.94 landed
legacy Microsoft Office support in `xerj autoindex` — `.xls` (BIFF via
calamine) and `.doc` (Word 97+ text via the FIB piece table), with OLE2
containers detected by content and `.epf`/others named and skipped — in
PR [#1324](https://github.com/xerj-org/xerj/pull/1324). Because calamine
0.26.1 is not panic-safe on hostile BIFF and the engine is `panic=abort`,
`.xls` is parsed in an rlimit-capped `xerj __extract-xls` worker
subprocess so a hostile spreadsheet in an untrusted dump aborts only the
worker, never the indexer (adversarially verified against four
allocation and two panic vectors); it was validated end-to-end against a
real 75 GB Jira/Confluence/GitLab dump (.xls and .doc both produced
records, nothing crashed). The rc.94 footer sweep also landed (#1325) and
is now part of the cut recipe. No open defect closed inside this window.
The rc.94 cut's record stands as written: the window since rc.93 landed
29 merged PRs. A
backlog-fix sweep closed six defects inside the window: the hybrid
`query`+`knn` page-local ranking (#1303, PR
[#1312](https://github.com/xerj-org/xerj/pull/1312)), the bulk
lone-surrogate whole-batch rejection (#1304, PR
[#1311](https://github.com/xerj-org/xerj/pull/1311)), the autoindex
`--watch` double-owner (#1232, PR
[#1307](https://github.com/xerj-org/xerj/pull/1307)), the sibling-fork
reconcile abort (#1230, PR
[#1309](https://github.com/xerj-org/xerj/pull/1309)), the map-only
`--dataset` accept-and-ignore (#1231, PR
[#1308](https://github.com/xerj-org/xerj/pull/1308)), and the
non-note-corpus Second Brain vocabulary (#1100, PR
[#1310](https://github.com/xerj-org/xerj/pull/1310)). Three more were
closed as already-fixed after verification against the code: #1212 (via
#1013), #1244 (via #1246/#1251) and #1173 (via #1174). Contributor
fixes and field reports from Vinz2168, thomas-villani, yogthos and
pierreaubert also landed (see CHANGELOG). The rc.93 cut's record stands
as written: **twelve open** at that cut — the ten carried from rc.92
(#1094, #1100, #1122, #1170, #1173, #1108, #1110, #1183, #1212, #1226)
plus #1250 and #1267, the slow-query population filed inside that window. #1260 was
filed and closed inside the window (PR
[#1262](https://github.com/xerj-org/xerj/pull/1262)): the per-digest
`bool.filter [term ax_file, exists]` probe fell off the columnar fast
path — `exists` had no compiler arm and the deletes bail keyed on a
monotonic ghost counter so a rewritten index never re-qualified — and
measured 3.4 s → 2.7 ms warm (~1,250×) on the restarted crawl node.
#1267 carries what #1260 did not reach: the finalize-catalog read-back
walk (`term run_id` + `_id` sort, 641 pages ≈ 35 min whenever a resume
rewrote the whole catalog) and the doc-lane residual — verify windows
with `size > 0` still brute-scan, 7–81 s per data index, and one
exceeded the client's 300 s request budget on the cve resume. The
#1183 count lane landed twice in this window:
[#1259](https://github.com/xerj-org/xerj/pull/1259) batched the verify
read-backs (~1.2 M serial searches → ~800 windows; record windows
63 ms cold / 6 ms warm) and
[#1262](https://github.com/xerj-org/xerj/pull/1262) columnarized
`exists`; the vuln-fix-commits rebuild completed on it (328,891
records, 97 batched windows verified) and its pre-registered G7 graded
4/5 PASS, so #1244's completion condition now rides the cve-records
rebuild alone, in flight at cut time. The rc.92 cut's record stands as
written: twelve open at that cut — with the correction that #1254 was
closed by that window's own
[#1258](https://github.com/xerj-org/xerj/pull/1258) merge (the
auto-close fired at 655ea93ec, 42 minutes before the tag; the cut's
"stays open" note predates the close), so its `code_files=0` labeling
remainder is not an open tracker. #1250 (multi-index ingest oscillating on the memory
breaker) had its attribution corrected the same day it was filed: the
rejecting breaker is the boot-time tiered process cap (95% watermark),
not the live-settable `max_query_memory_mb`, and the mitigation is the
`XERJ_MAX_PROCESS_MEMORY_MB` restart; the same cap was then measured
driving an *idle* flush loop — two parked A/B nodes wrote 644 GB and
858 GB to disk in seven hours with no client traffic. #1254
(re-attributed twice in one day, each time by measurement): the original
"silently unindexed" claim was retracted first (its probe wildcarded
`ax_file`, which stores content digests, not filenames — `ax_path`
shows all 79 `.proto` records present), then the txt-lines
chunk-splitting mechanism was retracted too (a simulated whole-document
route ranked the needle 25th, worse than the real chunks' 19th — length
dilution beats mass concentration). The single effective variable is the
`text^0.5` recall-leg weight, the #1238 calibration, which discounts
exactly the family a text-primary corpus lives on; the corpus-declared
`query.text_weight` fix ([#1258](https://github.com/xerj-org/xerj/pull/1258))
moved all four G7 needles to rank 1 on the unchanged index and the
suite re-graded 5/5. The issue **stays open** for the minor
`code_files=0` labeling gap in the index terminal line. #1244 **stays open** with both mechanism fixes merged
inside the window ([#1246](https://github.com/xerj-org/xerj/pull/1246),
[#1251](https://github.com/xerj-org/xerj/pull/1251)): its completion
condition is the pre-registered G7 suites passing on rebuilt corpora,
and both rebuilds (cve-records, vuln-fix-commits) were in flight at cut
time. The rc.91 cut's record stands as written: ten open at that cut,
one closed verified inside the window (#1238, PR
[#1241](https://github.com/xerj-org/xerj/pull/1241)). #1183 keeps its
measured mechanism (the 2026-10-08 comment table: finalize-verify pays
1.2 s bare / ~14 s term+exists per uncached `ax_file` value on the
572,992-doc index; the scored lane was fixed by rc.91's
[#1234](https://github.com/xerj-org/xerj/pull/1234), and this window's
[#1259](https://github.com/xerj-org/xerj/pull/1259) +
[#1262](https://github.com/xerj-org/xerj/pull/1262) landed the count
lane) and its completion condition: a full xerj-search
rebuild passing finalize with verified numbers.
**Two closed inside the rc.87 window and recorded in the CHANGELOG, not
carried here:** #1147 (the build-throughput class, PR
[#1195](https://github.com/xerj-org/xerj/pull/1195), merged just past
the rc.86 tag) and #1158 (both halves at last: the query-time half in
rc.86 by PR [#1185](https://github.com/xerj-org/xerj/pull/1185), the
build-time half by PR [#1200](https://github.com/xerj-org/xerj/pull/1200)).
The rc.86 cut's record stands as written: eleven open at that cut. **Six
closed inside the rc.86 window and recorded in the CHANGELOG, not
carried here:** #1109 (server-derived audit `actor` — PR
[#1194](https://github.com/xerj-org/xerj/pull/1194)), #1189 and #1190
(embed-mode proxy fail-closed and the unwired `batch_size` — PR
[#1193](https://github.com/xerj-org/xerj/pull/1193)), #1186 (the BM25
stats probe budget split by cost class — PR
[#1188](https://github.com/xerj-org/xerj/pull/1188)), #1146 (hybrid
keeping lexical-only indices in the BM25 leg — PR
[#1177](https://github.com/xerj-org/xerj/pull/1177), an external
contributor's first merged fix) and #1092 (`_settings`/`_analyze`
echoing real analysis — PR
[#1184](https://github.com/xerj-org/xerj/pull/1184), same contributor).
The rc.85 cut's record stands as written: ten open at that cut, with
#1168 — the election-churn root cause (self in the peer set) — closed
inside its window by PR [#1179](https://github.com/xerj-org/xerj/pull/1179)
and the clustering page corrected by PR
[#1180](https://github.com/xerj-org/xerj/pull/1180). The rc.84 cut's
record stands as written: eleven open at that
cut, with #1169 and #1171 — the fabricated-topology and
invisible-degraded-boot cluster defects — closed inside its window by
PRs [#1172](https://github.com/xerj-org/xerj/pull/1172) and
[#1175](https://github.com/xerj-org/xerj/pull/1175). The lesson of
2026-09-21, when several
entries went stale within hours of that review, is why the shortlist is
checked live at the cut rather than desk-carried. The rc.83 roll's record
stands as written: eight open at that cut, with #1136 closed inside the
window by PR [#1156](https://github.com/xerj-org/xerj/pull/1156) and
#1145/#1148 by PR [#1153](https://github.com/xerj-org/xerj/pull/1153). The rc.81 roll's record
stands as written: four open at that cut, with #1093 closed inside the
window by PR [#1112](https://github.com/xerj-org/xerj/pull/1112) (strict
unknown-field refusal). Later the same day the tracker was emptied: everything open after the cut — #1038, #1030, #1031, #1032 — was closed (the two shipped-half epics are in the rc.78 record below, the two defects are recorded below as deferred, not fixed), leaving zero open issues. The 2026-09-26 desk review (PR [#1036](https://github.com/xerj-org/xerj/pull/1036)) stands as recorded: it closed the CHANGELOG-gap GA item (the rc.19–rc.70 backfill, PR [#1035](https://github.com/xerj-org/xerj/pull/1035)), marked the stage-2 object-storage item done ([#965](https://github.com/xerj-org/xerj/issues/965) wired in rc.77), and corrected the mail-ingest memory line to the post-[#1002](https://github.com/xerj-org/xerj/pull/1002) reality. The *Shipping today* claims were last live-verified against rc.76 (unchanged by this pass), and *The zero-token direction* below was verified separately on 2026-09-18, against `main` @ `4d8dadbf`.

## Follow the roadmap

- **This file** is authoritative. If any other surface disagrees with it, this file wins — and that disagreement is a bug worth an issue.
- **[Milestones](https://github.com/xerj-org/xerj/milestones)** — the release-by-release view. Every open issue is triaged onto a milestone; the next RC's milestone is the short-term roadmap.
- **[Project board](https://github.com/users/xerj-org/projects/1)** — live status of every open item.
- **[Pinned plan tracker #1065](https://github.com/xerj-org/xerj/issues/1065)** — the working index of the ten-item program to 1.0 ([discussion #1054](https://github.com/xerj-org/xerj/discussions/1054)). This file remains authoritative; the tracker indexes the filed issues.

## Shipping today (for context)

These are implemented and exercised by real API requests / the test suite / benchmarks:

- Elasticsearch REST wire compatibility (1,366 / 1,369 ES-YAML conformance cases; the gate on every change is **0 failed**, and the case count grows as cases are added — read the current number off CI, not off this file).
- Full-text search (BM25) and **<!-- generated:query-type-count -->50<!-- /generated:query-type-count --> query types**. Neither the list nor the number is maintained by hand here: the list is generated from `xerj_query::parser::SUPPORTED_QUERY_TYPES`, printed in full in [engine/README.md](./engine/README.md#query-types-supported) and [llms-full.txt](https://xerj.org/llms-full.txt), and pinned to `parse_query`'s dispatch table by `parser::tests::dispatch_table_matches_capability_manifest`; the number above sits in a machine-checked region pinned to that constant's length by `docs_capability_lists::published_capability_counts_match_the_constants`. A further <!-- generated:rejected-query-type-count -->2<!-- /generated:rejected-query-type-count --> keys — `has_child` and `has_parent` — are recognised and **rejected with a 400**, and are listed as such in the same places (issue #211).
  **Honest caveat, unchanged:** that count is the *dispatch* surface — every name on it parses, plans and executes, which is not a claim that every one is semantically faithful to ES. The known divergences are enumerated under *Known partials* below, and the ES-YAML conformance suite is the measured answer.
- **Aggregations: <!-- generated:agg-type-count -->62<!-- /generated:agg-type-count --> types**, likewise generated from `xerj_engine::aggs::SUPPORTED_AGG_TYPES`, printed in [engine/README.md](./engine/README.md#aggregation-types-supported) and [llms-full.txt](https://xerj.org/llms-full.txt), and pinned by the same count test. This includes the full **pipeline family**. `weighted_avg` is **not** in `SUPPORTED_AGG_TYPES` — see *Known partials*.
  **Exactness, precisely.** No probabilistic sketch sits in the metric path: `cardinality` is a true distinct count rather than an HLL estimate, and `terms` `doc_count` is precise. Two deliberate exceptions, stated the same way in `engine/README.md` and `llms-full.txt`: (1) the **sampling family** is a sample by definition — `run_sampler` sorts the matched documents by `_score` and keeps the first `shard_size` (default **200**), so every sub-aggregation under `sampler`, `random_sampler` or `diversified_sampler` is computed over that slice rather than the whole match set, `diversified_sampler` additionally caps documents per `field` value, and `random_sampler` shares the `sampler` implementation and **ignores ES's `probability`** (an accepted-and-ignored input, #204); (2) `percentiles` with the `hdr` option returns HdrHistogram-quantized values, deliberately, so ES's own outputs reproduce — the default `tdigest` path sorts every value and interpolates instead.
- **Dense-vector kNN** (`knn` query and ES 8.x top-level `knn`): unfiltered kNN on a full-precision cosine field (≥1,024 docs) is served by a **persisted HNSW graph with exact rescoring** — measured recall@10 1.00 on the official bench query, 100-probe mean 0.976 (ES 8.13.4 same protocol: 0.937); filtered/nested kNN, non-cosine similarity, SQ8 fields, and small indexes run the exact brute-force scan (cosine mapped to `(1+cos)/2`).
- **Hybrid search** — BM25 + kNN combined in a single request via the `hybrid` **query type** with `rrf|linear` fusion, verified live. (`fusion: "learned"` is parsed and **rejected** with a 400 naming the supported values — it is not implemented.) (The ES-native top-level `{query, knn}` body also unions both halves since rc.72 — see *Known partials* for its performance caveat.)
- **Zero-config folder onboarding** — `xerj autoindex <folder>` sniffs files, infers datasets, and creates one index per dataset: tree-sitter AST extraction for 36 languages (symbols, defs, line numbers — the [#295](https://github.com/xerj-org/xerj/issues/295) expansion, plus Clojure and Scheme; still open: source-SQL waits on a usable grammar crate, Nim/Crystal have none, fixed-form Fortran is deliberately unclaimed), CSV/JSON/JSONL/XML/YAML/SQLite/PDF/DOCX/HTML/log/`.eml`/mbox (Google Takeout) formats, `.gitignore`/`.xerjignore` support, incremental re-runs, and a machine-parseable progress stream.
- **Agent-memory REST API** (`/_memory/*`), **second-brain knowledge graph** (`/_graph`), **anomaly detection** (`_ml` with continuous datafeeds), **auto-embed on ingest** (default embedder is deterministic **lexical** feature-hashing — never described as neural; `--embed-mode neural` runs the in-binary BERT encoder, `--embed-mode proxy` an external endpoint).
- **The System One wire, answered locally** (since rc.76) — `POST /v1/systemone` (native REST) and `POST /_decide` (ES-compat): typed judgement questions (`noul` / `choice`) answered by a rank-weighted kNN vote over a labelled-history index — no model, no provider key, no egress ([benchmarks/decisions-as-retrieval](./benchmarks/decisions-as-retrieval): Banking77 0.933 accuracy / ECE 0.012, SMS spam 0.983). The acceptance gate is the unmodified pip `jev-reranker` ranked off a XERJ node ([benchmarks/systemone-gate](./benchmarks/systemone-gate)). It is **not** a judge model and claims no zero-shot judgement: where there is no history, use the `rerank` stage and its provider.
- **Columnar storage** — the ZBS2 columnar block with 9 domain-aware encodings, ZSTD/LZ4 codecs, and SQ8 vector quantization, wired into the segment write path.
- Bulk / scroll / delete-by-query, aliases, index templates, **executed** index-lifecycle policies (ISM-modeled, `_ilm/*` + `_plugins/_ism/*`, since rc.15), `_cat/*`, `_cluster/health`, `_count` / `_msearch` / `_mget`, `_update` / `_update_by_query` — all live-verified.
- **A single native binary**, statically linked, no JVM, sub-second cold start.

The release-by-release record of how all of this landed is [CHANGELOG.md](./CHANGELOG.md) — this file no longer duplicates it. The record is now complete rc.1 → rc.78: the rc.19–rc.70 gap was backfilled on 2026-09-26 from `git log` (merge subjects and commit bodies) and the GitHub release records, with a provenance banner in the file — the reconstructed entries are drier than ship-time ones, and every issue/PR link they carry was checked to sit inside its release window (PR [#1035](https://github.com/xerj-org/xerj/pull/1035)).

## Next release — [v1.0.0](https://github.com/xerj-org/xerj/milestone/2)

The GA window. **rc.87 was cut on 2026-10-07** — the corpus-index trust
window: phase-B bulk bodies are coalesced across files, removing the
per-file round-trip ceiling on corpus ingest (#1147, PR
[#1195](https://github.com/xerj-org/xerj/pull/1195), A/B'd on two corpus
shapes with no regression, merged just past the rc.86 tag);
`xerj corpus index` proves the store answers a search before it says
"searchable", and an unsearchable build is retired instead of switched
in (#1183's item 2, PR
[#1197](https://github.com/xerj-org/xerj/pull/1197)); a retrying client
request announces itself instead of reading as a deadlock (#1183 item
1's client half, PR
[#1199](https://github.com/xerj-org/xerj/pull/1199)); and a JSON record
with no prose gains a synthesized `text` passage, with a plan-time
warning for any dataset that maps no text-searchable field at all
(#1158's build-time half, PR
[#1200](https://github.com/xerj-org/xerj/pull/1200) — closing the issue,
its query-time half having shipped in rc.86). **rc.86 was cut on
2026-10-06** — the
audit-and-config honesty window: every audit entry now carries a
server-derived `actor` class (`user`/`machine`) that no client input can
influence (#1109, PR [#1194](https://github.com/xerj-org/xerj/pull/1194)
— the class is inside the hash chain, and `xerj gain` stops parsing
notes to guess it); an explicit `embedding.mode = "proxy"` fails closed
at startup instead of silently running lexical (#1189), and
`embedding.batch_size` actually reaches the proxy (#1190) — both PR
[#1193](https://github.com/xerj-org/xerj/pull/1193); long BM25 queries
stop silently falling back to per-segment IDF/avgdl (#1186, PR
[#1188](https://github.com/xerj-org/xerj/pull/1188)); raw-JSON corpora
stopped answering every `xerj code` query with "No passage matches"
(#1158's query-time half, PR
[#1185](https://github.com/xerj-org/xerj/pull/1185) — the build-time
half stays open); and two external-contributor fixes landed: hybrid
keeps lexical-only indices in the BM25 leg (#1146, PR
[#1177](https://github.com/xerj-org/xerj/pull/1177), Vinz2168) and
`_settings`/`_analyze` echo the analysis actually in force (#1092, PR
[#1184](https://github.com/xerj-org/xerj/pull/1184), same contributor,
via maintainer rebase). `sort: [{"_doc": ...}]` pages in arrival order
(PR [#1127](https://github.com/xerj-org/xerj/pull/1127), Saurav Kumar's
first merged code). In flight past the cut: phase-B bulk coalescing for
#1147 (PR [#1195](https://github.com/xerj-org/xerj/pull/1195) — the
per-file round-trip ceiling, A/B'd on two corpus shapes with no
regression). **rc.85 was cut on 2026-10-06** — the cluster-ring-works
window: #1168's self-in-peers election churn fixed by PR
[#1179](https://github.com/xerj-org/xerj/pull/1179) (measured ~250 ms
leader failover, zero subsequent elections over 240 s) and the
clustering docs made to match reality by PR
[#1180](https://github.com/xerj-org/xerj/pull/1180) (replication is
roadmap, not shipped). **rc.84 was cut on 2026-10-06** — the
tell-the-truth window: cluster endpoints stopped fabricating ring topology
and report real membership with honest waits (#1169, PR
[#1172](https://github.com/xerj-org/xerj/pull/1172)), a degraded
cluster-transport boot surfaces in health as yellow plus an explicit
`xerj_cluster_transport: "degraded"` marker instead of green silence
(#1171, PR [#1175](https://github.com/xerj-org/xerj/pull/1175)), and
`xerj corpus index` stopped reporting a converged corpus as a failed
crawl (#1173's honesty half, PR
[#1174](https://github.com/xerj-org/xerj/pull/1174) — the
crawl-throughput half stays open). Docs: the hub counts un-staled (PR
[#1167](https://github.com/xerj-org/xerj/pull/1167)), and the
EPUB/notebook answer pages were recaptured on the rc.83 build (PR
[#1163](https://github.com/xerj-org/xerj/pull/1163), thomas-villani's
fourth merged PR). **rc.83 was cut on 2026-10-06** — the
corpus-hub hundred release: the reference-coding catalogue reached
**100 live corpora** at hub.xerj.org, each pinned, licence-reviewed at
its commit and gated by a pre-registered G7 suite, and the `xerj-code`
skill's domain-selection table now names all 100 (PR
[#1164](https://github.com/xerj-org/xerj/pull/1164)); the new-security
strata (rustsec-advisories, pysec, go-vulndb, ghsa-db, redos-precedent,
crypto-misuse-precedent) carry an ecosystem-first, defect-class-second
selection rule. The engine window: the vector-leg retrieval fix
(#1145/#1148, PR
[#1153](https://github.com/xerj-org/xerj/pull/1153)), corpus generation
hygiene (#1136, PR
[#1156](https://github.com/xerj-org/xerj/pull/1156)), and three more
[thomas-villani](https://github.com/thomas-villani) extractor PRs —
EPUB chapters, the redundant-overlap section fix, and Jupyter notebooks
plus named-and-exportable data files. **rc.82 was cut on 2026-10-05** — the
documents-people-actually-have release: man-page extraction plus the two
spreadsheet shapes real workbooks ship (vertically merged XLSX cells,
two-row grouped headers), all three
[thomas-villani](https://github.com/thomas-villani)'s first contributions
to the project; the `xerj code` retrieval fixes (issues #1137 and #1139
closed by PR [#1140](https://github.com/xerj-org/xerj/pull/1140), the
markdown sniff guards
[#1142](https://github.com/xerj-org/xerj/pull/1142)/[#1144](https://github.com/xerj-org/xerj/pull/1144));
the xerj-code skill pointing at the whole hub
([#1126](https://github.com/xerj-org/xerj/pull/1126)); and two field
reports whose CLA signatures extend the contributor list. **rc.81 was
cut on 2026-10-03** — its full contents are the
[CHANGELOG.md](./CHANGELOG.md) entry, not this file. It is **the
agent-intake release**: the zero-experience E2E harness and its four
defect fixes, `.pptx`/`.xlsx` extraction, the #1122 abort-under-memory-
pressure engine fix, and the public Corpus Hub at hub.xerj.org with the
G7 grade standing between a corpus and "live". **rc.80** (2026-09-30) was
the measured-gates release — the ten-item program to the first official
release ([discussion #1054](https://github.com/xerj-org/xerj/discussions/1054),
#1055–#1064) executed whole with every acceptance criterion measured on the
release binary — three gates passed outright, three failed and ship opt-in
or behind a flag with the numbers published, one split; the blog post
([the rc.80 gates](https://xerj.org/blog/the-rc80-gates)) carries each
verdict. The decide ladder is that release's spine — tier-2 local head behind
`--decide-mode local` (arming on the stock binary since PR
[#1097](https://github.com/xerj-org/xerj/pull/1097)), `p_cal` beside every
`p_raw` ([#1080](https://github.com/xerj-org/xerj/pull/1080),
[#1087](https://github.com/xerj-org/xerj/pull/1087)), the answer flywheel
([#1075](https://github.com/xerj-org/xerj/pull/1075)) and a `_watcher` that
evaluates one real watch shape ([#1082](https://github.com/xerj-org/xerj/pull/1082)) —
and the agent surface grew `POST /_ask` + `xerj_plan`
([#1076](https://github.com/xerj-org/xerj/pull/1076)), `xerj_map`
([#1074](https://github.com/xerj-org/xerj/pull/1074)), `max_tokens` on the
MCP search tools ([#1067](https://github.com/xerj-org/xerj/pull/1067)) and
stemming by default on profiler-marked prose
([#1070](https://github.com/xerj-org/xerj/pull/1070)). The console closes its
user gate with a knowledge surface that answers "what is indexed here" from
the engine's own numbers (PR
[#1095](https://github.com/xerj-org/xerj/pull/1095)). **rc.79 was folded into
this cut by direction — no rc.79 tag exists**, and both milestones are closed
empty. The plan tracker
[#1065](https://github.com/xerj-org/xerj/issues/1065) is complete and closed.

**What rides it:** the open defects below (the gate-run findings
and the console review's), the
`/_ask` token arm (designed, live-model cost not approved), the
`xerj-decide-v1` Hugging Face publication (operator credentials), and the GA
bar itself — *The road to v1.0.0 GA* below.

**Open defects.** Five, all live on the tracker — the
shortlist the release-notes gate checks. From the rc.97 window:
[#1635](https://github.com/xerj-org/xerj/issues/1635) (autoindex at the
default worker count stalled at 70.9% on a 13,765-file / 1,153 MB repo,
with `_bulk` timeouts, HTTP 500s and a node that stopped answering
searches; the server logs point at admission control, not yet
instrumented). From the rc.92 window:
[#1267](https://github.com/xerj-org/xerj/issues/1267) (the finalize
slow-query residual after #1260: the catalog_generation read-back walk
brute-pages `term run_id` + `_id` sort at ~5.2 s per 1,000-doc page —
641 pages ≈ 35 min on the 402,814-doc catalog whenever a resume
rewrote everything — and doc-shaped verify windows with `size > 0`
still brute-scan the data indexes at 7–81 s per window, one exceeding
the client's 300 s budget mid-crawl; PR
[#1270](https://github.com/xerj-org/xerj/pull/1270) bounded the fast-agg
verify window, the walk and doc-lane residual stay open).
From the rc.81/82 windows:
[#1122](https://github.com/xerj-org/xerj/issues/1122) (reopened
2026-10-05 when the reference node died at its memory ceiling with no
panic line). From the cluster-mode testing:
[#1170](https://github.com/xerj-org/xerj/issues/1170) (the Raft ring is
never connected to the engine). Filed from the corpus-program work:
[#1110](https://github.com/xerj-org/xerj/issues/1110) (the
generalization proof: second-domain pack + fresh-machine replay).
**Closed inside the rc.97 window and recorded in the CHANGELOG, not
carried here:** #1094 (decision flywheel write-back froze once the history
index had BM25 support: with the local decide tier armed, a split history
vote now escalates and the write-back keeps one cached answer per text, PR
[#1503](https://github.com/xerj-org/xerj/pull/1503); nodes without the
local tier are unchanged).
**Closed inside the rc.96 window and recorded in the CHANGELOG, not
carried here:** #1250 (multi-index ingest oscillating on the parent
RSS breaker: the watermark is live-tunable, PR
[#1323](https://github.com/xerj-org/xerj/pull/1323), and the bulk client's
AIMD is damped, PR [#1482](https://github.com/xerj-org/xerj/pull/1482)),
#1108 (the #1062 wrong-and-confident detection gate,
run through the real `_watcher` path and published as a FAIL — 0.2449 at
raw `p` ≥ 0.8 against the ≤ 0.05 bar, PR
[#1322](https://github.com/xerj-org/xerj/pull/1322)), #1183 (an aborted
resume no longer prints the "searchable" footer, PR
[#1318](https://github.com/xerj-org/xerj/pull/1318); the #1122
memory-ceiling class it named as its companion stays open) and #1226
(empty shards no longer queue behind the shared finalize gate, PRs
[#1319](https://github.com/xerj-org/xerj/pull/1319) and
[#1456](https://github.com/xerj-org/xerj/pull/1456)).
**Closed inside the rc.94 window and recorded in the CHANGELOG, not
carried here:** #1100 (second-brain vocabulary gated by corpus format, PR
[#1310](https://github.com/xerj-org/xerj/pull/1310)), #1230 (sibling-fork
reconcile scope, PR [#1309](https://github.com/xerj-org/xerj/pull/1309)),
#1231 (map-only `--dataset` refused, PR
[#1308](https://github.com/xerj-org/xerj/pull/1308)), #1232 (one `--watch`
owner per journal, PR [#1307](https://github.com/xerj-org/xerj/pull/1307)),
#1303 (hybrid query+knn exact BM25 under deletes, PR
[#1312](https://github.com/xerj-org/xerj/pull/1312)) and #1304 (bulk
per-item lone-surrogate reject, PR
[#1311](https://github.com/xerj-org/xerj/pull/1311)); and three verified
already-fixed and closed — #1212 (via #1013), #1244 (via #1246/#1251) and
#1173 (via #1174).
**Closed inside the rc.93 window and recorded in the CHANGELOG, not
carried here:** #1260 (per-digest `exists` probes fell off the
columnar fast path: columnar `exists` + per-segment ghost-bitmap
admission, 3.4 s → 2.7 ms warm on the live crawl node, PR
[#1262](https://github.com/xerj-org/xerj/pull/1262)) and #1254 (closed
by the rc.92 window's own #1258 merge — the auto-close fired at
655ea93ec, 42 minutes before the rc.92 tag; the rc.92 roll's "stays
open" note predates the close, so it is de-linked here).
**Closed inside the rc.91 window and recorded in the CHANGELOG, not
carried here:** #1238 (one sibling repo could fill a whole `xerj code`
page — every hit a different file, so the #1137 per-file cap could not
see the wall — and the plain-text extraction family could outscore
code-family hits; the per-index cap and the `text^0.5` recall leg, PR
[#1241](https://github.com/xerj-org/xerj/pull/1241)).
**Closed inside the rc.90 window and recorded in the CHANGELOG, not
carried here:** #1220 (the kNN exact scan's capture bracket and
capture-scoped admission, PR
[#1223](https://github.com/xerj-org/xerj/pull/1223)), #1222 (a no-match
by-query run no longer flushes, PR
[#1227](https://github.com/xerj-org/xerj/pull/1227)) and #1091 (hybrid
first-stage latency on FiQA; the latency half fixed in rc.89 by PR
[#1217](https://github.com/xerj-org/xerj/pull/1217) — semantic p50
3,865→261 ms — and closed by the #1219 field report that documented the
session; the `--embed-mode neural` re-measure it flagged is recorded
here, not left as a silent condition).
**Closed inside the rc.86 window and recorded in the CHANGELOG, not
carried here:** #1109, #1189, #1190, #1186, #1146 and #1092 (PRs
[#1194](https://github.com/xerj-org/xerj/pull/1194),
[#1193](https://github.com/xerj-org/xerj/pull/1193),
[#1188](https://github.com/xerj-org/xerj/pull/1188),
[#1177](https://github.com/xerj-org/xerj/pull/1177) and
[#1184](https://github.com/xerj-org/xerj/pull/1184)). **Closed inside
the rc.87 window and recorded in the CHANGELOG, not carried here:**
#1147 (PR [#1195](https://github.com/xerj-org/xerj/pull/1195), merged
2026-10-06 just past the rc.86 tag) and #1158 (both halves — the
query-time half by PR [#1185](https://github.com/xerj-org/xerj/pull/1185)
in rc.86, the build-time half by PR
[#1200](https://github.com/xerj-org/xerj/pull/1200)). **Closed inside
the rc.83 window and recorded in the CHANGELOG, not
carried here:** #1136 (stale index generations surviving `--fresh` swaps —
a state file with `index_prefix=null` queried all of them; fixed by PR
[#1156](https://github.com/xerj-org/xerj/pull/1156)) and two of the `xerj
code` retrieval quartet, #1145 (`k` never sent on the semantic clause) and
#1148 (`--lang` silently disabling the vector leg), both fixed by PR
[#1153](https://github.com/xerj-org/xerj/pull/1153). **Closed inside the
rc.82 window and recorded in
the CHANGELOG, not carried here:** #1137 and #1139 (per-file top-k diversity
and the txt-family search field, both fixed by PR
[#1140](https://github.com/xerj-org/xerj/pull/1140)). **Closed inside the
rc.81 window:** #1093 (unknown lookup fields — strict refusal shipped with
PR [#1112](https://github.com/xerj-org/xerj/pull/1112)); its siblings #1098
and #1099 were closed by #1095 inside rc.80.

## The road to [v1.0.0 GA](https://github.com/xerj-org/xerj/milestone/2)

The 1.0 bar: **every public claim verified against the release binary, and every input either honoured or refused loudly.** The gate list, each item an issue:

**The program to the first official release** ([discussion #1054](https://github.com/xerj-org/xerj/discussions/1054); filed as ten issues indexed in [tracker #1065](https://github.com/xerj-org/xerj/issues/1065)) — **executed whole in rc.80** (rc.79 folded into the cut by direction), every item measured on the release binary and every gate verdict published whether it passed or not. Passed: `xerj_map` ([#1055](https://github.com/xerj-org/xerj/issues/1055), unknown-field 400s 20/30 → 0/30), `/_ask` + `xerj_plan` ([#1056](https://github.com/xerj-org/xerj/issues/1056), macro-F1 0.9975 / p50 1.129 ms), MCP `max_tokens` ([#1058](https://github.com/xerj-org/xerj/issues/1058)), the calibration layer ([#1063](https://github.com/xerj-org/xerj/issues/1063), FiQA pair-level ECE 0.3109 → 0.0088 held-out), and the `xerj-decide` training harness with its measured eval card ([#1064](https://github.com/xerj-org/xerj/issues/1064) — the artifact's Hugging Face publication still PENDING operator credentials; no URL is claimed). Failed, shipped under its own loses-does-not-ship rule with the numbers public: the local judge ([#1060](https://github.com/xerj-org/xerj/issues/1060), −0.1077 SciFact vs hybrid — opt-in, no quality claim) and the flywheel's cache-share bar ([#1061](https://github.com/xerj-org/xerj/issues/1061), the write-back freeze is now defect #1094). Split: stemming ([#1059](https://github.com/xerj-org/xerj/issues/1059), +0.0160/+0.0179 BM25 nDCG@10, zero-hits 25 → 15 against ≤ 10). Real and, against #1062's wrong-and-confident ≤ 5 % bar, measured on rc.94 and **failing**: `_watcher` detections ([#1062](https://github.com/xerj-org/xerj/issues/1062) — the evaluate-or-501 half shipped; the detection gate was run through the real watcher path in [#1108](https://github.com/xerj-org/xerj/issues/1108) and scored 0.2449 wrong-and-confident at raw `p` ≥ 0.8 vs the ≤ 5 % bar, published under loses-does-not-ship in `benchmarks/systemone-watcher-gate/`). The tier-2 decide head ([#1057](https://github.com/xerj-org/xerj/issues/1057)) missed its SMS/AG News bars and ships behind `--decide-mode local` with the numbers published, arming on the stock binary since #1097. The packaging rule held for all ten: a `benchmarks/<name>` directory with raw results plus [the blog post](https://xerj.org/blog/the-rc80-gates) with the losses left in.

- **Close the accepted-and-ignored class** (the [#204](https://github.com/xerj-org/xerj/issues/204) umbrella closed once its members carried their own tracking; PR [#258](https://github.com/xerj-org/xerj/pull/258) carried one pass of the sweep and is merged). Known members still open: `nested` `inner_hits` unparsed; `random_sampler`'s ignored `probability`; `weighted_avg` returning HTTP 200 with an error buried in the aggregations body instead of a 400 (the 400 is part of #258). **Retired from this list:** `nested` `score_mode`, which was parsed-then-ignored until [#862](https://github.com/xerj-org/xerj/pull/862) made a nested query roll its matching children's scores into the parent per `score_mode` (rc.71).
- **Security hardening backlog** — cargo-audit and fuzzing landed in CI with rc.16 ([#207](https://github.com/xerj-org/xerj/issues/207) closed); the deferred TLS/auth/symlink hardening items from the Phase-2 security backlog remain.
- **The mixed read-under-write p99 gap** — the 4 benchmark losses out of 85 measured comparisons, all the same root cause (reads landing on the live memtable under writer pressure). Written up in [`demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`](./demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md); the candidate fix is a visibility/parity-mode design decision, not a micro-optimisation, and it stays on the GA gate until fixed or explicitly descoped with the benchmark loss kept public.
- **Ship-or-descope every entry in *Known partials* below.** GA does not ship with a "partial" section that reads like a feature list.
- **Close the CHANGELOG gap — closed 2026-09-26.** rc.19–rc.70 shipped without entries; the 52 sections are now backfilled from `git log` and the release records, with an in-file provenance banner and every link checked to sit inside its release window (PR [#1035](https://github.com/xerj-org/xerj/pull/1035), shipped in rc.78). A project whose pitch is verified numbers cannot ask users to reconstruct releases from `git log` — and now it does not.

## The zero-token direction

**Zero-token** is the name for work XERJ does locally, inside the engine, so that a person or an agent spends no model tokens *finding*, *judging*, *watching for* or *handing over* an answer. Search already works that way — no model runs to answer a query. This section is the rest of that idea, as product work (long form: [docs/ZERO_TOKEN_DIRECTION.md](./docs/ZERO_TOKEN_DIRECTION.md)), with each item's status checked against `main` on 2026-09-18 (`4d8dadbf`, 32 commits after `v1.0.0-rc.74`) by reading the code named beside it. "In flight" means a pushed branch exists and nothing of it is on `main`; "planned" means no code exists. Tracking issue: [#941](https://github.com/xerj-org/xerj/issues/941).

**Stage 1 — all of it merged and released in v1.0.0-rc.75** (each landed as its own PR; the status lines below name the release that carries each item):

- **Judged search — a rerank stage.** Retrieve with BM25 or `hybrid`, then have a second stage re-judge the top *N* before the page is returned. *On `main` since rc.75:* the opt-in `rerank` block on `_search`, inert until an operator configures a key; a LOCAL cross-encoder provider was measured and did NOT beat the shipped hybrid (SciFact 0.7021 hybrid vs 0.7186 local-base, interval spanning zero; NFCorpus 0.3445 vs 0.3597 local-small), so it stays opt-in and unmerged. *Before rc.75:* the ES `rescore` block (query rescorer) and the `hybrid` query type with `rrf|linear`; there is no rerank stage. *History:* branch `feat/rerank-stage` — a `rerank` block on `_search` that hands the top hits to an external relevance judge, opt-in per request and inert until an operator configures a key. It is the only *search-time* feature that would send document text off the machine — proxy embeddings (`[embedding] default_endpoint`) send it at write time and the WAL tap replays writes to an external `_bulk` endpoint, both operator-configured and off by default ([every outbound connection](./docs/RERANK.md#every-way-data-leaves-a-xerj-node)) — which is exactly why a *local* judge is on this list. *The bar for a local model is a measured one.* On the BEIR test splits with the opt-in neural embedder (`--embed-mode neural`, all-MiniLM-L6-v2, CPU) the shipped `hybrid` RRF query is already the best arm we have — nDCG@10 **0.699–0.704 on SciFact and 0.345 on NFCorpus** over three runs, against 0.657 / 0.302 for BM25 alone, 0.676 / 0.329 for vectors alone, and 0.686 / 0.332 for "BM25 top-30 re-ordered by the same bi-encoder", which is *worse* than hybrid on both. So re-ordering with the embedder we already ship is not worth building, and **a local cross-encoder ships only if it beats 0.699 / 0.345 on the same harness, by more than the run-to-run spread** ([`benchmarks/neural-path-triage/`](./benchmarks/neural-path-triage/)). Those figures are with the neural embedder; the default embedder is lexical feature hashing and was not part of this run. The hybrid figure was the only arm that did not reproduce — 0.6993, 0.7023 and 0.7044 on SciFact across three runs of unchanged indices — because tied RRF scores were ordered by a per-process hash seed; [#940](https://github.com/xerj-org/xerj/issues/940) closed that on 2026-09-21 (tied fused scores no longer ordered by HashMap iteration), and the spread above is the pre-fix measurement. The 0.699 / 0.345 bar stands until re-measured on the deterministic path — a gain inside a spread that no longer exists is still not a gain until you show it. *The next attempt on that bar is tracked as [#1060](https://github.com/xerj-org/xerj/issues/1060) (rc.80, plan item 4 of [discussion #1054](https://github.com/xerj-org/xerj/discussions/1054)): a local self-judge scoring `_p_relevant` per hit — same gate, loses-does-not-ship rule included.*
- **Share links and a guest reading room.** Hand one person a link to one corpus or one document, read-only, without creating them an account. *On `main` since rc.75:* `xerj share` with scoped, expiring links, a guest reading room, and a `--tunnel` mode that names Cloudflare as a reader of the traffic. *Before rc.75:* nothing — there was no share or guest code in the tree, and reading a corpus needs an account session or an API key. *History:* branches `feat/share-links` (scoped, expiring links; a share can never name a system index) and `feat/console-corpus-reader` (the corpus browser and the reader the link opens) carried the work before it landed; pre-merge, their commits were marked unverified by their authors.
- **Mail ingest.** *Shipped in rc.75:* `xerj autoindex` extracts `.eml` / MIME messages — headers, body, and attachments indexed as their own records ([#921](https://github.com/xerj-org/xerj/pull/921)) — and, since [#949](https://github.com/xerj-org/xerj/pull/949), streams `mbox` mailboxes and extracted Google Takeout exports (archives are never opened) and writes `replies_to` / `attachment_of` edges from the mail headers. Measured on a synthetic mailbox only; ingest memory remains the practical limit — [#948](https://github.com/xerj-org/xerj/issues/948)'s unbounded memtable retention is fixed by [#1002](https://github.com/xerj-org/xerj/pull/1002) (rc.77: variant-C retention ~677 → ~108 MB; the 300M run now fits its 8 GiB cap at 7,963.6 MiB, breaker engagements 178 → 2), and what remains is bounded per-segment cache retention until merge (issue [#1032](https://github.com/xerj-org/xerj/issues/1032), closed as deferred-not-fixed on 2026-09-29; the defect record under *Next release* carries its status).
- **`autoindex` resilience.** *Fixed in rc.75* ([#934](https://github.com/xerj-org/xerj/pull/934)): one refused dataset no longer aborts the run, an over-size catalog is split, a throttling node is waited out for 600 s, and `--fresh` adopts old state. Before it: one dataset the server refused aborted the whole run ([#929](https://github.com/xerj-org/xerj/issues/929)), the `--no-graph` progress line is indistinguishable from a hang ([#931](https://github.com/xerj-org/xerj/issues/931)), and `xc-index.sh --fresh` fails on a previously indexed corpus ([#930](https://github.com/xerj-org/xerj/issues/930)). *History:* PR [#934](https://github.com/xerj-org/xerj/pull/934) (`fix/autoindex-resilience`) carried the fix. It is on this list because every later item assumes a folder can be indexed unattended.

**Found while measuring stage 1, and gating it** — each was a filed issue with a literal repro; **all three closed with fixes that landed before the rc.77 tag** (2026-09-21):

- A declared analyzer stopped applying at flush: `analysis.analyzer.default` was honoured by the memtable only, a per-field `analyzer` was accepted and ignored, and an unknown analyzer name was accepted ([#937](https://github.com/xerj-org/xerj/issues/937)). Fixed by PR [#991](https://github.com/xerj-org/xerj/pull/991) — a declared default analyzer is honoured at flush, segment query and merge; stemming can be turned on, and the accepted-and-ignored membership for the GA gate above is retired.
- Neural ingest kept ~3.4 of 32 hardware threads busy: **6.1 documents/s** on ~1,470-character abstracts through one `_bulk` stream, **29.5 documents/s** from the same node when eight clients send concurrently, for the same total CPU ([#938](https://github.com/xerj-org/xerj/issues/938)). Fixed by PR [#995](https://github.com/xerj-org/xerj/pull/995) — bounded window concurrency for neural `_bulk` embedding.
- A `semantic` query on an index holding any multi-passage document was an exact scan that deep-copied every stored document per query: **~410 ms p50 on 5,183 documents, of which the model's forward pass is ~14 ms**; the same query on a 10,003-document single-passage index is ~14 ms ([#939](https://github.com/xerj-org/xerj/issues/939)). Fixed by PR [#979](https://github.com/xerj-org/xerj/pull/979) — the exact scan ranks addresses and hydrates only the winners; `vector_column.rs`'s own header records the old cost as something it "used to" pay.

**Stage 2 — planned; the status lines say what exists today, which in most cases is less than the API surface suggests:**

- **Semantic detections.** A standing question over incoming documents: a cheap `percolate` prefilter selects candidates, a typed judgment decides (a small closed set of outcomes, not free text), and an alert carries a **calibrated probability** — calibrated meaning measured on held-out labelled data and published with its reliability curve, or not shipped. *Since rc.80 ([#1062](https://github.com/xerj-org/xerj/issues/1062), PR [#1082](https://github.com/xerj-org/xerj/pull/1082)): the first real slice.* `PUT /_watcher/watch/{id}` either evaluates a watch on schedule or refuses it with a 501 naming what is not evaluated — the accepted-and-ignored surface this row used to carry is closed. One watch shape runs: an interval trigger, `input.search` over one index, a `condition.xerj_decide` question and an `index_alert` action, with a per-watch background task prefiltering via the watch's own query (keyset paging) and running each rendered question through the real `/_decide`; a fire lands in `.xerj_alert_fires` when the positive label wins non-abstaining at or above `p_min`, carrying the RAW confidence plus `p_cal` when a calibration is fitted ([#1063](https://github.com/xerj-org/xerj/issues/1063)). Ingest-time labels ship in the same release (`xerj autoindex --label <question-set.json>`). #1062's wrong-and-confident ≤ 5 % detection gate was **measured on the rc.94 release binary through the real watcher path and it FAILS** ([#1108](https://github.com/xerj-org/xerj/issues/1108), `benchmarks/systemone-watcher-gate/`): on a 120-email boundary tier, wrong-and-confident is **0.2449 at raw `p` ≥ 0.8** against the ≤ 5 % bar — 18 of the 24 confident errors sit at raw `p` = 1.000, so a monotonic `p_cal` cannot rescue the verdict. Published under loses-does-not-ship: the `_watcher` *mechanism* is real, but its default vote is **not** good enough to ship a detection watch as a "confidently correct" default, and no row claims it is. Still open on this row: a restart does not resume watches; the console's `.xerj_alert_rules` still has no writer.
- **A real object-storage backend.** *Done in rc.77:* [#965](https://github.com/xerj-org/xerj/issues/965) wired the segment path to object storage — `storage.backend = "s3"` packs each segment family into one immutable ZBM1 bundle object with `snapshot.json` as the publication point, merges publish before retiring inputs, and a fresh node adopts the bucket (see *Shipping today* for the full statement). The client (`s3.rs`, real S3-compatible: Cloudflare R2, MinIO, AWS S3), the read-through segment cache, per-request cost accounting by billing class and the stop-don't-warn budget had landed in rc.75. Separately, `xerj autoindex s3://bucket/prefix` reads documents OUT of a bucket, which is a source, not a home.
- **A block index mode for logs.** An index mode for log-shaped data that stores rows in time-partitioned columnar blocks and skips whole blocks by their min/max metadata, instead of building a per-term inverted index over every field. *Today:* the `xerj-logs` crate implements that design (columnar encoding, log-template extraction, time-range queries with block skipping, retention), is compiled into `xerj-engine` and `xerj-server` as a dependency, and is **called from no non-test code**. Log-shaped analytics run through the general columnar segment format and the aggregation suite. Wire it behind an explicit index setting and measure it against the general path on the same data, or remove it — the *Log-analytics data path* theme below is this same item.
- **User-code ingest plugins.** *Today:* the ingest pipeline's transforms are built-in native Rust plugins — rename, drop, add, JSON parse, timestamp parse, PII redaction, grok, route — and they do run on `_bulk`. The crate is named `xerj-wasm`, but **the wasmtime backend is not in the tree**: there is no `wasmtime` dependency and no `wasm` feature, only a note that one could be added behind the same trait. Planned: a sandboxed runtime for user-supplied transforms with fuel and memory limits and no ambient filesystem or network access. Until then the refusal rule holds: a pipeline naming a processor this build does not implement is stored as unrunnable and every ingest through it is refused, never quietly run as a shorter pipeline.
- **A corpus hub of signed, pre-indexed packs.** Download a reference corpus — a standard library, a specification set — already indexed, and mount it, instead of every user spending the same CPU-hours indexing the same public text. *Today:* the **records half is built and signed** — `xerj corpus build` (PR [#1046](https://github.com/xerj-org/xerj/pull/1046)) turns a declarative recipe (sources with licences, identity edges, merge precedence, derived fields) into a checksummed portable pack of *records* with a `format_version` readers refuse when unknown, per-file checksums verified on `corpus add --from <pack>`, and licence + provenance on every record; the first real pack is `rust-vulns` (`tools/packs/rust-vulns/`, identity-resolved across osv.dev and RustSec, with a test pinning its README numbers to a measured build), published by a scheduled workflow as a daily dated GitHub Release (immutable releases make tags single-use) with a **detached ed25519 signature** over SHA256SUMS (`xerj corpus keygen`/`sign`; `corpus add --verify-sig <pubkey>` checks origin before anything is indexed — the public key travels beside the recipe, never inside the pack; nobody in the prior-art survey signs their database). Still unbuilt: the **hub itself** (a directory of packs beyond this first one) and the **pre-indexed** half — the indexed-segment bundle that is the format this item would distribute, since indexing a records pack still costs each consumer the same CPU-hours. Its tracker, issue [#1030](https://github.com/xerj-org/xerj/issues/1030), was closed 2026-09-29: the records half shipped in rc.78, the pre-indexed half is descoped until the risks below are answered, and the work re-files when it starts. Three risks decide whether the rest ships: **redistribution licence** (an index is a derived copy of its source text — a pack may carry only what its licence lets us redistribute, which excludes much of what people most want indexed; the builder carries licence as record data and the pack author owns the choice); **pack safety** (a pack is untrusted input to the segment readers, so it needs the fuzzing the on-disk formats get, and a signature proves who built a pack, not that it is safe to open); and **format stability** (a published pack outlives the release that built it, so the pack format needs a compatibility promise the internal segment format has never had to make).

## Beyond 1.0 — themes

- **AST language expansion** — 25 further tree-sitter grammars, tiered by demand, one PR per
  tier. [#295](https://github.com/xerj-org/xerj/issues/295) delivered the expansion to 34
  languages and is closed; the remaining tiers have no tracking issue yet, so this theme is
  a plan rather than a commitment until one exists. Tier 1 (Kotlin, Swift, Scala, Dart, Lua, Perl, R, Julia, Haskell, Elixir) may land earlier in an RC if the grammar/ABI checks prove out.
- **Distributed clustering maturity** — embedded Raft handles cluster metadata today, but the default run is **single-node**; multi-node sharding/replication hardening is a post-GA track, and XERJ does not claim multi-node production readiness until it is measured.
- **Neural embedder ergonomics** — one loaded model is already shared by every index with the same embedder configuration (each index holds its own `NeuralHandle`, and identical configurations resolve to one lazily loaded model — `shared_neural_cell` in `xerj-ai/src/embedder.rs`; corrected 2026-09-18, the previous wording said each index held its own model). Still open: optional pre-warm at startup (the first embed pays the model load) and a larger default model option. The two measured defects this theme used to carry — ingest throughput ([#938](https://github.com/xerj-org/xerj/issues/938)) and the multi-passage exact scan ([#939](https://github.com/xerj-org/xerj/issues/939)) — closed with fixes (PRs [#995](https://github.com/xerj-org/xerj/pull/995) and [#979](https://github.com/xerj-org/xerj/pull/979)) before the rc.77 tag.
- **Log-analytics data path** — the dedicated `xerj-logs` columnar module is still not invoked from non-test engine/server code; log-shaped analytics run through ZBS2 + the generic aggregation suite. Wire it or remove it. The plan for wiring it is *a block index mode for logs* under *The zero-token direction* above.
- **Broader aggregation families** — geo/IP/nested/join coverage beyond the current surface; the conformance suite is the measure.

## Known partials

Honesty section: things that resolve without an error but do not implement full ES semantics. Each must be shipped or explicitly descoped before GA.

Re-verified against `main` 2026-08-30:

- **`weighted_avg`** — not in `SUPPORTED_AGG_TYPES`; still returns HTTP 200 with an embedded error instead of executing or returning 400 (the 400 is part of the #258 sweep).
- **`has_child` / `has_parent`** — recognised and rejected with a 400 (fail-loud by design until real parent-child join semantics exist; `REJECTED_QUERY_TYPES` in `parser.rs`).

Carried forward from the 2026-07-12 review, not re-verified live since:

- **`nested`** — matching is real and per-element (`test_nested_query`) and `score_mode` now rolls matching children's scores into the parent (`avg`/`max`/`min`/`sum`/`none`, [#862](https://github.com/xerj-org/xerj/pull/862), rc.71). Still missing: ES's separate nested-document indexing, and `inner_hits` is not parsed (#204 member, above).
- **`span_term` / `span_or` / `span_not`** — return 0 hits **standalone**, while composite span queries (`span_near` / `span_first` / `span_containing`) using the same clauses return correct hits.
- **`type`** — mapped to `MatchAll`.
- **`combined_fields`** — mapped to `multi_match cross_fields`; scoring is not exact. `rank_feature` passes through on plain fields (no `rank_feature` field type).
- **ES-native top-level `{query, knn}` — RETIRED in rc.72.** It now unions both halves and scores a document reached by both as the sum, aggregations included ([#825](https://github.com/xerj-org/xerj/issues/825) via [#879](https://github.com/xerj-org/xerj/pull/879)). It is kept in this list for one release with its replacement caveat, which is a performance one rather than a correctness one: the pinned clauses cannot project to the full-text index, so this shape is answered by a stored-document scan and is substantially slower than the `hybrid` query type (~208 ms vs ~2.5 ms measured on 100k documents at k=10). [#892](https://github.com/xerj-org/xerj/issues/892) carries the indexed-route fix. Correct-and-slow was chosen deliberately over fast-and-wrong.

---

Found something claimed but not working? That is a bug in our docs or our code — please [open an issue](https://github.com/xerj-org/xerj/issues). We would rather ship an honest roadmap than an overstated feature list.
