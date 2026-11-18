# Corpus Hub v2 — implementation plan

Status: plan. Discussion: https://github.com/xerj-org/xerj/discussions/1374
Target: `xerj "<question>"` answers from a hub corpus in one turn, with no clone and
no indexing on the user's machine. Packs are pre-indexed by CI, signed, pulled on
demand. The corpus is never required in the question and always named in the answer.

This document is written so that an implementer can follow it step by step without
deciding anything that is already decided here. Every step names the files, the
types, the tests and the condition that makes it done. Steps are ordered; do not
skip ahead. Where the plan says "check", the implementer reads the named file and
adapts, but does not change the contract.

Repository rules that apply to every step: scoped builds only
(`cd engine && cargo build --release -j 32 -p <crate>`), never `cargo clean`; the
ES-YAML conformance suite stays at 0 failed; commits are authored as xerj-org with
a full body; shipped docs state what is true of this binary, anchored to a version,
never a promise about a future release
(`engine/crates/xerj-engine/tests/docs_do_not_promise_the_next_release.rs` lists the
banned phrasings and fails the build on them). Before touching storage or snapshot code, read how the existing code
does it: `xerj code xerj-storage "snapshot restore manifest"` and the files named
below.

## 0. Where things are today (read before coding)

| Concern | Where | What exists |
|---|---|---|
| CLI dispatch | `engine/crates/xerj-server/src/main.rs` ~L1850 (`mcp`), ~L2293 (`code`), ~L2303 (`corpus`) | each subcommand calls into a crate and returns an exit code |
| Corpus lifecycle | `engine/crates/xerj-autoindex/src/xc.rs` | `run_corpus_cli`, `run_code_cli`, `code_root()` (`~/.xerj-code` or `$XERJ_CODE_HOME`), `corpora/<name>/corpus.json`, `state/<name>.json`, index names `xc-<name>-b<build>-*`, a `Node` trait with a `FakeNode` test double |
| Shared code-query contract | `engine/crates/xerj-common/src/xccode/` | `Mode`, `CodeParams`, `CodeOutcome`, `run_code_query(&impl XcHttp, …)`, `render::render`, used identically by the CLI and the MCP tool |
| MCP tools | `engine/crates/xerj-mcp/src/lib.rs` | `tool_specs()` (~L614), dispatch `"xerj_code_search" => run_code_search` (~L1112), `run_code_search` (~L1752) |
| Snapshots | `engine/crates/xerj-engine/src/engine.rs` ~L4315 `create_snapshot(repo_path, name, indices)`, ~L4414 `restore_snapshot(repo_path, name, indices)`; HTTP `/_snapshot/:repo` and `/_snapshot/:repo/:snapshot` in `engine/crates/xerj-api/src/router.rs` ~L604 | a snapshot is a directory per index (WAL + segments + schema) plus `manifest.json`; restore loads it into a running node without re-indexing |
| Segment bundles | `engine/crates/xerj-storage/src/bundle.rs` (`pack`, `unpack`, `parse_footer`, `CATALOG_KEY = "snapshot.json"`), `index_store.rs` (`fetch_and_hydrate_segment`) | the object-storage transport; not used by v1 of this plan (see step 2, "why snapshots") |
| Signing | `engine/crates/xerj-autoindex/src/harvest/sign.rs` | `generate_keypair`, `sign_pack(dir, seed_hex)`, `verify_pack(dir, public_hex)`, `verify_sig_at` — ed25519 over `SHA256SUMS` |
| Definitions | `engine/crates/xerj-autoindex/src/extract/code.rs` | private `struct Symbol { name, kind, line, end_line, code }` per definition; the index field `defs` holds "kind name" lines; locator `code:<line>:<name>` |
| HTTP client | `engine/crates/xerj-autoindex/Cargo.toml` | `reqwest` (blocking) is already a dependency |
| gRPC | `engine/proto/xerj.proto` (`package xerj.v1; service XerjSearch`), `engine/crates/xerj-server/src/grpc.rs` (`XerjSearchServer`, `Server::builder().add_service`) | tonic 0.12 / prost 0.13 |
| Auth | `engine/crates/xerj-api/src/auth.rs` `auth_middleware` | API-key auth on the node |
| Pack publishing CI | `.github/workflows/pack-publish.yml` | builds, signs and uploads the rust-vulns pack as a dated GitHub Release |

## 1. New crate `xerj-hub` and shared types in `xerj-common`

**Why a crate.** `xc.rs` is already large and owns the clone-and-index path. The hub
path is different (download, verify, restore, route) and must be testable alone.

**Do**

- Add `engine/crates/xerj-hub` to `engine/Cargo.toml` `members`. Dependencies:
  `xerj-common`, `xerj-autoindex` (for `xc::code_root` and the state file writer),
  `reqwest` (blocking, workspace), `serde`, `serde_json`, `toml`, `zstd`, `tar`,
  `sha2`, `ed25519-dalek` (whichever `harvest/sign.rs` already uses — reuse that
  crate, do not add a second one), `anyhow`, `base64`.
- Add `xerj-hub` to `engine/crates/xerj-server/Cargo.toml`.
- Put every type that crosses crates into `engine/crates/xerj-common/src/hub.rs`
  (new module, `pub mod hub;` in `lib.rs`). `xerj-common` must not depend on any
  other xerj crate.

**Types (exact; serde with `#[serde(deny_unknown_fields)]` on everything that is
read from the network)**

