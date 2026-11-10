# Cyber fuzz plan (`cyber.fuzz_plan`, #2773, #2767, #2769)

`xerj_common::cyber_fuzz_plan` is a network-free planning primitive. It does
not fetch corpora, compile harnesses, run a fuzzer, or send traffic. Ranking
uses caller-supplied surface signals and cited corpus bugs. There is no model.

## Entry-point ranking

`rank_candidates` scores each `FuzzCandidate` with an integer formula:

- untrusted bytes, slice, or reader input: 40
- unsafe blocks: 10 each, capped at 3
- arithmetic or length parsing: 20
- recursion: 15
- similarity to cited bugs: shared identifier tokens (5 each, capped at 3)
  plus 10 when the entry kind keyword appears in the bug summary, then capped
  at 40 across all citations

Ties break by function, file, line, then candidate id. Every `RankedTarget`
carries a rationale and at least one `CorpusBugRef` (`corpus`, `record_id`,
`cwe`, `summary`, `pin_sha`, `fetched_at`, `pinned_citation`). A candidate
with no cited bug is recorded in `abstained` with an explicit exclusion
reason. It is not given a guessed citation.

## Seeds, dictionaries, and the licence gate

`select_seeds` and `select_dictionary` use `hub::LicenceClass`. An `Excluded`
corpus returns `FuzzPlanError::ExcludedCorpusLicence` and contributes nothing.
`AdaptWithAttribution` and `ApproachOnly` are recorded with `pin_sha` and
`fetched_at`. This module stores provenance and content hashes, not corpus
bytes.

## Crash triage

`dedup_crashes` normalizes stack frames before hashing:

- ASLR `0x` addresses and `+0x` offsets are removed
- `(BuildId: …)` and bare `BuildId:` hex are removed
- `file:line:column` keeps `file:line`
- crate, module, function, and `file:line` are preserved

The bucket key is SHA-256 of the sanitizer kind, the normalized signal, and
the top 8 frames. Input order does not change the buckets. A known
`CorpusBugRef` matches only when its `record_id` or a backticked / snake_case
symbol from its summary appears in a normalized frame. Unmatched crashes stay
unmatched. `SeverityHint` comes from the sanitizer and signal, and is raised
to `Critical` only for a matched memory-corruption CWE.

## Harness skeletons

`generate_harnesses` emits text for each ranked target:

- cargo-fuzz: `libfuzzer_sys::fuzz_target!`
- libFuzzer: `LLVMFuzzerTestOneInput`
- AFL++: `afl::fuzz!`

The skeletons are not compiled or executed here.

## Sealed report

`FuzzPlanReport::plan` seals `report_version` `xerj-cyber-fuzz-plan/v1`, the
ranked targets, abstentions, seeds, dictionary, crash buckets, and skeletons.
`report_sha256` is SHA-256 of the RFC 8785 (JCS) preimage, excluding the
digest field. `to_jcs` emits canonical JSON. `verify_json` rejects duplicate
object keys.

Focused check, from `engine`:

```text
cargo test -p xerj-common --lib cyber_fuzz_plan
```
