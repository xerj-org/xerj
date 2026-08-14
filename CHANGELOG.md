# Changelog

All notable changes to XERJ are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **`match` / `multi_match` / `query_string` on a `keyword` field returned a
  different hit set before and after a flush** (#354). A keyword field is
  indexed with the keyword analyzer, so the query text is one case-preserved
  term and matching is whole-value equality — which is what a flushed segment
  did, but not what the memtable did. Both pre-flush evaluators were
  schema-blind: the stored-source scan (`doc_matches_query` /
  `score_query_against_doc`) took no mapping argument at all and analyzed every
  field as `text`, and a top-level `match` did not even reach it — it was
  answered from the memtable's BM25 index, which indexes keyword values through
  the standard analyzer. So `match {tags: "red blue"}` matched the document
  `{"tags": "red"}`, `match {tags: "red"}` matched `{"tags": "red blue"}`, and
  `match {tags: "Red"}` matched `{"tags": "red"}` — until a background flush
  fired, after which all three answered correctly. No arrays are involved, so
  this is independent of the multi-valued keyword limitation. `date` fields
  keep their date-aware pre-flush comparison, which is the correct side there.

## [1.0.0-rc.16] - 2026-08-13

Both rc.15 known issues (the progress-stream forgery and the outer-`.gitignore`
reach into nested checkouts) are fixed in this release — see Fixed below.

### Performance

- **An idle node with many indices burned ~4 CPU cores and made the host
  machine unusable.** `IndexStore` spawned one OS thread per index that woke
  every `wal_batch_ms` (default 100 ms) and locked *every* WAL shard of that
  store to ask whether it was dirty. With the default 16 ingest shards that is
  ~1.5M lock operations per second on a 9,382-index node, for an answer that is
  always "no" on an idle index. WAL fsync is now scheduled by one process-wide,
  event-driven pool (`(cores/2).clamp(2,8)` threads, spawned lazily): a shard
  arms itself when a write arrives, and **an index with no pending writes costs
  nothing — no thread, no wakeup**.

  Measured on a real 9,382-index corpus, idle, with zero clients (10 samples ×
  15s): threads **9,709 → 339**; CPU **718-760% → 59.7-68.3%**; context
  switches **~197,000/s → ~4,000/s**; interrupts **~142,000/s → ~7,600/s**.
  Thread count no longer scales with index count — 20 indices versus 200 is 335
  threads versus 335, where it was 365 versus 541.

  Durability is unchanged and was verified with `strace -f -y -tt` so WAL
  fsyncs are distinguishable from snapshot fsyncs: `sync` fsyncs before ack,
  `batched` issues exactly one fsync inside the `wal_batch_ms` window, `async`
  issues none, and `kill -9` plus restart recovered every acked document in all
  three modes. RSS is a smaller win and reported as such: marginal 0.699 → 0.682
  MB per index (~12 KB/index of thread stack), since per-index RSS is dominated
  by index state rather than the thread. (#334)

  Not fixed here: ~0.65 core remains on that node from node-global sampling and
  O(indices)-per-tick work on shared timers — a different mechanism, tracked
  separately.

### Security

- **`cargo-audit` and `cargo-fuzz` now actually run in CI — they were only ever
  documented** ([#207](https://github.com/xerj-org/xerj/issues/207)).
  `user-feedback/09-security/cves-and-vulnerabilities.md` listed "`cargo-audit`
  for dependency vulnerability scanning" and "fuzz testing (cargo-fuzz) on all
  input parsing paths" as part of XERJ's answer to Elasticsearch's CVE history.
  Neither existed: no audit job, no fuzz job, no fuzz target anywhere in the
  tree. A reader comparing the two engines priced in a fuzzed parser surface
  that was not there.
  - New CI job `security-audit` runs `cargo audit` on every push and pull
    request; a RUSTSEC vulnerability advisory fails the build.
  - New CI job `fuzz` builds and runs seven libFuzzer harnesses
    (`engine/fuzz/`) over checked-in seeds: the Elasticsearch query DSL, the
    Lucene `query_string` grammar, date math and date-format patterns,
    index-name date math, SQL, Painless scripts, and the aggregation engine's
    two separate script tokenisers. `.github/scripts/fuzz-smoke.sh` is the same
    entry point locally and in CI.
  - `xerj-engine/tests/security_tooling_claims.rs` fails if a documented tool
    stops running, if a fuzz target ships without a seed corpus, or if the
    "all input parsing paths" overclaim reappears in any prose file.
  - The claim itself is now specific about what is and is not covered.
  - **Known gap, stated because the point of this entry is not to overclaim
    again:** seven harnesses are seven parsers, not the engine's whole input
    surface, and one parser outside them is already known to abort.
    `aggs::parse_time_zone_offset` slices a `time_zone` string at byte 2 after
    a *byte*-length check, so a `date_histogram` with `"time_zone": "+中a"` in
    an unauthenticated `_search` body dies on a character boundary. It is
    pre-existing, is not touched by this change, and is tracked in
    [#272](https://github.com/xerj-org/xerj/issues/272) with a fix and a
    proposed harness for the aggregation date parsers.

- **Fixed two high-severity advisories in the document-ingest XML parser.**
  Turning the audit gate on found `quick-xml` 0.36.2 in `xerj-autoindex`, which
  parses `.docx`, `.xlsx` and `.xml` files supplied by whoever owns the folder
  being indexed: RUSTSEC-2026-0195 (unbounded namespace-declaration allocation →
  memory-exhaustion DoS, CVSS 7.5) and RUSTSEC-2026-0194 (quadratic run time on
  duplicate attribute names, CVSS 7.5). Upgraded to 0.41, which also
  de-duplicates the crate — 0.41 was already in the tree via `pdf_oxide`.
  `crossbeam-epoch` moved 0.9.18 → 0.9.20 for RUSTSEC-2026-0204.

### Fixed


- **Following the documented setup and running `xerj autoindex` failed with a
  401.** `AuthConfig::default()` is `enabled: true`, so copying the shipped
  config as the docs instruct produced an auth-enabled server, while the pages
  that show `autoindex` all pass `--insecure` and the pages that tell you to
  write a config never mention a key. `ping()` also discarded the HTTP status,
  so the 401 on the first round trip was swallowed and the run printed ~15 lines
  of healthy-looking progress before dying at the embedding-identity probe with
  a message about the server's *capabilities* — leading the reporter to conclude
  their server lacked a feature rather than a credential. `ping()` and
  `embedding_execution_identity()` now fail fast and actionably on 401/403,
  `autoindex` gained the `admin.key` fallback `brain` already had (loopback
  only), the server sends `WWW-Authenticate: ApiKey realm="xerj"`, and the 401
  body names the real data dir plus `XERJ_API_KEY`, `--api-key` and
  `--insecure`. Reported by a user on macOS. (#333)

- **`xerj-done` reported an identical success line whether or not any source
  code was indexed.** A corpus in which every code file was junked printed the
  same terminal record as a healthy one, which is how a silently degraded
  index went unnoticed. The terminal line and run document now carry
  `code_files`, `code_files_indexed` and `code_files_junked` on both the
  generated and legacy paths, and a warning is emitted before the terminal line
  when code files were detected and none produced an indexed record. Named
  `code_files_indexed` rather than `ast` deliberately: a code file with no
  captured symbols still indexes correctly, so `ast` would overstate. (#294,
  #316)


- **The licence detector was wrong on 2 of the 13 repositories it had
  classified.** `sonic` was recorded as `GPL` when it is **MPL-2.0** — MPL-2.0
  defines "Secondary License" in terms of the GNU GPL inside its own text, and
  restrictive-first *body* matching hit that first. `quickwit` was recorded as
  `Apache-2.0/UNKNOWN` when it is plain **Apache-2.0** — `LICENSE-3rdparty.csv`
  is a dependency inventory, not the project's licence. The classifier now
  reads the title line before the body and skips third-party inventories.
  Elasticsearch's triple licence still resolves to `AGPL` from its title, so
  the safe direction is preserved where it matters most; that is a test.

- **A corpus manifest could write outside the corpus directory and destroy an
  unrelated checkout.** `--from` is documented as "rebuild a corpus someone
  else defined", so a manifest is untrusted input — but only the *corpus* name
  was screened, while the per-repo `repo` field, from the same file, was used
  directly as a path. A manifest with `"repo": "../../../work/repo"` moved that
  checkout to the manifest's commit, discarded its uncommitted changes and
  untracked files (`checkout --force`, `clean -fd`), and rewrote its `origin`
  remote. `repo` must now be a plain directory name, enforced in `xc-corpus.sh`
  and in `validate_manifest.py`, with the destructive path double-guarded.

- **`xc-index.sh` silently ignored an unrecognised argument.**
  `xc-index.sh <corpus> --frsh` set no flag, ran an *incremental* index and
  exited 0. Because `autoindex` keeps incremental state in `~/.xerj/autoindex/`,
  a run that should have been fresh skips every file as "already indexed" and
  leaves a corpus with 0 documents that retrieves nothing, reporting no error
  anywhere. Unknown arguments are now rejected by name.

- **`xc.py` warned about only `GPL` and `LGPL`, by exact match**, so it was
  silent on every restricted repository the hub deliberately ships:
  elasticsearch (AGPL/SSPL/Elastic), meilisearch's BUSL Enterprise Edition
  parts, and — once the detector above was corrected from `GPL` to `MPL-2.0` —
  sonic, which had been warning before. The warning at retrieval time is the
  one that counts, because that is when an agent is looking at the code and
  deciding whether to lift it; it now covers the same set as `xc-corpus.sh`.

- New CI job `reference-coding`: shell and Python syntax, the offline
  round-trip suite (`tools/xerj-code/tests/test_xc_corpus.sh`, 28 checks, local
  git repositories over `file://`, never touching the real `~/.xerj-code`), and
  `validate_manifest.py --hub` over every shipped manifest.

- **A binary that does not understand `cluster_state.json` now fails closed
  before activating storage.** The previous loader accepted future and
  partially-understood shapes, dropped fields it did not know, and could later
  rewrite the reduced document over the original. Boot now accepts only the
  complete shipped format-1 envelope, including duplicate-key and nested-shape
  checks. A rejected document leaves liveness at 200 and readiness at 503, but
  opens no user index, Console system index, WAL, segment, or durable audit
  sink; Console bootstrap and storage background jobs remain disabled until a
  supported restart. After the existing request-size, authentication, and
  authorization gates, HTTP storage access returns the stable
  `cluster_state_unavailable` 503; authenticated gRPC calls return
  `UNAVAILABLE`. Existing 401/403/body-limit precedence is unchanged. Client
  responses give the category and recovery action without a local path or
  persisted object names; the server log carries the detailed classification
  and path for the operator. The original bytes and any legacy staging file
  stay in place, and a blocked boot creates no salvage copy. Format-1 rewrites
  retain the existing
  same-directory temp-file fsync and atomic rename; parent-directory sync is a
  best-effort attempt whose errors are ignored, so this change does not claim
  strict namespace durability across power loss. Data-directory creation,
  `node.lock` acquisition/PID diagnostics, and the server's earlier
  credential/TLS preparation retain their existing behavior; the no-open/no-
  replay claim begins at the cluster-state classification and covers storage
  stores, not those process-bootstrap files.

- **`PUT /{index}/_settings {"index.lifecycle.name": null}` actually detaches
  the index, and the detach survives a restart** (#282, ported from #262;
  regression-tested in [#290](https://github.com/xerj-org/xerj/pull/290)). The
  null previously fell through a string-only settings reader, so the operator
  got `200 acknowledged` while the index stayed attached — to a policy whose
  delete phase then destroyed it. A detach now removes the execution cursor,
  writes a persisted tombstone (`ism_managed_indices.json` grew a
  `managed`/`detached` envelope; the old bare-map file is still read), and
  scrubs the stale `index.lifecycle.name` from the stored settings.
- **The lifecycle delete action gained #262's safety rails.** It now refuses —
  visibly, in `explain`, never silently — to delete a dot-prefixed internal
  index (`.ds-*` backing indices exempt), a data stream's current write index,
  or an index whose age cannot be established from its execution cursor.
- **An ILM policy naming an action the engine cannot execute is refused at PUT
  time with the action named** (400), instead of being stored with the action
  silently dropped — the accepted-and-ignored class (#204). Executable ILM
  actions are `rollover`, `delete`, `readonly`.
- **`autoindex` no longer rejects a resumed generation after journal replay
  when inferred floating-point schema statistics need exact decimal
  round-tripping.** Unchanged reruns and junk-bearing corpus shrink now replay
  correctly; existing affected journals resume without a rebuild, while the
  manifest format and integrity checks remain unchanged.

- **A `query_string` containing one non-ASCII character aborted the process.**
  `{"query_string":{"query":"_\u0660"}}` — or the same text in `?q=` — took the
  server down. The Lucene tokeniser walks bytes but slices `&q[start..i]` as a
  `&str`, and it broke on `is_whitespace()`, which is true for Latin-1 NBSP
  (0xA0). In valid UTF-8 that byte is only ever a *continuation* byte, so the
  scan stopped between the two bytes of U+0660 and the slice panicked on a
  character boundary. Every delimiter test in that scanner is now `is_ascii_*`,
  which makes "the scan can only stop on a character boundary" true by
  construction. Nothing is lost: the only non-ASCII bytes the old test accepted
  were 0x85 and 0xA0, neither of which is a standalone character in UTF-8.

- **An index name of `<{}{}>` aborted the process.** Index-name date math finds
  the first `{` and the last `}` inside the braces; when the `}` comes first,
  `date_part[2..0]` is a panic, not an empty slice. Reachable from any request
  that names an index. The closing brace is now required to come after the
  opening one.

- **A Painless script containing one non-ASCII character aborted the process.**
  The second thing the new fuzz harnesses found, inside a second. The tokeniser
  walks *bytes* (`bytes[i] as char` reinterprets each byte as Latin-1) but
  slices `&src[start..i]` as a `&str`. Every continuation byte of a multi-byte
  character reads as an alphabetic Latin-1 char, so an identifier scan could
  start or stop inside one and the slice panicked on a char boundary —
  `end byte index 24 is not a char boundary; it is inside 'ʋ'`. Scripts arrive
  in ordinary search and update bodies and the release profile sets
  `panic = "abort"`, so this was not a 400, it was the whole server. The
  identifier scan is now ASCII-only, which keeps both ends on a character
  boundary by construction; a non-ASCII byte reaches the tokeniser's
  `unexpected char` error instead. String literals still carry any Unicode.

- **A search body could abort the whole process: `{"range":{"ts":{"gte":"now+33333333333333H"}}}`.**
  The first thing the new `date_math` fuzz target found, 20 seconds into its
  first run. `chrono`'s `Duration::hours` and its siblings **panic** when the
  count does not fit a `TimeDelta`, and they run before the
  `checked_add_signed` that was supposed to make
  `xerj_query::dates::add_unit` total — so the careful checking around them
  bought nothing. A range bound goes straight from the request body into this
  code (`parser.rs:931`), the release profile sets `panic = "abort"`, and the
  result is an unauthenticated remote denial of service on a released binary.
  Fixed with the fallible `try_*` constructors. Grepping for the same pattern
  found **three** more copies in the index-name date-math resolvers —
  `xerj-engine/src/index.rs`, and *both* branches of
  `xerj-api/src/es_compat.rs`: the `now` form (`<logs-{now+9999999999999d}>`)
  and the anchored form (`<logs-{2026-01-01||+9999999999999d}>`), which are
  handled by two different functions sixty lines apart. All three are fixed,
  their `n * 30` / `n * 365` multiplications are `checked_mul`, the unit is read
  as one character rather than one byte, and the addition goes through
  `checked_add_signed`. `es_compat`'s resolver is the one that runs on the wire
  — the index path parameter of every create/index call and the `_search` index
  list — so `crates/xerj-api/tests/index_name_date_math_is_total.rs` now drives
  it at the crate boundary; the same-file fix that missed the anchored branch
  passed because nothing exercised the public entry point.

- **A `_search` aggregation script containing one non-ASCII character aborted
  the process.** `painless::tokenize` was not the only Painless-subset scanner
  in the tree: `aggs.rs` holds two more with the identical byte-walk /
  `&str`-slice shape — `lex_script` (`scripted_metric`) and `tokenize_script`
  (`bucket_script` / `bucket_selector`) — and both took `&src[i..i + 2]` to test
  for a two-character operator at a byte index that can sit inside a multi-byte
  character, because every arm above it is ASCII-only.
  `{"m":{"scripted_metric":{"map_script":"中"}}}` was `end byte index 2 is not a
  char boundary`. Both also skipped whitespace with `is_whitespace()` on
  `bytes[i] as char`, which is true for Latin-1 NEL and NBSP — bytes that in
  valid UTF-8 are only ever *continuation* bytes. The two-character probe now
  requires both bytes to be ASCII (every such operator is ASCII, so nothing is
  lost) and whitespace is `is_ascii_whitespace`. A new `agg_script` fuzz target
  covers all three entry points, which is how the next one gets found instead of
  reported.

- **An XML entity nobody declared no longer discards the text around it.**
  Fallout from the `quick-xml` upgrade above, which is a breaking change to how
  character data arrives: 0.38 stopped folding `&amp;` into the surrounding
  `Event::Text` and began emitting each reference as its own
  `Event::GeneralRef`. Entity handling is preserved across that change (a
  reader that ignored the new event would have silently dropped every `&`, `<`,
  `>` and `&#233;` while the words around them still arrived), and one case
  gets better: 0.36's `BytesText::unescape()` failed the whole text node on an
  entity no DTD declared for us, and `unwrap_or_default()` then discarded it —
  now only the reference itself is dropped. Character data is also reassembled
  from its fragments before being emitted, so an entity-bearing field stays one
  value instead of becoming an array of pieces.

- **`constant_score` on a mixed flushed/unflushed corpus returned the wrong
  page, and any tied hit set could be re-sorted into `_id` order**
  ([#270](https://github.com/xerj-org/xerj/issues/270),
  [#300](https://github.com/xerj-org/xerj/pull/300)). Two defects. A
  top-level `constant_score` (or `boost`) wrapper defeated the match-all
  shortcut, so the stored scan ran in exact-counting mode and a bounded page
  came back all-memtable — `size:1` on 40 flushed + 300 unflushed identical
  docs returned the memtable doc where the oldest flushed doc was correct;
  the wrapper is now peeled for the count/bounds decisions only, with scoring
  unchanged. Separately, five post-sort re-sorts — the bool-text IDF rescore,
  the near-zero-BM25 TF-IDF fallback, and the three `rescore` sorts — tied by
  `_id` alone, so any of them firing on tied scores reordered the page; the
  main sort and all five now route through the one page-order key
  (`score DESC, seq_no ASC, _id ASC`). Known residual, documented rather
  than asserted around: bounded pages for multi-clause bools on a mixed
  corpus are still admitted under first-pass scores (#188's remit).

- **A shrinking file set on a junk-bearing `autoindex` generation aborted the
  cutover with `desired manifest digest does not match sync_begin payload`**
  ([#283](https://github.com/xerj-org/xerj/issues/283),
  [#296](https://github.com/xerj-org/xerj/pull/296)). A fresh plan kept
  duplicate-file aliases whose content was junk or skipped while the
  incremental projection dropped them, so the two disagreed on byte-identical
  junk duplicates — two empty files suffice. The fresh plan now projects
  exactly like the incremental path, so the shrink reconciles cleanly. Where
  a journal is *genuinely* inconsistent, the replay and validation paths now
  say what to do — no remote data changed, re-running will not repair it,
  rebuild with a new `--state-dir` and `--prefix` — with the invariant kept
  as the error cause.

- **An outer repository's `.gitignore` no longer judges files inside a nested
  checkout** (the second rc.15 known issue below;
  [#287](https://github.com/xerj-org/xerj/pull/287), repairing
  [#279](https://github.com/xerj-org/xerj/pull/279)). The rule walk had no
  repository-boundary stop, so a root `*.md` hid the README of every
  vendored dependency beneath it. The git-sourced rule kinds now stop at a
  `.git` boundary — the set kept under a nested checkout is byte-for-byte
  what `git status --untracked-files=all` reports there — while
  `.xerjignore` and the built-in defaults deliberately still govern the
  whole folder you named. Three more defects from the same merge: hidden
  directories pruned by the dotfile rule were deep-counted into
  `ignored_files_in_pruned_dirs` (97,731 phantom files on this repository's
  own walk); that number reached the `xerj-done` line and `--progress json`
  as a bare total when it is budget-capped — a new
  `ignored_files_in_pruned_dirs_exact` flag now says whether it is a total
  or a floor; and `--no-ignore`/`--no-default-ignores` were accepted and
  ignored (#204) on `autoindex map` and `status`, which never walk a
  filesystem — both are now refused there with the reason. The shipped docs
  that contradicted the code are corrected.

- **A finished `autoindex` run could leave its state-directory lock held, so
  the immediately following run on the same state dir was refused as
  already in use** ([#305](https://github.com/xerj-org/xerj/pull/305)).
  `flock` ownership follows the open file description, so a helper
  subprocess forked while the journal was live (the PDF extraction helper,
  git invocations) could inherit the descriptor and keep the lock held past
  the owner's close. The lock guard now records the acquiring PID and
  unlocks on drop only in that process — a fork-inherited copy closes its
  descriptor without releasing the live owner's lock. No retry, wait or
  takeover behaviour was added; crash recovery is unchanged. Covered by a
  same-OFD unit oracle and a real-fork integration regression on
  Linux/macOS.

- **`autoindex` indexed a `.mts` or `.cts` file as prose**
  ([#284](https://github.com/xerj-org/xerj/pull/284)). The extension
  registry claimed `.mjs`/`.cjs` but not TypeScript's own ESM/CJS
  counterparts, and an unclaimed extension does not degrade to "code without
  symbols" — it fell through to the prose extractor, so the file carried no
  `language`, no `defs`, no `symbols`, was split into several body-only
  records, and was invisible to symbol search.

- **An exported module `const` reached `defs` in neither TypeScript nor
  JavaScript** ([#285](https://github.com/xerj-org/xerj/pull/285),
  [#304](https://github.com/xerj-org/xerj/pull/304), closes
  [#293](https://github.com/xerj-org/xerj/issues/293)). Capture only fired
  when the value was a function, so a module whose whole public surface is
  built by factory calls — `export const users = pgTable(...)`, a router, a
  config object — extracted zero symbols and could not be found by symbol
  search at all. The new pattern is exported-only, anchored to module scope,
  and matches the `const` token, so function-local bindings and mutable
  exported `let` stay out; module-private `const` was measured and excluded
  because it added weight without retrieval gain.

- **A PHP 8.1 enum extracted zero symbols, and class constants were never
  captured** ([#286](https://github.com/xerj-org/xerj/pull/286)). An enum
  declares no class, interface or trait, so a file holding one could not be
  found by its own name through symbol search. Enums, enum cases and `const`
  declarations — class-level and file-scope — now land in `defs`, with
  cases treated as named constants the way Go's `const_spec` members are;
  `define()` calls stay uncaptured.

### Added


- **`xerj mcp` — the MCP server now ships in the installed binary.** XERJ had a
  complete, tested 10-tool MCP server that no user could obtain: releases built
  only `xerj-server`, the installer shipped only `xerj`, and MCP appeared in no
  agent-facing doc. It is now a subcommand, so anyone who ran
  `curl -fsSL https://xerj.org/get | sh` gets it on their next upgrade, with no
  installer or release-matrix change and no new third-party dependencies. Tools:
  `xerj_search`, `xerj_semantic_search`, `xerj_vector_search`,
  `xerj_hybrid_search`, `xerj_memory_store`, `xerj_memory_recall`,
  `xerj_brain_ego`, `xerj_brain_link`, `xerj_brain_unlink`,
  `xerj_brain_overview`. A XERJ node must already be running; `xerj mcp` does
  not start one. The published tool schema at
  `/docs/agents/schemas/mcp-tools.json` had drifted to six tools against ten
  served — it is now generated from a real `tools/list` and gated in CI against
  drifting again.

- **`[profile.quick]` and `[profile.ci-test]`** for the build and test halves of
  the verify loop. `[profile.release]` is `lto = "fat"` + `codegen-units = 1`,
  deliberately the slowest build Rust can produce because it yields the fastest
  binary — correct for a shipped artifact, wrong for "does this compile?". CI's
  test job spent 76m05s compiling for 1m52s of tests. Neither profile is ever
  valid for a released artifact or a published performance number.
  `docs/FAST_BUILDS.md` documents the remaining levers.


- **The reference-coding toolkit ships in the repo, and a corpus definition can
  now rebuild a corpus** ([#319](https://github.com/xerj-org/xerj/issues/319)).
  `xc-corpus.sh`, `xc-index.sh`, `xc.py` and `SKILL.md` lived only in a
  gitignored working copy, so `git ls-files | grep xc-` returned nothing while
  the case study, `CONTRIBUTING.md` and the landing page all told a reader to
  run them. They are now in [`tools/xerj-code/`](tools/xerj-code/), with a
  `!tools/**/*.md` re-include in `.gitignore` — the blanket `*.md` rule was what
  swallowed `SKILL.md`, the same failure mode that hid this repo's pull-request
  template for its entire history.
  - `corpus.json` records the **full 40-character SHA**. The old
    `rev-parse --short` value cannot be fetched from a remote
    (`git fetch --depth 1 origin e449d17` → "couldn't find remote ref"), so a
    manifest could not pin anything. A manifest carrying a short SHA is now
    rejected with a SHA-specific error rather than silently rebuilt at the tip.
  - `xc-corpus.sh --from <manifest>` rebuilds a corpus at its pinned commits —
    fetch-by-SHA at depth 1 with progressively deeper fallbacks — and an
    existing clone is *moved* to the recorded SHA instead of skipped.
  - [`tools/xerj-code/hub/`](tools/xerj-code/hub/) carries vetted definitions
    for the four corpora this repo uses. Each entry has a `review` block a
    human filled in: SPDX expression, `adapt-with-attribution` /
    `approach-only` / `mixed`, reviewer and date.
  - Hosting pre-built *indexes* remains out of scope (on-disk format version,
    embedder identity, redistribution of the indexed source), as does the
    measurement harness — `CASE_STUDY.md` and `SKILL.md` previously pointed at
    a `MEASURE.md` that is not in the repo, and now say plainly what ships.

- **`GET /_ilm/status`, `POST /_ilm/start`, `POST /_ilm/stop`** (#282): the
  operator can halt lifecycle execution without stopping the node; a stopped
  engine's tick touches nothing. The flag is in-memory — a restart resumes
  execution — while the per-index detach tombstone is the durable stop.
- **`POST /{index}/_ilm/remove`** (#282): ES's own detach verb; goes through
  the same tombstoned detach path as the settings-null route. A literal name
  that is not an index answers 404 rather than writing a tombstone for a
  ghost.

- **A dynamically-discovered nested object field is no longer permanently
  opaque to `GET _mapping`/`_field_caps`**
  ([#292](https://github.com/xerj-org/xerj/pull/292),
  [#307](https://github.com/xerj-org/xerj/pull/307)). A subfield like `metadata.kind`
  was already queryable (dotted-path resolution against `_source` doesn't
  care about the mapping) but had no way to be *discovered* by a client that
  reads the mapping instead of already knowing the field name — indistinguishable
  from not existing to Kibana/OSD's own field-list UI. Three compounding gaps,
  all fixed together: dynamic mapping never recursed into `Value::Object`
  (`{"type": "object"}` was a terminal, not real ES/OS default behavior);
  `GET _mapping`/`_field_caps` served the index-creation-time explicit mapping
  blob verbatim forever, so *any* field discovered dynamically after creation
  — flat or nested — never surfaced, no matter how many documents arrived; and
  `_field_caps`'s per-field walk only ever registered top-level names, so a
  nested object's own children were still invisible even once the mapping
  correctly described them. Measured end-to-end on `/_memory`'s own backing
  index: `GET .xerj-memory-{ns}/_field_caps?fields=metadata.kind` returned
  `{"fields": {}}` before, the real entry after. The cluster-wide
  `GET /_field_caps` (as opposed to the per-index `GET /{index}/_field_caps`)
  had the same gaps independently and is fixed the same way — the two no
  longer disagree about which fields exist. Dynamic mapping also now
  re-inspects nested fields on *every* document — a later document adding a
  new nested key merges it into the existing mapping instead of leaving it
  permanently invisible — and `Schema::field_count()` counts nested children
  and multi-fields recursively, closing a `max_fields_per_index` bypass
  where one top-level object could carry unbounded uncounted children. Note
  the tightening this implies: a dynamic text field with its `.keyword`
  multi-field now costs 2 against the default limit of 500, which matches
  how ES counts `index.mapping.total_fields.limit`. Two bounded residuals
  from review (a one-shot budget overshoot on a single deeply-keyed insert;
  array-nested keys deferred to the 100-doc recheck) are filed as
  [#312](https://github.com/xerj-org/xerj/issues/312).
  **Known limitation**: this only benefits indices created after this
  ships. An index (a `/_memory` namespace or otherwise) created before it
  already has its stored mapping written, and that isn't retroactively
  edited — existing indices need either re-creation or an explicit
  `PUT _mapping` to pick up the fix. No automatic migration is included.

- **A crafted filename can no longer forge records on the agent progress
  stream** (the first rc.15 known issue below). Every externally-controlled
  string — in-flight paths, the paths and error text interpolated into human
  notes, the terminal line's reason — is stripped of control characters, bidi
  overrides and zero-width characters and bounded in length before it reaches
  any progress surface. Measured on a corpus holding crafted names: a run that
  previously emitted a forged `xerj-done ok=true exit=0 reason=completed`
  2.2 s ahead of its real terminal line now emits one terminal line and no
  control characters at all. The same sanitisation covers the walker's
  "skipping unreadable entry" warning, which renders a path to the same stderr.
- **`--progress json` paces the bar like `--progress plain`.** The `bar` field
  bypassed the spacing slot entirely: measured at `--progress-interval 1`, 178
  of 178 ticks carried a bar against 18 bars on the plain surface. It is now a
  string on exactly the ticks that owe a bar and `null` in between — 26 of 320
  ticks on the same corpus, against 16 on plain.
- **The bar spacing floor is the 15 s the documentation states.** A half-tick
  tolerance made the enforced floor `interval/2` shorter — 12.5 s at the
  shipped defaults. Measured at `--progress-interval 10`: 11 of 11 same-phase
  gaps under 15 s (min 10.0 s) before, none after (min 19.3 s).
- **`/{index}/_refresh` reports segment-publication failures instead of
  claiming every shard succeeded.** The endpoint previously discarded every
  error returned by the synchronous engine flush and always answered HTTP 200
  with `successful == total`. It now includes the index, status, and underlying
  reason in `_shards.failures`, attempts every resolved index, and returns the
  first failed shard's HTTP status as Elasticsearch 8.13.4 does for refresh.

- **Text field names containing path separators can be flushed and reopened.**
  FTS side-car filenames previously interpolated the raw field name, so a name
  such as `styles_blocks_core/site-title` became a child path and prevented the
  segment from being published. Unsafe or overlong field names now use one
  deterministic bounded filename component. Existing portable names retain
  their byte-identical paths. Each segment has an immutable filename-layout
  discriminator: absence means the historical raw layout; an explicit marker
  means the encoded layout. Readers never infer the layout from side-car file
  existence, preventing partial files and raw names that resemble digests from
  aliasing another field. Before the discriminator or first encoded side-car
  is written, the index durably advances its data-directory marker to format 2;
  older binaries refuse that index instead of silently missing the new layout.
  A retry after a post-rename marker failure must re-establish durable marker
  publication before writing the discriminator or encoded FTS paths. Unix
  confirms the parent-directory fsync; Windows uses a same-directory Win32
  write-through replacement rather than claiming its directory-sync no-op is
  durable. Safe-only indices remain format 1.

### Changed

- **Default `_search` source payloads omit engine-generated embedding
  companions** ([#309](https://github.com/xerj-org/xerj/pull/309)). A search
  with no `_source` now returns ordinary source fields without
  `<field>_vector` and `<field>_vector_chunks` — identified from the
  mapping's embedding config, not by name; measured payload size fell from
  211,432 to 21,069 bytes for 5 hits (about 90%). Explicit `"_source": true`,
  `"_source": ["field"]`, and include/exclude forms are unchanged, vectors
  remain stored and used for kNN/semantic/hybrid scoring, and the
  document-copying paths (`_reindex`, index clone, enrich execution) request
  the full source explicitly so copies keep embeddings byte-for-byte. Add
  `"_source": true` to restore the previous complete payload, including
  vectors. Two residuals disclosed from review: a `fields` request naming a
  companion resolves against the already-filtered source and comes back
  empty under the new default — name it in `_source` instead
  ([#310](https://github.com/xerj-org/xerj/issues/310)) — and the new
  default arm copies each hit's source even on indices with no embedding
  fields, a constant-factor read-path cost filed as
  [#311](https://github.com/xerj-org/xerj/issues/311).

- **`merge.strategy = "log_structured"` is now refused at startup instead of
  silently running the size-tiered policy**
  ([#207](https://github.com/xerj-org/xerj/issues/207)). Nothing in the tree
  ever read `merge.strategy`; `run_merge_once` always builds a
  `SizeTieredMergePolicy`. An operator who chose a levelled policy for its read
  amplification got the other one, quietly. This follows the `storage.backend`
  and `vector.default_quantization` guards already in `Config::validate`.

- **The three dormant `[merge]` settings now say so, in the code, in the shipped
  TOML, on the docs site, and in the log at startup**
  ([#207](https://github.com/xerj-org/xerj/issues/207)). The issue reported
  merge I/O rate limiting as "claimed but dormant" and it is: the `RateLimiter`
  that honours `io_rate_mb_per_sec` is wired only into `xerj-storage`'s legacy
  `MergeExecutor`, which the engine never constructs. `min_segments` and
  `max_concurrent` are unread too — the real per-tier trigger is
  `min_merge_count`, and merge parallelism comes from `XERJ_MERGE_PARALLELISM`.
  `MergeConfig::dormant_overrides` names any of the three the operator has moved
  off its default and `xerj-server` logs each at WARN, so the operator throttling
  merges to protect query latency finds out instead of guessing. They stay
  accepted rather than becoming hard errors because the wrong value costs
  latency, not data, and `io_rate_mb_per_sec = 100` has shipped in
  `xerj.default.toml` since v0.1. Wiring a real throttle into `run_merge_once`
  is the fix that makes this list shorter.

- **`engine/xerj.default.toml` disagreed with the defaults it claims to
  document, in four places.** `max_segment_mb` said 5120 against a real 8192,
  `wal_max_size_mb` 512 against 1024, `flush_size_mb` 256 against 512, and
  `default_quantization` `"scalar8"` against `"none"` — so `cp xerj.default.toml
  xerj.toml`, the documented first step, silently changed four engine behaviours
  including turning on 8-bit vector quantization. The file is corrected and
  `shipped_default_config_documents_the_real_defaults` now diffs every leaf key
  against `Config::default()`, so it cannot drift again.

- **The docs site had drifted the same way, and now has the same guard.**
  `landing/docs/config.html` shipped `wal_max_size_mb = 512`,
  `flush_size_mb = 256` and `default_quantization = "scalar8"` in its
  copy-pasteable `[storage]` and `[vector]` blocks while its own DEFAULT
  table — thirty lines up, on the same page — said 1024, 512 and `"none"`;
  `landing/docs/storage.html` said the WAL rolls at 512 MiB;
  `landing/docs/vectors.html` called `scalar8` "the default" and listed a
  `scalar4` mode that no config or mapping value can reach; and
  `engine/README.md` put `flush_size_mb` at 256 MiB. All four are corrected,
  along with the docs search-index blob duplicated across 44 landing pages,
  which offered `scalar4` as a selectable quantization mode.
  A second guard, `the_docs_site_config_page_agrees_with_the_real_defaults`,
  now diffs *both* halves of that page — every DEFAULT cell in the reference
  table and every assignment in the example blocks — against
  `Config::default()`. An example that deliberately differs (the blocks whose
  point is switching TLS or cluster mode on) has to carry `# not a default` on
  the line, so the reader is told as well as the test.
  - **Known gap, found while correcting the `scalar4` prose and disclosed
    rather than quietly fixed:** the guard reads `config.html` against
    `Config::default()`, so it covers the *config* half only. Nothing checks a
    *mapping*, and a mapping is not validated either —
    `es_compat.rs` matches `"scalar8" | "int8" | "none"` on a `dense_vector`
    field's `quantization` and falls through `_ => {}`, so
    `"quantization": "scalar4"` (or `"binary"`, or a typo like `"sq8"`) is
    accepted with a 200, echoed back verbatim by `GET /_mapping`, and ignored
    — the field is stored at full-precision f32 while the mapping says it is
    quantized. Measured through the ES-compat router at this commit. It is
    pre-existing and outside this diff; it is another instance of the
    accepted-and-ignored class in
    [#204](https://github.com/xerj-org/xerj/issues/204), is filed with the
    repro and a proposed 400 as
    [#275](https://github.com/xerj-org/xerj/issues/275), and
    `landing/docs/vectors.html` now says so on the page instead of leaving
    "not a mode you can select" to imply the value is rejected.

- **Four other claims from [#207](https://github.com/xerj-org/xerj/issues/207)
  now match the source.**
  - *Settings count.* `config.rs` said 38 in its header and 56 twenty lines
    down while the count test asserted 60 — and the test asserted it by
    re-adding a hardcoded sum (`5 + 2 + 3 + … - 1 == 38`), an identity that
    could not fail whatever `Config` held. There was a second copy of the same
    non-test in `config.rs` itself (`count_user_facing_settings`, `61 == 61`).
    Both now serialise `Config::default()` and count leaf keys, section by
    section. The measured figure is **105** (103 when this work was done; the
    rc.14 `server.allow_insecure_network_bind` key made it 104 and #247's
    `lifecycle.tick_interval_secs` makes it 105 — which is the point: it is
    measured, so it moves with the code), and 38 / 50 /
    56 / 60 / 61 / "<50"
    are corrected in `config.rs`, `xerj-common/src/lib.rs`, `engine/README.md`,
    `xerj.default.toml` and the feedback responses, along with the stale
    per-sub-config annotations (limits 3 → 13, storage 5 → 10, embedding 4 → 19,
    merge 5 → 8, cluster 4 → 5, tls 3 → 4, auth 2 → 3). This closes item 10 of
    `user-feedback/ROADMAP-TO-GA.md`. 105 versus 3,000+ is still the winning
    story, told truthfully.
  - *Wire-compatibility test framing.* `journey_es_migration` was described as
    proving "the same curl commands" work. It calls the Rust engine API
    directly and never makes an HTTP request. The response-shape assertions are
    real and stay; the comment now says what the test does and points at the
    ES-compat YAML conformance suite, which is what actually covers the wire.
  - *OpenAPI spec.* `landing/openapi.json` declared `version: 1.0` while
    specifying 7 routes out of the 200-plus the router registers. It is now
    titled and versioned as a partial spec, and says so.

- **`autoindex` estimates the job on the user's own machine and hands the
  decision back before it takes their laptop.** Phase A already reads and
  parses every file to sniff and sample it, so it now *times* that work per
  format family and turns it into a range with its basis printed next to it —
  `code 500 files 749.3 MB at 11.7 MB/s measured over 500 file(s) → 64.2 s` —
  rather than a constant calibrated on someone else's hardware. A family is
  priced only from files phase A **provably** read end to end (whole-file
  parsers always; streaming parsers only when the file was under the sampling
  byte cap *and* stopped short of the record cap; never `sqlite`, never
  gzip), and everything else is named under `unmeasured_families` with its
  bytes instead of being priced at another family's rate. If nothing could be
  measured there is no number at all and no gate. The two ends are the
  classical list-scheduling bounds (Graham, 1969) over the phase-B worker
  count that #240's resource policy chose.
  The number is a **floor**, labelled as one on every surface: it covers
  client-side extraction only, because measuring the server, the network or
  embedding would mean writing to the index the estimate exists to ask
  permission for. Measured on a 68 MB source tree the floor was 0.1 s against
  a real 8.9 s run; on a 793 MB one, 64.2 s against ~350 s. The gate therefore
  under-asks and never over-asks, and says so where a reader would otherwise
  mistake silence for a promise.
- **`--max-minutes` (default 10) stops the run and asks instead of deciding.**
  Past the threshold with no answer, autoindex indexes nothing, writes a JSON
  decision request to stdout and exits **4** — a code of its own, because exit
  1 is already the catch-all for every real failure and an agent must be able
  to tell "choose something" from "your endpoint is down". The payload carries
  the estimate and its basis, file/byte counts, the per-band work order, the
  heaviest directories with real byte counts (flagged when they match the
  vendored/generated rule), and four options: `proceed`, `fast`, `narrower`,
  `cancel`. Answer with `--approve <id>` (`--yes` = proceed), which skips the
  gate; `--max-minutes 0` disables it. A person at a terminal gets the same
  facts as prose plus a prompt; a piped or agent-driven run is **never**
  blocked on stdin, and an unanswerable prompt is a cancel, never consent.
  `--approve fast` really applies `--no-semantic --no-graph`, and `--approve
  narrower` is refused with instructions rather than accepted and ignored
  (#204). The `fast` option states no speed-up factor: it reports the datasets
  and file count it changes and says plainly that this run did not measure the
  factor.
  The gate governs the phase-B route. An **incremental reconcile of an already
  committed generation** (a `--no-graph` re-run over a folder that already has
  one) processes only what changed and publishes from a sealed snapshot, so
  `--max-minutes` does not apply there — and that route now *says so* on
  stderr rather than accepting the flag and quietly not honouring it.

- **`autoindex` indexes what matters first.** Phase B's queue was sorted by
  size alone — right about scheduling, silent about value, so a user who
  stopped early got whatever was largest, which on a source tree is
  `node_modules`. Work is now ordered by value band (source and documents →
  configuration → structured data → logs and line files → vendored, generated
  and minified paths), with the old biggest-first rule kept *inside* each band
  so a large file still runs alongside its band instead of becoming the tail;
  a single-worker run goes smallest-first, where there is no tail to hide in.
  One exception keeps the new order from costing wall clock: a file whose own
  extraction outlasts everything ranked above it (`size × workers > the rest`)
  starts first regardless of band. The bands, their file/byte counts and the
  reason each sits where it does are printed with the plan and included in the
  decision payload as `priority_order` — an unexplained order is
  indistinguishable from an arbitrary one. Verified live on a 28 MB mixed
  corpus at two workers: the first second of phase B was spent on
  `src/mod_*.rs`, and the 1.7 MB `node_modules/**` files — the largest in the
  corpus, and therefore *first* under the old rule — drained last.
  Bytes-based progress and its percent are unaffected: reordering does not
  change the denominator.

## [1.0.0-rc.15] - 2026-08-10

### Known issues in this release

Both were found by adversarial review *after* their pull requests merged, so
they are present in these binaries. Fixes are in flight for rc.16.

- **A crafted filename can forge records on the agent progress stream.** The
  in-flight path is rendered onto the progress surface without control-character
  sanitisation, so a filename containing a newline can inject something that
  looks like a genuine `xerj-progress` or `xerj-done` record — including a false
  `ok=true` completion — into the stream this release tells AI agents to parse.
  Indexing a repository whose filenames you do not control is enough to trigger
  it. Until the fix lands, do not treat that stream as trustworthy when the
  corpus is untrusted.
- **An outer repository's `.gitignore` judges files inside a nested checkout.**
  The ignore lookup walks every layer with no repository-boundary stop, unlike
  git itself, so a vendored or submoduled repository is filtered by rules that
  have no authority over it. Files you expect to be indexed may be skipped.
  `--no-ignore` bypasses it.

Also unfixed and worth knowing: `--progress json` does not pace the bar (a JSON
consumer sees roughly ten times as many bar lines as a plain consumer), and the
documented "at most one bar per 15 s" floor is really 12.5 s.


### Added

- **Index lifecycle policies are executed, not just stored**
  ([#199](https://github.com/xerj-org/xerj/issues/199), contributed by
  @Vinz2168). A single internal engine modeled on OpenSearch's ISM state machine
  — named states, ordered actions, ordered transitions — exposed through two
  REST surfaces: native ISM at `_plugins/_ism/*` and the Elasticsearch-shaped
  `_ilm/*`. `spawn_lifecycle_manager()` runs the tick on
  `lifecycle.tick_interval_secs` (default 300s, matching OpenSearch ISM's own
  job interval), and the managed-index cursor survives restart. Retention now
  actually deletes and rolls over instead of being acknowledged and forgotten.
  One documented divergence: `min_age` is measured from when an index entered
  its current state rather than from rollover time, so a policy that rolls over
  *and* has a downstream phase meant to be measured from the rollover will
  advance on a different clock than Elasticsearch uses.

- **`xerj autoindex` honours `.gitignore` and `.xerjignore`**
  ([#276](https://github.com/xerj-org/xerj/issues/276)). Nested ignore files and
  negation patterns follow git's own precedence, with build-output defaults
  (`node_modules`, `vendor`, `target`, `dist`, `build`, `.venv`, `__pycache__`)
  on top; `--no-ignore` and `--no-default-ignores` turn them off. Pointing xerj
  directly at an ignored path still indexes it — an explicit instruction beats a
  rule the user did not write for this purpose. `--dry-run` reports what was
  skipped and by which rule, because a user whose files did not appear needs to
  know why. Measured on this repository: 274,826 files walked in 1.18s becomes
  1,385 in 0.25s.

- **Progress is relayed to an AI agent as a drawn bar.** The stream surface
  carries a rendered bar alongside its machine-readable `key=value` fields, so
  an agent driving `xerj autoindex` on someone's machine can show a real
  progress line rather than going silent for minutes. `--progress json` keeps a
  single parseable stream; `--quiet` still writes nothing on either.

### Fixed

- **Tied scores have one total order — a bounded page is a prefix of the full
  page** ([#191](https://github.com/xerj-org/xerj/issues/191)). A `size:5` page
  and a `size:1000` page disagreed about which tied document came back. The
  memtable's bounded candidates are now ranked by the same page key as the
  segment path, so the two agree. The remaining `constant_score` and post-sort
  re-sort cases are tracked separately in
  [#270](https://github.com/xerj-org/xerj/issues/270).

- **`date_histogram` no longer aborts the process on a multi-byte `time_zone`**
  ([#272](https://github.com/xerj-org/xerj/issues/272), contributed by
  @Vinz2168). `aggs::parse_time_zone_offset` sliced a `&str` on a byte index
  that could land mid-character, and with `panic=abort` that took the whole node
  down — an unauthenticated request was enough.

- **The legacy path-parameter form of scroll continuation works again**
  (contributed by @Vinz2168), and **`_cat/*?format=json` plus `_data_stream/**`
  wildcards** are supported (also @Vinz2168) — both are shapes real
  Elasticsearch clients send.

### Changed

- **`autoindex` parses each PDF once per fresh run instead of twice.** Phase A
  already paid a *complete* parse for every PDF — `extract::extract` routes
  `Family::Pdf` straight to the isolated worker and drops the sampling limit,
  so only delivery ever stopped early — and phase B then spawned the worker
  again for the same bytes. Phase A now retains each validated worker response
  in an anonymous temporary file under `--state-dir` and phase B replays it.
  Reuse is an optional accelerator, never correctness-critical state: it is
  bounded to 384 MiB of retained-plus-in-flight bytes and to an artifact-handle
  cap derived from live `RLIMIT_NOFILE` and open-descriptor counts, both
  re-probed at every admission; a 4 GiB-or-half-free filesystem floor is
  reserved for the journal and phase-B staging first; and every artifact is
  verified (physical length, content digest, JSON decode, worker-protocol
  identity) before a single record is published. Anything that cannot be
  measured conservatively declines and reparses — which is why the optimization
  is **Linux-only** today: other platforms have no live descriptor evidence.
  A restart has a frozen plan but no trusted handle, so it parses again; the
  spool is deliberately not journal state. `--json` gains a
  `pdf_extraction_reuse` block so the behaviour is observable rather than
  inferred from timing. Original work by Leonid Bugaev (@buger).

### Changed — autoindex refuses reruns that would strand documents

- **Deleting an indexed file and rerunning `xerj autoindex` now fails instead
  of exiting 0.** Nothing in the pipeline removes the documents, aliases,
  graph edges or catalog entries that the deleted file published, so a rerun
  that ignored the deletion left them live and searchable with no source file
  behind them. The rerun is now refused before any remote call other than the
  endpoint-readiness ping: no mapping, delete-by-query, bulk, refresh, graph
  or catalog write is attempted, and the journal is not appended to. The error
  names the removed files and their content keys — the first ten, then an
  `… and N more` tail, since a whole unmounted subtree can vanish at once —
  and gives three recovery routes: restore the files and rerun, rebuild in
  place by deleting the named indices and the state directory, or rebuild
  under a new state directory, prefix and brain. `--fresh` is refused for the
  same case, because it does not delete those documents either. `--json` emits
  the same facts — every removed entry, uncapped — as
  `xerj.autoindex.unsupported_sync_delta.v1` on stdout with exit 1. Deleting a
  file that was only ever *skipped* is not refused: a junk file publishes no
  documents, and the stale junk-catalog sweep removes its one catalog row, so
  nothing is stranded.
- **Files added after a plan was frozen are now called out on stderr.** They
  are still not indexed by a rerun that resumes an existing plan — the plan is
  a crash-resume boundary, not a folder-sync generation — but the run now
  lists them, records them as skipped (exit 3, completed-with-junk) and points
  at `--fresh`, which rebuilds the plan in place and picks them up. Adding or
  changing files and rerunning keeps working; only removals are refused.
- **`xerj brain` no longer turns an absent or zero node-count probe into an
  automatic reset.** Journal/server disagreement now fails with the journal,
  URL, prefix, and brain identity plus recovery instructions, including the
  explicit `--fresh` rerun for a genuinely wiped data directory. Probe
  transport and malformed-response failures remain errors instead of being
  reported as an empty destination. A nodes index that was deleted out from
  under a surviving brain meta doc reads as absence and reaches that recovery
  text, rather than surfacing as a raw HTTP 404.
- **`--fresh` still recovers a state directory whose journal cannot be
  parsed.** The rerun gate reads the durable plan before the run starts, and a
  plan that is malformed, or recorded for a different root/URL/prefix, is fatal
  to a resume — but not to `--fresh`, which deletes that journal unread. The
  preflight is now no more fatal than the open it precedes: under `--fresh` the
  unreadable plan is reported on stderr and rebuilt from the current folder.
  Without `--fresh` the refusal is unchanged. Because the removal gate has no
  comparison basis in that case, the warning says so: documents already
  published for files that are now gone cannot be identified from an unreadable
  plan and are not deleted.
- **Refusal and skip listings are capped at ten entries plus an "and N more"
  tail.** Unmounting a bind mount under an indexed root vanishes every content
  group at once, so the uncapped listing was one rendered entry per journalled
  file. `--json` still carries every entry.

### Fixed

- **An index whose metadata will not parse is refused instead of silently
  reopened with an empty mapping**
  ([#202](https://github.com/xerj-org/xerj/issues/202)). `Index::open` treated
  "this file is not there" and "this file does not parse" as the same thing:
  `load_schema`/`load_settings` mapped ENOENT, EACCES, EIO and malformed JSON
  alike to one anonymous error, and the caller answered all of them with a
  fresh dynamic mapping. Measured on a field deliberately mapped `keyword`:
  after `schema.json` was truncated to half its bytes the index still opened
  `Ok` with `field_count = 0`, the field came back `None`, and one further
  document re-inferred it as `long` — a mapping silently replaced by a
  different one, with nothing in any response saying so. Absent stays absent
  (indices predating create-time schema persistence legitimately have no
  `schema.json`); present-but-unparseable now fails the open with an error
  naming the file. `es_mapping.json` — the full-fidelity mapping behind
  `GET /_mapping` — was logged-and-ignored on a parse failure and now fails the
  index too, on the boot scan, on snapshot restore and on a retry.

  **Visible on upgrade:** a node whose data dir already holds a corrupt sidecar
  now boots **red** with that index unserved, where it previously came up green
  with an empty mapping. It joins the failed set
  [#206](https://github.com/xerj-org/xerj/issues/206) introduced, so every
  surface that set feeds already reports it — measured on a node holding two
  user indices, `victim` with a truncated `schema.json`:

  ```
  GET  /_cluster/health          -> red, unassigned_primary_shards: 1
  GET  /_cluster/health?level=indices
                                 -> victim red, unassigned_info.details = the
                                    absolute path of schema.json and the parse error
  GET  /_cat/indices             -> "red open victim <uuid> 1 0 0 0 0b 0b 0b"
  GET  /_cluster/indices/failed  -> the reason, plus the retry and delete calls
  PUT  /victim, POST /victim/_doc, POST /victim/_search
                                 -> 503 no_shard_available_action_exception
                                    carrying that same reason
  GET  /health/ready             -> 200 "ready (degraded): 1 of 2 indices
                                    serving, 1 failed to open; see GET
                                    /_cluster/indices/failed"
  POST /healthy/_search          -> 200
  ```

  The readiness counts above are the two-index test router's. A running node
  also holds its internal `.xerj_*` indices, so the same condition on the shipped
  binary reads `ready (degraded): 15 of 16 indices serving, 1 failed to open` —
  the 200 and its meaning are unchanged, only the totals move with what else the
  node holds.

  Recovery is the three doors #206 added. Two were measured end to end on the
  shipped binary: repair the file and
  `POST /_cluster/indices/failed/{index}/_retry` (503 while it is still torn,
  `200 {"reopened": true}` once it is not, health back to green), and
  `DELETE /{index}` (200, directory removed, health back to green). The third —
  restore the index from a snapshot — is the path this change touches (a
  successful restore now clears the recorded failure, which it has to or health
  would stay red after the repair worked); it is covered by an engine-level
  test rather than measured over HTTP. The node stays in kubelet rotation as
  long as any index is still serving.

- **Concurrent sidecar writes can no longer manufacture the torn file the reader
  now refuses.** `write_file_atomic` staged every write in
  `path.with_extension("tmp")` — one shared name for all writers of that file —
  so two concurrent writers of one sidecar (`PUT /{index}/_settings` racing
  another settings update, two API-key mints for `api_keys.json`) both opened it
  `O_TRUNC`, interleaved their bytes, and each renamed it into place. The rename
  is atomic; the content being renamed was not one writer's. Measured with the
  old shared name, 4 threads × two different-length settings bodies × 200
  rounds, over four runs: **86–294 of 800 writes failed with ENOENT** (the loser
  renaming a file the winner had already moved) and, with those errors swallowed
  as the callers did, **1–16 of 200 rounds left an unparseable `settings.json`**.
  The counts are race-dependent and vary with machine load; what does not vary is
  that both are non-zero on every run with the shared name and **0 and 0** on
  every run with a unique staging name. Each write now stages
  in its own sibling file (`<file>.tmp.<pid>.<seq>`) in the target's directory
  and removes its own debris on failure — which for `api_keys.json` is key
  material. `update_settings` also writes `settings.json` through
  `write_file_atomic` rather than a plain `fs::write`; it was the last
  non-atomic sidecar writer left.

- **`index` on the mapping is now honoured instead of silently ignored**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). `"index": false` was
  accepted by `PUT /{index}`, echoed back verbatim by `GET /{index}/_mapping`,
  and then had no effect at all: the field kept a full inverted index and
  `match`, `term`, `match_phrase`, `prefix` and `wildcard` against it all
  returned the document, where Elasticsearch answers 400. It is the sibling of
  the `doc_values` fix in rc.12 and follows its shape.

  The line is drawn where ES draws it. Since 8.1, `"index": false` on a
  keyword / numeric / date / boolean / ip field keeps the doc-values column and
  stays queryable — `MappedFieldType.isSearchable()` is "has postings **or** has
  doc values" — so those queries keep working here, unchanged. Only a field with
  neither (a `text` field, or anything with an explicit `"doc_values": false`)
  is unsearchable, and a query naming one is now rejected with ES's own
  sentence, `Cannot search on field [f] since it is not indexed nor has doc
  values.`, as a 400 `search_phase_execution_exception` /
  `query_shard_exception`. The check lives in one place — `search_inner` — and
  these surfaces were each measured reporting the rejection rather than
  absorbing it, including the three that were swallowing it: `_search`,
  `_count` on all four selectors (single index, `logs-*`, a comma list and the
  cluster-wide `POST /_count`), `_msearch`, `_msearch/template`, `_explain`,
  `_delete_by_query` and `_rank_eval`. `_explain` answers the 400 instead of a
  confident `"matched": false` for a query it never ran; `_msearch` and
  `_msearch/template` render it as a per-response 400 envelope with `type` and
  `root_cause`, leaving the sibling sub-requests in the batch untouched; and
  `_rank_eval` records the refused request under `failures` instead of dropping
  it from `details` *and* `failures`. It still publishes a `metric_score`, and
  that number is still the mean over the requests that actually ran — which is
  ES's behaviour too — but the batch no longer shrinks silently underneath it:
  a two-request batch in which one is refused now answers `details: {good}`,
  `failures: {bad}`, `metric_score: 1.0` (measured), so a relevance gate reading
  a clean score can see what did not contribute to it.

  A multi-index `_count` follows ES's broadcast rule rather than failing
  outright: the failure status is returned only when **no** index answered
  (`RestStatus.status`, semantics only), so `/t/_count` and a `logs-*` whose
  indices all carry the offending mapping both 400 exactly as `_search` does,
  while a mixed selector returns 200 with the partial count and the refusal
  visible as `_shards.failed: 1` plus a `_shards.failures` entry carrying the
  `query_shard_exception`. What it will not do any more is what it did before:
  report `count: 0` with `"successful": 2, "failed": 0`.

  `exists` is unaffected and still returns zero hits rather than an error, which
  is also what ES does.

  The multi-field query types follow ES's own split rather than a blanket rule.
  An explicitly *named* field is rejected like any other named field —
  `multi_match` / `simple_query_string` / `query_string` with
  `"fields": ["note"]`, and `query_string` with `"default_field": "note"` —
  while a *wildcard* spec such as `"fields": ["*"]` is never an error, because
  ES expands a pattern over the searchable fields and silently drops the rest
  (`QueryParserHelper.resolveMappingField`). Before this, the same user intent
  got two different answers: the `simple_query_string` form was rejected (the
  parser lowers it to a `match`) while the `multi_match` form returned the
  document through the unsearchable field.

  Reaching the `query_string` half of that meant fixing a second
  accepted-and-ignored key in the same family: **`query_string`'s `fields`
  array was never read by the parser**, so `{"query":"x","fields":["title"]}`
  searched every field, and — once `index: false` became a real rejection —
  disagreed with `{"default_field":"title"}`, which is the same request written
  the other way. `fields` now selects the targets for a clause that named no
  field of its own (a `dis_max` over one leaf per field when there is more than
  one, `^boost` honoured) — the same job `simple_query_string`'s `fields` list
  was already doing. A clause that does name its own field keeps it, as in ES.

  The field is dropped from the full-text index at flush and merge only where
  the stored-doc fallback is *equivalent* — a non-indexed `text` field. An
  exact-typed field that kept its doc values keeps its whole-value postings,
  because the fallback scan splits on non-alphanumerics and would start matching
  `192.168.0.1` against `192.168.0.2`. So no byte-saving claim is made for this
  change: for those types the footprint the option implies is knowingly forgone
  rather than bought with wrong answers.

  Known gaps, measured and left open rather than claimed shut. **Aggregations
  and sort do not inherit the check**: only the `query` clause is walked, so
  `{"size":0,"aggs":{"a":{"terms":{"field":"note"}}}}` still answers 200 with a
  bucket built from `_source`, and `{"sort":[{"note":"asc"}]}` still answers 200.
  ES rejects those too, but for an unrelated reason and with an unrelated
  sentence — `Fielddata is disabled on [f] in [i]…`, which it raises for *any*
  `text` field whether or not `index: false` was declared — so reusing this
  error there would misreport a wider divergence as this one. **The field-less
  arms of the stored-document scan are schema-free**, walking every `_source`
  key, so a token living only in an unsearchable field can still match when
  **no** field is named: `query_string {"query":"…"}` and `more_like_this` with
  no `fields` both still return the document. (Their *named* forms are
  rejected — `more_like_this` with `fields` lowers to a `bool.should` of
  `match` at parse time and inherits the check.) **A wildcard `fields` spec
  reaches that same field-less arm**: `simple_query_string` / `query_string`
  with `"fields": ["*"]` lowers to the `"*"` placeholder and is answered by the
  schema-free scan, so it can still match through an unsearchable field. It is
  never an *error*, which is ES's rule and deliberate — but it is not "answered
  over the searchable fields" either, and that is a pre-existing property of
  wildcard expansion, identical on `main`. (`multi_match` with a pattern is
  different: it is answered by the FTS projection, which does apply
  `isSearchable()`, and finds nothing in an unsearchable field.) **The opaque
  `query_string` fallback still ignores a multi-field `fields` list**: a query
  string the parser declines to lower keeps the whole string for the FTS path,
  where a one-element `fields` is carried across as `default_field` and a
  longer list has nowhere to go. And **`_validate/query` never opens the
  index** — it answers from the parse alone, so it reports `{"valid": true}`
  for a body `_search` refuses. That too is unchanged from `main`, but it now
  disagrees with `_search`, so it is written down rather than left to be
  discovered. Every one of these gaps is pinned by an assertion in
  `index_false_is_honoured::documented_gaps_are_still_open_and_still_documented`,
  so closing one fails the test that says it is open.

### Added — incremental `--no-graph` corpus reconciliation

- **`xerj autoindex --no-graph` now reconciles a changed folder instead of
  re-deriving it.** A `--no-graph` run commits a durable *corpus generation*: a
  manifest of content groups (stable group identity, content digest and byte
  length, canonical path plus aliases, dataset assignment, validated output
  counts) plus a sealed source snapshot of the prepared records. A later run
  projects the current inventory onto the committed manifest and publishes only
  the difference — additions, content changes, deletions, renames, junk
  transitions and duplicate-alias moves all converge, and a no-op re-run issues
  no data bulk and appends no new generation. A run interrupted mid-generation
  replays from its own sealed snapshot, so the result never depends on whether
  the source tree changed after the crash. Dataset and mapping identity is
  frozen at generation 1: a file that would need a *new* dataset or a mapping
  change is refused before any remote mutation rather than silently widening
  the schema.

  Scope, plainly: only `--no-graph`. Graph-enabled runs are unchanged, and each
  changed generation still copies and prepares the full corpus (O(N) staging) —
  the win is in what gets published and verified, not yet in what gets read.
  `--snapshot-max-gb` (default 64) caps the logical staged payload; it is a
  payload budget, not a disk-space or peak-memory guarantee.

  Original work by Leonid Bugaev (@buger).

- **Junk keeps the contract it always had on the generated path: recorded,
  never fatal.** A file that cannot be read or recognised (`plan.junk_files`)
  and a record that no dataset assignment accepts (`stats.junk`) both used to
  abort a `--no-graph` run outright — the first with
  `plan has no assignment for <file>`, the second with
  `durable preparation of <file> produced N junk records`. A single zero-byte
  file was enough to make a whole folder unindexable. Both are now counted
  instead: the sealed prepared artifact carries its junk-record count, the
  manifest group carries it into the generation, and the catalog's per-file
  `junk` and run-level `junk_records_total` report it instead of a hardcoded
  zero. Such a run exits **3** ("completed with junk"), the same signal the
  legacy path publishes — the generated path previously returned a flat `0`
  and lost it.

- **`--dry-run` is honoured on an already-generated state directory.** It was
  evaluated only after the pending-replay and reconcile branches had already
  published and committed, so on any state directory past generation 1 the flag
  was accepted, silently ignored, and the destination mutated. It is now decided
  before either branch: it prints the plan a real run would act on — the
  projected reconcile plan, or the sealed plan of a pending generation — and
  returns without opening the journal for write, without snapshot GC, and
  without a single bulk request.

### Changed — `autoindex --fresh` is refused on a durable generation

- **`--fresh` no longer silently destroys a committed corpus generation.**
  `--fresh` deletes the resume journal, and the snapshot GC that follows every
  journal open then sees an empty protected set and removes every sealed
  snapshot — so on a generated state directory `--fresh` would discard the
  committed manifest, the pending replay evidence, and the alias/path/
  stale-record knowledge, while cleaning nothing at the destination. It is now
  refused up front, naming the generation that blocks it, and pointing at the
  plain re-run (which reconciles the change) or at an isolated rebuild with a
  new `--state-dir` and `--prefix`.

  **Unchanged for everyone else.** On a legacy (non-generated) journal —
  including every graph-enabled corpus and anything `xerj brain` writes —
  `--fresh` behaves exactly as before: it ignores the journal and restarts,
  which is what `xerj brain`'s documented self-heal for a wiped data directory
  depends on.

### Migration — pre-generation `--no-graph` state directories must be rebuilt

- **A `--no-graph` state directory written before this release cannot be
  adopted as generation zero, and the first `--no-graph` run against it will
  refuse with a followable rebuild command.** This is a deliberate, versioned
  format boundary (`sync::GENERATION_FORMAT_VERSION = 1`), not a validation
  failure: a pre-generation resume plan records no stable group identity, no
  content byte length and no validated per-group output counts, so no amount of
  evidence in it could reconstruct a manifest group. The refusal prints the
  exact `xerj autoindex` argv for an isolated rebuild into a new `--state-dir`
  and `--prefix`; the old target and the shared `autoindex-catalog` are left
  alone and require explicit, validated cleanup once the new target is
  verified. Nothing is migrated in place and no destination data is touched by
  the refusal. Graph-enabled state directories are not affected.

## [1.0.0-rc.14] - 2026-08-10

### Changed

- **One documented core and memory policy, and the worker knobs are live**
  ([#240](https://github.com/xerj-org/xerj/issues/240)). XERJ had no single
  answer to "how much of this machine may I take?", and the scattered answers it
  did have disagreed with each other — and, on Darwin, with the OS.
  `engine.flush_workers`, `engine.merge_workers` and `engine.search_workers`
  were documented but inert; they now drive real pool widths, and a value of `0`
  refuses startup instead of being silently reinterpreted. `--bulk-mb` above 24
  and `--workers 0` are likewise refused rather than clamped, so a request the
  engine will not honour fails loudly. Only hand-written configs are affected:
  the shipped `xerj.default.toml` has no `[engine]` section.

- **One 16 MiB file no longer costs 65 GB of RSS**
  ([#239](https://github.com/xerj-org/xerj/issues/239)). `split_sections` was
  quadratic in both time and memory: a 16 MiB text file with no blank line
  needed **65.6 GB of peak RSS and 16.3 s** to produce 16.8 MB of sections.
  Now linear — the same input takes **8.3 ms and 35 MB**.

- **BREAKING — `server.bind_address` now defaults to `127.0.0.1`, and a
  cleartext node refuses to publish itself to the network**
  ([#228](https://github.com/xerj-org/xerj/issues/228)). The old default was
  `0.0.0.0` while TLS is off by default, so an out-of-the-box node accepted its
  admin API key over plain HTTP on every interface the host had — verified on a
  stock boot, where `curl -H "Authorization: ApiKey …" http://<lan-ip>:9200/…`
  answered `200` and the same URL over `https` had no listener to hand shake
  with. Auth being on did not help: the credential *is* the thing on the wire.

  A fresh node is now reachable from its own host and nowhere else. Exposing it
  is two statements, not zero: set `server.bind_address` (or `--bind` /
  `XERJ_BIND_ADDRESS`), and — while `tls.enabled = false` — also set
  `server.allow_insecure_network_bind = true` (env
  `XERJ_ALLOW_INSECURE_NETWORK_BIND`). Without the second, startup exits
  non-zero before the data directory is created or a first-run admin key is
  minted. `--insecure` does not evade it; it clears `tls.enabled`, which is
  exactly what the check keys on. Same fail-closed shape as
  [#229](https://github.com/xerj-org/xerj/issues/229), whose TLS-on gRPC check
  is untouched and cannot be relaxed by this opt-out.

  **Upgrading:** a deployment that relied on the old default becomes
  unreachable from other hosts until it sets both; one that already wrote
  `bind_address = "0.0.0.0"` without TLS now fails to start with a message
  naming the setting that unblocks it. The shipped Docker image, compose file
  and Helm chart set both, because a container's network namespace is the
  boundary and its published port is where TLS belongs.

  Two documentation claims that were false as written are now true: the CLI and
  security pages both said `--insecure` "refuses to run with a non-loopback
  bind", and it did not — a `--insecure` node on `0.0.0.0` accepted an
  unauthenticated `PUT /leak/_doc/1` from the LAN (`201`). The startup banner
  also prints the bind address on every listener line, so a loopback node and a
  world-facing one no longer look identical.

  A `bind_address` that is not an IP literal is rejected first, ahead of both
  exposure checks, with the fault it actually has. Host names have never been
  resolved — the pre-#228 code parsed `"{bind}:{port}"` as a socket address and
  failed on them too — but the exposure predicates fail closed on anything they
  cannot parse, so without that ordering `bind_address = "localhost"` was
  refused with a message asserting that localhost "is not loopback" and would
  "serve plain HTTP on a network-reachable interface", and pointed at an opt-out
  that fixed nothing: setting it let the boot run on to the bind and fail there
  instead, after the data directory, the `.xerj_*` system indices, the master
  key and a printed first-run `admin.key` already existed.

### Security

- **API key secrets are no longer stored in the clear**
  ([#201](https://github.com/xerj-org/xerj/issues/201)). `api_keys.json` held
  the secret verbatim. 0600 plus atomic rename is right for a secret a *process*
  owns, but a file has more readers than a process: a backup, a snapshot, a
  container layer, a support bundle, a decommissioned disk. Secrets are now
  stored as a salted SHA-256 (`$ssha256$<salt>$<digest>`) and compared in
  constant time, with existing plaintext records migrated on load. The fast hash
  is deliberate: the secret is two v4 UUIDs — 244 bits of CSPRNG output, never
  human-chosen — so offline guessing is out of reach whatever the hash costs,
  while this comparison runs on *every authenticated request*, where an Argon2id
  would add tens of milliseconds per request and hand out a free CPU-exhaustion
  DoS. The same release makes `_security/_authenticate` report the key's real
  identity instead of a hardcoded `superuser`, and gives the audit chain durable
  storage.

- **The node refuses to start when TLS is on but gRPC would answer in cleartext
  off-loopback** ([#229](https://github.com/xerj-org/xerj/issues/229)).
  `tls.enabled = true` encrypted two of the three data-plane listeners; the
  third never participated, because the gRPC server is built without tonic's
  `tls` feature. Nothing surfaced it — gRPC clients connected and worked
  identically either way — so an operator who enabled TLS and bound a network
  interface got no error, no failed handshake and no symptom, while API keys and
  document bodies crossed the network in the clear. Auth was never the gap;
  confidentiality was. Boot now fails closed unless
  `tls.allow_insecure_grpc_h2c` says otherwise.

### Fixed

- **`multi_match` returns the same hit set before and after `_flush`**
  ([#218](https://github.com/xerj-org/xerj/issues/218)). The memtable and
  segment paths disagreed on multi-token semantics, so flushing an index could
  change which documents a query matched — verified live on a one-document
  index where an OR query returned 0 hits before `_flush` and 1 after, with ES
  returning 1 throughout.

- **Dynamic string fields get Elasticsearch's default `.keyword` multi-field**
  ([#209](https://github.com/xerj-org/xerj/issues/209)). Dynamic mapping
  inferred every string as bare `text`, so `GET _mapping` never showed a
  `.keyword` sub-field and `_field_caps` never listed one. Anyone arriving from
  Elasticsearch — or any tool that reads `_field_caps` for field discovery,
  Kibana included — could not see that `category.keyword` existed.

- **`min_score` is honoured at `size:0`, and an external scalar N keeps its
  ghosts** ([#193](https://github.com/xerj-org/xerj/issues/193)). A
  `size:0 + min_score` body never materialised a hit, so the threshold had
  nothing to filter and `hits.total` reported the raw match count — measured at
  **65** where the same query at `size:100` counted **5**. The tie-break
  comparator item from that issue is deliberately not included here; it belongs
  with [#191](https://github.com/xerj-org/xerj/issues/191).

- **Index blocks can be removed, `read_only_allow_delete` means what its name
  says, and a blocked write answers 403 rather than 500.** Three compounding
  defects: `PUT /_settings` wrote only the display-side map while enforcement
  read the per-index `settings.json`, so a block was acknowledged, invisible to
  `GET /_settings`, and — with `_block` registered PUT-only — removable only by
  restarting with a hand-edited file. The one block that fires automatically did
  the inverse of its name, and clients hitting any of it were told the server
  had faulted.

- **`llms.txt` no longer tells agents six things the binary does not do.** The
  "Running an index for a human" section is executed, not just read: an agent
  follows it literally and then reports the result to its user. Corrected: the
  job-size line is not the first line printed; `--dry-run` does not write
  *nothing* (it creates and locks the state dir and appends a journal record);
  a dry run reuses a frozen plan instead of re-costing the job, so it is not a
  floor for the real run; exit `1` is the catch-all for every error, not
  "endpoint unreachable"; `status` needs the same explicit `--state-dir` the run
  used; and `--workers`/`--pdf-workers` bound the client-side extractor, not the
  server (with `--pdf-workers` capped to 1–4).

- **A failed index can now be inspected, deleted and retried without stopping
  the server** ([#206](https://github.com/xerj-org/xerj/issues/206)). An index
  directory that refused to open at boot was recorded in a map that only
  `Engine::health` ever read. Live-reproduced with a corrupt `snapshot.json`:
  it was absent from `_cat/indices` and `_cluster/state`, `DELETE /{index}`
  answered `404 index_not_found` and left the bytes on disk, and the only
  recovery was to stop the server and edit the data directory by hand. A
  failed index is now a real state: listed as `red` in `_cat/indices`, present
  in `_cluster/state` as an `UNASSIGNED` primary with `ALLOCATION_FAILED` and
  the verbatim open error, enumerated with its reason by
  `GET /_cluster/indices/failed`, reopenable via
  `POST /_cluster/indices/failed/{name}/_retry` once the cause is fixed, and
  deletable through the ordinary `DELETE /{index}`. Searches, writes and
  creates against it return `503 no_shard_available_action_exception` carrying
  the reason instead of a `404` that claimed it did not exist, while the
  metadata surfaces — `GET /{index}`, `HEAD /{index}` (the `indices.exists()`
  every ES client calls), `_mapping`, `_settings`, `_cat/indices` — report it
  as an index that exists, the way ES answers those from cluster metadata
  rather than from a shard. Before this, `HEAD` said the name was free and the
  following `PUT` said it was unavailable.

  Two related defects in the same report are fixed with it. **Readiness no
  longer hard-fails on a partly degraded node** — `/health/ready` returned
  `503` for any red status, so one broken index pulled a pod holding 200
  healthy ones out of service permanently; it now reports `200 ready
  (degraded)` while the node can still serve something and `503` only when
  every index it holds failed to open. And **`_cat/health` no longer prints a
  hardcoded `green`** — the one health surface an existing ES dashboard points
  at was the one that could not report a broken node; it and `_cluster/health`
  now count an unopenable index as an unassigned primary, agreeing with
  `/v1/health` and `/v1/cluster/health`.

  Because `red` is now reachable on `_cluster/health` for the first time, the
  two conditions that gate on it are consulted rather than assumed:
  `wait_for_active_shards=all` and `wait_for_status=green|yellow` both answer
  `408` with `timed_out: true` on a red node instead of `200 timed_out: false`.
  `GET /_cluster/health?wait_for_status=green&timeout=30s` is the bootstrap gate
  every docker healthcheck, CI wait loop and Kibana startup uses; a red node
  used to sail straight through it. A green or yellow cluster is unaffected.

- **A `DELETE /{index}` that could not remove the bytes no longer strands the
  index.** The open-index path pulled the handle out of the engine before
  `remove_dir_all`, so a removal that failed (read-only mount, `EACCES`) freed
  the name while the directory survived: no handle, no failed-index entry,
  nothing on `_cat/indices`, and the next `DELETE` answered `404` — the same
  dead end as [#206](https://github.com/xerj-org/xerj/issues/206), reached from
  the other side. The handle is now restored on failure, so the index stays in
  service and addressable and the operator can retry the delete once the cause
  is fixed.

- **Index templates, ingest pipelines, data streams and ILM policies survive a
  restart** ([#203](https://github.com/xerj-org/xerj/issues/203)). Index
  templates, legacy (v1) templates, component templates, ingest pipelines, data
  streams and ILM policies lived only in memory: `PUT /_index_template/logs`
  answered `{"acknowledged": true}` and `GET` answered 404 after the next
  restart, and the next index that should have matched the template was created
  without it — with no error anywhere. All six are now persisted together in
  `<data_dir>/cluster_state.json`, written atomically (tmp → fsync → rename →
  fsync the parent directory) so the write is committed at the request, not in
  a shutdown hook, and restored in `Engine::new` before the listeners come up.
  A write that fails is rolled back in memory and answered with a 500 instead
  of `acknowledged`. Restored pipelines are recompiled, not just re-read —
  storing the definition alone would have left `?pipeline=x` accepted and
  silently inert after a restart.

  Four defects found while verifying it, each reproduced first:

  - Concurrent management writes corrupted each other (all flushes staged
    through one fixed `.tmp` path — 32 parallel template PUTs returned
    `store_exception`). Snapshot-and-write is now serialized.
  - A rollover interrupted between "backing index created" and "generation
    persisted" wedged the data stream forever: every later rollover recomputed
    the same name and got `409 resource_already_exists_exception`. Boot now
    adopts the highest generation actually present on disk, warns, and persists
    the repair once.
  - A `cluster_state.json` that could not be **read** (EACCES after a uid
    change on a container volume, a backup tool's chmod, EIO) came up as empty
    maps, and the next management write renamed a snapshot of those empty maps
    over a file whose bytes were perfectly good. The load failure now latches:
    every management mutation is refused with a 500 naming the file, and the
    file is not touched, until a boot loads it cleanly. The corrupt-parse arm
    refuses too. The later format-compatibility fence above tightens that
    diagnostic boot further: it creates no `cluster_state.corrupt.json` copy.
  - `DELETE /_data_stream/<name>` recorded the removal before destroying the
    backing indices, so a crash in that window stranded `.ds-<name>-00000N`
    directories that no data-stream API could reach — GET and DELETE answered
    404 while `PUT /_data_stream/<name>` answered
    `409 resource_already_exists_exception` permanently. The order is reversed:
    the removal is recorded only once every backing index is gone, and a
    backing index that cannot be deleted now aborts the DELETE with a 500
    (it was previously swallowed and answered `acknowledged`) so the operator
    can retry. Read that 500 as "did not finish", not as "nothing happened":
    a multi-generation stream still loses every backing index that *could* be
    destroyed, the stream itself stays addressable, and the retry after the
    cause is fixed completes the delete.

  **Known gaps, deliberately not closed here.** `.ds-*` indices that no data
  stream claims — left by a data dir written before this change, or by a
  DELETE interrupted by an older build — are **not** cleaned up or adopted
  automatically; boot now names them in a warning and the recovery is
  `DELETE /<backing-index>` per index, because an orphan may hold the only copy
  of real data. And `aliases.json` still has both swallow shapes that were
  fixed here for `cluster_state.json`: an unreadable aliases file is swallowed
  at boot and overwritten by the next alias write, and a *failed* alias write
  is only logged, so an alias can survive on disk after the stream it named was
  deleted with a 200. Both are pre-existing behaviour, not introduced here or
  made worse here, and are tracked separately.

- **`autoindex` no longer leaves immortal catalog entries for skipped files**
  ([#238](https://github.com/xerj-org/xerj/issues/238)). A file added after the
  resume plan was frozen is skipped and reported in the catalog — but that
  report was written from a per-run `Vec` that nothing durable remembered, so
  once the file left the corpus no run could ever remove its `file:{key}`
  document. The catalog is the data map every `map`, `status` and agent query
  reads, and it kept advertising files that were gone. Skipped files are now
  recorded in the durable plan and swept from the catalog when they leave the
  corpus. The sweep is safe to do completely because a skipped file is never
  indexed and never enters the graph corpus — that one catalog document is its
  entire live footprint. Ordering is deliberate: the plan entry is added only
  after the document is written and dropped only after the delete has landed,
  so a failed catalog bulk is retried by the next run instead of stranding the
  document. This does **not** implement add/change/delete reconciliation for
  *indexed* files, which remains open.

- **Repeated `autoindex` scans keep agent-facing map metadata durable.**
  Dataset source bytes, parser-junk counts and coercion-drop notes now derive
  from committed per-file journal records instead of invocation-local worker
  counters, so an unchanged resume does not overwrite them with zero. Run
  timestamps now distinguish invocation start (`started`) from summary
  generation (`summary_generated_at`), and `junk_records_total` on the run
  document is the same durable sum the per-dataset `junk_records` reports
  rather than an invocation-local counter that read `0` after a no-op resume.
  Existing catalogs whose historical `started` field was dynamically mapped
  as text remain usable: the additive mapping upgrade no longer attempts an
  incompatible text-to-date type change. Concretely, this is catalogs written
  by **v1.0.0-rc.4** — the one release that had `autoindex` but not yet
  dynamic ISO-date inference (added in rc.5). Measured against a live engine,
  adding `started` as `date` to such a catalog is refused 400
  `mapper_parsing_exception` (*"field [started] already exists as [text],
  cannot add [date]"*), which aborts the invocation before any document work.
  Catalogs written by rc.5 or later already inferred `started` as `date` and
  were never affected.

  Three consequences worth knowing before you upgrade:

  - **`bytes` is redefined.** It used to be the bytes the latest invocation
    processed, credited to the first dataset a file was assigned to. It is now
    the complete canonical source size, counted once in **every** distinct
    dataset that source is assigned to. Per-dataset values therefore get
    larger, and summing `bytes` across datasets can exceed the corpus size
    when one source feeds several datasets — exactly as one source already
    contributed to several dataset-local file counts. It is a dataset-local
    source footprint, not a partition of physical storage.
  - **Journals written before this release carry no per-dataset coercion
    record**, because the new `dropped_by_dataset` field defaults to empty
    when an older journal is replayed. Byte and junk counts reconstruct fine
    (their journal fields already existed), but a dataset whose files are all
    unchanged loses its `N field values dropped by coercion` note on the first
    resume after upgrading, and only regains it once one of its files is
    reprocessed. Re-run with `--fresh` if the note matters more than the
    rescan.
  - **`junk_records_total` is narrower than the number it replaces**, and one
    class of failure has left it. It used to be an invocation counter that
    also folded in records the *backend* refused per bulk item; it is now
    parser junk only, so `xerj autoindex map`'s header counts exactly what the
    per-dataset `junk_records` values count. No signal is lost in practice: a
    single per-item rejection already aborts the run with
    `autoindex stopped with bulk/backend failures`, before any run document or
    map is written, so the folded-in number was never observable in a map.
    That abort message now also states how many records were refused, which
    the first-five error sample could not convey.

- **The installer is now fail-closed on checksum *verification*, not just on
  checksum *download*.** `landing/get` printed
  `warning: sha256sum/shasum not found — skipping checksum verification` and
  installed anyway — which is not what "refusing to install an unverified
  binary", three lines above it, implies. It now searches `sha256sum`,
  `shasum`, `openssl`, `sha256`, `busybox` and `cksum`, and stops if none is
  present. A machine with no hashing tool at all can still proceed, but only by
  setting `XERJ_INSECURE_SKIP_CHECKSUM=1` — an explicit choice, made by the
  user, not a silent default. The downloaded checksum is also validated as 64
  hex characters before use.

- **`XERJ_LIBC=gnu` makes the glibc Linux builds reachable from the one-line
  installer again.** `landing/get:61` unconditionally overrode the target with
  `unknown-linux-musl` after the `case` arm had set `unknown-linux-gnu`, so the
  script could never request a linux-gnu asset — while the code read as though
  it could. musl remains the deliberate default (static, no glibc-version
  floor, and xerj links jemalloc so musl's allocator is not in the path), and
  the comment now says so; `XERJ_LIBC=gnu` opts into the glibc build.

### Added

- **`autoindex` reports real progress, an honest percent and an ETA — no run is
  silent** ([#241](https://github.com/xerj-org/xerj/issues/241)). Between 47%
  and 64% of a typical run produced no output at all, which is the worst
  possible behaviour for the two audiences that matter: a person watching a
  laptop they cannot use, and an AI agent that has gone quiet on that person's
  behalf. Progress now covers phase, items done and total, rate and ETA, in both
  modes — a terminal gets a live redrawn line, a pipe gets periodic structured
  `xerj-progress` lines an agent can parse, and every run ends with a single
  `xerj-done` summary. The percent is bytes-based so a large file cannot make it
  stall at 99%, the first ETA is withheld for five seconds and labelled `rough`
  until there is enough evidence for it, and the line names the file currently
  being waited on. `--quiet` still produces exactly nothing on either stream,
  and `--progress none` is honoured everywhere — the resource policy's own
  startup notes were moved onto the same surface so they cannot leak into
  `--progress json`'s single parseable stream.

- **`/get` and `/get.ps1` are counted.** `functions/get.js` (a Cloudflare Pages
  Function) serves the installer straight out of `landing/get`, unchanged, and
  records the request. The install path had never had a counter of any kind —
  `curl` runs no JavaScript, so no page beacon could ever observe it, and the
  number of installs was simply unknown. It records timestamp, country,
  User-Agent and a coarse OS guess; it records **no IP address, no cookie and
  no identifier of any kind**, so two installs by one person cannot be
  distinguished from two installs by two people. It is fail-open: if the
  storage binding is missing or the write throws, the installer is still
  served. **No telemetry was added to the xerj binary, and none is planned.**

- **`metrics/release-downloads.jsonl` + a daily GitHub Action.** GitHub reports
  release `download_count` as a running total and keeps no history, so the one
  uninflated adoption number this project has could be read but never trended.
  One committed line per day turns it into a series. `scripts/adoption-snapshot.sh`
  prints the funnel on demand, including which repo-level numbers are
  contaminated and why.

## [1.0.0-rc.13] - 2026-08-08

### Security

- **All 13 open Dependabot advisories against `engine/Cargo.lock` closed**
  (#220). Twelve by lockfile-only version bumps — openssl 0.10.81 /
  openssl-sys 0.9.117 (8 advisories: GHSA-8c75-8mhr-p7r9, GHSA-ghm9-cr32-g9qj,
  GHSA-hppc-g8h3-xhp3, GHSA-pqf5-4pqq-29f5, GHSA-xp3w-r5p5-63rr,
  GHSA-phqj-4mhp-q6mq, GHSA-xv59-967r-8726, GHSA-xmgf-hq76-4vx2),
  rustls-webpki 0.103.13 (GHSA-82j2-j2ch-gfr8), quinn-proto 0.11.16
  (GHSA-4w2j-m93h-cj5j), rand 0.8.7 (GHSA-cq8v-f236-94qc), webauthn-rs 0.5.5
  (GHSA-22w3-693w-x895) — and one by removal: `protobuf` is gone from the
  dependency graph entirely (GHSA-2gh3-rmm4-6rq5; `prometheus` now builds
  without its protobuf feature — XERJ only ever served the Prometheus text
  exposition format, and two new tests pin that format). A reachability
  analysis found none of the 13 exploitable in a default deployment; closed
  anyway.
- npm developer-tooling bumps (#194): basic-ftp 5.3.1, ip-address 10.4.0,
  js-yaml 4.3.1, ws 8.21.3 — transitive from puppeteer, never part of a
  release artifact.

### Changed

- **The Helm chart now defaults to secure mode** (#213). It shipped
  `insecure: true` — TLS and auth off — while the `/get` installer shipped
  auth on. The default is now secure, and a new `NOTES.txt` states at install
  time exactly what insecure mode turns off.

### Documentation

- Roadmap to 1.0.0 GA from a source review against the user-feedback corpus
  (#212).
- Install one-liner moved first on README, xerj.org hero and llms.txt, plus a
  paste-to-your-agent install prompt (#219).

## [1.0.0-rc.12] - 2026-08-07

### Changed

- **`doc_values` on the mapping is now honoured instead of silently ignored.**
  `"doc_values": false` was accepted, echoed back by `GET _mapping`, and had no
  effect — aggregating and sorting on such a field succeeded, where
  Elasticsearch errors. The doc-values sidecar builder was schema-free and filed
  a column for every `_source` string it saw. The default now follows ES: no
  doc-values for `text`, `annotated_text`, `match_only_text`,
  `search_as_you_type`, `semantic_text`, `binary`, `object` and `nested`; an
  explicit `"doc_values": true` on a text field still builds the column and
  still returns whole-value buckets.

  Measured on a 500k-doc corpus (one analyzed `text` field), force-merged: index
  154,413,167 → 99,424,772 bytes (**1.553x smaller**, `index/raw` 0.433x →
  0.279x) and force-merge 139.13 s → 59.69 s (**2.33x faster**, merge re-encodes
  the sidecar and there are 55 MB fewer bytes to build). The `.dv` artifact falls
  95.9%; every other artifact is byte-identical. A `terms` aggregation on a text
  field got **1.34x faster** (3414 → 2547 ms) after losing its column, because
  walking a column of whole document bodies was slower than the brute path it
  falls back to.

  The saving is corpus-dependent: it removes 90-99% of the doc-values sidecar,
  and that sidecar is 2-37% of index size depending on how large text bodies are
  relative to `_source`. On source-code corpora where `_source` dominates it is
  nearer the low end.

### Performance

- **The per-segment FTS reader is cached instead of rebuilt on every query.**
  `FtsIndexReader::open` sat inside the per-segment search loop and performed two
  zstd decompressions per field — the whole `.post` blob and the whole `.meta`
  array — into owned buffers, then discarded them. Measured on one production
  field: ~50 ms and ~41 MB per (segment, field), per query.

  Readers are now cached per (segment, field-set), charged against the segment
  hydration budget under a new `fts_reader` category (visible in
  `_nodes/stats`), and evicted at merge completion. Segments are immutable, so
  no invalidation is needed; if the budget refuses the charge the reader is
  returned uncached, so an oversized corpus degrades to the previous behaviour
  rather than growing without bound.

  Measured p50 on 500k docs with the query cache disabled: `match` on text
  166.29 → 57.85 ms (2.87x), 500-doc page 168.96 → 64.05 ms (2.64x), `prefix`
  170.91 → 69.22 ms (2.47x), `wildcard` 180.52 → 74.79 ms (2.41x), `fuzzy`
  306.16 → 195.25 ms (1.57x), `match_phrase` 495.58 → 339.90 ms (1.46x).
  Brute-scan families (`function_score`, `boosting`) are unchanged — their cost
  is scanning and scoring, not opening a reader.

### Fixed

- **`xerj autoindex` no longer duplicates an index when an extractor improves**
  (#178). Datasets were inferred by comparing each file's whole field-name set,
  including names the extractor invents rather than reads from the file
  (`defs`, `symbols`, `symbol_count`, `title`, `page`, …). A better parser makes
  those names appear, which moved the file into a different dataset — and the
  dataset slug is an ingredient of every document `_id`, so the file was
  re-indexed under a new id in a new index while its previous document stayed
  behind, unreferenced. Measured on a 846-file Rust corpus, re-planning with an
  improved symbol extractor grew the index from 53,873 to 53,902 documents with
  29 stale leftovers and exit code 0; on a 2,328-file C corpus, 7,923 to 8,023.
  Clustering now uses only the field names that came from the file, so gaining
  or losing extractor fields can no longer re-home a document: both re-plans
  hold at 53,873 and 7,923.

  Nothing is deleted, so the first *re-plan* after upgrading (`--fresh`, a new
  `--state-dir`, or a lost journal) re-homes the files whose datasets merge and
  leaves their previous documents in place — 53,873 → 58,855 on that Rust
  corpus, a one-time cost paid once instead of once per extractor change. An
  ordinary re-run reuses the journal's frozen plan and is unaffected. Datasets
  inferred from real schemas are untouched: that corpus went from 9 datasets to
  7 (the two symbol-presence variants of one code dataset merged, likewise two
  section-presence variants of one prose dataset), and the C corpus from 407 to
  405, with every document accounted for in both.

## [1.0.0-rc.11] - 2026-08-04

### Added

- **`xerj autoindex` pins the server's embedding execution identity** (#160).
  Vectors produced by two different embedding backends — or by two different
  models, tokenizers, or vector widths within one backend — are not comparable,
  so mixing them in one index silently degrades every similarity result rather
  than failing. A semantic autoindex run now fetches
  `GET /v1/embedding/identity`, records the returned opaque digest in its
  journal before the index is created, and requires the same digest on every
  resume; a mismatch aborts before the next index-create or `_bulk`. There is
  no new command or setup step — `xerj autoindex ./reports` calls the endpoint
  itself over the already-configured `--url`, and plans with no semantic fields
  skip it entirely.

  The endpoint is authenticated (cluster-read) and deliberately opaque: it
  returns a version, the *effective* backend, a digest, the semantic contract,
  whether that backend is resumable, and a sanitized reason when it is not. It
  never returns credentials, provider URLs, model names, or local paths.
  `dimensions` is reported only for the backends whose width the server
  actually pins (`lexical`, and `onnx-experimental` at 384); it is omitted for
  `neural`, whose width comes from the loaded model's `hidden_size`, and for
  `proxy`, whose width is whatever the remote returns.

  `lexical` and `onnx-experimental` are resumable. `neural` and `proxy` are
  not: neither attests immutable model bytes, so a fresh semantic run is
  allowed but a resume is refused.

- **A detector that connects documents by what they say, not only by what they
  cite** (#164). Every shipped detector was structural — `wikilink`, `mdlink`,
  `href`, `pathcite` and `cratecite` need an explicit citation, `sequence` and
  `samedir` need a filesystem relationship — so a folder of PDFs or saved pages,
  where no document links to another, produced a graph whose only edges ran
  inside single documents and along directory chains. `sharedterm@1` emits a
  `shared_term` edge (weight 0.45, confidence 0.5) between two documents that
  use the same distinctive words, and carries those words as the edge evidence
  so a wrong link is inspectable exactly like a wrong `mdlink`.

  Measured on 240 arXiv PDFs filed in 24 topic folders (236 extractable), same
  binary, same corpus: before, 11,854 edges — 11,642 `sequence` inside single
  documents plus 212 `same_dir`, with 212 cross-document edges and **none**
  crossing a folder. After, 462 `shared_term` edges (271 further candidates
  refused by the density cap), 377 of them joining documents in different
  folders and 296 joining different top-level topics. Two independent runs
  produced identical counts and identical evidence.

  Density is capped rather than trusted: a term in more than 10% of the corpus
  is that corpus's vocabulary and is ignored, a link needs at least two shared
  word families (so a singular and its plural are not two signals), and no
  document takes more than 5 `shared_term` edges. What the cap refused is
  reported by the run as `edges_capped` instead of vanishing. A corpus of ten
  documents or fewer therefore produces no `shared_term` edges at all, which is
  the 10% rule being honest rather than an exception.

  The honest limit, which the docs and the console now state: this links
  documents that share VOCABULARY, not documents that share MEANING. Terms are
  compared as strings with no stemming and no model — XERJ's built-in embedder
  is lexical feature hashing and this detector does not use even that. Against
  the corpus's own topic filing, a random pair of those documents shares a
  top-level topic 17.0% of the time; `shared_term` links do so 35.9% of the
  time. Real signal, and a noisy one.

### Changed

- **BREAKING: an existing semantic `autoindex` state directory cannot be
  resumed** (#160). A journal written before this change carries no pinned
  identity, so there is no way to prove the server still produces the same
  vector space; rather than assume it, the resume fails closed with *"this
  semantic autoindex journal predates embedding identity pinning and cannot be
  resumed safely"*. The same path fires when a previous `--no-semantic` run is
  followed by a semantic run over the same state directory. Recover by
  rebuilding with `--fresh` and a new `--prefix`; before reusing the old
  prefix, delete and recreate its prior autoindex indices. Non-semantic runs
  and brand-new state directories are unaffected.

- `EmbeddingProxy::new` now rejects an endpoint that is not `http(s)` at
  construction time instead of at first request, so an invalid
  `embedding.default_endpoint` falls back to lexical at startup and
  `/v1/embedding/identity` reports `lexical` — the backend the server will
  really use — rather than `proxy`.

## [1.0.0-rc.10] - 2026-08-03

### Security

- **The Console data-sources proxy is no longer a way around a brain** (#149).
  The Console router is merged onto the engine routers after their layers are
  applied, so `authz_middleware` never ran on it and no index-visibility scope
  was installed; its only filter matched `.xerj_` (underscore) and therefore
  missed the reserved `.xerj-memory-` namespace. Any authenticated Console
  session of any role could read another tenant's second-brain documents, and
  the indices/fields listings leaked brain names and mappings. All three
  handlers now refuse the reserved namespace with the same `404` a system index
  gets, and the prefix moved to `xerj-common` so there is one definition.

- **Snapshot and restore can no longer reach another tenant** (#152).
  `authorize_expression` waved every wildcard through before consulting the
  principal's grants. That is sound where the engine expands a pattern over the
  caller's visible set, but `create_snapshot` walks the index map itself and
  `restore_snapshot` expands against the snapshot manifest and then removes and
  rewrites the index directory, never passing the visibility funnel. A
  non-superuser `POST /_snapshot/{repo}/{snap}/_restore {"indices":".xerj-memory-*"}`
  therefore rolled every tenant's brain back to the backup instant. Patterns on
  those two verbs are now decided up front: one that may reach the reserved
  namespace requires an explicit grant, exactly as an index template's
  `index_patterns` already did. Narrower patterns are untouched.
  The create handler also read `indices` only in its array spelling, so the
  string form ES equally accepts fell through to "absent" and captured the whole
  node; both spellings now parse, and patterns resolve against the
  visibility-filtered index list. (This also fixes `{"indices":["*"]}`
  previously producing an empty snapshot.)

- **Scripted updates are bounded by the same script budget as search** (#153).
  `_update` and `_update_by_query` reach Painless through
  `transform_document_serialized`, which established neither the request
  deadline nor the fault sink, so every evaluation fell back to its own full
  slice. With a fresh context per `;`-separated statement, a script's cost
  multiplied by its statement count and again by the hit count, and
  `wait_for_completion=false` detached the whole thing with no ceiling. One
  deadline now governs the request, faults are captured once rather than per
  document, and the hit loop stops at the deadline and reports `timed_out`
  honestly instead of a hardcoded `false`.

### Fixed

- **A client disconnect can no longer brick an index** (#150).
  `index_document` held the collection-publication guard across
  `self.schema.read().await`, the only suspension point between `begin()` and
  `commit()`. The PUT handler runs inline on the connection task, so a
  disconnect dropped that future; parked behind schema evolution's write lock
  the drop landed inside the interval and left the guard `Pending`, setting a
  sticky poison flag. Every later read and write on that index then failed
  permanently, and `_close`/`_open` do not reconstruct the index, so only a
  process restart recovered. The schema guard is now taken before the interval,
  which contains no `await` at all and cannot be cancelled part-way. Storage
  failures that occur before any visibility mutation now cancel the publication
  instead of poisoning it, so a transient disk error fails one request.

- **Columnar aggregations no longer drop a segment's documents** (#151, #143).
  `field_needs_brute_fallback` kept the fast path whenever any flushed segment
  carried a field's doc-values column, but array suppression is decided per
  segment, and the executors skip a column-less segment. Indexing scalar values,
  refreshing, then indexing array values and refreshing produced a terms
  aggregation that silently omitted the second segment's documents; term
  predicates and numeric metrics undercounted the same way. Coverage must now be
  total, and the check moved into `seg_field_kind` so it also covers sub-metric
  fields, `top_hits` sort fields, `matrix_stats` fields and composite sources.

### Thanks

- @Nicolas0315 for four detailed, measured performance reports (#144, #145,
  #146, #147) on fuzzy/wildcard term expansion, top-k scoring and the unused
  skip table, the `bbq_*`/`int8_hnsw` mapping contract, and filtered kNN never
  reaching the HNSW graph. Each arrived with a standalone harness and honest
  caveats. They are tracked for the following release rather than rushed into
  this one.


### Changed — runtime-field types (can break existing mappings)

- **`mappings.runtime` is now validated against ES's runtime-field type
  registry**, not against the mapping-property type list. Runtime fields
  have their own, much smaller registry in ES: `boolean`, `composite`,
  `date`, `double`, `geo_point`, `ip`, `keyword`, `long`, `lookup`.
  Anything else is refused at index creation with the same
  `mapper_parsing_exception` ES emits ("The mapper type [x] declared on
  runtime field [f] does not exist…").

  This cuts both ways, and both directions were wrong before:

  - **Newly rejected.** Types that are perfectly valid *mapping
    properties* but illegal under `runtime` — `text`, `match_only_text`,
    `nested`, `object`, `geo_shape`, `shape`, `point`, `dense_vector`,
    `sparse_vector`, `binary`, `percolator`, `completion`, `histogram`,
    `alias`, `join`, `version`, `constant_keyword`, `wildcard`,
    `date_nanos`, the range types, and the narrow numerics (`integer`,
    `short`, `byte`, `float`, `half_float`, `scaled_float`,
    `unsigned_long`, which collapse into `long`/`double` at runtime) —
    used to be accepted with a `200`. **A mapping that declared any of
    these under `mappings.runtime` will now fail to create.** Change the
    entry to the runtime type ES would have required (usually `keyword`,
    `long` or `double`), or move the field into `properties`.
  - **Newly accepted.** `composite` and `lookup` are runtime-ONLY types
    and are absent from the property list, so they were previously
    rejected outright — even though `lookup` runtime fields are
    implemented on the search path (via search-body `runtime_mappings`)
    and covered by the conformance suite. Both are now accepted under
    `mappings.runtime`.

  Scope: this is `PUT /{index}` **validation** only. It does not change
  evaluation: `mappings.runtime` is still stored and never evaluated —
  runtime fields are only computed when supplied in the search body as
  `runtime_mappings`. Search-body `runtime_mappings` and
  `PUT /{index}/_mapping` still do not validate runtime types at all
  (they never did) — unchanged here.

  Prior to this, the rule was half-enforced: `flat_object`/`flattened`
  were special-cased into a `400` while every other ES-forbidden type
  passed.

### Security

- **`x-forwarded-for` is no longer trusted for client identity** (#76 S5-4).
  The per-IP auth rate limiter and the audit-log source address used to be
  read straight out of `x-forwarded-for` — a header the caller writes. A
  rotating `x-forwarded-for: 1.2.3.$RANDOM` therefore reset the quota on
  every request, so the unauthenticated magic-link redeem and bootstrap-claim
  endpoints could be brute-forced without limit, while honest clients (who
  send no such header) stayed throttled. Client identity now comes from the
  TCP peer: `ConnectInfo<SocketAddr>` is threaded through both the plain and
  the TLS listener, and a handler reads it via the new `ClientIp` extractor.

  Forwarding headers are still honoured behind a real reverse proxy, but only
  from a proxy the operator has declared: the new **`server.trusted_proxies`**
  setting takes IP addresses or CIDR blocks (`["10.0.0.0/8", "::1"]`) and is
  **empty by default — an unconfigured node believes nobody**. When the peer
  is a declared proxy the forwarded chain is read right-to-left (the left end
  is caller-authored) and the right-most address that is not itself a listed
  proxy wins; a malformed element stops the walk rather than being stepped
  over. Malformed entries in the setting fail startup instead of silently
  widening or narrowing trust.

  Operators terminating TLS or load-balancing in front of XERJ must set
  `server.trusted_proxies` to their proxy's address, otherwise every user
  behind it shares one rate-limit bucket.
- **Cluster control frames are now authenticated (issue #75).** The inter-node
  transport on `cluster.port` accepted any TCP connection that spoke its wire
  format: anyone who could reach the port could send Raft `RequestVote` /
  `AppendEntries` frames and steer consensus. Every frame now carries an
  HMAC-SHA256 tag over a per-connection random challenge issued by the
  receiver, the sender's node id, the frame's position in the connection, and
  the payload; tags are compared in constant time, and the tag is verified
  before the payload reaches the JSON deserialiser. The challenge makes a
  captured frame non-replayable, including against a different node.
- **Cluster mode fails closed.** `[cluster] enabled = true` now requires
  `cluster.auth_secret` (or `XERJ_CLUSTER_AUTH_SECRET`), minimum 16
  characters. With cluster mode on and no secret from either source the node
  **refuses to start** instead of running an unauthenticated control port.
  Single-node mode — the default — is unaffected and needs no secret.

### Breaking

- **The cluster wire format changed and is not backward compatible.** A node
  running this version cannot talk to a node running rc.9 or earlier in either
  direction: the receiver now speaks first (magic, version, challenge) where
  the old protocol expected the sender to open with a length-prefixed JSON
  frame. Upgrading a cluster requires stopping every node and restarting them
  on the new version with a shared secret configured — a rolling restart will
  not interoperate. Only cluster mode is affected; it is off by default, so
  single-node deployments need no action.
- Scope note: the HMAC authenticates frames, it does not encrypt them. Cluster
  traffic is still plaintext and the secret is cluster-wide, so it proves
  cluster membership, not per-node identity. `cluster.port` still belongs on a
  trusted network segment; mTLS remains unimplemented.

### Added

- **`script` query support** (#87). A Painless predicate evaluated per document,
  wired into the doc-scan path. Missing fields fail closed the way modern
  Elasticsearch does: `doc['missing'].value` errors rather than coercing to a
  default, because since ES 7.0 reading `.value` off an empty `ScriptDocValues`
  throws (6.x returned a type default with a deprecation warning and 7.0 removed
  that fallback). Coercing would mean a script filter with a typo'd field name
  matched *every* document. The guard idiom ES documents still works, because
  that idiom is `doc['x'].size() == 0 ? <default> : doc['x'].value` and
  `.size()`, `.length` and `.empty` are total on a missing field.

  Compiled scripts are now cached, bounded at 128 entries and 512 KiB of source
  per thread. A script is evaluated once per document, so re-parsing per
  document made the per-document cost scale with the script's *size*. The cache
  key is attacker-supplied source, so the bound is a security property rather
  than an optimisation detail.

- **`getDayOfWeekEnum().getDisplayName()` in Painless** (#92).

### Fixed

- **Painless resource-limit trips surface instead of returning a wrong score**
  (#97, #123). `MAX_CALL_DEPTH = 32` is a real ceiling on legitimate recursion,
  and in the scoring paths it failed *silently*: a script recursing past it did
  not error, it returned a wrong score.

  Two failure classes had been collapsed into one and they need opposite
  treatment. Unsupported or unparseable script syntax still degrades quietly,
  which is what ES does and what seven of nine call sites rely on. A tripped
  resource limit now propagates, because a silently wrong score is undetectable.
  `_search`, `_msearch`, both template variants, scroll, async_search,
  `_rank_eval`, `_explain` and pivot transforms report it; `_count`, `_reindex`,
  `_delete_by_query` and `_update_by_query` refuse rather than act on a selection
  a fail-closed script truncated. All nine `eval_painless` call sites were
  audited and behaviour changed at none of them.

  `_rank_eval` records the fault per request in `failures` rather than failing
  the batch, and `_explain` — a diagnostic endpoint whose `matched` verdict does
  not depend on scoring — answers and discloses the fault rather than refusing.
  `_delete_by_query` and `_update_by_query` previously returned HTTP 200 with a
  400-shaped body, so a client branching on the status saw success.

  `MAX_CALL_DEPTH` stays 32: re-measured in release, the parser's worst case is
  40,720 bytes per call level, so depth 32 already uses 1.20 MiB of a 2 MiB
  worker stack.

- **`/_msearch` no longer skips the request-time script guard** (#111, #124). It
  built its sub-requests straight off `parse_request`, bypassing the validation
  the single-search path applies. Measured: a 240,531-byte `_msearch` body whose
  items carried scripts that `_search` rejects with a 400 came back 200 on every
  item. `_search/template` and `_msearch/template` had the same hole.

  Closing it initially made `_msearch` *stricter* than `_search`, because the
  new guard walked the whole body while `_search` checks six specific fields. A
  single `GuardedField` definition now drives every entry point, with the
  `aggs`/`aggregations` pair resolved exactly as the executor resolves it
  (`aggs` wins when both are present, regardless of document order). A
  compile-time assertion pins the guarded set at six, because removing an entry
  was not a type error and silently reopened the bypass.

- **`<field>.keyword` predicates resolve against the parent column** (#110).
  Four raw lookups inside `resolve_pred` failed closed, asserting that a flushed
  segment held no matching row, while the in-memory arm resolved the same field
  correctly. Measured: a `filters` aggregation on `extension.keyword` reported
  200 documents out of 3,200, and a `term` query on `group.keyword` reported 0
  hits out of 6,000.

- **`fast_aggs` falls back to brute force for unresolvable nested fields**
  (#104). Found live on real Kibana and OpenSearch Dashboards sample dashboards:
  `geo.dest` terms, `machine.ram` avg and the doubly-nested `machine.os.keyword`
  terms all returned empty or null through the fast path while the brute path
  computed the right answer.

- **The columnar histogram paths honour `config.limits.max_buckets`** (#121,
  partial). `exec_date_histogram` and `exec_histogram` carried their own
  hardcoded `65_536`. Measured at cap 37 on a 12,000-document index: 38 buckets
  returned, no error. The brute paths ignored the setting too, so this was live
  in both executors. **Still open:** `exec_terms` has no bucket cap at all, which
  is the actual OOM vector; see #121.

- **`bucket_script` resolves `_count` and evaluates parenthesised ternaries**
  (#95, #105). A `_doc_count`-weighted bucket and the `bucket_script` inside it
  disagreed about how many documents the bucket held. Verified per aggregation
  type. Parenthesised ternaries silently resolved to a null bucket value,
  because the split only looked at paren depth 0. Two equal infinities compared
  *unequal*, because `inf - inf` is NaN and an epsilon-only `==` rejected
  identical values.

- **Named ES date formats are resolved for `ignore_malformed` validation**
  (#89). Named shorthands were matched as literal text rather than expanded, so
  a value genuinely valid under the declared format was silently dropped on
  ingest. Confirmed against a real OpenSearch 2.11.1 node, which accepts the ISO
  strings and rejects the bare numbers — the opposite of what XERJ did.

- **Two `xerj-engine` tests no longer flake under parallel load** (#100). One
  mutated a process-wide bucket cap under a lock that only protected
  participants; the other depended on wall-clock timing. Measured on a 32-core
  box under 14 CPU hogs: 12/12 and 10/10 green, where the prior tree was 18%
  red.

- **`_field_caps` reports `flat_object`/`flattened` rather than generic
  `object`** (#86), and **`flat_object` is treated as an alias for `flattened`**
  for OpenSearch callers.

- **Keyed (object-shaped) bucket aggregations merge across multi-index search**
  (#103), and **`X-OpenSearch-Version` is sent to OpenSearch callers** (#101).

### Performance

- **`parse_date_ms` no longer pays an unbounded scan per document** (#91, #96).
  The no-colon zone-offset branch added in #91 sat before the naive fast paths
  and cost roughly 2x on offsetless ISO values, a hot per-document ingest path.
  The probe now reads a fixed six-byte window at the end of the value and
  nothing else — a constant window is sufficient because `%z` closes the pattern
  and chrono requires the whole value to be consumed.

  Measured across three independently built, CPU-pinned harnesses: offsetless
  ISO 217.0 → 110.2 ns/op, and the long-value classes that an interim scan-based
  guard regressed (4 KB prose 1171.4 → 198.3, 64 KB 16278.0 → 1009.2) are back
  at or below the pre-guard cost. Best-or-tied on every class, with zero
  mismatches across 72,462 constructed values.

## [1.0.0-rc.9] - 2026-08-01

Ninth release candidate: the **cross-platform correctness release**.
Headline for users, stated plainly because it is the most serious defect
this project has shipped: **the Windows binaries published as rc.4
through rc.8 could not start.** Every index creation returned
`Access is denied. (os error 5)`, and because the console bootstrap
creates a system index on first boot, the server aborted before it ever
listened. `xerj.org/get.ps1` installed those binaries for three weeks.
The cause was one unguarded Unix idiom in the durability path; the fix
is four lines, and a Windows runner now boots the binary, writes a
document, reads it back and indexes 400 datasets on every pull request.
Nothing else caught this because, until this release, no CI job had ever
*run* the binary anywhere except Ubuntu — `release.yml` built eight
targets and executed none of them.

The release also closes the self-contained half of the post-audit
security backlog — a snapshot location that could still escape
`data_dir`, index-name validation missing at the create boundary, a
field limit that dynamic ingest enforced and explicit mapping updates
did not, an unserialised magic-link redemption, and node identity leaking
from an unauthenticated endpoint — and it carries the consequences of
taking the Windows lesson seriously: a test pass over the surfaces that
shipped untested, and the four real defects writing those tests exposed.
A create-time file-descriptor exhaustion reachable from an index-create
request, inline `<script>` contents indexed as prose, XML record election
that changed shape between runs of the same file, and a core dump on
`xerj … | head`. The Rust suite grows from 1,420 to 1,486 test
functions; conformance is unchanged at 1360 passed / 0 failed / 3
skipped. The only performance change is a memory regression fix in the
ONNX embedding path; no speed measurement was taken and none is claimed.
The `_reindex` keyset behaviour is untouched — a test around it was
wrong, not the code, and that distinction is spelled out below because
it is easy to misread as a data-loss bug.

### Fixed

- **The server can start on Windows.** `xerj_common::fsio::fsync_dir`
  was `std::fs::File::open(dir)` followed by `sync_all()`, with no
  platform gate. On Windows, obtaining a handle to a *directory* that
  way always fails with `ERROR_ACCESS_DENIED (os error 5)`: the standard
  library cannot pass `FILE_FLAG_BACKUP_SEMANTICS`, which that call
  requires. `IndexStore::save_snapshot` fsyncs the parent directory
  after publishing the segment manifest, so **every index creation
  returned an I/O error**, and the unconditional `xerj-console`
  bootstrap — which creates `.xerj_users` on first boot — turned that
  into a fatal `xerj-console bootstrap` error at startup. Introduced in
  `297be60` (2026-07-12) as part of the power-loss-ordered publish
  chain, so it affected rc.4, rc.5, rc.6, rc.7 and rc.8. Windows now
  takes a separate `fsync_dir` that returns `Ok(())`, because Windows
  exposes no directory-flush primitive at all — `FlushFileBuffers` is
  not defined for directory handles. **State the consequence honestly:
  durability on Windows is weaker than on Unix.** The file contents are
  still fsynced by the callers before the rename; what is not forced is
  the rename itself, which relies on NTFS metadata journalling. That is
  the best available behaviour on the platform, and it is now a
  documented property rather than an error returned on every call.
- **`xerj … | head` no longer aborts with a core dump.** The Rust
  runtime installs `SIG_IGN` for `SIGPIPE` before `main`, so once the
  reader closed the pipe the next write returned `EPIPE`, `println!`
  panicked with `failed printing to stdout: Broken pipe`, and the
  release profile's `panic = "abort"` turned that into a core dump —
  which also swallowed the command's real exit status. The binary now
  restores the default disposition before anything can write to stdout
  (unix only), so the kernel terminates the process on the signal and
  shells report the conventional 141. Found because `xerj autoindex map
  | head -80` core-dumped on every run of a use-case harness. **Two
  consequences worth knowing.** Client sockets are unaffected: the
  standard library writes those with `MSG_NOSIGNAL` (`SO_NOSIGPIPE` on
  macOS), so a client disconnecting mid-response still surfaces as
  `EPIPE` and cannot signal the server. The server's own stdout does
  change — previously tracing discarded write errors and a server
  outlived a vanished stdout; now the next log line terminates it.
  Supervised deployments redirect stdout to a file or `/dev/null` (the
  `brain` boot path included), so in practice this reaches only
  `xerj | reader-that-leaves`.
- **Inline `<script>` and `<style>` contents are no longer indexed as
  prose.** The HTML extractor's `skip_until` state suppressed the
  *tags* inside those elements but the tokenizer's text branch appended
  their contents to the buffer unconditionally, and the closing tag
  continued without discarding it — so the buffered CSS and JavaScript
  were flushed into the document body at the next tag boundary.
  Minified stylesheets and scripts became indexed prose in every HTML
  record: BM25 noise, and — the reason this is more than a quality bug
  — inline configuration blocks carrying API keys, tokens and internal
  endpoints were stored in `_source`. For the "point xerj at my project
  folder" use case that is a real exposure. Script and style elements
  are now treated as the raw-text elements they are — the scanner jumps
  to the literal end tag instead of tokenising inside them, which also
  bounds an unclosed `<script>`: the tail of the file is discarded
  rather than buffered and indexed. Two narrower ways the leak survived
  a first attempt were caught in review and are closed here. A trailing
  `/` inside an *unquoted* attribute value (`<script src=/cdn/lib/>`)
  was read as a self-closing tag, so the element was never entered —
  legal HTML5, since `script` is never a void element. And an end tag
  whose name ran on into a non-alphanumeric character
  (`</script-foo>`) was accepted as the terminator, ending raw text
  early and indexing everything after it. The tag scanner now tracks
  attribute state to decide self-closing, and the end-tag test follows
  the spec's appropriate-end-tag rule: whitespace, `/`, or `>`.
- **XML record election is deterministic.** `elect_record_tag` chose the
  repeating element with `max_by_key` over a `HashMap`, and `max_by_key`
  settles ties by iteration order — which is seeded randomly per map.
  When a wrapper element and one of its children were both structured
  and equally frequent, elections over the *same bytes* chose different
  tags: 200 in-process elections over one fixture split 89 / 111. The
  record count and the locators held, so documents kept their `_id`s,
  but each build gave them a completely different field set, and field
  inference for that dataset never settled across builds. The comparison
  is now total — most occurrences, then the outermost tag, then the
  lowest name — with the outermost tag as the principled key: when a
  wrapper repeats as often as its child, the wrapper is the record and
  the child is one of its fields. **Two limits worth being precise
  about.** This bites across *independent* builds — a `--fresh` run, a
  new state directory, a second machine, a CI reproducibility check —
  not across ordinary repeated runs, because the resume journal skips
  files whose content digest has not changed. For the same reason an
  index built before this fix does **not** heal itself: it keeps
  whatever shape it rolled until the XML file's bytes change or the
  index is rebuilt with `--fresh`.
- **Three further inference elections are deterministic.** Reviewing the
  fix above found the same defect — `max_by_key` over a `HashMap`, ties
  settled by a per-instance random hash seed — at three more decisions,
  each with a wider blast radius than the XML one. Each now has a
  tie-break argued from what the decision means rather than a copied
  rule. The log extractor elects the template an *entire file* is parsed
  with, and every line missing that template is demoted to a
  continuation, so a flip changes the record count; ties now go to the
  most specific template, in the order `parse_kind` already tries them
  (app before clf before syslog), so an ambiguous file resolves the way
  an ambiguous line does. Field inference elects the date encoding
  written into the mapping, the catalog and the coercion plan; ties go
  to the lowest `DateEnc` in declaration order, which is `parse_date_str`'s
  own richest-first priority, so a field mixing `…T00:00:13` with
  `… 00:00:13` names the encoding that concedes the least. The third
  elects a field's entity type, keyword versus text.

- **The ONNX embedding path no longer materialises every window upfront.**
  `embed_semantic_jobs` built and cloned the passage text of every window
  before embedding any of them, so peak memory scaled with the whole job
  rather than with what was in flight. Windows now carry
  `(start, end, passages)` and their texts are built lazily and moved
  into `embed_batch`, with at most two batches in flight. This closes a
  memory regression at default settings (#71); no throughput measurement
  was taken and no speed claim is made.

### Security

Five items from the post-audit backlog, each with a regression test
where the defect is testable, plus one found while writing this
release's tests. The three that remain open are listed under Known
limitations rather than quietly omitted.

- **Snapshot repository locations could still escape `data_dir`
  (#73, High).** The residual half of F-PATH-02: a `settings.location`
  containing `..` that resolved to a *nonexistent* target slipped past
  the guard, because canonicalisation fell back to a lexical path and
  the containment check compared components of a path that had never
  been resolved. `..` components are now rejected outright, before any
  resolution. The regression test covers both the direct form and
  laundering it through an explicit allowlist entry.
- **Index-name validation is enforced at the create-index boundary
  (#80).** `IndexName::validate` existed and was applied elsewhere;
  the ES-compatible `create_index` handler did not call it. Wiring it up
  is defense in depth against path traversal through index names — the
  accepted set is unchanged, so no previously valid name is now
  rejected.
- **The field limit applies to explicit mapping updates (#76 S5-5).**
  `max_fields_per_index` was enforced on dynamic ingest but not on the
  explicit `add_fields` path, so `PUT _mapping` and schema evolution
  could push an index past the ceiling that exists to bound mapping
  explosion.
- **Concurrent magic-link redemptions cannot both mint a session
  (#76 S5-3).** The check-then-consume sequence in the redeem handler
  was not serialised, so two requests carrying the same token could both
  pass the used-check. Redemption is now serialised behind a gate.
  Stated precisely, because it is easy to overclaim: in the shipped
  engine the second session was already being blocked incidentally, by
  the index's optimistic concurrency rejecting the colliding write — so
  the observable pre-fix exposure was a 500 with internal detail instead
  of a clean 401, plus a single-use guarantee that rested on what the
  store happened to do with racing writes rather than on the handler.
  The fix moves that guarantee into the handler where it belongs. This
  is covered by 25 rounds of 6 concurrent redemptions.
- **The unauthenticated `cluster/info` body no longer leaks the node
  identifier or exact build version (#76 AUTHZ-2).** It now carries only
  mode and uptime.
- **A per-index setting could exhaust file descriptors at index-create
  time.** `index.xerj_ingest_shards` — added in rc.8 to *reduce*
  descriptor usage — was accepted with only a `>= 1` check, and the
  ES-compatible create-index handler threads the request body's
  `settings` through verbatim. `IndexStore::open` eagerly creates a
  directory and opens a WAL file descriptor per shard, so
  `{"settings":{"index":{"xerj_ingest_shards": 100000}}}` was a
  single-request descriptor and inode exhaustion — precisely the failure
  the setting exists to prevent. The override is now bounded to
  `1..=256`, the same ceiling `EngineConfig::validate` already enforced
  on the global `engine.ingest_shards`. Out-of-range values are
  **refused, not clamped**, so a typo falls back to the engine default
  rather than silently taking a different one. Found while writing the
  tests that rc.8 shipped without.

### Added

- **CI runs the binary on macOS and Windows.** A new
  `autoindex-fd-smoke` job on an `[ubuntu, macos, windows]` matrix boots
  the server under a constrained descriptor budget (soft 256 / hard
  4096 — between the pre-fix need of roughly 6,400 for 400 datasets and
  the post-fix 600), asserts a create-index / index-document / search
  round-trip, autoindexes a 400-dataset corpus and fails on any
  `EMFILE`. macOS runners default to `ulimit -n 256`, the exact
  condition the rc.8 descriptor bug regressed under, so the job
  reproduces it rather than approximating it. The explicit round-trip
  exists so that the next platform-specific break names the operation
  that failed instead of surfacing as an opaque "server exited during
  boot".
- **The use-case harnesses run in CI.** A new `usecase-smoke.sh` gate
  drives the second-brain transcript (overview counts and detector
  families, ego evidence and `not_shown`, idempotent link, unlink
  retiring rather than deleting, `as_of` replay, and the normative
  `hops > 2` / self-edge 400s and unknown-brain 404), the MCP tool
  surface (initialize handshake, the four `xerj_brain_*` tools listed
  with their honesty strings, and a real `tools/call` round-trip), and
  autoindex discovery with post-discovery search. These harnesses
  already existed but were manual, so a regression in the brain API or
  the MCP tool list was caught by nothing. Wiring them up immediately
  found two assertions that could never have passed: the transcript
  pinned detector versions at `@1` when five of them are at `@2`, and
  the autoindex idempotency check compared whole `_cat/indices` rows
  including on-disk size, which legitimately changes across a re-extract.
- **THE MAP's bounded-graph claims are tested.** The knowledge-graph
  pipeline shipped in rc.7 on two load-bearing claims — at most 13
  groups at any scale, and byte-identical output across runs — with no
  automated test in any language behind either; the only exercise was a
  manual browser script needing a live server and a real Chrome. The
  pipeline is pure dependency-free JavaScript, so 23 offline cases now
  run in under a second over four corpus scales (8, 12, 40 and 120
  folders, straddling the 12-cluster cap): the group bound with at most
  one pooled "everything else" body, every file landing in exactly one
  group with the membership lists and the index agreeing both ways, link
  conservation against the honesty row the UI computes from,
  run-to-run determinism, the pairwise bundle bound, and `as_of` replay
  making a retired link live again.
- **Regression cover for four surfaces that shipped without any.** The
  DOCX decompression and paragraph caps from rc.7 (both verified by
  removing each guard in turn and confirming the matching test fails);
  the six autoindex extractors that had no direct tests at all — HTML,
  CSV, JSON, JSONL, XML and SQLite, 49 cases; the magic-link redemption
  race hardened in rc.9's Phase-2 work, driven with 25 rounds of 6
  concurrent redemptions; and the per-index WAL shard override,
  including a reopen test that pins a shard count, writes documents that
  live only in the memtable and WAL, restarts, and asserts both the
  layout and every document survive. An installer lint gate covers
  `landing/get` and `landing/get.ps1`, which had no verification of any
  kind despite being the only user-facing entry point.

### Changed

- **A test was measuring the CI runner's core count, not the code.**
  `reindex_pages_past_10k_via_keyset` failed on any machine with real
  core count (`left: 0, right: 10050`) and passed on CI's two-core
  runners, so `cargo test --workspace` was red for every developer and
  green for the project. Turbo ingest routes documents to memtable
  shards by worker thread: on two cores they all land in one
  incidentally-visible shard, on a real box they scatter into
  unpublished shards that the search the reindex pages over cannot see.
  The sanity assertion above it passed either way because
  `live_doc_count` reads the version map rather than the search surface.
  The test now refreshes the source. **The `_reindex` product path was
  never affected** — driven end to end over HTTP, bulk-indexing 10,050
  documents and reindexing with `size: 1000` reports
  `total: 10050, created: 10050, batches: 11` and the destination counts
  10,050, both with and without an explicit refresh. This was the test
  reaching under the API, not a reindex defect, and it is recorded here
  because a bare changelog line about "reindex returning 0 documents"
  would badly misrepresent it.

### Known limitations

- **DOCX truncation at the decompression cap is silent.** When a
  document's XML inflates past the 72 MiB cap the reader simply hits
  end-of-file; because a truncated tag reads as a clean `Eof` rather
  than an error, nothing increments the junk counter. A bomb-truncated
  document is therefore indistinguishable from a legitimately short one
  — the caller gets a well-formed record that is quietly missing
  content. The memory bound itself holds and is tested; only the
  *reporting* is missing. Making truncation observable would change the
  statistics autoindex reports, so it is deliberately left for a
  separate change.
- **`index.xerj_ingest_shards` is only read in its nested spelling.**
  `{"index": {"xerj_ingest_shards": 1}}` is honoured;
  `{"index.xerj_ingest_shards": 1}` is not, unlike the neighbouring
  `index.sort.*` settings which accept both. Nothing in the product
  emits the flat form, so this is an inconsistency rather than a break.
- **A brain is still not an authorization boundary.** Any authenticated
  caller can read any brain's `overview` and `ego`; with a shared engine
  that is a multi-tenant exposure. This needs an access model rather
  than a patch and is tracked in #79, deliberately not rushed into a
  release candidate. The concrete part of that issue — the walker
  indexing `.env` and other dotfiles — was fixed in rc.7.
- **Cluster transport control messages are still unauthenticated**
  (#75). Reachable only with cluster mode enabled, which is off by
  default.
- **`x-forwarded-for` is still trusted for client identity** (#76 S5-4),
  which means a caller can spoof the address the per-IP rate limiter
  keys on. The fix needs `ConnectInfo` threaded through the shared
  serve and TLS path and is deliberately not bundled with the smaller
  items above. *(Fixed after this release — see Unreleased.)*
- **Windows durability is weaker than Unix durability**, as described
  under Fixed. The platform provides no way to make it otherwise.

## [1.0.0-rc.8] - 2026-07-31

Eighth release candidate: an **autoindex robustness + code-AST release**.
Headline for users: `xerj autoindex` no longer crashes with `Too many open
files` on large source trees, and source code is now parsed into searchable
symbols instead of plain text.

### Added

- **AST code extraction.** Source files are parsed with tree-sitter — Python,
  JavaScript, TypeScript, TSX, Rust, Go, Java, C, C++, Ruby, PHP, C#, Bash —
  rather than indexed as prose. Each file carries its `language`, a structured
  `symbols` array (`{name, kind, line}`), and a searchable `defs` list, so a
  query like `class Model` retrieves the file that defines it. Grammars are
  version-decoupled via `tree-sitter-language`, so one tree-sitter 0.25 core
  serves all of them and adding a language is a grammar dep plus one row.

### Fixed

- **Autoindex `Too many open files` (os error 24) crash** on large repos. Each
  index holds segment mmaps, a WAL and a merge task, so discovering a repo that
  infers hundreds of datasets (WooCommerce ≈ 413 indices, ~16 descriptors each)
  exhausted file descriptors on a default macOS soft limit (256), failing near
  the start. At startup the server now raises `RLIMIT_NOFILE` toward the hard
  limit using a **concrete** `rlim_max`: macOS rejects `setrlimit(RLIMIT_NOFILE)`
  when `rlim_max` is `RLIM_INFINITY` (its launchd default), so passing that back
  was a silent no-op and the limit stayed at 256 — the raise now sets a concrete
  ceiling and steps down for the `kern.maxfilesperproc` cap (best-effort).
  Reproduced and fixed against real django, redis and WooCommerce clones.
- **jemalloc `background_thread currently supports pthread only`** printed on
  every macOS launch — the `background_thread` option is now Linux-only, where
  it is supported; the decay policy is unchanged.
- **Empty console dashboards on a fresh or brain-only engine.** The AI, RAG,
  vector, agent-memory and logs dashboards fell back to mock data when their
  backing index was empty; each dashboard's nav entry is now gated on its data
  existing, so a brain-only launch opens on Second Brain and an empty engine
  shows only System.

## [1.0.0-rc.7] - 2026-07-30

Seventh release candidate: the **second brain and security-hardening
release**. Headline for users: point one binary at a folder with
`xerj brain <folder>` and get a bi-temporal, evidence-carrying link graph
over your own documents, with a console view — THE MAP — that stays
readable at any corpus size. Alongside it, six network-reachable security
findings from a whitebox self-audit are fixed.

### Added

- **`xerj brain <folder>`** — one command turns a folder into a queryable
  knowledge graph. Structural detectors (wiki-links, markdown links,
  hrefs, folder adjacency, reading order) record, for every link, the
  exact sentence that created it and the byte offset it came from.
  Measured: 10 notes → 25 links in 0.1s; a 943 MB repo copy → 7,571 files,
  2,128,360 notes, 20,930 links in 322.7s.
- **Graph HTTP API** — `POST`/`DELETE /_graph/{brain}/link`,
  `GET /_graph/{brain}/ego`, `GET /_graph/{brain}/overview`. Bi-temporal:
  links are retired, never deleted, and any past moment is replayable via
  `as_of`. Traversal is capped at 2 hops per call by design — XERJ is a
  search engine with a graph-shaped index, not a graph database.
- **THE MAP** (console) — a Canvas knowledge-graph view whose default
  "helicopter" altitude is bounded by construction: any corpus collapses
  to at most a dozen named groups plus one catch-all. Measured 19,299
  links / 1,550 documents → 13 groups in 44 ms; a synthetic 50,000-link
  budget → 13 groups / 60 bundles in 122 ms, byte-identical across runs;
  60 fps pan/zoom/expand. Small corpora skip clustering and draw real
  notes, so the view is adaptive rather than one-size.
- **Cross-file-type links** — new `pathcite@1` and `cratecite@1`
  detectors relate documents to the code and files they cite, so a PDF or
  a design note can be shown next to the crate it references, with the
  citing text as evidence.
- **Agent surface** — four MCP tools (`xerj_brain_ego`, `xerj_brain_link`,
  `xerj_brain_unlink`, `xerj_brain_overview`) under
  `contract: xerj-second-brain/1`.
- `limits.snapshot_repo_allowlist` — an Elasticsearch `path.repo`
  equivalent bounding where snapshot repositories may live.

### Fixed

- **Security (unauthenticated):** `POST /_sql` with a deeply nested
  `WHERE` clause overflowed the stack and aborted the whole process. The
  WHERE parser now bounds recursion depth (`MAX_SQL_DEPTH`), as the
  query_string parser already did.
- **Security:** search-template parameters were spliced into the query
  JSON unescaped and re-parsed, so a parameter could inject query
  structure and drop the template's own filter. Parameters are now
  JSON-escaped and can no longer emit structural tokens.
- **Security:** a snapshot repository's `settings.location` was an
  unvalidated absolute path, so snapshots could read and write index data
  outside `data_dir`. Locations are now bounded by `data_dir` or the new
  allowlist.
- **Security:** the console's session `last_seen` refresh blind-wrote a
  stale copy of the session, which could resurrect a session revoked
  moments earlier. The refresh now re-reads and skips revoked sessions.
- Console: the Second Brain dashboard is no longer always present — it
  appears once at least one brain exists, so a user who has never run
  `xerj brain` is not shown an empty dashboard.
- Console: several second-brain counts reported the engine's 10,000-hit
  floor as an exact total on multi-million-note brains; totals are now
  exact, and floors are shown as `≥`.

### Notes

- The second brain's link detection is **structural and lexical** — there
  is no LLM and no semantic model in the loop, and the node-store embedder
  remains a lexical feature hash. Scale beyond the measured corpus above
  is not claimed.

## [1.0.0-rc.6] - 2026-07-28

Sixth release candidate: the **semantic-analytics and bounded-memory
release**. Headline for users: a `knn`/`semantic` query can now carry
`aggs` in the same `_search` request — aggregations run over the
retrieved top-k neighbour set, forced onto the exact path so bucket
counts are exact — and a semantic hit can opt into a `_passage` field
that returns the winning chunk's text with byte-exact provenance.
Underneath, the seven immutable-segment hydration caches now share one
process-wide, cgroup-aware memory budget, and the storage layer gains a
bounded, cancellable selective-hydration primitive (deliberately not
wired to any engine route yet). Two correctness holes are closed: kNN
could return a superseded vector when a document ID was re-inserted
while owning the HNSW entry point (fix arrived from the probelabs fork),
and an oversized Painless expression could exhaust the native stack
instead of returning a bounded error. One change arrived from outside
the org: transparent gzip request-body decompression, contributed by
Vinz2168, which unblocks Filebeat and the other ES clients that compress
by default. ES-YAML conformance holds at 1360 passed / 0 failed / 3
skipped. Each change states its own limits in place; the aggregation
gaps left open are listed under Known limitations.

### Added

- **kNN + aggregations in a single request.** `POST /{index}/_search`
  with a `knn`/`semantic` query plus `aggs` now returns aggregations;
  previously the `aggs` field on kNN responses was hardcoded `null`. No
  new API surface — the existing `aggs` request field is honored on the
  kNN path, across all three executors (HNSW, exact brute-force,
  multi-kNN). Semantics follow ES top-level-knn: aggregations run over
  the retrieved top-k neighbour set after the `num_candidates` fan-out,
  independent of `from`/`size` paging — `"size": 0` returns aggs only
  with empty hits, and `hits.total.value` reports the neighbour pool
  (`k`), not a match count, so bucket counts scale with `k`, never with
  index size. To keep those counts exact, an aggs-bearing kNN request
  always executes the exact brute-force path even when it would
  otherwise qualify for HNSW — ANN recall is below 100% and approximate
  bucket counts would be silently wrong — trading ANN speed for
  exactness (no performance claims are made for this path). This
  removes the previous two-step workaround (kNN → collect ids →
  separate aggregation request). Limitation, stated up front:
  `significant_terms` over a kNN slice returns empty — the vector path
  does not yet supply a background corpus; use `terms` for raw in-slice
  counts, or the two-step pattern when statistical significance is
  needed (filed as a follow-up).
- **Winning-passage provenance (`_passage`).** Requesting
  `"fields": ["_passage"]` on a single `semantic` or kNN query adds
  `fields._passage[0]` to each hit: the winning chunk's source `field`,
  zero-based `ordinal`, UTF-8 byte `start_offset`/`end_offset`, the
  `text` slice, and `page` (only when the source carries a numeric
  `page`; omitted otherwise). The text is reconstructed by slicing the
  authoritative `_source` at compact ingest-time offset metadata rather
  than storing the chunk a second time — the committed measurement over
  256 varied Unicode documents reports 80.26 B/doc of raw metadata,
  3.98 B/doc after ZBS2 compression. All three ES `fields` wire shapes
  (scalar, string array, object form) are accepted and normalized
  identically, and scroll continuations keep the passage. Multi-kNN and
  hybrid-fusion queries requesting `_passage` are a deliberate HTTP 400
  with an actionable message — summed or fused contributions do not
  define one winning passage, and rejecting is more honest than
  guessing. Scope: provenance covers the exact per-chunk max-sim
  scoring path; the HNSW-served and SQ8-quantized paths reconstruct a
  passage only for single-chunk values, and a kNN clause on an
  arbitrary `dense_vector` field has no passage metadata, so `_passage`
  may be absent rather than erroring. The internal metadata (reserved
  prefix `__xerj_passage_meta__`) is engine-owned: supplying it at
  ingest is rejected per-document, and it is stripped from `_source`,
  Painless, field discovery, full-text indexing, and aggregations. The
  native `_search` endpoint accepts `_passage` (returned as a hit-level
  key) and rejects any other `fields` entry. The default embedder
  remains lexical feature hashing — neural semantics still require
  opting into neural embedding mode or an embedding endpoint.
- **Process-wide segment hydration cache budget.** The seven
  immutable-segment hydration caches (stored slices, doc values, parsed
  stored values, sort shadows, id positions, row sequences, decoded
  stored bytes) — previously independent or unbounded — now share one
  cgroup-aware retained-payload budget with CAS admission and
  last-reader refunds. Configure with
  `[limits] max_segment_hydration_cache_mb` (default `0` = automatic:
  20% of the effective cgroup/system memory limit, no floor and no
  fixed cap; an explicit value is clamped to 50% with a startup
  warning) or the `XERJ_SEGMENT_HYDRATION_CACHE_MB` env override
  (`auto`, `off`, or a MiB value; `off` refuses every admission — a
  diagnostic switch). Refusal is invisible to queries: the request
  keeps the value it already built and returns the same result,
  uncached; publish-time warm-only values are simply dropped, and a
  counter increments. `GET /_nodes/stats` gains
  `indices.segment_hydration_cache` with limit/current/peak, refusal
  and accounting-error counters, the budget source, and per-category
  breakdowns. Automatic sizing now honors the *tightest* cgroup-v2
  `memory.max` from leaf to root (previously the nearest finite value
  won, overstating the ceiling when a parent limit was smaller). A
  release-build accounting underflow that left counters unchanged is
  also fixed — both counters now clamp atomically and record exactly
  one accounting error. Honesty note, repeated in the docs: this is a
  retained-payload/key ceiling computed from deliberately conservative
  estimates, **not** an RSS bound — query-owned materialization, decode
  scratch, mmaps, memtables, vectors, and allocator behavior stay
  outside it, and a single highly compressed segment can still create a
  transient decode peak before admission (bounded/streaming decode is a
  named follow-up). The motivating profiles (~206 MiB of publish-warming
  RSS on one 4,096-document diagnostic run; 226.35 MiB of cumulative
  live allocations under a merge warm) are attributed measurements, not
  a claimed end-to-end memory reduction.
- **Bounded selective stored-field hydration (storage primitive).** The
  `xerj-storage` crate can now hydrate selected rows and selected
  top-level `_source` fields from a ZBS2 V2 stored section without
  materializing the whole canonical JSON array, with cancellable entry
  points (deterministic checkpoints every 128 rows), a retained-size
  preflight computed from framing metadata alone (fails closed on
  historical zstd frames that omit a content size), and
  malformed-input hardening — checked arithmetic, fallible reserves,
  and framing/varint/bit-width validation keep hostile headers on the
  error path instead of aborting in the allocator. The on-disk stored
  format is unchanged and historical segments remain fully readable.
  This is deliberately **not** wired to any engine route or cache in
  this release — no endpoint, no query change, and no end-to-end
  speedup is claimed. The committed report measures the trade-off on
  its deterministic 20,000-row fixture: one-row hydration at roughly
  59× lower peak RSS (~335 MB full decode vs ~5.6–5.7 MB selective) but
  roughly 4× slower wall time, because RAW/zstd JSON columns still
  stream through all rows — a memory primitive with a CPU cost,
  measured as a single-host storage microbenchmark, and LZ4 columns
  still decompress whole before selective materialization.
- **gzip request-body decompression (external contribution).** Both
  HTTP routers now transparently decompress `Content-Encoding: gzip`
  request bodies before parsing; previously the raw gzip bytes reached
  handlers and failed JSON parsing (the commit reports
  `PUT /_ilm/policy` returning 400 and `POST /_bulk` returning 500).
  This unblocks the ES clients that compress by default — the
  reproduced failure was Filebeat 8.13.4 with default config 400'ing on
  its first ILM setup call, marking the output connection permanently
  failed, and retrying forever with zero documents ingested; after the
  fix the same default run completes and every test event lands and is
  queryable. Semantics note: the decompression layer sits outside the
  body-size limit, so the configured request-body limit now bounds the
  *decompressed* size, not the on-wire size. Only gzip is handled
  (deflate/br/zstd remain unsupported), response compression is
  unchanged, and verification is the PR's own curl + Filebeat runs — no
  automated test was added in the diff. Contributed by Vincenzo
  Lombardo (`Vinz2168`).

### Fixed

- **kNN under document-ID reuse (HNSW entry point).** Re-inserting a
  vector under the external ID that owned the HNSW graph entry point
  could return the old, superseded vector as a hit for that ID, and the
  v2 graph writer could persist a CRC-valid file whose entry header
  paired the current slot's external ID with the orphaned slot's higher
  layer. Both are fixed (arrived from the probelabs fork, PR #64,
  landed via #66): a slot counts as live only when the reverse ID map
  resolves its external ID back to that exact slot, the entry point is
  re-selected as the highest-layer live slot, search excludes
  superseded slots from results (they stay traversable for graph
  connectivity — excluded, not reclaimed), and the writer derives the
  entry header from live topology, repairing a stale in-memory header
  on save. This is a write/search-time fix, not a retroactive file
  repair: graph files written before the fix still load as-is and are
  corrected the next time the fixed writer saves them. The on-disk
  graph format and byte layout are unchanged, and no authoritative
  documents or latest vectors are discarded.
- **Painless: oversized expressions return a bounded 400 instead of
  aborting the process.** Flat shapes the parser can build without
  recursing (`1+1+…` binary chains, `.member` chains, `[index]` chains)
  bypassed the parser's recursion guards, and the evaluator's depth
  guard counted frames only after entering evaluation — so a 5,001-node
  chain could exhaust the native stack before the intended error fired
  (reproduced as a process abort on the default test-thread stack).
  Expression depth is now computed exactly at parse time — 500 accepted
  and evaluated, 501 rejected, covered for all three shapes — and flat
  binary and member/index chains evaluate iteratively. Scripts found
  under `script`/`*_script` keys in `_search` bodies that exceed the
  limit get an up-front HTTP 400: "script evaluation exceeded maximum
  depth; split the expression into smaller statements". The supported
  depth contract itself is unchanged (still 500) — this enforces the
  existing contract before evaluator recursion instead of during it —
  and semantics of accepted expressions (precedence, associativity,
  short-circuiting and error order, `doc`/`params`/`Math` dispatch)
  are test-covered as unchanged. Statement nesting no longer consumes
  the expression-depth budget; it stays bounded by the parser's
  separate limit.

### Changed — internal (no behavior change claimed)

- **HNSW read path behind a storage-view seam.** Search, construction
  traversal, diverse-neighbor selection, diagnostics, and graph
  serialization in the vector index now run through a private read-only
  storage trait with static dispatch, so a future immutable graph
  backend can implement the same read contract without rewriting the
  search path or adding runtime branching. Public API, mutation model,
  distance behavior (cosine/L2/dot), filtered-search and tombstone
  semantics, and graph format v2 are all unchanged — a checked-in
  pre-seam format fixture loads, rewrites, and reopens correctly, and
  parity tests compare exact result ordering and exact serialized
  bytes. The commit's controlled A/B (one 10K × 64-d graph, 30K
  deterministic queries, nine alternating CPU-pinned runs) reports
  −0.31% median qps with identical checksums, stated to be within run
  noise: explicitly not a speedup, and this refactor does not by itself
  reduce ingest memory.

### Docs

- **calltree.ai case study**
  (`docs/case-studies/calltree-analytics/`): three runnable scripts +
  README showing one `POST /conversations/_search` combining
  `query.knn` with `terms`/`percentiles`/`avg` aggregations for
  deep-research questions, over a 130-conversation generated corpus,
  with a primitives table mapping sub-questions to query shapes.
- **RAG hybrid retrieval example**
  (`docs/examples/rag-hybrid-retrieval/`): pure-cosine vs BM25 vs
  `query.hybrid` (RRF) over an 18-chunk synthetic corpus with real
  768-dim EmbeddingGemma vectors, documenting the pgvector-to-XERJ
  drop-in migration (same chunks and embeddings, swap the cosine
  `SELECT` for the hybrid `_search`). The honest result is included:
  on the identifier query, equal-weight RRF ranks the right chunk #3,
  not #1, and the README recommends weight tuning or a query router.
- **daily.dev Postgres CDC case study**
  (`docs/case-studies/daily-dev-postgres-cdc/`): logical-replication
  sync from a pgvector-equipped `post` table into XERJ via `_bulk`,
  with both a polled `test_decoding` consumer and a push-streaming
  consumer with LSN checkpointing (live-verified resuming from
  `confirmed_flush_lsn` after a stop/change/restart). A correction is
  recorded in place: the first draft's "no fused RRF node" limitation
  was the author's own request-syntax error, not an engine gap —
  `query.hybrid` with `fusion: {type: rrf}` is the real syntax.
- **Semantic-analytics use case** on xerj.org
  (`landing/use-cases/semantic-analytics.html`, wired into the
  use-case subnav, hub, solutions index, sitemap, and
  `llms.txt`/`llms-full.txt`): documents the rc.6 kNN+aggregations
  behavior, including every gap listed under Known limitations below.
  All corpora in this Docs group are small and synthetic, and nothing
  in them was timed at any corpus size; daily.dev and calltree.ai are
  the parties whose questions motivated the case studies, not
  customers.
- **Production-deployment recipe** gains "Bound retained segment
  hydration" (the budget knob, env override, clamping, refusal
  semantics, and the stats fields); the passage-retrieval recipe gains
  the `fields: ["_passage"]` example and response shape.

### Known limitations

Known-open items in this release's own features, stated here rather
than left implied:

- **`significant_terms` over a kNN slice returns empty buckets** — the
  vector path does not yet supply a background corpus. Use `terms` for
  raw in-slice counts, or the two-step kNN → ids → aggregation pattern
  when statistical significance is needed. Filed as a follow-up.
- **Aggregations on a kNN query are bounded by `k`:** buckets count the
  retrieved neighbour set only, never the full matching corpus, and
  `hits.total.value` echoes the neighbour-pool size, not a match count
  — the documented top-level-knn semantics.
- **An aggs-bearing kNN request always runs the exact brute-force
  scan:** cost scales with corpus size × dims, `num_candidates` has no
  effect there, and HNSW never serves it. A hybrid (RRF) query carrying
  `aggs` is rejected with a 400, and a kNN clause inside `bool.filter`
  is not peeled and returns no buckets — slice aggregation works for
  pure kNN only.
- **The hydration budget is not an RSS bound** — it caps conservatively
  estimated retained payload and keys only, and a single highly
  compressed segment can still create a transient decode peak before
  admission; bounded/streaming decode is a separate follow-up.
- **The selective-hydration primitive is not wired** into any engine
  read path or the shared cache budget yet, and LZ4 columns still
  require whole-column decompression before selective materialization.
- **Request decompression handles gzip only** (deflate/br/zstd request
  bodies remain unsupported); response compression is unchanged.
- **HNSW graph files written before the entry-header fix** still load
  with the inconsistent header retained; a file is only corrected the
  next time the fixed writer saves it.

## [1.0.0-rc.5] - 2026-07-27

Fifth release candidate: the **real-client compatibility release**. Much of
it came from pointing real Elasticsearch and OpenSearch tooling at
XERJ — Kibana 8.13, Kibana OSS 7.10.2, OpenSearch Dashboards 2.11.1/3.6,
and their own shipped sample datasets (flights, eCommerce, logs) — and
fixing the wrong answers, 500s, and stalls those clients produced. The rest
is storage, ingest and `autoindex` work that did not come from client
testing. Headline for users: the query classes that silently matched
**zero** documents (booleans, `.keyword` multi-fields, `match_phrase` on
arrays — and keyword arrays before a flush, see the qualification below)
now return what ES returns; a real Kibana/OSD instance boots,
logs in, and saves objects against XERJ end to end; and a dashboard firing
several panel queries at once no longer stalls the node. ES-YAML
conformance holds at 1360 passed / 0 failed / 3 skipped. Zero-hit defects
found during this cycle that are **not** fixed here are listed under Known
limitations.

### Fixed — query semantics (classes that silently matched zero docs)

- **Keyword arrays (partial — memtable half only):** a `term`/`terms` query
  on a multi-valued keyword field only ever compared element `[0]` — the
  memtable stored just the first value and the segment keyword column is
  one ordinal per doc — so exact lookups returned silent false negatives
  while `terms` aggregations and `match` (which join all elements) looked
  correct. The memtable path is fixed: those doc-values lookups now bail
  for array fields and fall through to the array-aware source scan, so a
  `term` on a multi-valued keyword field is correct **before** a flush.
  Flushed segments are **not** fixed — the segment keyword column is still
  one ordinal per document, so after a flush a `term`/`terms` on a
  non-first array element can still silently miss. The complete fix needs
  multi-valued segment keyword columns (a storage-format change); the
  regression test
  (`xerj-engine` `test_term_matches_non_first_array_element`) is committed
  but `#[ignore]`d until that lands.
- **Booleans:** `term`/`match_phrase` on a boolean field undercounted to 0
  while a `terms` aggregation on the *same* field bucketed `true`/`false`
  correctly. The trigger was the count shortcut over memtable-resident
  data — i.e. any bulk import before an explicit `_flush`, which is what
  every real importer (OSD's own sample-data loader included) produces.
- **`.keyword` multi-fields:** the brute-force scan's field resolver had no
  multi-field fallback, so `term`/`match_phrase` on `category.keyword`,
  `manufacturer.keyword` and friends matched nothing — `_source` never
  contains a literal `"category.keyword"` key. It now strips the trailing
  segment and retries against the parent when the parent is a leaf value
  (guarded, so a genuinely absent nested/object field is still absent).
- **`match_phrase` / `match_phrase_prefix` on arrays:** both arms only
  handled scalar values, so an array-valued field never matched any
  phrase. ES semantics restored: the doc matches if any element does.
- **`match_phrase` with a non-string query value:** `{"query": true}` was a
  hard parse error surfaced as `search_phase_execution_exception` — the
  exact shape OSD's filter bar sends for a boolean filter pill, which
  broke every panel on the dashboard at once. Scalars are now coerced the
  way `match` already coerced them.
- **Empty query strings:** `match`/`match_phrase`/`match_phrase_prefix`
  with `""` returned 400; ES treats an empty analyzed query as zero terms.
  They now resolve to `match_none` (200, no hits) — this is what Kibana's
  saved-objects `_find?search=*` builds, so that endpoint works again.
- **`geo_point`:** aggregations and `geo_distance`/`geo_bounding_box`
  rejected string-encoded `{"lat": "50.03", "lon": "8.57"}` coordinates
  (the shape the flights sample data ships), and `geo_centroid`/
  `geo_bounds` used a flat lookup that never found a nested geo field.
  Both now behave like ES.
- **Aggregation scripts:** `terms` required a top-level `field` and
  returned `{"buckets": []}` whenever it was absent — any script-based
  terms aggregation silently produced no buckets. `script` is now a real
  key source alongside `field`.
- **Painless date accessors:** `doc['t'].value` returned a plain string, so
  `.getHour()`, `.getDayOfWeek()`, `.getYear()` and the rest of the common
  accessor set failed the whole script. They now parse and extract the
  requested UTC component.
- **Dynamic date mapping:** ISO-8601 strings are inferred as `date` instead
  of `text`; `date_detection: false` was accepted-but-ignored and is now
  honored; a non-date value written into an inferred date field returns
  `mapper_parsing_exception` instead of being silently stored and then
  being invisible to range queries, sorts, and time filters; and
  `date_detection` survives the `PUT /_mapping` merge path.
- **Segment-path parity:** projected FTS bool queries dropped
  `minimum_should_match` (so `match_bool_prefix` with `mm=3` matched every
  single-term hit after flush) and doc-values wildcards did not case-fold;
  nested kNN returned zero hits for any parent doc living in a segment
  because the reassembled `{_id, _seq_no, _source}` shape was not
  unwrapped. `_count` now resolves `terms` lookups.

### Fixed — API surface (what real clients call)

- **Aliases:** single-index endpoints (`GET /{alias}`, `/_mapping`,
  `/_settings`, …) never resolved an alias to its backing index, and
  aliases were never persisted at all — index data survived a restart but
  the alias pointing at it did not. Aliases now resolve everywhere ES
  accepts them and are written to `aliases.json` (atomic temp+rename).
  Together this unsticks a fresh OSD container looping on "Another
  OpenSearch Dashboards instance appears to be migrating the index".
- **`_field_caps`:** never listed declared multi-fields (so `fields=*`
  omitted every `.keyword` entry and index-pattern refresh reported
  "field not found"), and only special-cased the literal `*`/`_all`
  wildcards — any real glob such as `wiki-test*` returned empty caps.
  Both fixed; wildcard/comma index specs also now resolve on 9 further
  endpoints that previously 404'd, 400'd, or — for `_refresh` and
  `_cat/count` — silently reported success / zero documents while doing
  nothing (`_refresh` answered `{"_shards":{"successful":1,…}}` without
  refreshing; `_cat/count` returned 0, indistinguishable from a genuinely
  empty index).
- **`POST /{index}/_update`** never returned a `get` block, so every Kibana
  saved-object write crashed client-side reading `body.get._source`. It is
  now returned exactly when the caller passes `_source`/`_source_includes`/
  `_source_excludes`, matching ES (absent otherwise, so `_doc` responses
  are byte-identical to before).
- **`_source` vs `stored_fields`:** the implicit `_source` suppression that
  `stored_fields` triggers is now only a default — an explicit top-level
  `_source` in the same request wins, as in ES. This is what starved
  almost every column in OSD's Discover.
- **`_bulk`** reports each item's real `_seq_no` and `_version` instead of
  a wall-clock microsecond timestamp and a hardcoded `1`.
- **`_update_by_query` / `_delete_by_query`** honor
  `wait_for_completion=false` and return the async `{"task": …}` form;
  `GET /_tasks/{id}` reports completion with the final response.
- **`/_xpack`** respects `--compat-version` (it had its own hardcoded
  `8.13.0`, which Kibana OSS 7.10.2 surfaced as an opaque "license not
  available" refusal to start), and `/_xpack` + `/_xpack/usage` report the
  real auth state instead of `security.enabled: true` — so `--insecure`
  no longer makes Kibana render a login screen for auth that is off.
- **Missing HTTP verbs:** `_refresh`, `_analyze`, `_msearch` accept GET and
  `_clone`/`_shrink`/`_split` accept PUT, as ES does — the PUT `_clone`
  405 was a fatal error during Kibana's saved-object migration.
- **Login-path endpoints:** HTTP Basic auth (Kibana's interactive realm),
  `POST /_security/profile/_activate`, `GET /_security/profile/{uid}`,
  `GET`/`POST /_security/user/_has_privileges`, and a real
  `GET`/`PUT`/`DELETE /_security/privilege` store with ES-shaped 404s —
  each of these was a 500 or a hang on the Kibana login and home pages.
- **`GET /_cat/templates/{pattern}`** (a 404 that crashed OSD) and
  **`POST /_index_template/_simulate`** (body-only template preview) added.

### Changed — index resolution (can break existing callers)

These three align index addressing with ES 8 and turn some
previously-succeeding requests into errors. Review any caller that relies
on the old behavior.

- **A comma-separated index spec now 404s when a concrete name is
  missing**, implementing ES's default `ignore_unavailable=false`. The
  missing name used to be dropped silently — `POST /real,typo/_refresh`
  answered `200` with `_shards.total: 1`. Wildcard/`_all` specs that match
  nothing are still a valid empty result (ES's `allow_no_indices` default).
- **Wildcard and `_all` expansion no longer sweeps in hidden
  (dot-prefixed) indices**, so an operation like `POST /*/_close` can no
  longer hit `.xerj_users`-class system indices. A dot-prefixed pattern
  opts back in, as ES's hidden addressing does.
- **`POST /_close` refuses a wildcard or `_all` target outright** with ES
  8's `action.destructive_requires_name` `illegal_argument_exception`.

### Added

- **OpenSearch client auto-sensing.** XERJ answers the identity endpoints
  (`GET /`, `GET /_nodes`) per request based on the caller's User-Agent, so
  one running instance serves an `opensearch-py`/`opensearch-js`/OSD client
  and an Elasticsearch client simultaneously, each seeing a
  version/distribution block its own compatibility gate accepts. Explicit
  `--compat-distribution` / `--compat-version` (and the matching env vars)
  still pin the identity for every client. The `x-elastic-product` header
  is no longer sent to a detected OpenSearch caller.
- **Opt-in ingest memory attribution (developer feature).** Setting
  `XERJ_INGEST_MEMORY_TRACE=summary` emits a bounded `xerj.ingest_memory.v1`
  NDJSON ledger — per-owner logical bytes across HTTP bodies, raw and
  parsed semantic sources, prepared docs and vectors, active memtables and
  drained flush snapshots — alongside jemalloc allocated/active/resident,
  RSS, CPU time, and accounting/dropped-event counters, plus a separate
  `/proc`-derived `xerj.process_sample.v1` stream. Default is fully off.
  Merge and read-cache owners are reported `unavailable`, not zero. This
  makes ingest retention *measurable*; it is not itself a memory bound.
  A deterministic bounded diagnostic suite
  (`demo/usecases/autoindex/scale/bounded/`) drives ingest → refresh →
  flush → force-merge → restart with exact count and sentinel checks.
- **Private pprof debugging toolkit.** A Linux-only `debug-profiling`
  feature compiles in bounded CPU and jemalloc heap profiling with no
  network endpoint; artifacts are written mode-0600 to an operator-created
  directory, with `capture.py`/`inspect.py` wrappers that hash the binary
  and every artifact. Not in any shipped feature set; no runtime overhead
  or speedup is claimed.
- **Experimental ONNX embedding backend**, wired through `autoindex` behind
  the `onnx-experimental` feature. Its lazy session initialization is
  cancellation-safe: model loading moved to one process-owned thread whose
  single terminal result is shared, so a cancelled first request can no
  longer strand every later one.
- The ES-YAML conformance runner now exits non-zero when any case fails
  (it previously reported failures but exited 0).

### Changed — performance (concurrent dashboard bursts)

- **Full-corpus aggregations no longer deep-clone the memtable under the
  shard lock.** The `need_full_corpus` path (any request with `aggs` that
  the columnar fast path can't serve) cloned every buffered document's JSON
  tree while holding the per-shard read guard, serialising concurrent
  panels behind O(docs) work each. It now Arc-shares out under the lock and
  clones after releasing it. Measured: 15 concurrent `date_histogram`
  queries against a real ~14k-doc memtable-resident index, 1.2–2.7 s →
  ~150–170 ms (commit `6a54cf5`).
- **Full-corpus aggregations no longer re-decode segments per query.** The
  same path did an unconditional open + decompress + full `serde_json`
  parse of a segment's entire stored section on every request once data had
  flushed. Segments are immutable, so it now uses the existing
  single-flight stored-value cache. Measured: 20 concurrent requests
  against a real flushed eCommerce segment, 15–26 s → 1–4 ms including the
  cold first hit (commit `f6daf70`). A segment read failure during
  full-corpus assembly is now a hard error rather than a silent skip that
  undercounted every bucket.
- **The scan path no longer stalls on cold, concurrent bursts.** Two
  compounding costs: every scanned doc's `_source` was deep-cloned to
  splice `_id` in even though only a deeply-nested `ids` clause needs it
  (now computed once per scan from the query shape), and the raw-fallback
  decode had no single-flight protection, so N concurrent requests against
  a cold segment each paid the full open+decompress. Measured against real
  Kibana OSS 7.10.2 and eCommerce sample data: a 24-concurrent
  `query_string`+`match_phrase` burst immediately after restart (worst-case
  cold caches) went from ~4.5 s per request to 100–165 ms, with the warm
  repeat at 1–5 ms (commit `7e963ff`).
- **Semantic and vector work is bounded by the request.** One absolute
  deadline computed at search entry is carried through single-flight
  waiting, admission, embedding, hybrid/multi-kNN recursion, and exact
  scanning; partials set `timed_out` and `hits.total.relation: gte`, and
  the ES handler aborts its child task when the request is dropped.
  Previously a cancelled semantic search could drain for minutes past its
  timeout. Cold vector segment loads also moved to bounded `spawn_blocking`
  producers so they can no longer pin every async worker.

### Fixed — storage, durability & ingest

- **Raw ingest validates before it publishes.** Caller bytes were appended
  to the WAL before being proven to be complete JSON, so malformed input
  could reserve sequence numbers and become durable, with the later parse
  path silently substituting `{}`. Whole-batch UTF-8/JSON/nesting
  validation now completes before any sequence, WAL, version-map, memtable,
  or schema mutation, and malformed bulk sources return per-item
  `document_parsing_exception`. The parsed `Value` is now the single
  authority for turbo ingest (live indexing and WAL replay could previously
  disagree), and `copy_to` is applied before WAL publication so GET,
  search, flush, replay, and restart all observe the same source.
- **Per-shard WAL buffers bounded to 64 KiB** (was 8 MiB). Every index
  eagerly opens one writer per ingest shard, so a 16×16 default reserved
  ~2 GiB of allocator capacity before a single document was written;
  capacity above a frame cannot batch across requests because every
  acknowledged append already drains the buffer. Measured on 256 empty
  writers: jemalloc allocated 2,147,503,496 → 16,797,064 bytes (−99.2%),
  with ingest throughput ratios of 0.976 (single shard) and 1.020
  (eight shards) and exact replay preserved (commit `761b915`). Cost:
  records larger than the 64 KiB buffer issue more write syscalls
  (measured 1,372 → 3,372 on 131 KiB × 1000), with throughput unchanged.
- **Same-ID writes are serialized.** A keyed publication coordinator now
  spans the current-state/CAS check through WAL, version map, FTS, and HNSW
  publication for single-document paths, closing races that could admit two
  creates, lose an update patch, or let a delete overtake a write. Scripted
  `_update` and `_update_by_query` run through the same boundary, so two
  concurrent `ctx._source.n += 1` requests can no longer both read `n=0`.
  Distinct IDs do not contend. (Turbo batch paths remain follow-up work.)
- **Semantic vectors stay out of full-text indexes.** The dynamic-field
  walkers treated pooled embeddings and their `_chunks` companions as
  ordinary text and fed every float into FTS as a decimal token. In one
  failed diagnostic run that produced 2.338 GiB of vector FSTs inside a
  3.449 GiB partial index. Embedding outputs are now excluded by schema
  designation (not a name heuristic) across pre-analysis, flush, and merge.
  Existing vector FST sidecars are not rewritten on upgrade — reindexing is
  the safe way to reclaim them.
- **The admin API key persists across restarts.** The server wrote
  `data_dir/admin.key` but never read it back, minting a fresh key on every
  start and locking out every client configured against the previous one.
  An existing well-formed key is now reused; anything missing or malformed
  still falls through to fresh generation.

### Fixed — autoindex

- **Content deduplication with crash-safe replacement.** Byte-identical
  paths were indexed independently (one reproduction journaled 2,442
  records for 1,221 live documents). Sources are now hashed with streaming
  XXH3-128 and digest peers byte-verified before one canonical path is
  chosen; every current name is preserved in `ax_paths`. Publication became
  an explicit durable generation transaction (synced `file_replace_start`
  → staged, source-verified extraction → synced `file_done` commit), and
  journal replay repairs torn tails and rolls back failed appends.
  Ambiguous legacy prefix collisions fail closed before any backend
  mutation instead of guessing.
- **Follow-on fixes to that rework:** resume now guarantees exclusive
  plan-key ownership (two files resolving to one key each ran the
  replacement transaction and deleted the other's documents);
  `delete_by_query` repeats until a pass deletes nothing, because the
  server implements it as a single `size:10000` pass and >10k-doc
  generations left permanent ghost documents; and both recoverable error
  paths now name the offending files or journal offset instead of
  advising a blanket `--fresh` re-extract.
- **PDFs are parsed by `pdf_oxide` in isolated same-binary workers.** The
  previous byte scanner interpreted font character codes without resolving
  page resources or ToUnicode maps, producing NUL-separated and shifted
  text that schema inference (correctly) refused to elect as
  `semantic_text`. Workers get process groups, a 1536 MiB `RLIMIT_AS`, and
  descendant kill/reap on timeout — documented as resource isolation, not
  a security sandbox, with no OS memory cap on non-Unix. New
  `--pdf-workers` and `--pdf-timeout-secs` flags. Cost: the no-default
  release binary grows 34,160,328 → 39,018,184 bytes (+4.6 MiB).
- **Text files are classified by sentence density, not line length.** The
  old `avg_len > 60` rule split markdown across two datasets with two
  different field names — adding `## headings` to a document made it *more*
  likely to be treated as a record stream, which split BM25 statistics
  across two corpora. Terminal-punctuation density separates prose
  (0.43–0.57 on the measured corpus) from logs and source (0.00–0.20);
  CSV rows fall on the record-stream side of the same threshold.

### Changed — autoindex (what it produces)

- **Line-oriented text is chunked into overlapping windows** (40 lines, 10
  overlap) instead of one document per line, and prose sections dropped
  from 32 KB to 2 KB with overlap, because BM25 scores per document and a
  single line rarely contains the caller's own wording. Measured on this
  repository (234 files, 170k LOC + docs + 460 commit messages) with 8
  "where/why is X" questions: 3/8 → 7/8 answered, 162,883 → 5,508 records,
  indexing 234 s → 1.9 s (commit `4510189`). Every chunk carries
  `start_line`/`end_line`.
- **`semantic_text` election is by language, not length.** The rule was
  "largest `text` field with `avg_len >= 200`", which elected 300-character
  base64 and concatenated-id columns while skipping a genuinely semantic
  150-character summary. A field must now actually look like natural
  language — `word_ratio >= 0.55` (tokens matching `[A-Za-z]{3,}`) **and**
  `mean_tokens >= 3`. Measured `word_ratio` is 0.00 for
  trace_id/user_id/order_id/numeric columns and 0.78–1.00 for prose, log
  messages and source. This changes which field gets embedded, so it
  affects both semantic-search quality and ingest cost (the built-in neural
  backend measures ~2.8 docs/s). The election note records the numbers that
  drove the decision.

### Added — autoindex

- **`--bulk-timeout-secs`** (default 300, range 1–3600) applies only to
  `POST /_bulk`, so a legitimately slow neural bulk can be accommodated
  without loosening the deadline on control requests. Retries are bounded
  at six attempts with the identical body and deterministic document IDs.

### Docs

- **WordPress core security audit case study**
  (`docs/case-studies/wordpress-security-audit/`): a reproducible record of
  an AI agent auditing real WordPress core (1,492 PHP files, ~619k lines)
  with XERJ as the retrieval substrate — sink census, interprocedural taint
  analysis, authorization graph, POP-gadget hunt — plus a copyable Claude
  Code skill and a step-by-step playbook. The honest result is that **core
  came back hardened**: the documented findings are three Medium-severity,
  known-class items (an incomplete SSRF deny-list missing `169.254.0.0/16`,
  an ImageMagick parse surface, and an unguarded `role` sink in
  `user-new.php`), a fourth downgraded to not-reachable-in-core, and
  verified negatives for IDOR, SQL de-escaping, and sanitizer composition.
  None of it is claimed as a novel 0-day. The published grep-wins
  counter-examples are shown alongside the wins, and the XERJ bug the audit
  surfaced (the keyword-array `term` defect partially fixed above — the
  memtable half) is disclosed.
  Site use case 06 (`/use-cases/code-security-audit.html`) is built from it.
- **Token-usage guidebook** (`docs/TOKEN_USAGE.md`): a measured decomposition
  of what a XERJ answer costs an agent in tokens — envelope overhead,
  answer, and materialized intermediate data — instead of hand-waving.
- **AST + graph + FTS vulnerability research**
  (`docs/research/ast-graph-vuln-detection.md`, `docs/examples/ast-vuln-graph/`),
  including a multi-language tree-sitter taint scanner tested at real
  WordPress scale.
- Verified embedding examples: XERJ with Google AI (EmbeddingGemma, Gemini
  API, ADK) and with any OpenAI-compatible `/v1/embeddings` endpoint.

### Known limitations

Known-open items found while validating this release and **not** fixed in
it. The first three are zero-hit defects — a query returns no documents
that ES would return:

- **`multi_match` looks inverted.** On a 6,022-document index a long query
  returned 0 hits with `operator: "or"` (the ES default) and 2 hits with
  `operator: "and"`. OR must be a superset of AND.
- **`match` against a `semantic_text` field returns 0 hits** for a long
  natural-language string, while the same query against a plain `text`
  field returns thousands.
- **`term`/`terms` on a multi-valued keyword field can still miss after a
  flush** — only the memtable half of that fix landed (see the first bullet
  of "Fixed — query semantics").
- **`autoindex` dataset clustering merges same-shape files** with different
  subjects: 213 source files sharing the schema `{text}` collapse to one
  index, and embedding centroids separate the subject groups only weakly.
- 3 of 1,363 ES-YAML conformance cases are skipped.

## [1.0.0-rc.4] - 2026-07-22

Fourth release candidate: the **production-hardening release**. A 9-review
release-readiness audit produced 17 blockers and ~60 follow-on items; all
were fixed across four hardening waves and verified against the live binary
(ES-YAML conformance 1360/1363, full-matrix benchmark 52 WIN / 0 LOSE / 25
TIE vs live ES 8.13.4). Headline for users: acknowledged writes survive
crashes, wrong-but-200 responses are gone, the node defends itself under
resource pressure, and the bundled Console gains Kibana-quality editable
dashboards.

### Fixed — durability (acknowledged writes survive failure)

- **Acked-write loss closed:** verified WAL prune + power-loss-ordered
  publish chain; `wal_sync="sync"` honored on ALL bulk paths and the
  `wal_batch_ms` fsync loop actually implemented; torn-frame recovery so a
  disk-full/crash tear cannot poison a WAL generation; consecutive `_bulk`
  delete actions no longer dropped; acked deletes survive restart
  (WAL-shard pinning); delete tombstones end WAL pinning segment-durably.
- **Merge-window reads:** GET never 404s during the merge-publish window;
  merges can never silently drop docs; `_forcemerge` is synchronous and
  quiescent like ES.
- **Startup/data safety:** exclusive `node.lock` on the data dir (second
  process fails fast); data-dir format marker refuses newer-than-supported
  or corrupt dirs BEFORE any destructive GC; refuse-on-corrupt snapshot
  restore; HNSW persistence fsyncs file + dir around rename; periodic
  background flusher no longer aborted at spawn; sharded-WAL FTS replay
  restored on reopen.

### Fixed — correctness (no silent wrong answers)

- **Fail-loud sweep:** the silent-wrong-query classes on `_search` are
  rejected with real 400s (unknown fields, unsupported constructs), as are
  CCR auto-follow (501), remote reindex, `has_child`/`has_parent`, learned
  fusion, and SQL `HAVING` — previously all silently returned wrong data.
- **Doc CRUD wire semantics:** real per-doc `_version` and ES seq_no
  convention; `POST /{index}/_doc/{id}` route added; malformed bulk docs
  rejected per-item with ES-shaped 400s instead of stored as empty `{}`.
- **Aggregations:** real `sum_other_doc_count`; composite bucket keys typed
  from the source field mapping; `multi_terms` raises `too_many_buckets`
  as a real 400 past the cap; `top_hits` emits the doc's real `_seq_no`.
- **Query semantics:** ES-exact date resolution for range bounds (rounding,
  format, date math); Painless compares strings as strings (every string
  previously compared equal) with depth + source-length guards; highlight
  offsets correct on multibyte text; `combined_fields` OR pooling;
  `query_string` fallback discloses operator handling; kNN threads
  filter+boost through top-level kNN and honors similarity cutoffs.
- **Doc-values counting (P0):** a `range` filter on non-numeric values
  admitted every memtable document in `size:0`/`_count`/filter-agg paths
  (a one-day date window over-counted 3.4×); date/keyword range bounds now
  compile to the columnar fast path instead of falling to the brute scan.
- **Multi-valued fields:** a field that is multi-valued anywhere in a
  segment no longer ships a lying doc-values column that silently dropped
  those docs from count shortcuts — consumers fall back to the exact scan.

### Added — resource governance (the node defends itself)

- Parent circuit breaker keyed on ACTUAL RSS, global search pool, disk
  flood-stage watermark, per-query memory guard, ANN coverage guard, and a
  search timeout that actually preempts term-dictionary walks; scroll and
  async-search contexts are TTL-swept and capped. Classic node-killers
  (huge `size`, deep pagination windows, bucket explosions) return bounded
  400/429 instead of taking the process down.

### Added — security

- gRPC listener authenticated; health probes exempt from auth; constant-time
  compare for the admin API key; `admin.key` and TLS private keys created
  0600; CORS configurable and restrictive by default; API keys persist
  across restart with an honest role surface; `/_memory` list paginated
  with a documented auth model.

### Added — Console: Kibana-quality editable dashboards

- Durable backend CRUD for dashboards (create/replace/patch/delete with
  ETag optimistic concurrency) — user dashboards survive localStorage
  clears AND server restarts; a real panel builder with live preview
  (11 viz types, index/query/metric pickers); free-form `{x,y,w,h}` panel
  resize + move; first-launch seeding of 13 built-in dashboards as durable
  managed rows; edit-mode chrome no longer overlaps titles or the sub-nav.

### Added — observability

- ES `_stats`/`_cat` surfaces and the 101-series Prometheus endpoint
  reflect real load (docs, bytes, search/indexing counters); slow-query
  log; structured logging minors; `_cat/indices` uuid + bytes columns and
  ES-shaped snapshot responses.

### Changed — performance

- **kNN flipped:** HNSW-served top-level kNN — official benchmark cell
  23,325 ms → 1.87 ms at recall@10 1.00 (vs ES 0.80).
- **Date-filtered aggregations:** 41–49× (one-day window 9.9 s → 241 ms)
  via keyword/date columnar range predicates; filtered `extended_stats` /
  `percentiles` / `percentile_ranks` / `median_absolute_deviation` served
  columnar with filter-aware gathers (11–264×).
- **Scored-columnar family at the ES floor:** multi_match, query_string,
  fuzzy, prefix/wildcard, highlight, match_phrase, deep pagination,
  `more_like_this`, `function_score`, composite aggs, `rare_terms` /
  `significant_terms` / percentile families — full-matrix result
  52 WIN / 0 LOSE / 25 TIE against live ES 8.13.4.
- Mixed read-under-write hardening: one memtable walk per query, flush cap,
  merge-publish count seeding, open-loop iso-load writer for honest
  measurement.

### Fixed — autoindex & agent search path

- `xerj autoindex` no longer aborts the whole run on ordinary UTF-8 in the
  SQL-dump sniffer (byte-buffer accumulation; junk files are skipped and
  recorded, never fatal) and no longer mojibakes non-ASCII SQL values.
- `highlight` is applied before `_source` filtering, so fragment-only
  responses work (measured: 3.2× fewer tokens into an agent context at
  equal recall).

### Docs

- Honesty ledger: canonical audited scorecard, ROADMAP claims flipped to
  measured reality, phantom-claim purge across README/site/docs.
- Production recipes: TLS + auth hardening, air-gapped deploy, ES→XERJ
  migration.

## [1.0.0-rc.3] - 2026-07-10

Third release candidate. Headline: XERJ gains a **built-in neural embedder** —
real in-process BERT semantics with no Python and no external service — behind a
single backend-agnostic embedding handle, plus two new end-to-end-validated
retrieval recipes.

### Added

- **Built-in neural BERT embedder — shipped in the binary.** A pure-Rust sentence
  encoder via `candle` (default `all-MiniLM-L6-v2`, 384-dim) that runs in-process
  and **downloads its weights (~90 MB) automatically on first use** (or reads them
  from `embedding.local_model_dir` for air-gapped deployments). It is compiled into
  the default release binary — end users just add `--embed-mode neural` at runtime,
  no special build and no separate binary. A progress bar and one-time-download log
  make the first run legible. The binary is ~36 MB as a result; a
  `--no-default-features` slim build without the neural backend is ~23 MB.
- **Unified three-backend embedding handle (`xerj_ai::Embedder`).** `semantic_text`
  ingest and `semantic`/`hybrid` queries run through one of three interchangeable
  backends — **lexical** (default, zero-dep feature-hash), **neural** (built-in
  BERT), or **proxy** (external OpenAI-compatible `/v1/embeddings`) — selected with
  `embedding.mode`, the `--embed-mode` flag, or `XERJ_EMBED_MODE`. Misconfiguration
  degrades to lexical, never a crash; `auto` preserves the historical behaviour.
- **Recipe — All-you-can-eat search.** One corpus retrieved five ways from a single
  index: full-text (BM25), semantic, vector kNN (more-like-this), hybrid (RRF), and
  semantic-scoped-by-keyword-filter. Guide `docs/recipes/all-way-search.md`,
  runnable `recipes/all_way_search.py`.
- **Recipe — Zero-config folder → neural semantic search.** `xerj autoindex` a
  mixed-format folder against a `--embed-mode neural` server, then search the
  discovered prose by meaning while structured files stay exactly filterable. Guide
  `docs/recipes/autoindex-semantic-search.md`, runnable `recipes/autoindex_semantic.sh`,
  sample corpus `demo/data/support-folder/`.

### Changed

- `--embed-mode {lexical|neural|proxy|auto}` CLI flag and `XERJ_EMBED_MODE` env on
  the server; new `embedding.{mode,neural_model,model_cache_dir,local_model_dir}`
  config keys.
- Documentation updated for honesty consistency (README, AGENTS.md, ROADMAP.md,
  llms.txt, recipe guides): the **default** embedder is lexical; the neural embedder
  is an **opt-in** upgrade — output is only described as neural when that mode runs.

## [1.0.0-rc.1] - 2026-07-06

First public release candidate of XERJ — an Elasticsearch-wire-compatible search,
vector, and log-analytics engine written in Rust and licensed under Apache-2.0. This
is a release candidate: the wire protocol and on-disk format are considered stable
for evaluation, but may still change before the final 1.0.0.

### Added

- **Elasticsearch-compatible REST API.** Drop-in wire compatibility with the ES
  8.x HTTP surface, served from `xerj-api` (`es_compat.rs`) on port `9200`:
  - Document APIs: `PUT`/`GET`/`DELETE /{index}/_doc/{id}` and
    `POST /{index}/_update/{id}`.
  - Search: `POST /{index}/_search` with `query`, `from`, `size`, `sort`, `aggs`,
    `_source`, and `highlight`.
  - Bulk API: `POST /_bulk` with `index`, `create`, `update`, and `delete` actions.
  - Scroll API: `POST /{index}/_search?scroll=1m` and `POST /_search/scroll`.
  - `POST /{index}/_delete_by_query`, index templates (`PUT /_index_template/{name}`),
    and aliases (`POST /_aliases` with `add`/`remove`).
- **Full-text search (`xerj-fts`).** BM25 scoring with an analyzer registry and
  on-disk postings lists. Supported query types include `match_all`, `match_none`,
  `match`, `match_phrase`, `match_phrase_prefix`, `multi_match`, `term`, `terms`,
  `range`, `prefix`, `wildcard`, `exists`, `ids`, `bool`, `fuzzy`, `regexp`,
  `query_string`, `simple_query_string`, `constant_score`, `boosting`, `dis_max`,
  and `geo_distance`.
- **Vector search (`xerj-vector`).** Dense-vector HNSW index for k-NN and semantic
  search, exposed through the `knn`, `semantic`, and `hybrid` query types.
- **Aggregations.** `terms`, `stats`, `avg`, `sum`, `min`, `max`, `value_count`,
  `cardinality`, `range`, `histogram`, `date_histogram`, `percentiles`, `filter`,
  `missing`, and `composite`, with a columnar fast path for `size: 0` aggregations.
- **Sharded ingest and storage (`xerj-storage`).** Write-ahead log with a single
  monotonic sequence-number writer, a 16-shard in-memory memtable
  (`shard = xxh3_64(doc_id) & 15`), flush to immutable segments, and background
  segment merging. WAL replay rebuilds both the storage and FTS memtables on restart.
- **Log analytics (`xerj-logs`).** Columnar log ingestion with retention.
- **AI helpers (`xerj-ai`).** Text chunking, an embedding proxy, and a memory store
  for semantic workflows.
- **Clustering (`xerj-cluster`).** Embedded Raft consensus for cluster metadata with
  no external dependencies.
- **Bundled console (`xerj-console-api`).** Dashboards, auth, preferences, and
  cluster awareness, compiled into the `xerj` binary and mounted under
  `/_xerj-console/api/v1/*`.
- **Transform pipeline (`xerj-wasm`).** Built-in transform plugins with an optional
  WASM backend.
- **Block compression (`xerj-compress`).** LZ4 and Zstd codecs for segment blocks.
- **Single static binary.** `cargo build --release -p xerj-server` produces `xerj`;
  run with `./target/release/xerj --data-dir ./data --insecure`.
- **ES-YAML conformance harness.** A workspace test runner (`es-yaml-runner`) that
  executes the ES 8.13 REST-API-spec YAML suites (search, aggregations, vectors,
  bulk, indices, scroll, cluster) against a live server. XERJ passes 1,326 of 1,329
  cases.
- **Reproducible head-to-head benchmarks.** A 91-cell XERJ-vs-Elasticsearch-8.13
  matrix (ingest, read, vector, and disk dimensions), published and reproducible at
  <https://xerj.org/benchmarks>. The scorecard is honest about both wins and losses.

### Changed

- `_forcemerge` is now synchronous and quiescent, matching Elasticsearch semantics,
  and merge status is exposed through `_stats`.
- Search hit materialization for `size > 0` is bounded to the top `from + size`
  candidates, reducing per-query cost from O(N) toward O(from + size).
- Bulk ingest avoids redundant JSON round-trips and batches schema evolution to
  raise throughput under concurrent load.

### Fixed

- Consecutive `_bulk` `delete` actions that were previously dropped are now applied
  correctly.
- `hits.total` for `size > 0` searches is delete-aware, resolving a conformance
  regression.
- Corrected top-N sort behavior and delete-awareness across the memtable/segment
  merge path.

### Known limitations

- 3 of 1,329 ES-YAML conformance cases do not yet pass.
- This is a release candidate; some Elasticsearch APIs and query/aggregation options
  outside the list above are not yet implemented. See
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and `engine/CLAUDE.md` for the
  current supported surface.

[Unreleased]: https://github.com/xerj-org/xerj/compare/v1.0.0-rc.5...HEAD
[1.0.0-rc.5]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.5
[1.0.0-rc.4]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.4
[1.0.0-rc.3]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.3
[1.0.0-rc.2]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.2
[1.0.0-rc.1]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.1