```rust
// engine/crates/xerj-common/src/hub.rs
pub const CATALOGUE_FORMAT_VERSION: u32 = 1;
pub const PACK_FORMAT_VERSION: u32 = 1;
pub const DEFAULT_HUB_URL: &str = "https://hub.xerj.org";
pub const AUTO_PULL_MAX_BYTES: u64 = 50 * 1024 * 1024;   // decision 2 in the discussion
pub const DISK_BUDGET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum LicenceClass { AdaptWithAttribution, ApproachOnly, Excluded }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CatalogueEntry {
    pub slug: String,
    pub domain: String,            // SEC, DB, NET, CRYPTO, WEB, API, OPS, LAW, HEALTH, AI, WRITE
    pub topics: Vec<String>,
    pub ask_for: String,           // the registry's one-line "an agent working on … queries this for …"
    pub pin_sha: String,           // full 40-hex commit
    pub pin_date: String,          // YYYY-MM-DD
    pub refresh_cadence_days: u32, // 7 for code, 1 for feeds
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub pack_bytes: u64,
    pub pack_url: String,          // https://github.com/xerj-org/xerj/releases/download/pack-<slug>-<pin8>/<slug>-<pin8>.hubpack.tar.zst
    pub pack_sha256: String,
    pub sig_url: String,           // same name + ".sig"
    pub dependency_names: Vec<String>, // crate / npm / go module / pypi names this corpus answers for
    pub quality_grade: Option<String>, // "G7 4/5" etc, or None — never invent one
    pub term_sketch: Sketch,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sketch { pub m_bits: u32, pub k_hashes: u32, pub bits_base64: String }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Catalogue {
    pub format_version: u32,
    pub generated_at: String,      // RFC 3339
    pub corpora: Vec<CatalogueEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackManifest {          // pack.json inside the archive
    pub format_version: u32,
    pub slug: String,
    pub pin_sha: String,
    pub pin_date: String,
    pub built_at: String,
    pub builder_version: String,   // `xerj --version` of the builder
    pub index_prefix: String,      // "xc-<slug>-b<build>"
    pub indices: Vec<PackIndex>,   // every index in the snapshot, docs index and defs index included
    pub snapshot_name: String,     // the name used with create_snapshot
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub sources: Vec<PackSource>,  // copied from corpus.json repos[]
    pub defs_count: u64,
    pub files_count: u64,
}
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackIndex { pub name: String, pub docs: u64, pub bytes: u64 }
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PackSource { pub repo: String, pub url: String, pub sha: String, pub licence: String }

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DefRecord {             // one line of defs.jsonl and one document of the -defs index
    pub symbol: String,
    pub kind: String,              // as extract/code.rs names it: function, const, define, struct, …
    pub lang: String,
    pub file: String,              // path relative to the corpus root, forward slashes
    pub line: u32,                 // 1-based line the NAME sits on
    pub text: String,              // the declaration, signature only (Symbol.code), max 400 chars
    pub value: Option<String>,     // for constants/macros/defines: the literal, see step 3
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Answer {                // what every surface returns; JSON for --json, MCP and HTTP
    pub corpus: String,
    pub pin_sha: String,
    pub pin_date: String,
    pub age_days: u32,
    pub licence_spdx: String,
    pub licence_class: LicenceClass,
    pub answered_from: AnsweredFrom,
    pub mode: AnswerMode,
    pub hits: Vec<Hit>,
    pub truncated: bool,
    pub warnings: Vec<String>,     // e.g. "pin is 23 days old; question asks for the current source"
}
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AnsweredFrom { LocalPack, Hosted, LocalIndex }
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AnswerMode { Def, How, Files }
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Hit {
    pub file: String, pub line: u32, pub kind: String, pub text: String,
    pub value: Option<String>, pub score: f32, pub licence_warning: Option<String>,
}
```

**Text rendering (exact, so every surface prints the same thing):**

```
<corpus>  pin <pin_date> (<age_days> day[s] old)  <licence_spdx>  <file>:<line>
<text>
[value: <value>]            # only in def mode when value is Some
answered from: <local pack|hosted hub|local index>
[warning: …]                # one line per warning
```
For several hits, repeat the first three lines per hit; print the `answered from`
line once at the end.

**Tests** (`xerj-common/src/hub.rs` `#[cfg(test)]`): serde round-trip for every
type; `deny_unknown_fields` rejects an extra field; `LicenceClass` kebab-case
strings; the renderer output for one hit and for three hits matches a literal
expected string (keep the expected strings in the test, not in a fixture file).

**Done when** `cargo test -p xerj-common hub::` passes and `xerj-hub` builds empty.

## 2. Pack build and pull (`xerj hub build-pack`, `xerj hub pull`)

**Why snapshots, not bundles.** The engine already has `create_snapshot` /
`restore_snapshot` and their HTTP routes; a restored snapshot is queryable with no
re-indexing. The bundle/`snapshot.json` transport in `xerj-storage` would need a
new read-only HTTP `StorageBackend`. Use snapshots for v1. Record this decision in
the pack format (`format_version: 1` means "snapshot layout").

**Pack layout** (`<slug>-<pin8>.hubpack.tar.zst`, where `pin8` is the first 8 hex of `pin_sha`):

```
pack.json              PackManifest
SHA256SUMS             sha256 of every other file, "hash  path" lines, sorted by path
snapshot/              exactly what create_snapshot wrote: manifest.json + one dir per index
defs.jsonl             one DefRecord per line, sorted by (file, line)
files.jsonl            {"path","lang","bytes","licence"} per indexed file, sorted by path
```
The signature is a separate file `<slug>-<pin8>.hubpack.tar.zst.sig` (ed25519 over
the archive bytes), because a signature cannot sit inside the file it signs. Use
`harvest/sign.rs`'s key format; add `sign_bytes(bytes, seed_hex) -> String` and
`verify_bytes(bytes, sig, public_hex) -> Result<()>` next to the existing functions.

**Builder: `xerj hub build-pack <slug> --registry <dir> --out <dir> --url <node> --sign-key <seed-file>`**

1. Read `<registry>/<slug>.json` (the existing hub manifest; `xerj corpus add --from`
   already parses it — call the same function, do not write a second parser).
