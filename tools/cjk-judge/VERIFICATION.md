# CJK lexical judge (#2580)

Written by an AI coding agent (OpenAI Codex), operated by @Jarvis636431.
macOS arm64, Rust 1.97.1, source-built xerj v1.0.0-rc.100.
Original verification: independent branch initially based on upstream `25b108084`, rebased onto
`8f62343c7`. The intervening commit changes only `.contributors`; the tested
Rust sources and runtime snapshot are unchanged. No stacked dependency.

## Main refresh after #2697 (2026-10-30)

Merge upstream `6a7b390ac` into the prior PR head `25c61738d`. The only
conflict is the CHANGELOG insertion point introduced by #2697; preserve its
32 MiB batch-response fix entry and the CJK entry. The previous maintainer
merge remains in the ancestry. The judge implementation, HTTP regression,
smoke harness and rerank documentation are byte-identical to the previous
PR head; the effective main diff is still the same six files.

Fresh reruns on this composition: default rerank 59 passed, `judge-local`
61 passed and HTTP judge 15 passed, all with zero failures, ignores or
filters. Scoped rerank release build, formatting, strict Clippy in both
configurations, Python syntax, whitespace and the workflow-timeout guard
passed. The isolated node/runner build passed with the existing autoindex
`resolve_reporting` dead-code warning. Original failing-before-fix evidence
and scope exclusions below remain applicable; no new behavior is added by
this conflict resolution.

Fresh private-node ES-YAML on this composition: **1,453 passed, 0 failed,
10 skipped across 233 files**, retaining 74 existing catch-lenient cases.
The same executable then passed HTTP/MCP order, CJK evidence, threshold
counts, engine totals and original `_score` before flush, after flush and
after graceful restart. Scores remained exact 0.5900621118012421, partial
0.24340586742297435 and unrelated 0.0. The harness, runner and node exited 0.
Snapshot SHA-256:
`71735dfe2699509fd66c6febc2e5e84b9b7414e795854d0c8a9a7add56dc9907`.
These are fresh local receipts; hosted CI for the pushed refresh remains
separate. Linux/Windows, trained-model quality and broad relevance were not
re-tested. No performance claim is made.

## Earlier main integration (2026-10-30)

