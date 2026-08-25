# grep-vs-retrieval benchmark (Apache Lucene)

> **Status: harness only. No results are published here yet.**
> The runner, the question set and the analyzer are complete and tested against a
> live XERJ instance. The measurement matrix has **not** finished, so this
> directory deliberately contains **no** `results.json`, `runs/` or `logs/`.
> Numbers will be added in a follow-up once every run is graded.

This fills a gap the reference-coding study named explicitly: *"The measurement
harness - the three-arm runner, the purpose-built reference libraries, the
hidden-test task specs - is **not** in this repository; only its outputs are."*
(`docs/case-studies/reference-coding/CASE_STUDY.md`). This is the equivalent
harness, shipped.

## What it measures

For real questions about Lucene internals, how many tokens a coding agent spends
answering them three ways:

| arm | how it answers |
|---|---|
| `native` | Claude Code with Grep/Read over the real Lucene tree |
| `xerj`   | passages retrieved from a XERJ symbol index, injected |
| `hybrid` | both available |

Token and cost figures come from the `claude` CLI's own accounting
(`claude -p --output-format json`), not from estimates or byte proxies.

## Honesty properties built in

- **Answers are blind-judged.** `analyze.py` refuses to score a run it has not
  graded and reports `ungraded` as UNKNOWN rather than assuming success. A
  cheaper wrong answer is a loss, not a win.
- **The native arm is not handicapped.** It runs with `cwd` set to the real
  6,012-file Lucene checkout and keeps its normal tools.
- **No answer leakage.** The retrieval arm receives passages, never the answer.
- **Prior-hostile questions are marked as such**, so questions the model may
  already know from training are separated from ones it cannot recall.

This repository has a standing precedent for publishing where XERJ *loses*
(`docs/research/wordpress-verification-and-xerj-vs-grep.md` concluded the
structured call graph was worse than grep for reachability, with silent false
negatives). The same rule applies to whatever this harness measures.

---

## Original harness notes

Measures the claim the post wants to make. It does not assume it.

