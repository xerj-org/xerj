# Agent Gate

A repeatable, deliberately hostile measurement of whether XERJ makes an AI
coding agent **measurably better off** at ordinary work — and an honest report
when it does not.

This is not a benchmark that XERJ is supposed to win. It is a gate that
refuses to *let* XERJ claim a win it did not earn.

## Why this exists

Every number in this repo's history that later turned out to be wrong was
wrong in one of five ways. The gate encodes a countermeasure for each, because
a rule that lives in a script survives; a rule that lives in a reviewer's head
does not.

| Failure mode | How it happened | Countermeasure enforced here |
|---|---|---|
| **Cheap wrong answers** | A path returns fewer tokens because it returns nothing useful. A line-level index scored 3/8 on recall while looking 4.7× cheaper than grep. | Token savings are computed **only over tasks both paths answered correctly**. A wrong answer contributes zero savings and is reported as a loss. |
| **Steered baselines** | The "baseline" greps were written by someone who already knew which file held the answer, making the baseline look better than a real cold agent. | Baseline commands may use **only vocabulary present in the question**. Every command is stored next to its task so the steering is auditable. |
| **Cache mirages** | A repeated query returned `took: 0` because the whole-result cache cloned it. | Every query is varied per run and the gate reports the **first, uncached** timing. |
| **Silent truncation** | An aggregation returned `count: 4,215,954` of 5,600,000 with `timed_out: true`, `_shards.failed: 0` and no error. | Every engine response is checked for `timed_out`, and every aggregation total is cross-checked against `_count`. A truncated answer is scored **wrong**, not fast. |
| **Grading your own homework** | An answer was scored a miss because it came from a different file than expected, though it contained the correct fact. | Ground truth is an **explicit assertion on the content**, declared before the run, not a path guess. |

## What it measures

Seven task families, chosen because they are what an agent actually does — not
what a search engine demos well:

1. `orient` — "what is in this data?" from cold
2. `lookup` — find every record for one identifier
3. `concept` — "where/why is X" answered in the asker's own words
4. `aggregate` — a statistic over the whole corpus
5. `join` — a question spanning two sources
6. `drilldown` — an incident investigation, each step depending on the last
7. `recall` — retrieve a fact stored in an earlier session

## Reading the output

The gate prints three things and refuses to collapse them into one number:

- **Correctness per path.** The headline. A path that is wrong is not cheap.
- **Token ratio over jointly-correct tasks only.** The only savings figure
  that means anything.
- **Corpus composition.** How much of the corpus is *searchable text* versus
  structured records. This single property decides the result more than any
  engine change: on 170k LOC of text, search beat grep 5.3× on tokens; on a
  corpus that was 97% log records with 11 prose files, grep beat search 2.4×.
  A report that hides corpus shape is not a report.

## Running it

```bash
# 1. build the corpus (deterministic, seeded)
python3 demo/agent-gate/make_corpus.py /tmp/gate-corpus

# 2. start XERJ and index it
xerj --insecure --data-dir /tmp/gate-data &
xerj autoindex /tmp/gate-corpus

# 3. run the gate
python3 demo/agent-gate/gate.py /tmp/gate-corpus
```

Exit status is **0 when the report is trustworthy**, not when XERJ wins.
It exits non-zero only if a path produced an answer the gate could not verify
— that is, if the measurement itself is broken.

## The kit

| File | What it is |
|---|---|
| `SCENARIOS.md` | Six real Claude Code scenarios with measured before/after — start here |
| `gate.py` | Analytics regime: record-heavy corpus (XERJ wins correctness, costs tokens) |
| `gate_retrieval.py` | Retrieval regime: prose/code corpus (XERJ wins recall; tokens scale with size) |
| `make_corpus.py` | Deterministic 36 MB heterogeneous corpus (logs/CSV/SQLite/docs/code) |
| `make_corpus_prose.py` | Deterministic prose/code corpus (the regime where retrieval saves tokens) |
| `RESULTS_analytics.txt` / `RESULTS_retrieval.txt` | Committed reference runs |

Both gates share one discipline: correctness before savings, savings only over
jointly-correct tasks, question-vocabulary baselines, both paths equally tuned,
corpus composition beside every ratio. See the table at the top of this file
for which historical mistake each rule prevents.
