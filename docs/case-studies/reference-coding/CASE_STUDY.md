# XERJ.code case study — does reference retrieval beat native Claude Code?

> **Status: complete (2026-08-05).** Six tracks, real tokens and dollars, across
> **five languages** (Rust, Python, JavaScript, C, Java): thirteen purpose-built
> unfamiliar-code libraries plus two memorised controls (tantivy, valkey+memcached).
> Every number produced by `csrun.py` / `csmulti.py` + `summarize.py` on this
> machine. Full method (what/which repos indexed, how indexed, how searched, how to
> reproduce) below. Raw data in `data/results-*.json`; captured generated
> programs in `generated/`.
>
> **One-line answer:** on the seven domains carrying a contract the model cannot
> recall (a seal, a generational handle, a lazy refill, a specific hash scheme, an
> exact tie-break), XERJ makes the *same* Claude Code solve **21/21 where bare
> solves 1/21**, at **1.5× fewer output tokens and 1.3× lower cost than native
> Claude Code**, and **6.5× cheaper than flailing from memory** ($3.38 vs $21.9).
> The compiler can leak an API *name* but never a *contract*. On memorised public
> code, retrieval is overhead. The value is gated by memorization; the line is
> sharp.

## The question, stated so it can be answered

The earlier results (`RESULTS.md`) established one thing cleanly and left the
central claim open:

- **Established:** on public code the model has memorised (memchr, regex,
  aho-corasick — three of the most-cloned Rust crates), the bare model is at
  ceiling. Retrieval adds context and changes nothing, because the model already
  knows the answer. A retrieval system *cannot* demonstrate value on a corpus its
  reader has memorised.
- **Open:** the case the tool is actually for — code the model has **not**
  memorised (private, recent, or idiosyncratic) — was never measured.

This study measures that case, in the currency the claim is made in: **real
tokens and real dollars**, from `claude -p --output-format json`, not the byte
proxies the earlier work was limited to. And it compares against the right
baseline — not a bare model, but **native Claude Code with tools**, which is what
a developer actually competes against.

## Three arms — and why the two peers must be identical

The load-bearing insight (and a correction made mid-study): **xerj *is* Claude
Code plus a prompt plus a retrieved snippet.** It is the same model and the same
agent. So the honest comparison is not "a crippled model vs a full agent" — it is
two runs of the *identical* agentic Claude Code that differ in exactly one thing:
whether XERJ pre-retrieved the reference, or the agent had to grep for it.

| arm | modality | reference material | what it models |
|---|---|---|---|
| **native** | agentic Claude Code, full tools, `cargo build` self-verify | the cloned repos mounted on disk | Claude Code as-is: grep the source, then write |
| **xerj** | **the same** agentic Claude Code, same tools, same self-verify | one retrieved passage injected; corpus **not** mounted | Claude Code + XERJ: the retrieval replaces the grep |
| _bare_ (floor) | tool-free, memory only | none | a lower bound: what the model knows without any lookup |

The **native vs xerj** comparison is the headline — both are the same Claude
Code, so any difference is attributable to the retrieval, not the harness. `bare`
is a supplementary floor (no tools, no self-verify) that shows the raw knowledge
gap; it is not a peer of the other two and is never compared on cost as if it
were.

> **A discarded first design, kept as a lesson.** The xerj arm was initially run
> tool-free and single-shot — no `cargo build`, no self-verify — while native was
> fully agentic. That handicapped xerj: on BM25 it copied tantivy's internal
> `Score`/`FieldNormReader` types out of the injected file and, unable to compile,
> looped and failed, while native (which could compile and adapt) passed. That is
> a harness artifact, not a fact about retrieval: the same Claude Code cannot be
> worse for having *more* information in front of it. The arms were rebuilt so the
> two peers are byte-for-byte the same agent. The earlier run was discarded.

The honesty rails that remain:

- **The verdict is objective and hidden.** No arm sees the tests. `validate_*.py`
  proves, before any model runs, that the tests reject the wrong answers a model
  would actually produce (a forgotten finalizer, a textbook-instead-of-Lucene
  formula, guessed API names). A benchmark whose tests pass wrong code measures
  nothing.
- **`bare` is genuinely tool-free.** `--disallowedTools` blocks Bash/Read/Grep etc.
  (An even earlier draft used `--tools ""`, which does *not* disable tools — the
  "bare" model shelled out and vendored the dependency source. Discarded, and
  documented, because it is exactly the leak that turns a null into a false
  positive.)
- **xerj is not given the corpus to grep.** It gets only the retrieved passage,
  because the whole point is that XERJ's index answers the query so the agent does
  not carry or crawl the whole corpus.

