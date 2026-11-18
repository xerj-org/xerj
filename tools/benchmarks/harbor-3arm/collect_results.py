#!/usr/bin/env python3
"""Collect the XERJ 3-arm campaign into one aligned report.

Walks harbor-campaign/jobs/<bench>.<model>.<arm>/ job dirs, reads each trial's
result (reward + agent metrics), and prints:
  * per-cell resolve rate with Wilson 95% CI
  * COMPLETE-MATRIX cross sections (only tasks finished in EVERY arm of a
    benchmark+model count toward arm-vs-arm claims — same rule that caught the
    unaligned 2026-08-18 run)
  * cost, tokens, wall-clock per cell and per solve

Trial layout (harbor): <job>/<task>__<trial-id>/result.json  (fields observed
from harbor 0.21; unknown fields are tolerated, missing trials reported).
"""
import json
import math
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path

JOBS = Path(__file__).parent / "jobs"


def wilson(k, n, z=1.96):
    if n == 0:
        return (0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    m = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return (max(0.0, (c - m) / d), min(1.0, (c + m) / d))


def first_present(*values):
    return next((value for value in values if value is not None), None)


def agent_metric(trial, *names):
    # Harbor v0.21 stores multi-step usage on each step instead of agent_result.
    context = trial.get("agent_result")
    contexts = [context] if context is not None else [
        step["agent_result"] for step in trial.get("step_results") or []
        if step.get("agent_result") is not None
    ]
    values = [first_present(*(context.get(name) for name in names)) for context in contexts]
    known = [value for value in values if value is not None]
    return sum(known) if known else None


def elapsed_seconds(trial):
    explicit = first_present(trial.get("elapsed_seconds"),
                             (trial.get("agent_result") or {}).get("elapsed_seconds"))
    if explicit is not None:
        return explicit
    try:
        start = datetime.fromisoformat(trial["started_at"].replace("Z", "+00:00"))
        end = datetime.fromisoformat(trial["finished_at"].replace("Z", "+00:00"))
        seconds = (end - start).total_seconds()
        return seconds if seconds >= 0 else None
    except (KeyError, AttributeError, TypeError, ValueError):
        return None


def load_trials(job_dir: Path):
    """Yield (task, dict) per finished trial; tolerant of layout drift."""
    for rj in sorted(job_dir.glob("**/result.json")):
        try:
            r = json.loads(rj.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        task = (r.get("task_name") or r.get("task", {}).get("name")
                or rj.parent.name.split("__")[0])
        reward = None
        for path in (("verifier_result", "rewards"), ("verifier_result", "reward"),
                     ("reward",), ("result", "reward")):
            v = r
            for k in path:
                v = v.get(k) if isinstance(v, dict) else None
            if v is not None:
                reward = v
                break
        if isinstance(reward, dict):        # named rewards -> main one
            reward = reward.get("reward", reward.get("resolved"))
        yield task, {
            "reward": reward,
            "resolved": (reward or 0) >= 1 if reward is not None else None,
            "cost": first_present(agent_metric(r, "cost_usd", "total_cost_usd"), r.get("total_cost_usd")),
            "in_tok": agent_metric(r, "n_input_tokens", "input_tokens"),
            "out_tok": agent_metric(r, "n_output_tokens", "output_tokens"),
            "sec": elapsed_seconds(r),
            "exception": r.get("exception_info") or r.get("error"),
        }


def main():
    cells = defaultdict(dict)               # (bench, model, arm) -> {task: trial}
    cell_trials = defaultdict(list)         # all attempts, including failed retries
    for job_dir in sorted(JOBS.iterdir()) if JOBS.exists() else []:
        if not job_dir.is_dir():
            continue
        parts = job_dir.name.split(".")
        if len(parts) < 3:
            continue
        bench, model, arm = parts[0], ".".join(parts[1:-1]), parts[-1]
        for task, t in load_trials(job_dir):
            cells[(bench, model, arm)][task] = t
            cell_trials[(bench, model, arm)].append(t)

    if not cells:
        sys.exit(f"no trials under {JOBS} — run run_matrix.sh first")

    print("=" * 90)
    print("XERJ 3-ARM CAMPAIGN — per-cell status")
    print("=" * 90)
    fmt = "{:<20} {:<16} {:<11} {:>5} {:>7} {:>16} {:>9} {:>9}"
    print(fmt.format("benchmark", "model", "arm", "n", "solved", "95% CI",
                     "$ total", "hrs"))
    for (bench, model, arm), tasks in sorted(cells.items()):
        done = {t: v for t, v in tasks.items() if v["reward"] is not None}
        k = sum(1 for v in done.values() if v["resolved"])
        lo, hi = wilson(k, len(done))
        attempts = cell_trials[(bench, model, arm)]
        cost = sum(v["cost"] or 0 for v in attempts)
        hrs = sum(v["sec"] or 0 for v in attempts) / 3600
        errs = sum(1 for v in attempts if v["exception"])
        note = f"  !! {errs} errored" if errs else ""
        missing_cost = sum(v["cost"] is None for v in attempts)
        missing_time = sum(v["sec"] is None for v in attempts)
        if missing_cost or missing_time:
            note += f"  metrics missing: cost {missing_cost}/{len(attempts)}, time {missing_time}/{len(attempts)} trials"
        print(fmt.format(bench, model, arm, len(done), k,
                         f"[{lo:.0%},{hi:.0%}]", f"{cost:.2f}", f"{hrs:.1f}")
              + note)

    print()
    print("=" * 90)
    print("COMPLETE MATRIX — arm comparison on identical tasks (per benchmark+model)")
    print("=" * 90)
    by_bm = defaultdict(dict)
    for (bench, model, arm), tasks in cells.items():
        by_bm[(bench, model)][arm] = {
            t for t, v in tasks.items() if v["reward"] is not None}
    canonical_arms = {"native", "xerj", "xerj-tuned"}
    for (bench, model), arms in sorted(by_bm.items()):
        if len(arms) < 2:
            continue
        present = set(arms)
        common = set.intersection(*arms.values())
        # A comparison that isn't the full 3-arm set must say so LOUDLY — an
        # absent arm otherwise reads as a footnote and a reader can mistake, e.g.,
        # native vs xerj-tuned for the headline result (#516).
        coverage = "" if present == canonical_arms else (
            f"   ⚠ PARTIAL ARM COVERAGE — missing "
            f"{sorted(canonical_arms - present) or 'nothing'}, "
            f"unexpected {sorted(present - canonical_arms) or 'nothing'}; "
            f"this is NOT the canonical 3-arm comparison")
        print(f"\n{bench} / {model}: {len(common)} tasks finished in all "
              f"{len(arms)} arms ({', '.join(sorted(present))}){coverage}")
        for arm in sorted(arms):
            tv = cells[(bench, model, arm)]
            k = sum(1 for t in common if tv[t]["resolved"])
            rew = [tv[t]["reward"] for t in common
                   if isinstance(tv[t]["reward"], (int, float))]
            mean_r = sum(rew) / len(rew) if rew else float("nan")
            cost = sum(tv[t]["cost"] or 0 for t in common)
            sec = sum(tv[t]["sec"] or 0 for t in common)
            print(f"   {arm:<11} resolved {k}/{len(common)}   "
                  f"mean-reward {mean_r:.3f}   ${cost:.2f}   {sec/3600:.1f}h")


if __name__ == "__main__":
    main()
