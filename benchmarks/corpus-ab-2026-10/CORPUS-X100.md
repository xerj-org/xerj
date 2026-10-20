# CORPUS-X100 — scaling rust-vulns ×100, and what we expect it to change

Status: design recorded 2026-10-01, immediately after PR #1116 (the #1111 A/B tie).
This file is (a) the engineering plan for the scaled corpus, (b) the **pre-registered
protocol + predictions for the re-test** (same discipline as #1111: written before the
run, tie/loss published as such), and (c) the source notes for the blogpost. Sample
artifacts that back every claim below: [corpus-x100/](corpus-x100/) (miner script, seeds,
raw output).

---

## 1. Why: the measured autopsy of the #1111 tie

From [RESULTS.md](RESULTS.md): arm X (corpus+XERJ) filed higher-precision findings but
**zero confirmed findings trace to corpus-assisted discovery** (x1: one unused sweep
query; x2: zero invocations). Baseline measured off the gen-1 pack (`rust-vulns-pack.zip`,
gen 1, 2026-10-01, 16 shards, 10.5 MB):

| fact | number |
|---|---|
| records | 1,963 (advisory prose, identity-resolved RustSec+OSV; 4,128 envelopes → 1,963) |
| records containing **any code** | 277 (**14%**) — advisory bodies, mostly prose |
| `affected_functions` populated | 271 (13%), names only, no source |
| published ≥2025 (**un-memorized stratum**) | **1,001 (50%)** — 716 from 2026 alone |
| seeds with a GitHub fix-commit ref + semver range | 995 (50%) |

Three causes, in order of weight:

1. **Surface — zero invocations.** The agent never queried the corpus. Nothing in the
   task, tool description, or workflow made retrieval the natural move at the moment of
   "is this pattern a known bug class?". A corpus that is never queried cannot help,
   regardless of content.
2. **Shape — prose cannot match code.** A reviewer's mental query is *code-shaped*
   ("does `&fmt[i..]` walking bytes have precedent?"). Advisory prose about
   `hubuum_client` does not retrieve against that, semantically or lexically. 14% code
   coverage ≈ no code corpus.
3. **Size/memorization — the cause we expected, half-wrong.** We hypothesized the model
   had memorized the content; in fact **half the pack is post-cutoff** and was still
   unused — because causes 1 and 2 dominated. Size alone would not have fixed the tie.
   But scale still matters for the *tail*: bug classes outside the famous ones live in
   niche crates' histories, which is where retrieval beats memorization (our own
   reference-coding measurement: 1.65× fewer output tokens on un-memorized code).

**Consequence for design:** ×100 must fix shape first, surface second, size third.
A ×100 of more prose would tie again.

## 2. What ×100 honestly requires (the arithmetic is recorded, not guessed)

Measured on a 4-seed live sample (zeptoclaw, fuel-vm, ml-dsa/RustCrypto monorepo,
hubuum_client — `corpus-x100/s1-sample-output.jsonl`): **9.8 vulnerable functions and
3.8 trigger tests per advisory seed**, ~35 published crate versions ≤ fixed per seed
(version-pinning metadata). One seed walked end-to-end (GHSA-5wp8-q9mx-8jx8, zeptoclaw
shell-allowlist bypass): advisory → fix commit → `validate_command()` source at `fix^`
(3,047 B) → fix diff → the before/after regression-test pair (the before-test even
asserts the *wrong* behaviour and the after-test cites the GHSA id — the trigger pair
is a self-documenting exploitability record).

The strata:

| stratum | content | projected unique records | provenance/confidence | status |
|---|---|---|---|---|
| S0 (existing) | advisory spine, identity-resolved | 1,963 | advisory-backed | done (gen 1) |
| S1 | version-pinned **vulnerable function source** extracted at `fix^` per advisory | **~10,000** (995 × 9.8, measured; tree-sitter pass will trim over-attribution to ~5–15k) | advisory-backed | sample validated |
| S2 | **trigger pairs**: before/after regression tests from the fix commit | **~4,000** (995 × 3.8, measured) | advisory-backed | sample validated |
| S3 | **mined hardening commits**: security-relevant fixes (panic/overflow/traversal/escape/injection keywords + `.rs` diff) from the git histories of the top ~2,000 crates — bugs that never got an advisory | **~150,000–200,000** (2,000 crates × ~50–100 commits × ~2 fns; measured per-repo rates TBD at stage-3 dry-run) | **provisional** (keyword+LLM-classified, never advisory-backed) | designed, not run |
| **total** | | **~196,000 ≈ ×100** | ~8% advisory-backed, ~92% provisional | |