2. `corpus add --from` and `corpus index` into the builder node at `--url`
   (call `xc::run_corpus_cli` with the same arguments the CLI would get). After it,
   `state/<slug>.json` holds the `index_prefix`.
3. Build the defs index (step 3) as `<index_prefix>-defs`.
4. `PUT /_snapshot/hub-build {"type":"fs","settings":{"location":"<out>/<slug>/snapshot"}}`
   then `PUT /_snapshot/hub-build/<slug>-<pin8>?wait_for_completion=true` with
   `indices: "<index_prefix>-*"`. Check `put_snapshot_repo` in
   `engine/crates/xerj-api/src/es_compat.rs` (~L30736) for where a repository
   location is allowed to live; if the node restricts it to a configured root, start
   the builder node with that root set to `<out>` — do not weaken the check.
5. Write `pack.json`, `defs.jsonl`, `files.jsonl`, `SHA256SUMS`; tar + zstd (level 9);
   sign; write `<slug>-<pin8>.hubpack.tar.zst` and `.sig`.
6. Print one line: `pack <slug> <pin8> <bytes> <indices> <defs>`.

**Client: `xerj hub pull <slug> [--quiet] [--url <node>]`**

1. Load the catalogue (step 4). Unknown slug → exit 2, message names the catalogue date.
2. Download `pack_url` and `sig_url` to `~/.xerj/hub/packs/<slug>/<pin8>.tmp/`.
   Verify `sha256(archive) == pack_sha256`, then the signature against the embedded
   hub public key (`xerj_common::hub::HUB_PUBKEY_HEX`, overridable by
   `XERJ_HUB_PUBKEY` for tests only). Any failure: delete the temp dir, exit 3,
   message says which check failed. Nothing is extracted before both checks pass.
3. Extract to `~/.xerj/hub/packs/<slug>/<pin8>/`. Verify every `SHA256SUMS` line.
   Read `pack.json`; refuse `format_version != 1`.
4. Register and restore: `PUT /_snapshot/hub-<slug> {"type":"fs","settings":{"location":"<that dir>/snapshot"}}`,
   `POST /_snapshot/hub-<slug>/<snapshot_name>/_restore?wait_for_completion=true`.
   If indices with the same `index_prefix` already exist (same pin pulled before),
   skip restore and say so.
5. Write `~/.xerj-code/state/<slug>.json` the way `corpus index` writes it (reuse
   its writer in `xccode/state.rs`), with the pack's `index_prefix`, `pin_sha`,
   `pin_date`, and a new field `"source": "hubpack"`. `xerj code <slug>` must now
   work unchanged.
6. Print one line: `pulled <slug> <pin8> <bytes> → <n> indices, <defs> definitions`.

**Staleness rule change (in `xccode/mod.rs`, the 30-day refusal):** a state file
with `"source": "hubpack"` is never refused for age; instead the answer gets a
warning when the catalogue lists a newer `pin_sha`. The 30-day refusal stays for
locally indexed corpora.

**Node for the client.** `pull` needs a node like `corpus index` does. Use the
same `--url` / `XERJ_URL` / `localhost:9200` resolution as `xc.rs` (`resolve_url`).
Step 5 adds the automatic node.

**Tests** (`engine/crates/xerj-hub/tests/pack_roundtrip.rs`, start a node the way
`engine/crates/xerj-api/tests/ask_endpoint.rs` does):

- Fixture corpus at `engine/crates/xerj-hub/tests/fixtures/mini-c/` (five C files with
  `#define`s and functions, plus a `LICENSE` with MIT text), plus a registry manifest
  for it with a fake `pin_sha` and `url` pointing at a local bare git repo created in
  the test. Build a pack with a test keypair; assert the archive lists exactly the
  files above; assert `SHA256SUMS` covers all of them.
- Pull into a second, empty data directory: `xerj code mini-c "…"` answers and the
  defs index has `defs_count` documents; the state file has `source: hubpack`.
- Flip one byte of the archive: pull exits 3 before anything is extracted
  (assert the pack dir does not exist).
- Pull the same pin twice: second run says "already present" and restores nothing.
- `format_version: 2` in `pack.json`: refused with exit 3.

**Done when** the roundtrip test passes against the real binary, and a fresh
machine (`HOME` empty) can `xerj hub pull lz4` and answer the benchmark's lz4
questions through `xerj code lz4 --def` without `corpus add` or `corpus index`.

## 3. Definitions table and `def` mode

**Do**

- In `engine/crates/xerj-autoindex/src/extract/code.rs`, make the per-file symbol
  extraction reachable: `pub fn definitions(path: &Path, lang: &str, text: &str) -> Vec<DefRecord>`
  built from the existing `Symbol` values (`name → symbol`, `kind`, `line`,
  `code → text` truncated to 400 chars). Do not change how `defs`/`symbols` are
  indexed today.
- `value` extraction, applied to `text`, first match wins, all anchored at the
  start after optional whitespace and attributes; the captured value is trimmed and
  has a trailing `;` or `,` removed:
  - C/C++/ObjC: `^\s*#\s*define\s+(\w+)\s+(.+?)\s*(?:/[/*].*)?$` → group 2; also
    `\b(?:static\s+)?const\s+\w[\w\s\*]*\b(\w+)\s*=\s*(.+?)\s*;` → group 2.
  - Rust: `^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+(\w+)\s*:\s*[^=]+=\s*(.+?)\s*;` → group 2.
  - Go: `^\s*(?:const\s+)?(\w+)\s*(?:\w+\s*)?=\s*(.+?)\s*$` only inside `const (` blocks or lines starting with `const`.
  - Python/Ruby/Shell: `^\s*([A-Z_][A-Z0-9_]*)\s*=\s*(.+?)\s*$`.
  - Anything else: `None`.
