---
name: xerj-code
description: Reference-coding with XERJ. Clone the libraries that already solved your problem, index them locally, and retrieve the exact implementation before writing code — so the agent reads passages instead of re-deriving algorithms across retry loops. Use when starting a task in an unfamiliar API, porting an algorithm, or when you have already looped twice on the same error.
---

# XERJ.code — reference coding

An agent that does not know an API guesses, runs, fails, and guesses again. Each
lap costs **output** tokens, which are the expensive kind. This skill replaces
laps with a lookup: clone the repositories that already contain a correct
implementation, index them with XERJ, and retrieve the passage before writing.

The trade is deliberate. Retrieval spends **input** tokens, which are cheaper per
token and cacheable; loops spend **output** tokens. See [`COSTS.md`](../../docs/case-studies/reference-coding/COSTS.md) for
the arithmetic and for the conditions under which this trade **loses**.

> **Status, stated plainly: measured, and the value is real but narrow.**
> (Full case study with real tokens and dollars: [`docs/case-studies/reference-coding/CASE_STUDY.md`](../../docs/case-studies/reference-coding/CASE_STUDY.md), 2026-08-05.)
>
> The comparison that matters is against **native Claude Code** — the same agent
> with tools that would grep the source itself — not a bare model. Measured across
> three regimes, same agent, one objective verdict (compiles + passes hidden tests):
>
> - **Unfamiliar code with a contract the model can't recall** (a seal, a
>   generational handle, a lazy refill, a specific hash scheme — measured across
>   **seven purpose-built libraries, 21 runs**): native and xerj both solve
>   **21/21**; **xerj uses 1.5× fewer output tokens and costs 1.3× less than
>   native**, because retrieval replaces the grep. Pure memory **fails 1/21** and
>   burns **6.5× the cost** flailing. This is the win, and it is decisive. The
>   compiler can leak an API *name* across a retry loop, but never a *contract*.
> - **Memorised code** (any popular public crate — even a 256-value table, or a
>   protobuf-style varint): retrieval is **overhead**. Pure memory is cheapest;
>   injecting a big reference can be the *worst* arm, because the model re-emits
>   what it was handed.
>
> So: **use this when the reference is code the model has not memorised** — your
> own private/proprietary code, an internal API, a post-cutoff or niche library.
> On public library references the model already knows, it costs more than it
> saves. The native-vs-xerj gap grows with corpus size (grep pulls the whole tree
> into context; retrieval pulls one passage).