## Cost model (why output tokens are the ones that matter)

Opus 5 list rates: input $5/MTok, output **$25/MTok** (5×), cached input
~$0.50/MTok. A retry lap spends output; a retrieved passage spends input. So the
trade is: pay cheap input once to avoid expensive output laps. Full arithmetic in
`COSTS.md`. This study reports output tokens, total input tokens, and
`total_cost_usd` (the honest bottom line, which already prices cache reads at
0.1×) for every arm.

---

# Method — what was indexed, how, and how it was searched

Every arm that retrieves does so through the shipped `xerj-code` scripts against a
local XERJ instance. Nothing is mocked. This section is the full provenance.

## The reference corpora

| corpus | what it is | source | indexed records |
|---|---|---|---:|
| `sift-lib` | 1 purpose-built Rust lib (`sift`) | `measure/reflib/sift` | 3 |
| `novel-libs` | 9 purpose-built Rust libs + the tantivy tree as bulk | `measure/reflib/*` + tantivy | 102,623 |
| `ml-libs` | 4 purpose-built libs, one each in Python / JS / C / Java | `measure/reflib-ml/*` | 5 |
| `search-eco` | tantivy (real public Rust search engine) | github.com/quickwit-oss/tantivy | 102,606 |
| `kv-oss` | valkey + memcached (real public C servers) | github.com/valkey-io/valkey, github.com/memcached/memcached | 7,923 |
| `rust-text` | memchr + regex + aho-corasick (real public Rust) | github.com/BurntSushi/*, rust-lang/regex | 53,873 |

Two kinds of reference deliberately:

- **Purpose-built libraries** (`reflib/`, `reflib-ml/`) — real code (each compiles
  and passes its own test suite) written *for this study*, so it cannot be in any
  training set. Each carries a runtime **contract the compiler cannot warn about**
  (a seal-before-read, a generational handle, a lazy refill, a promote-on-fetch).
  This is the only honest way to measure retrieval on "code the model has not
  memorised" without waiting for a genuinely private codebase.
- **Real public repos** — tantivy, valkey, memcached, memchr/regex/aho-corasick.
  These are the *memorised* controls: the model has trained on them, so they show
  what retrieval is worth when the model already knows the answer (nothing — see
  Tracks 2–3).

## How they were indexed

Two wrapper scripts, shipped at the time in [`tools/xerj-code/`](../../../tools/xerj-code/)
(since removed — the `xerj` subcommands that replaced them are listed under
[How you reproduce it](#how-you-reproduce-it)):

```sh
xc-corpus.sh <name> <git-url>…   # shallow clone (--depth 1) + record each repo's licence
xc-index.sh  <name>              # xerj autoindex <dir> --url <XERJ> --prefix xc-<name> --no-graph
```

`xc-corpus.sh` writes a `corpus.json` next to the clones — URLs, full commit
SHAs and licences, a few hundred bytes, no source. That file is the shareable
definition of a corpus: `xc-corpus.sh --from <manifest>` rebuilds it at the same
commits somewhere else, which is what makes two people's retrieval results
comparable. Pinned, licence-reviewed definitions for the corpora used here are
in [`tools/xerj-code/hub/`](../../../tools/xerj-code/hub/).

`xc-index.sh` runs **`xerj autoindex`** — XERJ's zero-config folder ingester: it
sniffs file types, extracts per-file records (for source it pulls symbol/definition
metadata via tree-sitter), and bulk-loads them into the local engine under the
prefix `xc-<name>`. `--no-graph` skips the relationship-graph pass (this use needs
ranked passages, not edges). Indexing is fast: the 102k-record `novel-libs` tree
indexed in ~10s. A purpose-built lib is 1 record (one file); a real repo is
thousands.

## How they were searched

`xc.py <corpus> "<query>"` hits XERJ's Elasticsearch-compatible `_search` and
returns ranked passages with `file:line` provenance:

- default ranking is **BM25** (`multi_match` over the file body and its extracted
  definitions); a hybrid BM25+vector RRF mode exists but BM25 is the default and
  what these runs used.
- `--no-symbol` returns the **whole matching file** (right for a small,
  self-contained reference library — the model gets the full API + doc example in
  one passage); without it, `xc.py` returns just the matching **definition**
  located from the file's `symbols[]`.
- the query is behavioural keywords (e.g. *"generational handle plant pluck wither
  stale reuse"*); the top hit is the correct library even when it is one file
  among 102k records (measured score 80–95, ranked above all of tantivy).

The `xerj` arm gets that passage injected into its prompt. The `native` arm is
handed the same corpus tree on disk and must `grep`/read to find the API itself —
that search cost is what the two arms differ by. See a worked query and its output
in [`tools/xerj-code/SKILL.md`](../../../tools/xerj-code/SKILL.md).

## How you reproduce it

The **retrieval half** ran as follows at the time: start XERJ, then
`xc-corpus.sh` (or `xc-corpus.sh --from tools/xerj-code/hub/<corpus>.json`, which
pins the exact commits these runs used) → `xc-index.sh` → `xc.py`. It needs the
`xerj` binary, `git` and `python3`. To point it at **your own** code, index your
repo instead of a corpus — that is the real use case.

*(As of [#977](https://github.com/xerj-org/xerj/issues/977) these are binary
subcommands — `xerj corpus add --from`, `xerj corpus index`, `xerj code` — and
no longer need `python3`; the commands above are what was actually run at the
time of this study.)*

The **measurement harness** — the three-arm runner, the purpose-built reference
libraries, the hidden-test task specs — is **not** in this repository; only its
outputs are (`data/results-*.json`, `generated/`). What it does, if you want to
build the equivalent: run three arms of the same agent (memory only / grep-driven
/ retrieval-injected) over tasks whose verdict is objective — the code compiles
and passes a hidden test the model never sees — using the `claude` CLI's own
token and cost accounting (`claude -p --output-format json`, no API key), and
report medians. Validate each task first by injecting a known-correct and a
known-wrong solution; a benchmark whose tests pass wrong code measures nothing.

---

# Track 1 — novel API (the mechanism, isolated)

**Corpus:** `sift`, a purpose-built, real, compiling Rust library (a
conservative-update count-min sketch) with an API the model cannot have seen:
`furnish`/`absorb`/`seal`/`crest`/`fuse`, and a two-phase contract (you must
`seal()` before any read; the compiler cannot warn you if you forget — only a
runtime panic does). Source in `reflib/sift/`; it passes its own test suite.

**Why synthetic here is correct:** this track isolates one variable — *can the
model use an API it must retrieve because it cannot recall it?* A real library
would confound "unfamiliar" with "partially memorised." The three tasks each
return a real `sift::Sift`, so a HashMap workaround cannot satisfy them, and each
probes a different unguessable facet:

- `sift-build` — construction + the seal-before-read contract + the crest query
- `sift-merge` — `fuse` must happen before `seal` (an ordering the compiler
  cannot hint at)
- `sift-until` — `absorb` *returns* the running estimate (a non-obvious semantic)

**Task soundness (measured before any model ran):** `validate_sift.py` — golden
solution passes; a version that forgets `seal()` fails (runtime, not compile); a
version using guessed `new()`/`push()` names does not compile. All three
discriminate.

## Results — retrieval wins decisively

3 tasks (`sift-build`, `sift-merge`, `sift-until`) × 3 trials, corrected agentic
arms. Medians per arm; totals are the sum across all 9 runs.

| arm | solved | med out-tok | med in-tok | total $ |
|---|---:|---:|---:|---:|
| **bare** (memory) | **0/9** | 18,843 | 306,000 | **$9.25** |
| **native** (grep the dep) | 9/9 | 1,220 | 123,000 | $1.93 |
| **xerj** (retrieval injected) | 9/9 | **756** | 100,000 | **$1.54** |

- **xerj vs bare: 24.9× fewer output tokens, 6.2× cheaper — and success vs total
  failure.** Bare never solves a single sift task in 9 attempts, and it burns
  **$9.25** doing so: it writes an entire count-min sketch from scratch (one round
  hit 27,928 output tokens) with guessed method names, fails to compile, and gives
  up. This is the "many loops wasting tokens" the premise describes, measured.
- **xerj vs native: 1.6× fewer output tokens, 1.2× cheaper, and 4.4 vs 7.0 turns.**
  Both are the same agentic Claude Code and both solve every task; xerj is leaner
  because the retrieval replaces the grep. The gap is modest here only because the
  sift reference is a single file — native's grep is cheap. It widens with corpus
  size (see Track 3's input-token blow-up).

The result is dead stable across trials: xerj sift-build ran 620 / 580 / 565
output tokens on its three trials. This is the regime the tool is built for — an
API the model cannot recall — and it is the only track where retrieval wins.

---

# Track 2 — search internals (the real-product anchor)

**Corpus:** `search-eco` — tantivy (Rust), cloned and indexed on a second XERJ
instance. This is the case XERJ's own developers live in: implement a search
internal so it is byte-compatible with an existing engine, using the ecosystem as
reference. It is also the track where **native vs xerj** is most informative:
native must grep a large multi-file corpus to find the reference; xerj retrieves
it in one call, and the token cost of that difference is the measurement.

**Task — `fieldnorm-quant`:** reproduce tantivy's field-norm quantization — the
exact 256-entry `FIELD_NORMS_TABLE` and the `id_to_fieldnorm` / `fieldnorm_to_id`
functions that encode field lengths into one byte. The table is a specific
non-linear curve (id 100 → 3096, id 200 → 16,777,240); **it is not derivable by
formula**, so it is the purest form of "knowledge the model cannot have and must
retrieve." Soundness checked: golden (the real table) passes; a plausible linear
guess fails.

> **Why not BM25 (a discarded task, kept as a lesson).** The first Track-2 task
> was "implement BM25 exactly as Lucene/tantivy compute it." It turned out to be a
> *bad* measurement, and finding out why was itself instructive. BM25 has several
> real conventions that even the references disagree on — the `ln(1+x)` idf form,
> and crucially the `(k1+1)` numerator factor, which **Lucene ≥8 removed but
> tantivy keeps**. Both `native` and `xerj` test-failed it: native read tantivy's
> code (which includes `(k1+1)`) and then *reasoned itself out of it*, citing the
> Lucene-8 change; the exact-1e-9 tolerance also turned tantivy's `Score=f32` and
> lossy fieldnorm cache into precision noise. So BM25 measured "does the agent
> argue with the reference," not "does retrieval fill a knowledge gap." It was
> replaced with `fieldnorm-quant`, where there is nothing to argue with: you have
> the 256 numbers or you don't. The finding stands on its own — **codegen against
> entangled, convention-divergent reference code is unreliable for reasons that
> have nothing to do with where the code came from.**

## Results — retrieval is neutral-to-harmful (the code is memorized)

`fieldnorm-quant`, 3 trials, corrected agentic arms.

| arm | solved | med out-tok | med in-tok | total $ |
|---|---:|---:|---:|---:|
| bare (memory) | 3/3 | 10,213 | 17,470 | $0.98 |
| **native** (grep + copy) | 3/3 | 1,440 | 121,042 | **$0.60** |
| xerj (inject) | 3/3 | 8,504 | 149,217 | $1.43 |

**All three arms pass — because tantivy is memorized.** `bare` reproduced the
exact 256-entry `FIELD_NORMS_TABLE` from memory (id 100 → 3096, id 200 →
16,777,240), even writing the correct structural comment ("piecewise-exponential,
groups of 8, doubling step"). A table that *looks* impossible to guess is
memorized when its source is a popular public crate — the same wall the original
`RESULTS.md` hit on memchr/regex, now confirmed on a fourth corpus.

Here **retrieval is the *worst* arm.** `native` greps `code.rs` and copies the
table concisely; `xerj` has the whole file injected and re-emits the 256-value
table as output (sometimes retranscribing it), spending 5.9× native's output
tokens. **Injecting a large reference backfires when the task is bulk-data
reproduction.** (Caveat: this makes `fieldnorm-quant` partly a transcription
benchmark; the clean retrieval-value signal is Track 1.)

---

# Track 3 — realism control (public, memorised code)

**Corpus:** `kv-oss` (valkey + memcached). Re-runs the existing RESP/memcached-KV
tasks — public protocols the model knows — now with real tokens and the native
arm. Expected to reproduce the earlier "bare is at ceiling" finding; included so
the contrast with Track 1 is measured on the same instrument, not asserted.

## Results — pure memory wins; retrieval and grep are both overhead

`resp-kv` + `memcached-kv`, 3 trials each, corrected agentic arms.

| arm | solved | med out-tok | med in-tok | total $ |
|---|---:|---:|---:|---:|
| **bare** (memory) | 6/6 | 7,886 | 17,500 | **$1.49** |
| native (grep) | 6/6 | 21,380 | **1,060,000** | $9.14 |
| xerj (inject) | 5/6 | 13,954 | 141,000 | $4.40 |

The ranking **inverts** from Track 1. On code the model has memorised, **`bare`
is cheapest and most reliable** — it just writes the protocol handler. `native` is
the *worst*: it grep-crawls valkey + memcached for code the model already knows,
averaging **28 turns** and pulling **~1.06M input tokens** of file content into
context — an 7.5× input blow-up over xerj, and the concrete cost of "grep the
corpus" scaling with corpus size. `xerj` sits in between and was also the only arm
to miss a solve (5/6): the injected snippet is unnecessary here and can distract.

**xerj beats native 2.3× on cost here** — but that is the wrong headline: *both*
lose to doing nothing (`bare`). When the model knows the answer, every form of
retrieval is a tax.

---

# Track 4 — three more unfamiliar domains, buried in a large corpus

Track 1 (sift) showed retrieval wins on one unfamiliar API, but two questions
stayed open: does it **generalise past a single library**, and does the
native-vs-xerj gap **widen when the reference is hard to find** (sift was one
file, so native's grep was cheap)? Track 4 answers both.

**Three new purpose-built reference libraries**, each real (compiles, passes its
own tests), each in a different domain, each with an API the model cannot recall
*and* a runtime contract the compiler cannot warn about:

- **`grove`** — a bump arena with **generational handles**: `sprout`/`plant`/
  `pluck`/`wither`, where a freed handle is detectably stale and its slot is
  reused under a bumped generation. (contract: stale reads return `None`, never
  the new occupant.)
- **`weft`** — a table-driven byte scanner built `warp`→`thread`→`weave` into a
  separate `Loom` type. (contract: builder and scanner are different types; you
  must `weave` before you can `scan`; maximal-munch runs.)
- **`tally`** — fixed-point decimals with **banker's rounding**: `of`/`add`/
  `quantize`, result of `add` carries the larger scale. (contract: `quantize`
  rounds half-to-even, not half-up.)

Each task returns the library's own type, so a hand-rolled workaround cannot
satisfy it; `validate` confirms golden passes and the guessed-API
(`new`/`insert`/`build`…) does not compile.

**The corpus is large on purpose.** All four novel libraries are indexed
**alongside the entire tantivy tree** (≈102k records, 560 files). So the
reference is a needle in a real haystack. `xerj` retrieves it directly (top hit,
score 80–95, ranked above all of tantivy); `native` is pointed at the same tree
and must *search* it — the realistic "grep your vendored deps" cost.

## Results — the sift win generalises, cleanly

3 tasks (`grove-compact`, `weft-scan`, `tally-round`) × 2 trials.

| arm | solved | med out-tok | med in-tok | total $ |
|---|---:|---:|---:|---:|
| **bare** (memory) | **1/6** | 16,112 | 367,698 | $5.93 |
| native (search the tree) | 6/6 | 1,344 | ~150,000 | $1.24 |
| **xerj** (retrieval) | 6/6 | **892** | ~98,000 | **$0.95** |

- **Generality:** across **four** unfamiliar domains now (streaming sketch, arena
  allocator, lexer, fixed-point decimal), bare essentially fails (**1 of 15**
  solves combined with sift) while native and xerj each solve **15/15**. The sift
  result was not a quirk of one library.
- **xerj vs native: 1.5× fewer output tokens, 1.2× cheaper, and 4.7 vs 7.7 turns**
  — the same shape as sift, reproduced on three fresh domains. xerj skips the
  search; native spends 3 extra turns finding the API.
- **The reference was buried in ~102k records** (the tantivy tree), and xerj still
  retrieved the right library as the top hit (score 80–95). Native found it too —
  grep stays precise when the API name is distinctive — but pulled **~1.5× more
  input tokens** into context doing so (~150k vs ~98k). The corpus-size penalty on
  native is real but modest *here*; it explodes only when the search is unfocused
  (see Track 3's kv, where native hit **1.06M** input tokens).

### One honest wrinkle: the compile-loop can leak an API

`weft` bare **passed once** (1/2) — not from memory, but because after its first
compile error, `rustc`'s own "did you mean `thread`?" suggestions leaked the real
method names, and it recovered on round two. It cost **15,075 output tokens** to
xerj's 610 (**25×**). So the tool-free loop is a weak, expensive substitute for
retrieval *on API names the compiler can suggest* — but it cannot leak a runtime
**contract** (grove's generational staleness, tally's banker's rounding), which is
why bare fails those outright. Retrieval delivers names and contract together, in
one shot.

---

# Track 5 — five more domains, and a sharper picture of *when*

Five more purpose-built libraries, five more domains, same three arms (2 trials):
**`spool`** (power-of-two ring buffer that returns what it evicts), **`cadence`**
(lazy-refill token bucket), **`quill`** (zig-zag + LEB128 varint codec),
**`trellis`** (topological sort with a smallest-index tie-break and a cycle
error), **`sieve`** (Bloom filter with Kirsch–Mitzenmacher double hashing and a
`settle`-before-`sense` seal). Each real, each with a hidden test; corpus is the
same ~102k-record tree.

| task | bare | native | xerj | med out-tok (bare/native/xerj) |
|---|:--:|:--:|:--:|---|
| cadence-run | **0/2** | 2/2 | 2/2 | 18,162 / 1,114 / **744** |
| trellis-order | **0/2** | 2/2 | 2/2 | 21,318 / 1,091 / **782** |
| sieve-member | **0/2** | 2/2 | 2/2 | 13,817 / 1,048 / **670** |
| spool-window | 2/2 | 2/2 | 2/2 | 15,974 / 935 / **656** |
| quill-codec | 2/2 | 2/2 | 2/2 | 1,356 / 1,242 / **999** |
| **total** | **4/10**, $9.11 | 10/10, $1.82 | 10/10, **$1.49** | xerj vs native: **1.5× / 1.2×** |

The five cases **map the boundary precisely**, and that is their value:

- **Three clean wins (`cadence`, `trellis`, `sieve`).** The contract is genuinely
  unrecallable — a lazy refill, a specific tie-break + cycle-node set, a specific
  double-hashing scheme. bare fails **0/2** on each, burning 14–21k output tokens
  reinventing the wheel; xerj takes it to **2/2** at ~700 tokens. Retrieval is
  the difference between working and broken code, at ~20× fewer tokens.
- **One expensive recovery (`spool`).** The spec necessarily says "the buffer
  reports an evicted value," which leaks the one non-obvious bit of the contract,
  and `rustc`'s suggestions supply the method names. So bare *does* reach 2/2 —
  but over two rounds and **16k output tokens against xerj's 656 (24×)**.
- **One honest non-win (`quill`).** A zig-zag + LEB128 varint is protobuf's wire
  format; the model has it memorised and reproduces it in ~1,356 tokens without
  the reference. Retrieval saves nothing here — it is a known convention, and the
  study says so.

## What the generated code actually looks like

`sieve-member`, one task, three arms — this is the whole argument in one frame.

**bare** (tool-free) cannot read the crate, so it *guesses the API from compiler
error messages* and hedges with fallback traits — its own opening comment:

```
//! The vendored `sieve` crate source is not readable from this session ...
//! so the exact spelling of the constructor / record / finalize entry points has
//! to be inferred. The compiler told us one thing for sure: `Sieve` has no `new`
//! ... To avoid betting the whole module on a single spelling, the calls below
//! are written against the most likely names and backed by fallback traits.
```

It produced **282 lines** of this, and it still **failed the test** — it guessed
the API surface but not the Kirsch–Mitzenmacher hashing, so its membership answers
were wrong. Cost: ~20k output tokens, two rounds.

**xerj** got the reference injected and wrote **8 lines** — and its own comment
shows it *learned the seal contract from the passage*, the exact thing bare could
never recover:

```rust
pub fn build(bits: usize, k: u32, keys: &[u64]) -> sieve::Sieve {
    let mut s = sieve::Sieve::mesh(bits, k);
    for &key in keys { s.dust(key); }
    // `sense` panics unless the sieve has been sealed first.
    s.settle();
    s
}
```

Cost: ~670 output tokens, one round, correct. (`native` reached the same 8-line
answer, at ~1,048 tokens, after searching the tree for the source.) The full
captured programs are in `measure/generated/`.

---

# Track 6 — the same result in four more languages

Everything above is Rust. The mechanism is not language-specific, and this track
shows it. Four purpose-built reference libraries, one per language, each with an
unguessable API and a contract the compiler cannot hint at — verdict is the real
toolchain (`python3 test.py`, `node test.js`, `cc *.c && ./prog`,
`javac *.java && java Test`):

- **Python — `warden`** (LRU cache): `hold`/`stow`/`fetch`/`trail`, where a
  `fetch` *promotes* to most-recently-used and overflow evicts the LRU key.
- **JavaScript — `garner`** (prefix trie): `plant`/`sprig`/`seek`/`crop`, where
  `seek` is true only for a *stored word*, false for a mere prefix.
- **C — `arena`** (generational allocator): `sprout`/`plant`/`pluck`/`wither`,
  where a withered handle stays stale even after its slot is reused.
- **Java — `Ledger`** (append-only log): `open`/`append`/`seal`/`replay`, where
  you must `seal` before you may `replay`.

Two tasks per language (8 tasks × 3 arms × 2 trials = 16 runs/arm). The harness is
`csmulti.py`; tasks in `tasks-ml/`; each task returns the library's own type or is
judged by its contract, and `validate` confirmed golden passes and the guessed-API
version does not build/run, in every language.

## Results — retrieval is cheapest in every language, by 2–278×

| task | lang | bare | native | **xerj** (median output tokens shown) |
|---|---|---|---|---|
| warden-run | Python | 1/2, 19,972 | 2/2, 1,520 | 2/2, **139** |
| warden-evict | Python | 2/2, 10,769 | 2/2, 1,379 | 2/2, **342** |
| garner-seek | JS | 2/2, 4,300 | 2/2, 1,229 | 2/2, **229** |
| garner-crop | JS | 2/2, 4,421 | 2/2, 1,361 | 2/2, **1,106** |
| arena-build | C | **0/2**, 28,404 | 2/2, 2,617 | 2/2, **2,159** |
| arena-reuse | C | 2/2, 9,056 | 2/2, 2,529 | 2/2, **820** |
| ledger-replay | Java | 2/2, 17,081 | 2/2, 1,328 | 2/2, **92** |
| ledger-checkpoint | Java | **0/2**, 36,453 | 2/2, 1,274 | 2/2, **103** |
| **total** | | **11/16, $11.18** | 16/16, $3.27 | 16/16, **$1.58** |

Median output tokens per language — the token-savings headline, and it is
consistent everywhere:

| language | bare | native | xerj | xerj vs native | xerj vs bare |
|---|---:|---:|---:|---:|---:|
| Python | 14,752 | 1,440 | **214** | 6.7× | 69× |
| JavaScript | 4,300 | 1,253 | **646** | 1.9× | 6.7× |
| C | 18,792 | 2,617 | **988** | 2.6× | 19× |
| Java | 27,108 | 1,274 | **98** | 13× | 278× |

**xerj and native both solve 16/16; xerj does it for 9,982 output tokens against
native's 26,477 (2.7×) and $1.58 against $3.27 (2.1×).** bare solves 11/16 and
burns 26× the output tokens of xerj overall — and *fails outright* on the two
tasks whose contract is both forced and non-leaky (`arena-build`'s opaque `Arena*`,
`ledger-checkpoint`'s truncate-then-seal).

### How bare behaves depends on the language, and it is honest to say so

- **Forced + non-leaky contract → bare fails** (`arena-build`, `ledger-checkpoint`).
- **Forced but the error message leaks the step → bare recovers, expensively.**
  Java throws `IllegalStateException("replay before seal")`; that message *names*
  the missing `seal`, so bare recovers `ledger-replay` — over ~17,000 output
  tokens, against xerj's 92 (**185×**). The compiler/runtime is a costly, partial
  substitute for the reference; XERJ is neither.
- **Not forced (dynamic language, known algorithm) → bare reimplements.** A trie
  (`garner`) is standard, so JS bare rebuilds it — correctly, but at 4,300 tokens
  to xerj's 229. The LRU contract (`warden`) is subtler, so Python bare failed one
  trial before inferring it.

In every one of those regimes, xerj wrote the least code and was correct.

### The same task, three ways (Java, `ledger-checkpoint`)

**bare** cannot read `Ledger.java`, so it reinvents an append-log with checkpoint
semantics — **503 lines** — and still fails the truncate-then-seal contract.
**xerj**, handed the reference, writes the whole thing in four lines:

```java
public class Solution {
    public static Ledger upTo(long[] vals, long seq) {
        Ledger l = Ledger.open();
        for (long v : vals) l.append(v);
        l.checkpoint(seq);   // keep 0..seq
        l.seal();            // required before replay
        return l;
    }
}
```

103 output tokens, one shot, correct — where bare burned ~36,000 and failed. The
captured programs are in `measure/generated/`.

---

# Verdict — reference-coding's value is gated by memorization, and the line is sharp

Put the regimes side by side (solve rate and total $ across each regime's runs):

| regime | bare | native | xerj | who wins |
|---|---|---|---|---|
| **unrecallable contract** — 7 domains, 21 runs (sift, grove, tally, weft, cadence, trellis, sieve) | **1/21**, $21.9 | 21/21, $4.26 | 21/21, **$3.38** | **xerj** |
| unfamiliar but spec-/compiler-recoverable (spool) | 2/2, $2.07 | 2/2, $0.36 | 2/2, **$0.29** | xerj (native close) |
| memorised convention (quill varint) | 2/2, **$0.33** | 2/2, $0.36 | 2/2, $0.31 | tie / bare |
| memorised bulk data (fieldnorm) | 3/3, $0.98 | 3/3, **$0.60** | 3/3, $1.43 | native |
| memorised logic (kv) | 6/6, **$1.49** | 6/6, $9.14 | 5/6, $4.40 | bare |

**The answer to "is xerj-code better than native Claude Code on the same model?"
is: yes, whenever the reference carries knowledge the model cannot recall — and
the harder the recall, the bigger the win.**

- **The core result, across nine unfamiliar domains and 25 runs:** native and
  xerj each solve **25/25**; bare solves **5/25**, and four of those five are the
  two soft cases (a memorised varint, and a ring buffer whose one non-obvious rule
  the spec had to state). On the **seven domains with a genuinely unrecallable
  contract** — a seal, a generational handle, a lazy refill, a specific hash
  scheme, an exact tie-break — bare solves **1/21** (a lone `rustc`-suggestion
  recovery) while burning **$21.9** flailing; xerj takes the *same* Claude Code to
  **21/21 at $3.38**, **1.5× fewer output tokens and 1.3× cheaper than native**
  ($4.26), ~35% fewer turns, and **6.5× cheaper than bare**.
- **The compiler is a partial, expensive stand-in for retrieval.** `rustc`'s "did
  you mean" can leak method *names* (so `weft`/`spool` bare sometimes recovers),
  but never a runtime *contract*, and only ever at 20–25× the token cost. XERJ
  delivers names and contract together in one shot.
- **The native gap grows when its search is unfocused:** grep pulled **1.06M
  input tokens** into context on the kv corpus, against retrieval's flat ~100–140k.
- **It is not a Rust artefact.** Track 6 reproduces the win in Python, JavaScript,
  C and Java: across 16 runs, xerj and native both solve 16/16, xerj at **2.7×
  fewer output tokens and 2.1× lower cost than native**; bare solves 11/16 and
  spends up to **278×** xerj's tokens (Java) recovering or reinventing. In a
  concise language the ratio is starkest — xerj wrote correct Java in a median of
  **98 output tokens**.
- On memorised code — which is *all popular public code*, including a 256-value
  quantization table that looks unguessable — retrieval is overhead. Pure memory
  is cheapest; injecting a reference ranges from neutral to actively harmful
  (worst on bulk-data tasks, where the model re-emits what it was handed).

The product implication is exact: **XERJ.code's market is the customer's own
private, proprietary, or post-cutoff code** — where the model cannot fall back on
memory and native Claude Code must crawl an unindexed tree. On public library
references it earns nothing, and the honest skill description must say so.

## What is measured, and what is not

Measured, on this machine, 2026-08-05, real tokens and dollars from
`claude -p --output-format json`:
- 3 regimes × 3 arms × 3 trials each (sift/kv) or 3 trials (fieldnorm); the
  headline sift result is 9/9 vs 0/9, not a marginal effect.
- The two peers (native, xerj) are the byte-identical agentic Claude Code; the
  bare floor is tool-free memory.

Not measured / honest gaps:
- **The native-vs-xerj gap stays ~1.5×, not 10×.** Track 4 buried the reference
  in ~102k records and the gap did *not* explode, because `grep` on a distinctive
  API name is precise regardless of tree size — native found the file in ~3 extra
  turns and ~1.5× the input. The regime where retrieval should crush grep is
  **behavioural / semantic** search: when you know *what you need* but not the
  symbol name, so there is no keyword to grep. XERJ's shipped embedder is lexical
  (feature-hash), so this study cannot show that advantage; a neural embedder on a
  behaviourally-described task is the untested lever most likely to widen the gap.
- **A genuinely large *private* codebase** (e.g. XERJ's own source) as the corpus,
  vs a purpose-built library. Track 4's libraries are unfamiliar by construction
  but small; a real private corpus is the honest end state.
- **n=2–3 per cell.** Effects this large (1/15 vs 15/15) do not need more; the
  memorised-track cost orderings are noisier and should be read as directional.
- **One model, Rust only.**
- **xerj's 5/6 on kv:** retrieval did not make the model *more* reliable on
  memorised code; on unfamiliar code it was 15/15.

## Bugs this work found

- **A real XERJ crash**, fixed in **PR #182**: the timestamp parsers in
  `xerj-compress/src/field_codec.rs` sliced strings by byte index after a
  byte-length guard, so any multibyte-UTF-8 value ≥19 bytes (Lucene's accented
  test data) panicked the server thread mid-index. Fixed with checked slicing
  (`v.get(a..b)?`) across all three parsers, with regression tests that reproduce
  the panic. Found by feeding a real multilingual corpus to `autoindex`.
- **The `--tools ""` leak** (harness): it does not disable tools; the "bare" model
  shelled out and vendored the dependency. Caught and fixed before any reported
  number depended on it.
- **BM25 is an ambiguous codegen target** (task design): Lucene ≥8 and tantivy
  disagree on the `(k1+1)` factor, so both agent arms "argued with" the reference.
  Replaced with `fieldnorm-quant`.