- Builder (step 2) writes `defs.jsonl` and indexes it into `<index_prefix>-defs` with
  this mapping: `symbol: keyword` plus `symbol_text: text` (same value, analyzed),
  `kind: keyword`, `lang: keyword`, `file: keyword`, `line: integer`,
  `text: text`, `value: keyword`.
- `def` lookup, one function used by CLI, MCP and HTTP:
  `xerj_hub::def::lookup(http: &impl XcHttp, slug, symbol, k) -> Answer`: term query on
  `symbol` (exact), fallback `match` on `symbol_text`; `k` default 5; hits sorted
  exact-first then by file path. Zero hits is a normal answer with an empty `hits`
  and the text `no definition named <symbol> in <corpus> (pin <date>)`.
- Add `AnswerMode::Def` to the CLI: `xerj code <slug> --def <SYMBOL>` and to
  `CodeParams` as `def: Option<String>`; when set, `run_code_query` calls `lookup`
  and renders with the step-1 renderer instead of passages.

**Tests**

- Unit (`extract/code.rs`): the regexes on these literal lines return exactly:
  `#define LZ4_VERSION_RELEASE  0    /* for tweaks */` → `"0"`;
  `pub(crate) const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);` → `"Duration::from_secs(30)"`;
  `MDB_MAXKEYSIZE = ((MDB_DEVEL) ? 0 : 511)` inside a C `#define` → the full expression;
  `batchSize = 16 << 20` (Go const block) → `"16 << 20"`;
  `fn main() {` → `None`.
- Integration: build the fixture pack; `lookup(…, "MINI_LIMIT")` returns one hit with
  `value: Some("128")` and `line` equal to the line in the fixture file.
- Benchmark acceptance: with the five benchmark packs pulled, run the 50 registered
  questions (`benchmarks/corpus-tasks/tasks/<slug>.json` on branch
  `bench/corpus-tasks`) through `xerj code <slug> --def <name> --json` and grade with
  the suite's `check.py`: 50/50, every answer under 80 output tokens when rendered.

**Done when** the three test groups pass.

## 4. Catalogue (`xerj hub build-catalogue`, client cache, signature)

**Do**

- Builder: `xerj hub build-catalogue --registry <dir> --packs <dir> --out <file> --sign-key <seed-file>`.
  One `CatalogueEntry` per live corpus in `backlog/backlog-100.json` that has a pack
  in `--packs`; fields from the manifest, the backlog row and `pack.json`;
  `dependency_names` from a new optional manifest field `"dependency_names": [...]`
  (empty if absent — do not guess names); `quality_grade` only from the graded G7
  files the site generator already reads (`tools/corpus-hub-site/gen.py`,
  `parse_graded_suites`), else `None`.
- Term sketch: Bloom filter, `m_bits = 8 × max(1024, 10 × distinct_terms)` rounded up
  to a multiple of 64, `k_hashes = 7`, double hashing with FNV-1a 64 (`h1`) and
  FNV-1a 64 of the reversed bytes (`h2`), `h_i = h1 + i·h2`. Terms: every
  `DefRecord.symbol`, every path component of `files.jsonl` without extension, and
  every lowercase word of 3+ letters in `ask_for` and `topics`. Deterministic: the
  same inputs produce byte-identical `bits_base64`.
- Signature: `catalogue.json.sig` = ed25519 over the exact bytes of `catalogue.json`.
  The site deploy (`deploy-pages.yml`, corpus-hub branch) publishes both at
  `https://hub.xerj.org/api/v1/catalogue.json` and `….sig`.
- Client: `xerj_hub::catalogue::load() -> Catalogue` reads `~/.xerj/hub/catalogue.json`
  if younger than 24 h; otherwise `GET` with `If-None-Match` (store the ETag in
  `~/.xerj/hub/catalogue.etag`), verify the signature before replacing the cached
  copy, keep the old copy on any failure and add a warning `catalogue is <n> days old`.
  `--offline` never fetches.

**Tests** (`xerj-hub/src/catalogue.rs`, `xerj-hub/src/sketch.rs`):

- Bloom: `contains` is true for every inserted term; false-positive rate on 10,000
  random terms is below 2%; byte-identical across two builds.
- Catalogue builder on the fixture registry produces the expected entry (compare
  to a literal JSON in the test).
- Signature tamper: a catalogue with one changed byte is not accepted and the
  cached copy is kept.
- Size: the catalogue built from the real registry (515 corpora) is under 2 MB
  (run in CI on the corpus-hub branch as a check, not a unit test).

**Done when** `GET https://hub.xerj.org/api/v1/catalogue.json` returns a signed
catalogue that the client loads, and the tests pass.

## 5. The router and `xerj "<question>"`

**CLI surface** (`xerj_hub::cli`, dispatched from `main.rs` next to `code` / `corpus`):

```
xerj ask "<question>" [--corpus <slug>] [--def <SYMBOL> | --how | --files]
                      [--offline] [--no-fetch] [--hosted on|off|auto]
                      [--max-tokens N] [--json] [--url <node>]
xerj "<question>"      # same: argv[1] is not a known subcommand, does not start with "-", and contains a space
xerj hub pull <slug> | status | sync [--install-timer] | update | rm <slug> | config hosted on|off
```

**Router** (`xerj_hub::router`):

```rust
pub struct Evidence { pub slug: String, pub score: f32, pub source: EvidenceSource }
pub enum EvidenceSource { ProjectDependency(String), Hosted, Sketch }
pub enum Routing { Confident(String), Ambiguous(Vec<Evidence>), NoCorpus }
pub fn route(question: &str, cwd: &Path, cat: &Catalogue, hosted: Option<&dyn HostedSearch>, cached: &[String]) -> Routing
```
Order and scoring, exactly:

