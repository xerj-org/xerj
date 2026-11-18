# SWE-bench Verified × XERJ reference coding — turnkey harness

Measures whether XERJ reference-coding improves an agent on **SWE-bench Verified**,
the benchmark the AI labs score coding agents on. It is split so the **verdict is the
official one**: this repo only *generates* patches; scoring is the standard
`swebench.harness.run_evaluation` in Docker. The number you get is the real leaderboard
number, for the subset you run.

> **Why this isn't run in the XERJ sandbox.** SWE-bench's evaluation runs each repo's
> tests in per-repo **Docker** images, and its instances pin **2013–2020-era Python**
> commits. A sandbox without a Docker daemon, or with only a modern Python (3.14), can
> neither evaluate nor even set up baselines (verified: `flask-5014`, `requests-1142`
> both fail environment setup on 3.14). Run this on a **Docker-capable Linux box**.

## Arms

Identical agent, one difference — the clean test of reference-coding:

- **`native`** — the repo is checked out; the agent must **grep** to find the code to change.
- **`xerj`** — same, but XERJ has indexed the repo and **pre-retrieved** the relevant
  passages into the prompt.

(No `bare` arm: patching a real repo from pure memory is not a meaningful task. The
`bare` floor lives in the contamination-free study, `measure/CASE_STUDY.md`.)

## Prerequisites

```sh
pip install swebench datasets            # official harness + dataset loader
docker info                              # must succeed (daemon running)
claude --version                         # authenticated Claude Code CLI
xerj --version                           # XERJ release binary  (set XERJ_BIN to its path)
```

Start a XERJ node the `xerj` arm will index into (a private port, throwaway data dir):

```sh
xerj --insecure --port 9200 --data-dir /tmp/swe-xerj --embed-mode lexical &
export XERJ_URL=http://localhost:9200 XERJ_BIN=$(command -v xerj)
```

## 1. Generate predictions (both arms, per model)

Start small — one lightweight repo — to confirm the loop end-to-end, then scale.
A task proceeds only after both cloning and checkout of its pinned base commit
succeed; a failed checkout cannot silently run the agent on the default branch.

```sh
REPOS="psf/requests pytest-dev/pytest"     # or drop --repos for the full 500

for arm in native xerj; do
  ANTHROPIC_MODEL=claude-opus-4-8 \
  python3 gen_predictions.py --arm $arm --repos $REPOS --limit 20 \
    --out preds-opus48-$arm.jsonl --tokens-out tokens-opus48-$arm.jsonl
done
```

Repeat with `ANTHROPIC_MODEL=claude-opus-5`, `claude-opus-4-6`, … The model that
actually ran is recorded from claude's JSON `modelUsage` in the `--tokens-out` log, so
the selection is verifiable, not assumed.

## 2. Score with the OFFICIAL harness (Docker)

```sh
for arm in native xerj; do
  python3 -m swebench.harness.run_evaluation \
    --dataset_name princeton-nlp/SWE-bench_Verified \
    --predictions_path preds-opus48-$arm.jsonl \
    --run_id opus48-$arm --max_workers 4
done
```

Each run prints/writes a report with **resolved / total** — that is the SWE-bench
resolve-rate for that arm+model, computed by the repos' own `FAIL_TO_PASS` /
`PASS_TO_PASS` tests. The XERJ claim is the **native → xerj delta** in resolve-rate,
alongside the **token/cost delta** from the `--tokens-out` logs.

## 3. Read it honestly

- Report **resolve-rate per arm** and the **native→xerj delta**, plus output-tokens and
  \$/instance from the token logs. State **n** (instances actually evaluated) and which
  repos — do not extrapolate a subset to the full 500.
- On SWE-bench repos the model has largely trained on, the *resolve-rate* lift from
  retrieval may be small (memorized code — see the boundary in
  `../../measure/CASE_STUDY.md` and `demo/playbooks/REFCODING_BENCHMARK_3MODEL_2026-08-18.md`);
  the defensible signal there is usually **efficiency** (fewer grep turns / output
  tokens for the same fix). Report whatever the data shows, wins and losses.
- Keep the agent scaffold identical between arms (same `--max-turns`, same prompt
  scaffolding) so the only variable is XERJ pre-retrieval.

## Files

- `gen_predictions.py` — generates the `native`/`xerj` prediction JSONLs (+ token logs).
  Patches compare with the original task base and include staged, committed and
  new non-ignored files, including binary changes; ignored artifacts stay out.
- scoring — the official `swebench.harness.run_evaluation` (not vendored here on purpose).