Merge current upstream `befe92035` into the PR's remote head `e45809c31`,
preserving the maintainer-authored merge and its egress-inventory fix (#2639).
Resolve the sole conflict in `CHANGELOG.md` by retaining both the CJK entry
and every upstream entry. The judge implementation, HTTP regression, smoke
harness and `docs/RERANK.md` are byte-identical to the previous PR head.
The effective diff remains the same six files; no stacked prerequisite,
workflow change, upstream rollback or new product behavior is introduced.

Fresh commands on the composed tree, from `engine/`:

```sh
cargo test --locked --offline -p xerj-rerank
cargo test --locked --offline -p xerj-rerank --features judge-local
cargo build --locked --offline --release -j 32 -p xerj-rerank
cargo fmt --all --check
cargo clippy --locked --offline -p xerj-rerank --all-targets -- -D warnings
cargo clippy --locked --offline -p xerj-rerank --features judge-local --all-targets -- -D warnings
cargo build --locked --offline --profile ci-test -j 8 -p xerj-server -p es-yaml-runner
cargo test --locked --offline --profile ci-test -p xerj-api --test judge_stage_http
```

Default rerank: 59 passed; `judge-local`: 61 passed. Both have zero failures,
ignored tests or filters, including all four egress-inventory tests. The
old filter below is historical and is no longer required. Scoped release
build, formatting and strict Clippy in both configurations passed.
The workflow-timeout guard and Python syntax check also passed.

HTTP judge: 15 passed, zero failures, ignored tests or filters. The isolated
node/runner build passed with the existing autoindex `resolve_reporting`
dead-code warning. From repository root, the unchanged smoke command above
(`python3 tools/cjk-judge/smoke.py --binary engine/target/ci-test/xerj
--runner engine/target/ci-test/es-yaml-runner`) exited 0:

- ES-YAML: 1,453 passed, 0 failed, 10 skipped across 233 files; 74 existing
  catch-lenient cases. No suite, skip or catch-lenient rule changed in this PR.
- The same executable snapshot passed CJK HTTP/MCP order, threshold counts,
  engine totals and original `_score` checks before flush, after flush and
  after graceful restart. Scores were exact 0.5900621118012421, partial
  0.24340586742297435, unrelated 0.0 in all three checks.
- Executable snapshot SHA-256:
  `c0a919e7c69e8a8c9418386e5703081ea3f3da19fc3380c0e0839760d5d69e5a`.

Audited the composed diff against `docs/CONTRIBUTION_REVIEW.md`: upstream
API/storage/workflow changes are intact, existing CJK/non-CJK assertions are
unchanged, and the original failing-before-fix receipt below remains the
regression evidence. All original scope limits and non-applicable durable
state, migration, accelerator and distributed-write sections still apply.
Linux/Windows and broad relevance/model quality were not re-tested. Hosted
CI on the pushed head is separate from these local results.

## Scope and evidence before implementation

The old lexical judge splits only at non-alphanumeric characters. The query
`雷同` therefore has no token in common with `该段内容存在雷同需要核查`.
Both new CJK score tests were run against that implementation and failed:
the containing document and the unrelated document both scored 0.0.
The tests now pass with the same assertions.

Reference approach, retrieved before implementation: Tantivy's MIT-licensed
[`src/tokenizer/ngram_tokenizer.rs`](https://github.com/quickwit-oss/tantivy/blob/main/src/tokenizer/ngram_tokenizer.rs),
lines 115–119 (all inner grams), 154–164 (codepoint-bounded slices), 253–280
(UTF-8 frontiers). Its [LICENSE](https://github.com/quickwit-oss/tantivy/blob/main/LICENSE)
was checked. No implementation was copied. XERJ's existing
`xerj-fts/src/analyzer.rs:799–862` also isolates CJK runs; its bigram-only
policy is not changed by this patch.

The new judge emits characters and adjacent pairs within CJK runs. Non-CJK
alphanumeric runs keep their old lowercase tokens. Separators and the
title/body boundary do not synthesize pairs. No dictionary, normalization,
model, calibrated confidence or first-stage analyzer change is introduced.
Single-character overlap can contribute even when the whole query phrase is
absent; adjacency is additional evidence, not a phrase-match guarantee.

## Local tests and builds

Run from `engine/`:

```sh
cargo test --locked --offline -p xerj-rerank --lib judge::tests -- --nocapture
cargo test --locked --offline -p xerj-rerank -- --skip every_outbound_client_in_the_engine_is_on_the_published_list
cargo test --locked --offline -p xerj-rerank --features judge-local -- --skip every_outbound_client_in_the_engine_is_on_the_published_list
cargo test --locked --offline --profile ci-test -p xerj-api --test judge_stage_http
cargo fmt --all --check
cargo build --locked --offline --release -j 32 -p xerj-rerank
cargo clippy --locked --offline -p xerj-rerank --all-targets -- -D warnings
cargo clippy --locked --offline -p xerj-rerank --features judge-local --all-targets -- -D warnings
```

Observed: default rerank 58 passed / 0 failed / 1 explicit filter;
`judge-local` rerank 60 passed / 0 failed / the same filter; HTTP judge 15
passed / 0 failed. Release build, formatting and both strict Clippy variants
passed. The HTTP suite's first sandboxed run passed 13 tests and failed the
two tests needing a loopback listener with PermissionDenied; the full suite
then passed outside the sandbox.

The unfiltered rerank suite fails its unchanged egress inventory check:
`new outbound network client(s) in ["xerj-mcp/src/status.rs"]`. The same
unchanged test target was compiled/run on a clean `25b108084` worktree and
failed with the identical path (3 passed / 1 failed). No egress table,
assertion, committed skip or unrelated client is altered here.

Tests cover the embedded Chinese term, one-character evidence, adjacency,
punctuation/emoji/Latin boundaries, supplementary Han, Japanese/Hangul
syllables, title/body separation, determinism and finite bounded scores at
the existing 4,000-query/8,000-document character ceilings for 30 candidates.
One non-CJK golden fixture records the old score bits
`[4599937497660334437, 4591716393592272481, 0]`; the candidate matches them
exactly. The query-term frequency table counts every emitted token for length
normalization and keeps fixed summation order, without rescanning document
tokens for each new gram.
No latency, RSS, throughput or general retrieval-quality claim is made.

## Real private-node control and candidate

From repository root, `smoke.py --binary <path>` snapshots the executable,
starts a temporary lexical-mode node and seeds three synthetic documents.
It uses `match_all` to hold the candidate set fixed, judges `雷同`, checks
threshold counts/engine totals and original `_score`, compares HTTP/MCP,
flushes, and gracefully restarts from the same private data directory.
Only this temporary node uses a 99% disk flood watermark on the shared
laptop. Hosted rerank is disabled; no model, personal corpus or existing node
is used. No index format or reindex/migration is involved.

The original control snapshot from prior source base `7d3ca96b1` is
`bf8e611d814f935e5a313cb735f33fb3d7ff532b78c04a5fe3af87eb020f3853`.
That base and current `25b108084` have the identical original judge blob
`fd51419b804a9240fb2248256671851a7a480ee1`. The control is behavior evidence,
not a matched-build performance comparison. With `--expect-zero`, all three
documents score 0.0 before flush, after flush and after restart; a positive
threshold drops them all.

The candidate snapshot is
`9bef44e59d43b3403f15364452c3e8c7e67f6bdc5dcce4d40498d293754c0c87`.
Observed scores in this fixed page: exact-containing document
0.5900621118012421, partial single-character evidence 0.24340586742297435,
unrelated document 0.0. All HTTP/MCP, pruning/count, flush and restart checks
passed. These are lexical fixture scores, not probabilities calibrated
against human relevance judgments.

The same candidate executable passed the full ES-YAML gate on a fresh private
node: **1,451 passed, 0 failed, 10 skipped across 232 files**, with the
existing 75 catch-lenient cases. Runner and node exited 0. The subsequent
HTTP/MCP, threshold, flush and restart checks repeated the same scores.

```sh
# Run the scoped builds from engine/ before this repository-root command:
# cargo build --locked --offline --profile ci-test -j 8 -p xerj-server -p es-yaml-runner
python3 tools/cjk-judge/smoke.py \
  --binary engine/target/ci-test/xerj \
  --runner engine/target/ci-test/es-yaml-runner
```

## Contribution review and exclusions

Audited against `docs/CONTRIBUTION_REVIEW.md`: preserve opt-in behavior,
non-CJK word tokens, deterministic arithmetic, input ordinals, scorer name,
original engine scores, count semantics, egress policy and model path. The
HTTP test uses the real router/storage before and after flush. The private
binary harness adds MCP transport and durable restart coverage.

No dependency, workflow, stored schema, journal, migration, provider/model
execution, feature-gate or installation/deployment change. Durable-state
failure atomicity, optional accelerator fallback and distributed replication
are not applicable. The ordinary first-stage search path is unchanged.

Not tested: the original six-file Windows corpus, broad CJK relevance,
cross-language threshold calibration, dictionary segmentation, half-width
kana/jamo normalization, or Linux/Windows execution. In particular, first-stage
candidate misses and rank order without `judge` in #2580 remain separate.
The existing model scorer/tokenizer is unchanged; feature tests use its tiny
synthetic fixture, not a real trained-model quality evaluation. Hosted CI
remains separate from local receipts.