1. Project dependencies: parse, if present in `cwd` or any parent up to the git root:
   `Cargo.lock` (`[[package]]` `name = "…"` lines), `package.json` (keys of
   `dependencies` and `devDependencies`), `go.mod` (`require` paths, last path
   segment and full path), `requirements.txt` (name before `==`, `>=`, `[`),
   `pyproject.toml` (`[project] dependencies` names). Any corpus whose
   `dependency_names` intersects gets score 1.0 with `ProjectDependency(name)`.
2. Hosted federated search (step 6) when `hosted` is `Some`: each corpus in the
   response gets score `0.9 × (its best hit score / the top hit score)`.
3. Sketch: tokenize the question into identifiers (`[A-Za-z_][A-Za-z0-9_]{2,}`) and
   lowercase words; for each corpus, score = (tokens present in its sketch) /
   (tokens total), ×0.8; tokens present in more than 25% of all sketches count half.
4. Merge by slug taking the max score. `Confident` if top ≥ 0.6 and top ≥ 1.5 × second
   (or no second). `Ambiguous` if two or more are ≥ 0.3 and the best reaches 0.35
   (a full overlap of common tokens scores 0.4 and stays a real tie). Two or more
   between 0.3 and 0.35 is `NoCoverage`: a weak overlap, not a choice. Else `NoCorpus`.
   A question that names a project no entry contains (`Phaser`, `setInteractive`) is
   `NoCoverage` even when another token ties several corpora. `xerj ask` exits 1 for
   both `NoCorpus` and `NoCoverage` (the latter names `xerj autoindex` and `xerj hub find`);
   exit 4 is only a real ambiguity. `--corpus` skips routing. Corpora already cached
   (`cached`) get +0.05.

   A question that names a language or a technology is decided before those four
   steps, from the catalogue entry itself (slug, topics, a language word in
   `ask_for`, then the rest of `ask_for`). An exact slug the question spells
   out still wins first. The sketch path above still decides when no distinctive
   name matches. See `xerj_hub::router`.

**Answer flow** (`xerj_hub::ask`):

1. If the question names a symbol (`--def`, or the whole question is one identifier,
   or it matches `what is the (exact )?value of (\w+)`), mode is `Def` with that
   symbol; else `How`; `--files` forces `Files`.
2. Route. `Ambiguous` → print the candidates (`slug — ask_for`, one per line) and
   exit 4 (the same "needs a decision" code autoindex uses); the agent re-asks with
   `--corpus`. `NoCorpus` → exit 1 with `no hub corpus matches; see xerj.org/llms.txt`.
   `NoCoverage` → exit 1 with `no hub corpus covers this topic` and the local-index
   hint (`xerj autoindex <path>`, then `xerj code` or `xerj search`; `xerj hub find`
   to see what the hub does have). Not exit 4.
3. If the pack is cached locally → answer locally (`AnsweredFrom::LocalPack`).
4. Else if hosted is allowed → answer through the hosted node now; then, unless
   `--no-fetch`/`--offline`, start the pull in the background: spawn
   `current_exe() hub pull <slug> --quiet`, detached, stdout/stderr to
   `~/.xerj/hub/pulls/<slug>.log`, PID to `~/.xerj/hub/pulls/<slug>.pid`. Policy:
   pull only if `pack_bytes ≤ AUTO_PULL_MAX_BYTES` and `licence_class != Excluded`;
   above the limit print `run: xerj hub pull <slug> (<MB> MB)` instead; for
   `ApproachOnly` add the warning `approach-only licence (<spdx>): read, do not copy`.
5. Else (no cache, no hosted): if `--offline` → exit 1 saying which pack to pull;
   otherwise pull synchronously (same policy), then answer locally.
6. Disk budget: before any pull, if the packs dir exceeds `DISK_BUDGET_BYTES` after
   adding this pack, remove least-recently-used packs (by the `last_used` file each
   pack dir gets on every answer) until it fits, never the one being pulled.
7. Always print the step-1 rendering; `--json` prints `Answer`.

**Hosted switch:** `--hosted auto` (default) is on if `~/.xerj/hub/config.toml` has
`hosted = "on"`; otherwise off when `cwd` is inside a git work tree, on when it is
not. Decision 1 in the discussion; the first time the switch resolves to off inside a
repo, print once: `hosted answers are off in git repositories; xerj hub config hosted on`.

**Node** (`xerj_hub::node`): resolve `--url`/`XERJ_URL`/`localhost:9200`; if the
health check fails, start `current_exe() --insecure --port <free port ≥ 9300> --data-dir ~/.xerj/hub/node`
detached, PID to `~/.xerj/hub/node.pid`, wait for green (max 60 s), remember the port
in `~/.xerj/hub/node.port`. Never start a node on a port that answers. `xerj hub status`
shows it; `xerj hub stop` stops only that PID.
A node the user started themselves needs no config for `xerj hub pull` to restore
into it: by default a server allows snapshot **restores** (never creates) from the
pack cache of the user it runs as (`limits.hub_pack_restore`, `xerj_common::hub::hub_packs_dir`,
refused if that directory is a symlink or owned by another user). A server that does
not share the cache (other user, container, other machine) refuses, and the pull
error prints the exact `snapshot_repo_allowlist` line to add.

**Tests** (`xerj-hub/tests/routing.rs`, `xerj-hub/tests/ask_flow.rs`):

- Labelled routing set `engine/crates/xerj-hub/tests/fixtures/routing.jsonl`:
  at least 100 lines `{"q":…, "deps":[…], "expected":"<slug>"}` across 20 corpora of
  the real catalogue, written by hand from the registry's `ask_for` lines (the
  author of the test writes them; the implementer does not generate them from the
  sketch). Pass criteria: top-1 ≥ 90% with `deps`, ≥ 80% without; every `Ambiguous`
  result contains the expected slug.