Two things this arithmetic forces us to say plainly (blogpost §3):

- **Advisory-backed scaling caps at ~×8.** The world has ~2k Rust advisories; mining them
  perfectly yields ~16k code records. Anyone promising a big advisory-only corpus is
  padding with duplicates.
- **The ×100 is real only via S3**, whose records are provisional by construction. That
  is acceptable *for retrieval evidence* (a reviewer wants precedent, and precedent with
  a confidence tag), and every record carries `provenance` so downstream claims can
  stratify. Uniqueness key: `(repo, path, function, introducing commit)`; the same
  function fixed twice for two different bugs is **two** records (legitimately — review
  value). Cross-version near-duplicates collapse into one record + version list — the
  ×100 counts unique records, not shards.

## 3. Record schema (additive; S0 unchanged)

One new discriminated field, `record_type`:

- `advisory` — S0, current shape untouched.
- `vuln_function` — `{record_type, advisory_ids[], package, versions[], first_seen_version,
  fixed_version, repo, commit, path, function, lang_line_span, source, fix_diff,
  bug_class, licence, provenance: "advisory-backed"|"mined-provisional",
  extraction: "tree-sitter-rust"|"regex", mined_at}`.
- `trigger_pair` — `{...same header..., test_before, test_after, asserts}` — the PoC shape.

`bug_class` is a normalized taxonomy (~40 canonical Rust security classes, seeded from
the #1111 validation gate's own 33 confirmed classes — panic-under-abort, slice
char-boundary, unbounded recursion on request-controlled input, path join traversal,
CRC-not-covering-footer, silent-merge data loss, early-exit auth, unbounded map growth…
our own audit output becomes taxonomy seeds). Queries against the corpus then match
*class + code shape*, which is the thing a reviewer actually holds in their head.

**Licence discipline (non-negotiable, same rules as corpus.json):** per-record `licence`
field — RustSec/OSV metadata CC0; crate and repo source under each project's own licence
(MIT/Apache dominant; GPL/BUSL crates are fine **for retrieval evidence** but tagged, so
any future "copy this pattern into code" decision can filter; copying rules unchanged
from CLAUDE.md). S3 records carry the repo's licence and are never copied, only cited.

**Scale budget:** ~200k records × ~5 KB ≈ 1.0–1.5 GB JSONL → ~0.6–0.9 GB zipped pack
(GitHub asset cap 2 GB/file — one asset, no split). Build compute: 995 partial clones +
`git show fix^` ≈ ~1 h parallel-16; S3 = 2,000 partial clones + `git log --grep` scans
≈ 6–11 h single-threaded, <1 h at ×16. Indexing cost measured at build time (expect
minutes; goes in the blogpost as a XERJ scale datapoint).

## 4. Re-test protocol (pre-registered — committed before any re-run)

Changes vs #1111, each addressing a diagnosed cause:

1. **Surface fix:** arm X's task file names the corpus the way a tool would:
   "a Rust vulnerability-pattern corpus is queryable via `xerj code rust-vulns-xl '<code
   shape or bug class>'`; consulting precedent before filing is part of the method."
   Arm P gets the symmetric sentence about its own available knowledge ("you have no
   external knowledge base; use your own judgment"), so instruction effort is matched.
2. **Shape fix:** corpus is the xl build (S0+S1+S2+S3) with `bug_class` taxonomy.
3. **Instrumentation (no more inferred provenance):** every `xerj code` call is logged;
   a finding may cite corpus record ids; the gate scores `cited_records` per finding
   instead of replaying transcripts.
4. **Two targets, because they should behave differently:**
   a. **Memorized-idiom target** — XERJ's own engine code (same as #1111).
   b. **Un-memorized target** — a niche dependency-heavy Rust project (selected after the
      corpus freeze; ≥6 direct deps, <5k GitHub stars, not in the model's comfort zone),
      with ground truth from its post-cutoff advisories. Held-out discipline: the
      project's advisories are *in* the corpus — that is not leakage, it is the corpus
      doing its job; the gate scores whether the agent finds what the corpus could have
      told it.
5. **Stratified scoring:** every scored finding is tagged pre-cutoff/post-cutoff
   memorization stratum and advisory-backed/mined provenance.

### Pre-registered predictions (falsifiable; published now, before the run)

- **P1 (surface):** arm X issues ≥1 corpus query per run. If zero again, the re-test is
  void and the surface design failed — say so and stop.
- **P2 (shape):** corpus-cited findings cite S1/S3 `vuln_function`/`trigger_pair`
  records, not S0 prose. Cites confined to prose = shape fix failed.
- **P3 (where an advantage is even possible):** on the un-memorized target, X's confirmed
  findings on post-cutoff ground truth exceed P's. On the XERJ-own-code target we
  **expect a tie** — that is the prediction, not a hedge.
- **P4 (precision):** X's false-positive rate stays ≤ #1111's (0). If the provisional
  S3 stratum degrades precision below P's, S3 gets down-weighted in the pack's suggested
  ranking and that is recorded as a cost of scale.
- **P5 (the honest end):** if P1–P3 hold and end-to-end advantage still does not, the
  publishable conclusion is "retrieval corpora of this shape do not measurably help
  LLM security review at this scale" — a real negative result, and the blogpost says it.

## 5. Blogpost skeleton (for the next post; numbers trace to this repo)

1. **The bet and the honest tie** — #1111 by the table (RESULTS.md): 13 vs 16
   confirmed-exploitable, 1 FP vs 0, $5.06 vs $4.41 per confirmed finding — and zero
   corpus-assisted discoveries. Published as a tie because that is what happened.
2. **Autopsy of a tie** — the three causes with the baseline numbers (§1 above). The
   genuinely uncomfortable finding: half the corpus was already un-memorized and it
   still never got queried. The bottleneck was not model knowledge; it was that a prose
   corpus has no answer to a code-shaped question.
3. **What ×100 really means** — the arithmetic nobody publishes: advisories cap at ~2k
   worldwide; honest advisory-backed scaling is ×8 (measured 9.8 fns + 3.8 triggers per
   seed); the ×100 lives in the ecosystem's un-advised fix history, at the cost of
   provisional provenance. Hero demo: the zeptoclaw walkthrough (advisory →
   `validate_command()` at `fix^` → the regression test that documents the exploit).
4. **The discipline** — pre-registration, provenance tags, licence tags, stratified
   scoring; P1–P5 published before the run.
5. **The re-run** — whatever it says. Either the corpus earns a qualified claim
   (un-memorized stratum, code-shaped corpus), or the negative result stands.
6. **Portable lessons** (true regardless of P1–P5): dynamic re-verification beats any
   corpus for precision; two independent reviewers find ≈1.5× one (33 classes, 2 shared);
   retrieval's lane is precedent the model cannot have memorized.

## 6. Execution order (tracked as #61 / #1110 follow-on)

1. **S1+S2 production harvester** — extend `xerj-autoindex` harvest with a
   `git-fix-mining` source (partial clone, `git show fix^:<path>`, tree-sitter-rust
   function extraction — the sample miner's regex pass is the prototype and its
   known miss is recorded: fuel-vm's fix touched no `fn`-attributable code, whole-file
   fallback required). Engine-side change; reference-code mandate note: mining design
   adapts the published CVEfixes/Vul4J method (commit-mining, not copying) — cited in
   the harvester's docs.
2. **Taxonomy pass** — `bug_class` classifier over S0–S2 (LLM-assisted, human-audited
   sample ≥200), seeded from #1111's 33 confirmed classes.
3. **S3 dry-run** — 20 crates, measure commits/repo, then full 2,000-crate run.
4. **Pack gen 2** — `rust-vulns-xl`, signed, published; freshness badge reports both
   total and advisory-backed counts (a 200k-record pack whose badge hides that 92% is
   provisional would be a dishonest pack).
5. **Re-test** per §4, then RESULTS-2.md and the blogpost.
