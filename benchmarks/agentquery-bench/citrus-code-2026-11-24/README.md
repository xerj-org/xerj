# XERJ and ripgrep in Citrus Code

The configuration with XERJ and tool selection rules used **27.8% fewer tokens** than the ripgrep baseline across 51 matched pairs of correct answers. With free tool choice, total tokens fell **15.9%** across 50 pairs, while the median pair used **8.6% more**. Both configurations saved tokens on aggregate in Ruff and used more in Lucene. The numbers describe 18 read-only source questions on two pinned repositories using a single model.

## Study design

Each question ran three times in each configuration, for **162 runs**. The questions cover symbol definitions, implementation behavior, and exhaustive literal matching, with three questions of each type per repository. Each repetition starts a fresh Citrus Code session. A fixed seed controls the order, and each configuration appears once in each order position for every question across its three repetitions. Six workers run different repetitions concurrently.

| Configuration | Tools and instructions |
|---|---|
| ripgrep baseline | Citrus Code's `read`, `grep`, `find`, and `ls` |
| Free tool choice | Baseline tools plus direct local XERJ `def` and `search` wrappers |
| Guided tool choice | Same tools as free choice. The prompt asks the agent to start with XERJ for declarations and behavior, use grep for exact and exhaustive matches, and read more when evidence is incomplete. |

Citrus Code runs with model `deepseek/deepseek-flash`, XERJ `v1.0.0-rc.105` and ripgrep `15.2.0`. The XERJ node uses lexical indexing, semantic and graph indexing are disabled, and the node query cache is disabled. The wrapper uses the CLI's default hit count and retains its 10,000-character response guard. The benchmark has no cumulative token threshold or model-turn limit. It retains Citrus Code's normal per-response model configuration.

The grader compares answers with definitions and behavior verified in the pinned source and literal match lists produced by ripgrep. It accepts a standalone JSON object or a sole fenced JSON block for the format score. Correct JSON accompanied by prose earns content credit. Five runs had provider retries and are excluded from complete-token comparisons. All 162 runs reached normal completion.

## Accuracy and usage

| Configuration | Content correct | Correct with requested JSON format | Complete token usage | Median total tokens | Tokens per correct answer | Total model turns | Median wall time |
|---|---:|---:|---:|---:|---:|---:|---:|
| ripgrep baseline | 54/54 | 52/54 | 53/54 | 10,334 | 18,703 | 186 | 7.41 s |
| Free tool choice | 54/54 | 51/54 | 51/54 | 9,642 | 15,212 | 184 | 8.69 s |
| Guided tool choice | 53/54 | 53/54 | 53/54 | 6,398 | 13,800 | 158 | 7.04 s |

Total tokens add provider-reported input and output, including reported cache-read input. Tokens per correct answer include all runs with complete token usage in the numerator and their correct answers in the denominator. Wall time includes provider and worker contention. The guided group's one wrong answer added a function declaration to a literal match list even though that line contained a generic type parameter before the opening parenthesis.

## Token comparisons on matched correct answers

A pair contains the same question and repetition in both configurations. Both answers must be correct and both usage records complete. The change compares the sum of tokens over those pairs. The interval resamples whole questions, keeping repeats within each question, and describes variation within this set.

| Comparison | Pairs | Reference tokens | Treatment tokens | Total change | Median pair change | Lower / higher token count | 95% question-bootstrap interval |
|---|---:|---:|---:|---:|---:|---:|---|
| Free choice vs baseline | 50 | 915,680 | 769,818 | −15.9% | +8.6% | 22 / 28 | −37.3% to +25.0% |
| Guided choice vs baseline | 51 | 974,962 | 703,707 | −27.8% | −16.3% | 30 / 21 | −45.1% to −1.5% |
| Guided vs free choice | 50 | 766,892 | 671,988 | −12.4% | −3.2% | 26 / 24 | −33.8% to +4.2% |

In the 49 repetitions that qualified under all three configurations, the baseline used 909,987 tokens, free choice used 760,899, and guided choice used 668,173.

| Scope | Free choice total change | Guided choice total change |
|---|---:|---:|
| Lucene | +25.2% | +2.9% |
| Ruff | −36.6% | −45.9% |
| Definitions | −27.9% | −37.2% |
| Behavior | −28.5% | −23.2% |
| Exact literals | +53.6% | −40.7% |

The free-choice aggregate is sensitive to two long Ruff behavior baselines. Excluding the two pairs with the largest absolute token savings after the run changes the free-choice result to **+4.5%**. The guided result on the same exclusion is **−12.2%**. On definitions, both configurations saved tokens in aggregate while a majority of matched repetitions in each configuration used more. The per-pair distributions and repository results matter when interpreting the totals.

## Tool selection

| Question type | Free choice | Guided choice |
|---|---|---|
| Definition | `xerj_def` first in 15/18 | `xerj_def` first in 17/18 |
| Behavior | `xerj_search` first in 5/18 | `xerj_search` first in 9/18 |
| Exact literals | `grep` first in 18/18 | `grep` first in 18/18 |

A listing or read can precede the first search. Neither configuration called XERJ at any point on the exact-literal tasks. The literal results therefore reflect the agent's search and read sequences under the two configurations, with no XERJ retrieval involved. The comparison measures the full agent configuration.

## Reproduction and evidence

- Apache Lucene commit `894d24e9b0c7340e66a47812985a4b16a3d5d672`. [Source snapshot](https://github.com/apache/lucene/commit/894d24e9b0c7340e66a47812985a4b16a3d5d672)
- Astral Ruff commit `a1df255cc1d7fbc1233cf4b2c5d5184fb61d7583`. [Source snapshot](https://github.com/astral-sh/ruff/commit/a1df255cc1d7fbc1233cf4b2c5d5184fb61d7583)
- Citrus Code base commit `e7e6fc95517225229416a01d81cfb8cdde0fe642`. The evaluation used a modified working tree with Python source SHA-256 `e5aa97740c4b2cdf22313791bb6905220e91c13e99d7c2fd715daa537251fa9a`. Reproducing that runtime exactly requires the matching working tree. [Citrus Code base](https://github.com/big-orange947/citrus-code/commit/e7e6fc95517225229416a01d81cfb8cdde0fe642)

Run the offline verification with Python 3.9 or later. It rechecks the answer grades and the model usage records, then recomputes the group and paired comparisons.

~~~sh
python benchmarks/agentquery-bench/citrus-code-2026-11-24/verify.py
~~~

The manifest contains the frozen questions, expected answers, model options, ordering and source hashes. The JSONL contains one row per run with the answer, tool choices, per-call usage, outcome and timing. A new live measurement requires the corresponding Citrus Code runtime, the pinned source snapshots and a configured model provider. [Manifest](manifest.json) · [Per-run results](runs.jsonl) · [Offline verification](verify.py)

The experiment covers read-only questions. Its outcomes are specific to these repositories, this set of questions and the tested model. Related upstream work is tracked in [xerj-org/xerj #1787](https://github.com/xerj-org/xerj/issues/1787).