- `ask` with a cached pack never touches the network (assert with a hosted stub
  that panics on call).
- `ask` with hosted on and no cache returns `Hosted` and leaves a pull PID file; the
  pull completes; the second `ask` answers `LocalPack`.
- Policy: a catalogue entry with `pack_bytes` above the limit is not pulled and the
  message names the size; an `Excluded` entry is never pulled; `ApproachOnly` adds
  the warning.
- `--offline` with no cache exits 1 with the pull hint; `--no-fetch` answers hosted
  without starting a pull.
- Disk budget: three fake packs over budget → the oldest `last_used` is removed first.
- Node: with nothing on 9200, `ask` starts a node on 9300+, answers, and
  `xerj hub stop` stops exactly that PID (assert another dummy process named like
  the binary survives).

**Done when** an agent transcript (the harness in `demo/playbooks/AGENT_FTX_HARNESS.md`,
shell persona) shows one tool call per question in steady state.

## 6. Hosted endpoints (`xerj-api/src/hub_api.rs`) and the public node

**Do**

- New module `engine/crates/xerj-api/src/hub_api.rs`, routes added in `router.rs`
  next to `/_memory`:
  - `GET /_hub/v1/catalogue` → bytes of the file at config `hub.catalogue_path`, with
    `ETag` = its sha256 and `Cache-Control: public, max-age=3600`.
  - `GET /_hub/v1/def/{slug}/{symbol}?k=` → `Answer` from step-3 `lookup`.
  - `GET /_hub/v1/code/{slug}?q=&k=&max_tokens=&mode=` → `Answer` from `run_code_query`
    (the same function the CLI uses; implement `XcHttp` for the in-process case by
    calling the node's own loopback address with the admin key — simplest; document
    it in the module doc).
  - `GET /_hub/v1/search?q=&k=` → `multi_match` on `body,defs,symbol_text` across
    `xc-*-docs,xc-*-defs`, group by corpus (parse the slug from the index name with
    the `xc-<name>-b<build>` rule in `xc.rs`), return `{corpora: [{slug, best_score, best_hit}]}`
    sorted by score, at most 10.
- All four are `GET`, read-only, slug validated against the catalogue (unknown → 404),
  `symbol` and `q` limited to 512 bytes, `k ≤ 50`. A config key `hub.public = true`
  makes exactly these four routes skip `auth_middleware`; everything else keeps auth.
- Rate limit: if a per-key limiter exists in `xerj-api` reuse it; otherwise a token
  bucket per client IP, 60 requests/minute, 429 with `Retry-After`.
- The public node: one node started with `hub.public = true` and every live pack
  restored (`xerj hub pull` for each slug against its own URL), running the way the
  reference-coding node runs on the maintainer machine (a systemd user unit; keep
  its data directory under its own path, never a shared one). `hub.xerj.org/api/v1/*`
  is a reverse proxy to it; the static `catalogue.json` stays on the site so the
  catalogue is reachable even when the node is not.

**Tests** (`engine/crates/xerj-api/tests/hub_api.rs`, same harness as the other
endpoint tests):

- With the fixture pack restored: `def` returns the fixture value; `code` returns at
  least one hit with `file:line`; `search` groups by the fixture slug.
- Unknown slug → 404; `k=1000` → clamped to 50; a 600-byte `q` → 400.
- `hub.public=false` → the routes require the key; `true` → they do not, and
  `POST /_hub/v1/…` is 405.
- Rate limit: the 61st request in a minute from one IP is 429.

**Done when** `curl https://hub.xerj.org/api/v1/def/lz4/LZ4_VERSION_RELEASE` returns
`value: "0"` with the pin date, and the test file passes.

## 7. MCP tools and gRPC

**MCP** (`engine/crates/xerj-mcp/src/lib.rs`): add to `tool_specs()` and the dispatch:

```
xerj_hub_find  {question: string, k?: integer}            → the step-6 /search grouping
xerj_hub_def   {slug: string, symbol: string, k?: integer} → Answer (def)
xerj_hub_ask   {question: string, corpus?: string, mode?: "def"|"how"|"files", max_tokens?: integer} → Answer, via the step-5 flow
```
Local packs first, hosted second, exactly like the CLI (call `xerj_hub::ask`; do not
reimplement). `XERJ_HUB_URL` overrides the hosted base URL. The tool result text is
the step-1 rendering; the structured content is the `Answer` JSON.

**gRPC** (`engine/proto/xerj.proto`, `engine/crates/xerj-server/src/grpc.rs`):

```proto
service XerjHub {
  rpc Ask(HubAskRequest) returns (HubAnswer);
  rpc Def(HubDefRequest) returns (HubAnswer);
  rpc Catalogue(HubCatalogueRequest) returns (HubCatalogueResponse);
}
message HubAskRequest   { string question = 1; string corpus = 2; string mode = 3; uint32 max_tokens = 4; }
message HubDefRequest   { string slug = 1; string symbol = 2; uint32 k = 3; }
message HubCatalogueRequest {}
message HubCatalogueResponse { bytes catalogue_json = 1; bytes signature = 2; }
message HubHit { string file = 1; uint32 line = 2; string kind = 3; string text = 4; string value = 5; float score = 6; string licence_warning = 7; }
message HubAnswer {
  string corpus = 1; string pin_sha = 2; string pin_date = 3; uint32 age_days = 4;
  string licence_spdx = 5; string licence_class = 6; string answered_from = 7; string mode = 8;
  repeated HubHit hits = 9; bool truncated = 10; repeated string warnings = 11;
}
```
Register `XerjHubServer` with `add_service` next to `XerjSearchServer`, behind the
same interceptor. Field numbers are fixed by this document; never renumber.