Since [#977](https://github.com/xerj-org/xerj/issues/977) the whole loop is
**inside the `xerj` binary** — `xerj corpus add` / `xerj corpus index` /
`xerj code` — plus the `xerj_code_search` MCP tool for agents without a shell.
The old wrapper scripts (`xc-corpus.sh`, `xc-index.sh`, `xc.py`) are gone; your
existing `~/.xerj-code` corpora and indexes work unchanged (see
[Migration](#migration-from-the-wrapper-scripts-977) below).

## Standing corpora for this repository (MANDATORY since 2026-08-06)

In the xerj repo, retrieval-before-writing is **required** for non-trivial
engine work, not optional. Each corpus below has a pinned, licence-reviewed
definition in [`hub/`](hub/) — `xerj corpus add --from hub/<name>.json` rebuilds
it at the same commits anyone else is using:

| corpus | projects | use it for |
|---|---|---|
| `xerj-search` | lucene, tantivy, meilisearch, quickwit, sonic, elasticsearch | FTS, BM25, postings, merge policy, typo tolerance, segment layout, ES wire semantics |
| `xerj-vector` | qdrant, usearch, instant-distance, hnswlib | HNSW build, neighbour heuristics, quantisation, filtered kNN |
| `xerj-storage` | sled, fjall, redb | WAL, flush epochs, crash recovery, compaction, page allocation |
| `xerj-columnar` | clickhouse | columnar storage, aggregation execution, compression codecs, vectorised scans |

**Approach-only sources:** `elasticsearch` (AGPL-3.0 / SSPL-1.0 / Elastic-2.0)
and `sonic` (MPL-2.0). Never copy their code into Apache-2.0 XERJ; for ES,
pasting source would also falsify the project's public "shares no code with
Elasticsearch" claim. `meilisearch` is MIT at the core but **BUSL-1.1 for its
Enterprise Edition** parts — check the header/path first. Read the design,
write our own implementation. Each manifest's `review.use` field says which
bucket a repo is in; `hub/README.md` explains the three.

## Choosing a corpus for the project at hand

The four engine corpora above are this repository's standing set. The full
live catalogue — every corpus pinned, licence-reviewed and **G7-graded**
(a five-question retrieval gate whose suite was pre-registered *before*
retrieval ran) — is browsable at **[hub.xerj.org](https://hub.xerj.org)**
and queryable live with `xerj corpus list --url <node>`. Pick by the
project's domain, not by size:

| the project is… | start with | why |
|---|---|---|
| a search / query engine | `xerj-search` | postings, BM25, merge policy, segment layout, ES wire semantics |
| vector search / ANN | `xerj-vector`, `faiss`, `annoy` | HNSW build, neighbour heuristics, quantisation, filtered kNN; faiss is the reference ANN implementation, annoy the forest variant |
| storage engine / KV | `xerj-storage`, `duckdb`, `rocksdb`, `leveldb`, `badger`, `lmdb`, `pebble`, `valkey-src` | WAL, crash recovery, compaction, page allocation; LSM-vs-B trees; pebble for L0 sub-levels and range-deletion semantics; valkey for an in-memory KV server's data structures |
| analytical / columnar | `xerj-columnar`, `duckdb`, `arrow-rs`, `parquet-format`, `iceberg` | columnar layout, codecs, vectorised scans, table formats |
| a database server | `sqlite-src`, `postgres-src` | parsers, planners, executors — the two most-studied codebases in the field |
| messaging / consensus | `kafka-src`, `rabbitmq`, `nats-server`, `etcd-src`, `raft-rs`, `dragonboat` | log compaction, delivery semantics, quorum/lease correctness |
| monitoring / timeseries | `influxdb`, `prometheus-src` | TSDB layout, downsampling, scrape/ingest paths |
| TLS / crypto / tunnels | `rustls`, `openssl`, `s2n-tls`, `boringtun` | modern-TLS-by-hand, hardening precedent, constant-time patterns |
| QUIC / HTTP / networking | `quinn`, `nghttp2`, `curl-src`, `libuv`, `caddy`, `envoy`, `nginx` | protocol state machines, connection pooling, event loops |
| compression / serialization | `zstd`, `lz4`, `brotli`, `zlib-ng`, `snappy`, `protobuf`, `flatbuffers`, `capnproto`, `msgpack-c` | format design, blocked/entropy coding, zero-copy schemas |
| secrets / config serving | `openbao` | KV store with seal/unseal, lease machinery |
| API design review | `rust-api-guidelines`, `msft-api-guidelines`, `zalando-restful-api-guidelines`, `google-eng-practices` | naming, error shape, versioning; Zalando's is the most clause-complete REST policy set, google-eng-practices owns code-review doctrine |
| protocol/spec conformance | `openapi-spec`, `graphql-spec`, `json-schema-spec`, `w3c-css`, `whatwg-specs`, `ecma262`, `nats-protocol`, `kafka-protocol` | the normative text an implementation is judged against — HTML/DOM/URL living standards, ECMAScript grammar, wire-protocol framing |
| locating the right RFC before reading it | `ietf-rfc-abstracts` | one record per RFC (9,015): number, title, date, abstract — "which RFC actually covers X" without pulling full texts (TLP-5.0) |
| production ops / SRE | `gitlab-runbooks`, `school-of-sre`, `tldr-pages`, `mozilla-ssl-configs`, `twelve-factor` | real incident playbooks, a full SRE course curriculum, command syntax by tool, deployed TLS configs, deployment doctrine |
| administering Unix systems | `freebsd-handbook`, `openbsd-faq` | rc/ZFS/jail/networking procedures, OpenBSD's canonical FAQ positions |
| documenting against a hosted API | `github-api-description` | the OpenAPI description of github.rest — field semantics, enum values, preview headers |
| datastore operations docs | `valkey-docs`, `etcd-docs` | command semantics, config knobs, tuning guidance — the operational reference for each |
| telemetry / observability wiring | the semconv half of `otel-proto` (candidate — G7 2/5, parked to the semantic lane) | OpenTelemetry semantic conventions for attribute names; lexical rounds could not bridge paraphrase-to-normative-prose |
| internationalisation | `unicode-cldr` | CLDR locale data structure, calendar/number/date skeletons |
| security review of Rust dependencies | `rustsec-advisories`, the `rust-vulns` pack (`xerj corpus add --from` the hub recipe) | one text record per advisory (1,963: RustSec + crates.io OSV, zero alias overlap): affected versions, fixed releases, affected-function symbol lists on 271; the pack stays the vehicle for patched-pair mining (#1111's defect-class study) |
| security review of Python dependencies | `pysec` | PYSEC advisory records with affected and fixed versions, CVE/GHSA aliases |
| cross-ecosystem dependency audit | `ghsa-db` | GitHub-reviewed advisories (36,263 text records): CVSS vectors, patched ranges, affected packages across npm/PyPI/Go/Rust/maven and the rest |
| security review of Go modules | `go-vulndb` | Go-team-curated advisories (4,577): affected packages and symbols, fixed versions, GO ids with CVE aliases |
| regex denial-of-service precedent | `redos-precedent` | 634 regex-specific ReDoS advisories (CWE-1337/185 class, not bare CWE-400): the pathological input, the affected function, the bounded rewrite |
| crypto misuse precedent | `crypto-misuse-precedent` | 721 CWE-326/327/328/329/347-class advisories: weak ciphers, hardcoded keys, nonce reuse, signature-verification bypass, padding oracles, ECB |
| vulnerability / weakness taxonomy | `mitre-cwe`, `mitre-capec` | CWE weakness definitions and CAPEC attack patterns, id-level |
| adversary techniques / detection engineering | `mitre-attck` | ATT&CK v19.2 per-technique records: technique ids, tactics, detection data sources, procedure examples (enterprise/mobile/ics; software and group objects out of scope) |
| patch-priority triage | `cisa-kev` | the Known Exploited Vulnerabilities catalogue, one record per CVE — exploitation status in the wild, binding remediation dates |
| web application security how-to | `owasp-cheatsheets` | the Cheat Sheet Series' countermeasure recipes per topic — session management, authentication secrets, error handling, upload handling |
| planning a web app penetration test | `owasp-wstg` | the Web Security Testing Guide v4.2 catalogue: per-test objectives and method, organised by category (authn, authz, session, input handling) |
| triaging an active campaign | `cisa-alerts` | CISA advisory IOCs, required actions and mitigations (2025-05..2026-10 window) |
| querying endpoint visibility | `osquery-config-examples` | real deployed osquery packs and scheduled-query shapes |
| non-software (tax, employment, UK law) | `ecfr-title-12`, `ecfr-title-29`, `ecfr-title-26`, `uk-legislation` | consolidated current regulation, section-per-file |
| aviation (FARs) | `ecfr-title-14` | 14 CFR section-per-file: visibility minimums, transponder airspace, inspection intervals, right-of-way |
| maritime rules of the road | `navrules` | COMDTINST M16672.2D per-rule text — risk of collision, narrow channels, crossing situations, lights and sound signals |
| accessible UI components | `govuk-design-system` | GOV.UK component options and accessibility criteria — the exact option semantics (maxwords vs maxlength, divider text, focus-on-load) |
| pharma / drug labels | `dailymed` | FDA label sections: indications, contraindications, administration timing |
| clinical treatment guidance | `cdc-clinical` | CDC STI treatment regimens and doses, syphilis desensitization, measles clinical signs |
| food-service safety | `fda-food-code` | FDA Food Code 2022 provisions: hand-contact surfaces, Time as a Public Health Control, date marking, bare-hand readiness |
| traffic control devices | `mutcd` | FHWA MUTCD 11th Ed (Rev 1): sign and signal warrants, markings, work-zone typical applications |
| building works (England) | `uk-building-regs` | Building Regulations 2010 (SI 2010/2214) per-provision: Parts A–P requirements, Wales variants labelled, 44ZB/44ZC dual numbering documented |
| EU digital regulation | `eurlex-core` | GDPR, AI Act, DSA, DMA, NIS2 per-article and per-recital from the EUR-Lex Cellar (annexes excluded by design) |
| accessibility (built environment) | `ada-2010` | ADA 2010 Standards: reach ranges, clear floor space, slope ratios, per-section |

**Inside the security block, pick twice — ecosystem first, defect class
second.** Advisories about the same CVE exist in several databases; start
with the corpus whose ecosystem matches the project (`rustsec-advisories`
for Rust crates, `pysec` for PyPI, `go-vulndb` for Go modules,
`ghsa-db` when the tree is mixed or the ecosystem is unclear — it is the
superset, GitHub-reviewed). Then, when the review is about a *pattern you
are looking at* rather than a package — a regex fed user input, a cipher
or key-management call site — switch to the stratum corpus
(`redos-precedent`, `crypto-misuse-precedent`): smaller, selected for
exactly that defect class, so the mechanism (the pathological input, the
weak primitive, the bounded rewrite that fixed it) ranks instead of
thousands of generic advisories.

Three rules that keep the choice honest:

1. **A corpus only helps on code you have not memorised.** On popular
   public library code the model already knows, retrieval is overhead —
   the measured crossover is in `measure/DEVQA_RESULTS.md`. Private,
   internal, niche and post-cutoff code is where it wins decisively
   (bare-P 20% vs with-corpus-X 98% across 33 measured corpora —
   `benchmarks/corpus-tasks/IMPACT.md`).
2. **Shape beats size.** A corpus whose answer sits in one giant file
   (a 3 MB single-page spec, a 50 MB JSON bundle) loses to a
   section-per-file mirror of the same content — whole-file indexing skips
   what it cannot chunk, and near-duplicate bundles crowd the answer. The
   hub's G7 gate exists to catch exactly this; corpora that failed it were
   demoted with the fix pre-registered, not padded into "live".
3. **Cite what you adapted.** A retrieved hit is evidence of how *that*
   project solved it under *its* licence and constraints — `file:line` in
   the comment, and check the manifest's `review.use` bucket before
   copying anything.


### Operational traps, both hit in practice

- **Nothing under `/tmp`.** Clones live in `~/.xerj-code/corpora/`, the index in
  `~/.xerj-code/data` (override the root with `XERJ_CODE_HOME`). A `/tmp` data
  dir is lost on reboot and the whole corpus silently retrieves nothing.
- **`xerj corpus index <corpus> --fresh` is the rebuild, and it never costs you
  a working index.** It builds a replacement *beside* the existing one — under
  `xc-<corpus>-b<stamp>-*`, with an autoindex `--state-dir` of its own in
  `~/.xerj-code/autoindex-state/<corpus>/` — verifies it (autoindex exited `0`
  or `3` **and** `_count > 0`), switches `state/<corpus>.json` to it by atomic
  rename, and only then deletes the old indices, by exact name. A build that
  fails or comes back empty is removed and the old index, the old state file
  and the old state directory stay exactly as they were. Use it after the data
  dir was wiped or moved, when `xerj code` says the index is older than 30
  days, or when a plain re-run says the state directory "cannot become
  generation authority". It is **not** `xerj autoindex --fresh`, which the
  command never forwards: that flag only discards autoindex's resume journal
  and is refused outright once a corpus generation has committed — which is why
  `--fresh` used to fail on every corpus that had been indexed before
  ([#930](https://github.com/xerj-org/xerj/issues/930)).
- **A record count the node does not answer is never read as zero.** The count
  is a tri-state: a number, a 404 (no index matches — genuinely zero), or
  *Unknown* (timeout, 5xx, unparseable reply). *Unknown* authorises no delete
  and no swap: if the node cannot count the existing index, it is presumed to
  be a working one and no failed build is kept over it; if it cannot count the
  new build, nothing is deleted and nothing is switched. Only a number — or a
  404 — authorises a delete or a swap.
- **An interrupted FIRST build is kept, and `xerj code` says so.** When a build
  fails after writing records and there is no working index to fall back to,
  the command keeps it rather than leave no corpus at all, and records
  `salvaged: true` plus the real `autoindex_exit` in the state file. Such a
  build can be partial, so `xerj code` warns on stderr with every query that
  coverage is INCOMPLETE — **a miss is then not evidence that the code is
  absent** — and `xerj corpus list` marks the corpus. A plain
  `xerj corpus index <corpus>` resumes it (same prefix, same state directory)
  and clears the mark when it finishes.
- **A plain `xerj corpus index <corpus>` updates in place.** It re-runs
  autoindex against the recorded build's prefix and state directory, so
  additions, edits, deletions and renames reconcile incrementally.
- **The state file has two prefixes on purpose.** `prefix` is always
  `xc-<corpus>` — the whole namespace, so anything that globs `xc-<corpus>*`
  keeps working across rebuilds. `index_prefix` is the one verified build, and
  it is what `xerj code` queries, so a half-built replacement never leaks into
  answers during a rebuild.
- **During a rebuild both builds exist**, so the node briefly holds the corpus
  twice. Budget disk for it on a large corpus.
- Always verify: `curl -s "$URL/xc-<corpus>*/_count"` must be > 0. The command
  does this itself and says so; do it again if you are scripting around it.

## When this is worth it

Use it when the task touches code someone else has already written correctly:

- an unfamiliar library, framework, or protocol
- porting an algorithm whose reference implementation exists
- matching an established convention across a large codebase
- **any time you have already looped twice on the same error** — that is the
  signal that guessing is not converging

Do not use it for code that only exists in this repository (ordinary file reads
are cheaper), for one-line edits, or when you already know the API. Indexing has
a fixed setup cost; a task you would finish in two tool calls will not repay it.

## The loop

```
xerj corpus add <name> <git-url>...   # clone reference repos (once per domain)
xerj corpus add --from <manifest>     # ...or rebuild a corpus someone else defined
xerj corpus build <name> --recipe <r> # ...or harvest RECORDS into a portable pack
xerj corpus index  <name>             # index them with xerj autoindex (once per corpus)
xerj code <name> "<what you need>"    # retrieve before writing (every task)
```

Status at any time: `xerj corpus list` shows every corpus in the ledger, its
index prefix, whether it is **loaded on this node** (`— loaded — N index(es)` /
`— NOT loaded here (0 indices) — stale/other-server`), and any INCOMPLETE
marks.

### 1. Build a corpus

Group repositories by *problem domain*, not by language. A corpus for "async
Rust" holds tokio, hyper, and tower; a corpus for "parsers" holds nom, pest, and
tree-sitter. Keep corpora small and sharp — a corpus that contains everything
retrieves like a search engine with no query.

```sh
xerj corpus add async-rust \
  https://github.com/tokio-rs/tokio \
  https://github.com/hyperium/hyper
```

Clones are shallow (`--depth 1`) and land in `~/.xerj-code/corpora/<name>/`.
Every build writes a `corpus.json` beside them — URLs, full commit SHAs and
licences, a few hundred bytes, no source. That file *is* the shareable corpus
definition: hand it to someone else and

```sh
xerj corpus add --from their-corpus.json
```

rebuilds the same commits on their machine (an existing clone is moved to the
recorded SHA, so both trees match). Vetted definitions for the corpora this
project uses live in [`hub/`](hub/):

```sh
xerj corpus add --from hub/xerj-storage.json && xerj corpus index xerj-storage
```

Corpus names and repo names are validated before anything touches the
filesystem — a name with `/`, `*`, a leading `.`, or a reserved word is
rejected at parse time, because the pinned-clone checkout runs `git checkout
--force` inside that path.

Not every corpus is source code. **Record packs** — advisories, datasets, any
structured data — are built by a recipe instead of cloned:

```sh
xerj corpus build rust-vulns --recipe tools/packs/rust-vulns/recipe.toml
xerj corpus add rust-vulns --from ~/.xerj-code/builds/rust-vulns/pack/rust-vulns
```

The recipe declares sources, identity edges (the same vulnerability arriving as
RUSTSEC/GHSA/CVE becomes ONE record), merge precedence and derived fields; the
pack it emits is checksummed and installs with the same `corpus add --from` as
a corpus definition. A pack from someone else (e.g. the daily
[rust-vulns releases](https://github.com/xerj-org/xerj/releases?q=pack-rust-vulns))
carries an ed25519 signature over its SHA256SUMS — add it with
`--verify-sig <pubkey-file>` to check origin before anything is indexed
(the key ships beside the recipe, never inside the pack). The in-repo example
is [`tools/packs/rust-vulns/`](../packs/rust-vulns/) — see its README for what
a measured pack looks like.

### 2. Index it

```sh
xerj corpus index async-rust
```

This runs `xerj autoindex` in-process against the local XERJ instance
(`--url` / `$XERJ_URL`, default `http://localhost:9200`). **Exit code 3 means
"completed with junk files" and is not a failure** — some files in any real
repository are unparseable. Treat 0 and 3 as success; anything else is real.
The node must be reachable; nothing is built offline.

### 3. Retrieve before writing

```sh
xerj code async-rust "graceful shutdown with a broadcast channel"
```

`xerj code` returns the **matching definition**, not a byte window:

```
─── valkey/src/networking.c  (score 12.68, BSD)
    [function addBulkStringToReplyIOV @ line 2635 — 366 of 272,498 chars]
static void addBulkStringToReplyIOV(char *buf, size_t buf_len, ...) {
```

That line number is the whole point. A record is one whole file, so ranking can
only tell you *which* file matched; the definition is located from the `symbols[]`
the index already carries. Before this, an 8 KB slice of a 272 KB file was a coin
flip on whether the answer was inside it — a query about null replies correctly
ranked `networking.c` and then returned its licence banner, because `addReplyNull`
lives at line 1460. Read the named definition, cite it, and if the top hits are
irrelevant say so and fall back to normal work rather than forcing them in.

`--full N` caps each passage at N chars (`--full 0` prints the file head and
says how to get the matching definition instead). `--no-symbol` falls back to a
raw window (only useful for data files with no symbols at all).

### Exit codes

`xerj code` distinguishes *no match* from *broken* from *not loaded* — script
around it:

| exit | meaning |
|---|---|
| `0` | hits (the passages are on stdout) |
| `1` | **no match** — including `--json` with an empty `hits.hits`; the fall-back prose tells you to fall back to normal work, not to retry |
| `2` | usage error, unreachable node, transport failure, 30-day staleness refusal, standalone-semantic failure |
| `3` | corpus is in `state/` but has **0 live indices on this node** — "This is NOT a 'no match' — the corpus simply is not loaded on this server." Re-index, or point `XERJ_URL` at the node that holds it |

This is deliberately different from sibling `xerj search`, which exits `0` on an
empty result — different tool, different contract.

### Retrieval modes

The default is **`bm25`**. Measured across two corpora and twelve hand-labelled
queries:

|          | rust-text |      | kv-oss (C) |      | combined |       |
|----------|----------:|-----:|-----------:|-----:|---------:|------:|
|          | top-1     | top-3| top-1      | top-3| top-1    | top-3 |
| **bm25** | 3/6       | 6/6  | **6/6**    | 6/6  | **9/12** |**12/12**|
| hybrid   | **5/6**   | 6/6  | 2/6        | 4/6  | 7/12     | 10/12 |
| semantic | 4/6       | 4/6  | 2/6        | 3/6  | 6/12     | 7/12 |

top-3 is the operative metric — the agent reads *k* passages, not one — and BM25
is perfect on it while never being the worst arm on either corpus. It is also
~5× faster, with no vector round trip and no mapping lookup.

Hybrid was previously the default, chosen on `rust-text` alone where it wins
top-1. That was a corpus-specific result: on the C corpus the vector arm reaches
only **5 of 407** indices (issue #173), so fusion mixes a good ranking with one
that cannot see 98.8% of the material. Use `--mode hybrid` when a corpus has
broad `semantic_text` coverage *and* you specifically care about top-1 — and
measure it before trusting it.

```sh
xerj code rust-text "lazy DFA cache eviction"                 # bm25 (default)
xerj code rust-text "lazy DFA cache eviction" --mode hybrid   # + vector, RRF-fused
xerj code rust-text "lazy DFA cache eviction" --mode semantic # vector only
```

The table above was measured with the script-era client-side fusion. Since #977
hybrid is **fused server-side**: one native top-level `hybrid` query with
`{"fusion":{"method":"rrf","k":60}}`, no client-side rank merging — so per-hit
per-arm ranks are gone (the server does not expose them) and every hybrid run
opens with an arms-ran note instead, e.g.
`[hybrid RRF(k=60) — BM25 over 9 index(es), vector over 3 of 9]`. On a corpus
where only some indices are semantic-capable, the fused query is aimed at the
capable set only and the note names the excluded lexical-only indices — a
semantic leg fired at a wildcard covering plain-text indices 400s the whole
request.

A hybrid answer opens with a line naming the arms that actually ran, and says
`BM25 only — ...` rather than pretending when the vector arm could not run (no
`semantic_text` mapping for `body`, or the fused request failed). **The vector
arm only works where `body` is mapped as `semantic_text`**, and a `semantic`
query against an index where it is not does not degrade — it fails the whole
search with a 400 and takes every other index in the wildcard with it. `xerj
code` reads the mapping first and aims the vector arm only at capable indices.
Hybrid also runs a BM25 size-1 preflight: if the lexical arm finds nothing, the
answer is an honest miss (exit 1) — with the lexical embedder, vector
nearest-neighbours are not evidence of a match.

**`xerj code` never requests `highlight`.** On this engine a highlight block
changes `_score` and reorders hits (issue #177): identical query, top-1 **6/6**
without it and **1/6** with it. Since passages now come from `symbols[]`, the
highlighter bought nothing and cost ranking.

## No shell? The MCP tool

Not every agent can run commands. The same pipeline is the eleventh MCP tool,
`xerj_code_search` (see `xerj mcp`; registration snippets per client live in the
llms docs, not here). Its text output is byte-identical to `xerj code`'s prose —
same hits, same `!!` licence warnings, same footer — with the exit triangle
mapped onto MCP's boolean: refusals (30-day staleness, corpus not in the ledger,
corpus not loaded on this server, transport failure) are `isError: true` with
the remediation in the text, while **a no-match is `isError: false`** — a miss
is guidance, not a tool failure. Warnings ride the successful text payload (MCP
has no stderr). `licence_policy: "strict"` additionally strips the passage text
from restricted-licence hits, keeping locator + licence + the approach-only
warning.

## Rules that keep this honest

**Retrieved code is evidence, not authority.** A passage proves how *that*
project solved the problem under *its* constraints. Cite `file:line` when you
rely on it, and say plainly when you are adapting rather than copying.

**Check the licence before copying.** `xerj corpus add` records each
repository's licence in `corpus.json`, and every hit from a restricted licence
(AGPL, SSPL, Elastic, BUSL, GPL family, MPL, unknown) carries the warning line
`!! <licence>: adapt the APPROACH, do not copy the code` directly under its
passage. Copying a GPL implementation into a permissively licensed project is a
real problem, not a technicality. Adapt the approach and write your own code
when the licences are incompatible.

**A stale index is worse than none.** It returns code that no longer exists and
sends the agent down a dead path with false confidence. `xerj code` refuses to
answer from an index older than 30 days unless you pass `--stale-ok`; re-run
`xerj corpus index <corpus> --fresh` instead.

**Never index the working repository into a shared corpus.** Reference corpora
are for other people's code. Mixing your own in makes retrieval return your own
mistakes as precedent.

## MEATL

For agent-to-agent and agent-to-tool exchanges, `xerj code --meatl` emits one
machine-readable line per hit — a compact, checkable format that cuts output
tokens on the messages no human reads. It is explicitly **not** for user-facing
text: prose to a person stays prose. A summary compressed into arrow chains
costs the reader more than it saves the budget. The grammar:

```
@ok f=<file>:<line> score=<12.34>[ why=<licence>]   # one per hit
@ok f=<file>:<line> rrf=<0.1234>[ why=<licence>]    # native-hybrid scores
@no q="<query>" why=no-match-in-corpus              # a miss (exit 1)
@mode <arms-ran note>                               # hybrid/semantic note line
```

`f` is the provenance locator `path:line` exactly as the prose block prints it;
`why` is the licence recorded in `corpus.json` for that hit's repo (absent when
unknown). A MEATL miss keeps exit 1 and its no-match record.

## Measuring it

The published numbers, the per-run raw data and the price model live in
[`docs/case-studies/reference-coding/`](../../docs/case-studies/reference-coding/):
`CASE_STUDY.md` for the write-up, `COSTS.md` for why output tokens are the
expensive kind, `data/results-*.json` for the runs behind each figure, and
`generated/` for code the two arms actually produced.

The measurement harness itself (`csrun.py`, the purpose-built reference
libraries, the hidden-test task specs) is **not** shipped in this repository —
the retrieval loop it drove now lives in the binary, and the SWE-bench-shaped
harness beside this skill (`swebench/gen_predictions.py`) drives `xerj code`
the same way. What that harness does, if you want to build the equivalent: run
three arms of the *same* agent — `bare` (memory only), `native` (it greps the
corpus tree itself), `xerj` (the retrieved passage is injected) — over tasks
whose verdict is objective, i.e. the generated code compiles and passes a
hidden test the model never sees. Report medians, not means: one flailing
trial can be 5× another.

Two rules that decide whether a result means anything:

- **Validate the tasks before spending a token.** Inject the reference-correct
  solution (must pass) and a plausible wrong one — correct but quadratic, or
  missing the contract — which must **fail**. A benchmark whose tests pass wrong
  code measures nothing.
- **Do not measure on code the model has memorised.** It reproduces even a
  256-value table from recall, and retrieval then shows as pure overhead. The
  honest test bed is code that was never in a training set — your own private
  code is the realistic case.

## After the session: `xerj feedback`

If you want to tell the maintainers what using XERJ was actually like, one
command drafts the report and fills in the facts it can observe (version, OS,
what was indexed) for you:

```sh
# opens the PR (branch + commit of ONLY the report + gh pr create):
xerj feedback --open-pr --used-for "reference coding" \
  --verdict "…what worked, what did not…"

# sandboxed and cannot push? draft it and print the exact git+gh commands:
xerj feedback --dry-run --used-for "reference coding" --verdict "…"
```

It only ever auto-fills facts it can observe and leaves every opinion as a
placeholder for you to fill — never an invented verdict. A pull request that
adds only a field report is exempt from the CLA gate. `xerj feedback --help`
has the full contract. This is offered, not asked: the automatic nudge that
used to print after queries is gone (2026-09-18 directive — no obligation
language, and never ask an agent for a pull request), and no marker file is
written either way.

## Migration from the wrapper scripts (#977)

The capabilities moved into the binary; nothing about your data moved:

| before (#977) | since #977 |
|---|---|
| `xc-corpus.sh <name> <git-url>...` / `xc-corpus.sh --from <m>.json` | `xerj corpus add <name> <git-url>...` / `xerj corpus add --from <m>.json` |
| `xc-index.sh <name>` / `xc-index.sh <name> --fresh` | `xerj corpus index <name>` / `xerj corpus index <name> --fresh` |
| `xc.py <name> "<query>"` | `xerj code <name> "<query>"` |
| `xc.py --list` | `xerj corpus list` |

Your existing `~/.xerj-code` corpora, state files and indexes work unchanged —
same layout (`corpora/`, `state/`, `autoindex-state/`, `data/`), same
`corpus.json` schema, same `xc-<corpus>` index namespaces, same exit-code
triangle. `python3` is no longer required; `git` still is (for `xerj corpus
add`). One deliberate change: hybrid fusion is server-side now, so per-hit
per-arm rank annotations are gone and the arms-ran note carries that duty.