**Target claim (user's words, to be corrected by data):** *"your claude code / codex wastes
40% tokens just on grep/sed files; XERJ reduces it many times."*

**Status: harness built and validated end-to-end. Pilot = 1 question x 2 arms, real runs.
Full matrix not yet run.** Everything below marked PILOT is measured; everything else is
design. The pilot already contradicts part of the claim — see "What the pilot already shows".

---

## 0. What `claude -p --output-format json` actually returns

Verified by running it, not assumed. Real keys from a live throwaway run:

```
is_error duration_api_ms duration_ms num_turns stop_reason session_id uuid subtype
total_cost_usd usage modelUsage permission_denials terminal_reason api_error_status
fast_mode_state fast_mode_disabled_reason result ttft_ms ttft_stream_ms time_to_request_ms type
```

`usage` contains:
```json
{"input_tokens":6,"cache_creation_input_tokens":5956,"cache_read_input_tokens":68601,
 "output_tokens":608,"output_tokens_details":{"thinking_tokens":89},
 "server_tool_use":{"web_search_requests":0,"web_fetch_requests":0},
 "service_tier":"standard",
 "cache_creation":{"ephemeral_1h_input_tokens":5956,"ephemeral_5m_input_tokens":0},
 "iterations":[ ... ]}
```

`modelUsage` is per-model: `inputTokens, outputTokens, cacheReadInputTokens,
cacheCreationInputTokens, costUSD, contextWindow, maxOutputTokens, canonicalModel, provider`.

### What it does NOT expose — this is the crux

1. **No per-tool token attribution whatsoever.** There is no field anywhere that says how
   many tokens a Grep or Read cost. The "40% on grep" number **cannot be read off this
   JSON**. It must be reconstructed from the transcript, and that reconstruction is an
   estimate. This is the single most important fact for honesty: the headline must rest on
   something exact, not on the reconstructed share.
2. **`input_tokens` is a trap.** It was `2` on a real run whose actual billed input was
   ~24,000 tokens. Nearly everything lands in `cache_creation_input_tokens` and
   `cache_read_input_tokens`. Quoting `input_tokens` would understate cost ~1000x.
   Billed input = `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
3. **`usage.iterations` is not a full per-call log.** On a 3-API-call run it contained
   **1** entry (the last). Per-call data must come from `--output-format stream-json
   --verbose`, deduping `assistant` events by `message.id`.
4. **Streamed `output_tokens` are partial.** Summing them gave **9** against a true **608**.
   Output must be taken from the final `result` event. (`ledger.py` asserts this: input and
   cache counters reconcile exactly, output is taken from `result`.)
5. **A second model is billed.** Every run also invokes `claude-haiku-4-5` (~$0.001).
   It is unrelated to retrieval strategy and is reported separately, never charged to an arm.

---

## 1. The questions (10, ground truth verified by reading the source)

In `questions.jsonl`. Each carries the exact file, line, and the fact that makes an answer
right or wrong. All were verified against the corpus before being written down.

| id | class | asks | ground truth (verified) |
|---|---|---|---|
| q01_block_size | prior_hostile | `ForUtil.BLOCK_SIZE` in the *current* default postings format | **256**, `lucene104/ForUtil.java:34` — five backward-codec copies say 128 |
| q02_hnsw_level | mechanism | how HNSW picks a new node's level | `ml = M==1?1:1/Math.log(1.0*M)` :183; `(int)(-log(u)*ml)` :536-541 |
| q03_mmap_chunk | prior_hostile | `DEFAULT_MAX_CHUNK_SIZE` in MMapDirectory | not a constant: `JRE_IS_64BIT ? (1L<<34) : (1L<<28)` :185 |
| q04_merge_score | mechanism | how TieredMergePolicy scores a merge | `mergeScore=skew`, `*= pow(totAfterMergeBytes, 0.05)` :713-717; `deletesPctAllowed=20.0` :82 |
| q05_hnsw_limits | lookup | hard caps in Lucene99HnswVectorsFormat | `MAXIMUM_MAX_CONN=512` :99, `MAXIMUM_BEAM_WIDTH=3200` :109, `HNSW_GRAPH_THRESHOLD=100` :134 |
| q06_bm25_k3 | prior_hostile | query-side saturation param in BM25Similarity | **`k3` exists**, default `-1f` (disabled), `((k3+1)*qtf)/(k3+qtf)` :51,122,126 |
| q07_iwc_flush | lookup | default flush triggers | `DEFAULT_RAM_BUFFER_SIZE_MB=16.0` :83; `DEFAULT_MAX_BUFFERED_DOCS=DISABLE_AUTO_FLUSH` :78 |
| q08_osq_params | lookup | OptimizedScalarQuantizer defaults | `DEFAULT_LAMBDA=0.1f` :57, `DEFAULT_ITERS=5` :59, `MINIMUM_MSE_GRID` :45 |
| q09_exhaustive_count | **index_hostile** | count files containing `implements Accountable` | **43** (`grep -rl ... \| wc -l`) |
| q10_tragic_event | multi_hop | which method records a tragic exception, per call path | `tragicEvent(...)` :1869 vs `onTragicEvent(...)` :713 — two *different* methods |

**Why these and not trivia.** Every one is a question an engineer actually asks before
changing indexing or vector-search behaviour: what block size will my postings use, how do I
size an HNSW graph, why did my merge not fire, what flushes my writer, why is my quantizer
lossy. None can be answered by reading a README.

**Three question classes exist on purpose:**

- `prior_hostile` (q01, q03, q06) — the checkout **contradicts** what a model remembers about
  Lucene. q01 is 256 where the world says 128. q06's `k3` does not exist in upstream Lucene
  at all. An agent that answers from memory scores **zero** here. These exist because of a
  problem found while building this (section 4.1).
- `index_hostile` (q09) — deliberately built to make XERJ lose. Exhaustive counting across a
  tree is exactly what top-k retrieval cannot do and what grep does perfectly. It is in the
  set so the result can be reported against us, following the precedent in
  `docs/research/wordpress-verification-and-xerj-vs-grep.md`, where XERJ's call graph was
  found **worse** than grep for reachability with silent false negatives.
- `lookup` / `mechanism` / `multi_hop` — the ordinary middle.

---

## 2. The arms

| arm | tools | cwd | what it represents |
|---|---|---|---|
| `native` | `Grep, Glob, Read` (Bash **denied**) | the real checkout | what a Claude Code user has today |
| `xerj` | `Bash(xq:*)` only; Grep/Glob/Read **denied** | an **empty** dir | can the index alone answer it? |
| `hybrid` | `Grep, Glob, Read, Bash(xq:*)` | the real checkout | the realistic product experience |

`xerj` runs in an empty directory so it physically cannot fall back to reading the tree.
`hybrid` is the fairest head-to-head (both sides agentic, each picks its own strategy) and is
the number that should anchor a public post; `xerj` is the strict floor.

**Both arms are agentic and write their own queries.** Neither gets a human-tuned query. `xq`
(the only retrieval affordance) is deliberately generic — one fixed ES template, no
per-question logic. If `xq` contained question-specific tuning the benchmark would be rigged.

### Exact commands

```bash
cd outreach/bench
./calibrate.sh                 # re-derive chars/token for this corpus
REPEATS=3 ./run.sh all all     # 3 arms x 10 questions x 3 repeats = 90 runs
python3 analyze.py             # emits judge_queue.jsonl
# blind-grade judge_queue.jsonl -> judge_verdicts.jsonl, then:
python3 analyze.py
```

Under the hood, per arm (verbatim from `run.sh`):

```bash
# native
cd /home/claude/.xerj-code/corpora/lucene-meta/lucene && claude -p "$RULES\n\nQUESTION: $q" \
  --output-format stream-json --verbose \
  --allowedTools "Grep,Glob,Read" --disallowedTools "Bash,WebSearch,WebFetch,Task"

# xerj
cd outreach/bench/.sandbox && claude -p "$RULES ... xq \"<query>\" [k] ...\n\nQUESTION: $q" \
  --output-format stream-json --verbose \
  --allowedTools "Bash(xq:*)" --disallowedTools "Grep,Glob,Read,WebSearch,WebFetch,Task"

# hybrid
cd /home/claude/.xerj-code/corpora/lucene-meta/lucene && claude -p "$RULES ... xq ...\n\nQUESTION: $q" \
  --output-format stream-json --verbose \
  --allowedTools "Grep,Glob,Read,Bash(xq:*)" --disallowedTools "WebSearch,WebFetch,Task"
```

Every run keeps its full raw `stream-json` transcript in `runs/`. Nothing is summarised at
capture time, so any number in the writeup can be re-derived from the evidence.

---

## 3. What is measured, and how it is attributed

**Primary (exact, from the CLI's own billing counters):**
- `total_tokens_billed` = billed input + output.
- **`tokens per CORRECT answer`** = total tokens spent by an arm / number it got right.
  This is the headline. Tokens-per-*run* would reward an arm for giving up or hallucinating.
- `cost_usd_cache_neutral` — see 4.2.

**Secondary (estimated — label it as such in public):**
- tool-payload share of tokens, via `len(tool_result) / 2.785`.
  2.785 chars/token was **measured** on this corpus by `calibrate.sh`: two prompts differing
  by 20,000 chars of real Lucene source differed by 7,181 billed tokens. The usual chars/4
  rule understates Java source by ~30%.
- An alternative estimator — the `cache_creation` delta between consecutive calls — was
  tried and **rejected**: with extended thinking on it is dominated by re-sent thinking-block
  signatures, reading 1,613 tokens for 819 chars of grep output (~5x too high). Kept only as
  a loose upper bound.

Three different shares are reported because they answer different questions: share of unique
context, share of *billed* tokens (a tool result pulled in at turn 2 is re-billed on every
later call), and share of dollars (cache reads cost 10% of fresh input). **The user's "40%"
is a billed/dollar-share style claim; say which one you mean.**

`ledger.py` refuses to emit numbers it cannot reconcile against the CLI's own totals.

---

## 4. What would make this dishonest, and what stops it

### 4.1 Giving the model the answer in the prompt
Neither arm's prompt names a file, class, or constant beyond what the question asks. `xq` is
generic. **Found while building this:** on an early probe the model answered a Lucene question
with *zero tool calls*, straight from memory ("a codec header is `9 + codecName.length`
bytes"). A benchmark of memorised Lucene trivia measures nothing about retrieval and would
have shown the native arm using ~0 grep tokens. Mitigations: an explicit instruction to answer
only from what is retrieved in-session, mandatory file:line citation, and three
`prior_hostile` questions where memory gives a **wrong** answer, so parametric leakage is
detectable rather than silent.

### 4.2 Cache warmth — the biggest threat, and it is real
On the pilot, `xerj` used **1.5x fewer tokens** but cost **2.4x more dollars**
($0.2345 vs $0.0999). Cause: `native` ran against a warm prefix cache (57,945 cheap
cache-*reads*) while `xerj` ran cold (21,693 expensive cache-*writes*). The arms have
different prompt prefixes (different tool sets, different cwd) so they can *never* share a
cache, and warmth depends on run order and a 5-min/1-hr TTL.

Consequences, all applied: **raw dollars are not a fair cross-arm metric.** Report tokens, and
report `cost_usd_cache_neutral` (every input token priced as fresh input), which removes
warmth luck. On the pilot the cache-neutral ratio (1.52x) matches the token ratio (1.53x),
while raw dollars invert. Also: interleave arms, repeat 3x, use medians, discard warm-ups.

The price model was **derived, not assumed** — solving the CLI's reported `costUSD` against
its own counters recovered exactly $5.00/Mtok input and $25.00/Mtok output. That also caught
an error: these runs use the **1-hour** cache (2.0x write multiplier), not the 5-minute cache
(1.25x). Assuming 1.25x produced a *negative* output price.

### 4.3 Cherry-picking questions that suit an index
`q09` exists solely to make XERJ lose, and results are reported per question class so a win
driven entirely by one class is visible. Ground truth was written **before** any arm ran.
Questions are never dropped after seeing results; `results.json` keeps every run.

### 4.4 Letting the native arm behave unlike a real user
The native arm gets exactly `Grep, Glob, Read` in the real checkout — no more, no less. It is
not handicapped (not forced to read whole files) and not helped (no Bash, no pre-seeded paths).
Note `xq` output is capped at 1,600 chars/hit so the index arm cannot dump the corpus either.

### 4.5 Grading the arm you like more generously
Grading is blind: `analyze.py` writes `judge_queue.jsonl` with arm labels stripped and rows
shuffled under a fixed seed. Mechanical string checks can only mark a run FAIL or
NEEDS-JUDGE — never correct — because containing "256" is not the same as asserting it.

### 4.6 Ignoring that retrieval can actively mislead
Real, observed: for q01 `xq` returned an irrelevant `CharBlockArray` first, and the **wrong**
`ForUtil` (backward-codecs lucene103, `BLOCK_SIZE = 128`) second, with the correct lucene104
one third. The index contains backward-codecs and test near-duplicates of many classes. If
that misleads the model in the full run, it must be reported.

### 4.7 The indexing cost the post claims is free
"no tokens used for indexing" is true only of *LLM* tokens. Index build is CPU/disk/wall time
over 6,012 files, plus 75,578 docs kept live. If the post says "free", it should say **"no LLM
tokens"**, not "free".

---

## 5. What the pilot already shows (PILOT — n=1 question, 1 repeat/arm)

Both arms answered q01 **correctly (256)**.

| | native | xerj |
|---|---|---|
| total tokens billed | 64,487 | 42,068 |
| tool calls | 2 (Glob+Grep) | 1 (`xq`) |
| est. tool-output tokens | **145** | 1,072 |
| tool share of billed tokens | **0.5%** | 5.1% |
| cache-neutral cost | $0.3283 | $0.2153 |
| raw cost | $0.0999 | $0.2345 |

**This does not support "40% wasted on grep."** On a well-targeted question the native arm
spent ~145 tokens on grep output — **0.5%** of its billed tokens. The dominant cost in both
arms is the **fixed ~24k-token system prompt and tool definitions, re-billed on every API
call** (3 calls native vs 2 xerj). XERJ's token win here (1.53x) comes mostly from finishing
in **fewer turns**, not from grep output being large.

Interpretation to carry into the writeup: any large win is likely to come from *turn
reduction*, and any large "grep share" will only appear on exploration-heavy questions where
the agent hunts across many files. Whether that is 40%, 18%, or 3% is exactly what the full
matrix is for.

---

## 6. What would DISPROVE the user's claim

The claim has two independent halves. Either can fail on its own.

**Half A — "40% of tokens go to grep/sed/read."** Disproved if the median estimated
tool-output share of billed tokens in the `native` arm is **materially below 40%** (say <25%)
across the 10 questions. *The pilot measured 0.5% on q01, so this half is already in serious
trouble and the number in the post must change.* It is confirmed only if the median lands
near 40%.

**Half B — "XERJ reduces it many times."** Disproved if any of:
- median **tokens per correct answer** for `hybrid`/`xerj` is not meaningfully below `native`
  (a ratio under ~1.3x does not survive being called "many times");
- XERJ wins tokens but loses **accuracy** — cheaper wrong answers are not a win;
- the win exists in raw dollars only via cache luck and vanishes under `cost_usd_cache_neutral`;
- the win is carried by one question class and reverses on others (q09 is expected to go
  against XERJ; if it loses on `mechanism` and `multi_hop` too, there is no story).

**Pre-committed reporting rule.** Whatever the measured numbers are, they go in the post
unchanged, including q09 if XERJ loses it, and including a corrected figure for the 40%.
If Half A collapses but Half B holds, the honest post is *"grep isn't where your tokens go —
turns are; retrieval cut tokens-per-correct-answer N x"*, which is a **better** and more
defensible claim than the original.

---

## Files

- `questions.jsonl` — 10 questions + verified ground truth + grader notes
- `run.sh` — runs the arms, keeps raw transcripts
- `xq` — the only retrieval affordance (intentionally generic)
- `ledger.py` — transcript to auditable token ledger; reconciles or refuses
- `analyze.py` — blind grading + medians + per-class breakdown
- `calibrate.sh` — measures chars/token for this corpus
- `runs/`, `logs/`, `results.json`