**Tests:** MCP `tools/list` contains the three names; `xerj_hub_def` against the
fixture pack returns the fixture value; gRPC `Def` returns the same `HubAnswer` as
the HTTP route (compare field by field), and the existing
`grpc_h2c_fail_closed.rs` still passes.

**Done when** the tests pass and `xerj init` (step 9) prints the MCP registration.

## 8. Lockfile, freshness, sync, status

**Do**

- `xerj.lock` (TOML) in the project root (first parent of `cwd` containing `.git`,
  else `cwd`):
  ```toml
  version = 1
  [[corpus]]
  slug = "quinn"
  pin_sha = "…"
  pin_date = "2026-10-12"
  pack_sha256 = "…"
  ```
  Written the first time an answer is served in that project; appended per corpus;
  never rewritten silently. `--locked` (on `ask`) exits 2 if the catalogue's pin
  differs from the lock. `xerj hub update` prints the diff and rewrites the lock
  when `--write` is given.
- Freshness warning: if the question matches `\b(current|latest|newest|now)\b` and
  `age_days > refresh_cadence_days`, add the warning
  `pin is <n> days old and the question asks for the current source`.
- `xerj hub sync`: refresh the catalogue; for every cached pack whose catalogue pin
  is newer, pull the new pin and remove the old one after the new one is restored;
  verify signatures; prune to the disk budget; write `~/.xerj/hub/last_sync`.
  `--install-timer` writes a systemd user timer (Linux) or a launchd plist (macOS)
  running `xerj hub sync` every 6 hours; print the unit path; nothing else.
- `xerj hub status`: cached packs (slug, pin8, age, bytes, last used), node (running
  or not, port, PID), hosted setting, last sync, catalogue age. Plain text, one line
  per item.
- CI (corpus-hub branch, new `.github/workflows/hub-packs.yml`, copy the job shape of
  `pack-publish.yml` including its signing secret): on push, for each live corpus
  whose manifest changed, build the pack (step 2) and upload it as a release
  `pack-<slug>-<pin8>`; then rebuild and deploy the catalogue (step 4). A scheduled
  job (weekly for code domains, daily for feed corpora with `refresh_cadence_days = 1`)
  re-pins: run `git ls-remote` for the manifest's URL, and if the default branch
  head differs from `pin_sha`, open a PR that updates the manifest (a human merges;
  packs follow on merge).

**Tests:** lock written and appended; `--locked` mismatch exits 2; the freshness
warning fires only with both conditions; `sync` with a newer pin replaces the pack
and the old dir is gone; `status` output lists exactly the packs on disk.

**Done when** two machines with the same `xerj.lock` and `--locked` give identical
`Answer` JSON for the fixture question.

## 9. Agent-facing surface

**Do**

- `landing/llms.txt` section "Reference corpora: hub or local" becomes:
  the one-line command, the MCP call, the `--def` form, when to use it, the three
  success lines, the `xerj hub pull` form for large packs, and nothing about
  manifests. Longest line under 300 characters. `landing/llms-full.txt` §11 updated
  to match. Follow `docs/research/llms-txt-2026-09/README.md` before editing either.
- `xerj init` installs a second skill `.claude/skills/xerj-hub/SKILL.md` (and the
  Cursor rule) with: the decision rule (hub for public material you cannot quote,
  local for your own files, neither for what you already know), the one line, `--def`
  for exact questions, "stop only the node you started", and the MCP registration
  per client (`claude mcp add …`, `.mcp.json` `mcpServers`, VS Code `servers`).
- Hub pages (`tools/corpus-hub-site/gen.py`, corpus-hub branch): the copy prompt and
  the "Use it" block become the one line plus the `--def` form; the catalogue link
  is on every page.

**Tests:** the docs drift tests pass; `python3 scripts/seo/*.py --check` gates pass;
the first-time-agent harness (`demo/playbooks/AGENT_FTX_HARNESS.md`) shell-less
persona reaches an answer through `xerj_hub_ask` in one call.

## 10. Measurement (before anything is published)

Pre-registered plan in `benchmarks/hub-pov/PLAN.md` on branch `bench/corpus-tasks`
(write it before running). Arms: memory only; normal agent with git; normal agent
with web only (no git; requests counted; a fetch that returns a bot-protection page
or 429 is recorded as a failed fetch); hosted regex code search; a web search API;
XERJ today (`corpus add` path); XERJ v2 first use; XERJ v2 steady state (pack cached).
Task classes: the registered exact-lookup suites (control; a tie with git is
expected), how-does-it-work questions across files, non-git multi-source data,
ten-question sessions per corpus. Metrics: correct, cost, output tokens, turns,
bytes downloaded, wall time on a quiet machine with load recorded, failed fetches,
routing accuracy. At least three repeats per cell. Every agent runs in the sandbox
recipe in `benchmarks/hub-pov/sandbox.sh` (bubblewrap, own PID namespace, read-only
filesystem outside its home). Results go to the discussion, win or not; the site
shows them only if XERJ v2 beats both normal-agent arms on cost and output tokens at
equal or better accuracy.

## 11. Order, and what not to do

Order: 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 9 → 10. Steps 2 and 3 can be developed
together; 6 needs 2 and 3; 5 needs 4 and 6 (hosted search) but can ship with the
sketch router first.

Do not:

- clone or index a hub corpus on the user's machine (that is the v1 path; v2 pulls);
- require a corpus name in the question, or omit it from the answer;
- extract or restore anything before the sha256 and the signature both verify;
- block an answer on a download when a hosted answer is allowed;
- pull above `AUTO_PULL_MAX_BYTES`, or an `Excluded` licence class, or send the
  question to the hosted node when hosted resolves to off;
- print passages when `Def` mode answers the question;
- start a node on a port that already answers, or stop any process other than the PID
  this tool recorded;
- renumber proto fields, change `format_version` semantics, or add a second JSON
  parser for the registry manifest;
- publish benchmark numbers unless the step-10 condition holds.

## 12. Release gate: backfill, binaries only, end-to-end

Steps 1–11 describe how one pack is built and used. This section defines what must
be true before a release advertises Hub v2 (`xerj ask`, `xerj "<question>"`, the MCP
hub tools, the hub pages' one line). Until every item below holds, a release ships
the hub code switched off and says nothing about it.

**Prerequisite: the key.** `HUB_PUBKEY_HEX` holds the public half of the
`HUB_SIGNING_SEED` secret, and the `hub key and secret in place?` job in
`hub-packs.yml` passes. Without it nothing below can run.

**Who routes a question to a corpus: the client.** `xerj_hub::router` runs inside the
user's `xerj` binary. It routes from the project's lockfiles and from the per-corpus
term sketches in the signed `catalogue.json`. That file is static, so hub.xerj.org can
stay a static site, and routing works with no hub server. The hosted search and answer
routes (step 6) need a live public node, which does not exist. For this release,
hosted is OUT of scope: the client must work, and the gate is passed, with hosted
unreachable. A static host answers unknown paths with the HTML home page and status
200 (today `/api/v1/catalogue.json` and `/api/v1/search` both do), so the client must
treat a non-JSON or unsigned response as "unavailable", never as data.

**12.1 Backfill: every existing corpus becomes a pre-indexed pack.**
- Run `hub-packs.yml` (`workflow_dispatch`, `corpora: all`) in capped batches until
  every live corpus in `backlog/backlog-100.json` has a release for its current pin.
  "No release for the current pin" already covers the first run, so the backfill
  needs no new code.
- A corpus whose licence class is `Excluded` gets no pack and leaves the catalogue.
- A corpus whose build fails is listed with its error in the backfill report
  (`benchmarks/hub-backfill/REPORT.md`: slug, pin8, bytes, defs, result) and is fixed or
  delisted on purpose. Never let it disappear silently.
- The two record packs from `pack-publish.yml` (`rust-vulns` and
  `agent-session-trajectories`, signed JSON record archives) are rebuilt as hubpacks
  (`format_version: 1`, snapshot layout) under the same pipeline and key.

**12.2 Binaries only.** Users get pre-indexed packs, never data to index themselves.
- Hosted downloads are the signed `.hubpack.tar.zst` + `.sig` release assets. The
  only JSON served is the signed `catalogue.json` (+ `.sig`), which is the index of
  the packs and not corpus data.
- The hub pages and `llms.txt` stop linking per-corpus manifest JSON and stop
  printing `xerj corpus add --from <url>`. The registry manifests stay in the repo as
  build inputs for `build-pack` (step 2), and are not a consumer path.
- The JSON record-pack releases (`pack-rust-vulns-*`, `pack-agent-session-trajectories-*`)
  are retired once their hubpacks verify, and `pack-publish.yml` stops publishing
  them. `xerj corpus add --from <url>` keeps working for anyone's own manifests, but
  the hub no longer points to it.

**12.3 End-to-end, per pack (a CI gate in `hub-packs.yml`).** Each pack job, after
`Verify the signature and the layout` and BEFORE `Publish the draft`, runs the
consumer path with the real binary:
1. A fresh node with an empty data dir and an empty `HOME`.
2. `xerj hub pull <slug>` from the draft release URL, verified against the embedded key.
3. `xerj code <slug> --def <symbol> --json`, for one symbol taken from the pack's own
   `defs.jsonl`, must return that definition with `answered_from: local pack`.
4. `xerj code <slug> "<a word from ask_for>" --json` must return at least one hit
   with `file:line`.
5. Any failure leaves the release a draft and fails the job. A pack that has not
   passed this is never published.

**12.4 End-to-end, whole hub (the release gate).** Before tagging the release that
advertises v2, run from a clean machine (the sandbox recipe in
`benchmarks/hub-pov/sandbox.sh`; empty `HOME`; the release-candidate binary;
production `hub.xerj.org`):
- The catalogue loads and verifies, and its corpus count equals the number of
  published packs.
- For EVERY catalogued corpus, `xerj hub pull` + one `--def` + one question pass
  (12.3 again, against the published assets and not the drafts). Results go into the
  backfill report. The gate is 100% of catalogued corpora; a failing corpus is
  delisted before the release, not shipped broken.
- `xerj "<question>"` with no corpus named routes and answers for the lz4 benchmark
  questions (step 2's "done when") and for `curl https://hub.xerj.org/api/v1/def/lz4/LZ4_VERSION_RELEASE`
  (step 6's).
- Hosted off, hosted unreachable (including an HTML 200 from the static site),
  offline (`--offline`) and over-the-cap (`AUTO_PULL_MAX_BYTES`) paths each give
  their documented message. The `curl …/api/v1/def/…` check above applies only once a
  public node exists.
- The shell-less persona of the first-time-agent harness answers through
  `xerj_hub_ask` in one call (step 9).
- A tampered pack (one flipped byte) and a tampered catalogue are refused, with nothing
  extracted.

Runnable as `tools/hub-packs/whole_hub_e2e.sh` (`.github/workflows/hub-e2e.yml`,
`workflow_dispatch`: a release tag or `build`, and the hub URL). It covers every
item above except the `curl …/api/v1/def/…` check (no public node) and the
shell-less persona (the agent harness). Per-corpus results are backfill-report rows
(`kind: "e2e"`); the gate-level checks go to `GATE.md` and the job summary. Until a
signed catalogue is deployed, `--rehearse <pack tags>` runs the same checks against a
loopback catalogue of published packs re-signed with a throwaway key; their real
signatures are checked separately, and every check says which key it ran against.

Only after 12.1–12.4 pass do the held agent-surface PRs merge and the release notes
mention Hub v2. Step 10's measurement is separate and still governs any published
performance claim.
