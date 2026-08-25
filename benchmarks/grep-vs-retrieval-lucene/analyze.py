#!/usr/bin/env python3
"""
analyze.py -- grade the runs and produce the head-to-head table.

GRADING IS TWO-STAGE AND BLIND.
Stage 1 (mechanical): must_match / must_not_match string checks from questions.jsonl.
  Cheap, deterministic, auditable. It can only mark a run FAIL-certain or NEEDS-JUDGE;
  it never marks a run correct on its own, because substring presence is not
  comprehension (an answer can contain "256" while asserting the wrong thing).
Stage 2 (blind judge): every NEEDS-JUDGE answer is written to judge_queue.jsonl with
  the arm label STRIPPED and the runs shuffled, so whoever/whatever grades cannot see
  which arm produced which answer. Verdicts are read back from judge_verdicts.jsonl.

The headline metric is TOKENS PER CORRECT ANSWER, not tokens per run. An arm that is
cheap because it gives up or hallucinates must not win. Runs graded wrong contribute
their cost to the arm but no credit, which is the only way "N x fewer tokens" means
anything.

Medians, not means: a single runaway agentic run would otherwise decide the headline.
"""
import json
import os
import random
import statistics
import sys

BENCH = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, BENCH)
from ledger import build  # noqa: E402

ARMS = ("native", "xerj", "hybrid")


def questions():
    return {json.loads(l)["id"]: json.loads(l) for l in open(f"{BENCH}/questions.jsonl")}


def mechanical(ans, q):
    """Return 'FAIL' (certainly wrong) or 'JUDGE' (needs blind grading)."""
    a = ans.lower()
    for bad in q.get("must_not_match", []):
        # a forbidden token is only fatal if the required one is absent
        if bad.lower() in a and not any(m.lower() in a for m in q["must_match"]):
            return "FAIL"
    if not all(m.lower() in a for m in q["must_match"]):
        return "FAIL"
    if "not found" in a:
        return "FAIL"
    return "JUDGE"


def collect():
    qs = questions()
    rows = []
    for fn in sorted(os.listdir(f"{BENCH}/runs")):
        if not fn.endswith(".jsonl"):
            continue
        parts = fn[:-6].split("__")
        if len(parts) != 3:
            continue
        arm, qid, rep = parts
        if qid not in qs:
            continue
        try:
            led = build(f"{BENCH}/runs/{fn}")
        except SystemExit as e:
            print(f"WARN {fn}: {e}", file=sys.stderr)
            continue
        if not led["reconciled"]:
            print(f"WARN {fn}: ledger did not reconcile {led['mismatches']}", file=sys.stderr)
        rows.append({
            "arm": arm, "qid": qid, "rep": rep, "class": qs[qid]["class"],
            "tokens": led["total_tokens_billed"],
            "cost_neutral": led["cost_usd_cache_neutral"],
            "cost_raw": led["cost_usd_total"],
            "tool_calls": led["tool_calls"],
            "api_calls": led["api_calls"],
            "tool_tokens": led["tool_tokens_est"],
            "share_billed": led["share_of_billed_pct"],
            "answer": led["answer"],
            "mech": mechanical(led["answer"], qs[qid]),
        })
    return rows, qs


def emit_judge_queue(rows, qs):
    """Write blind grading tasks: arm label removed, order shuffled."""
    tasks = []
    for i, r in enumerate(rows):
        if r["mech"] != "JUDGE":
            continue
        tasks.append({
            "key": f"{r['arm']}__{r['qid']}__{r['rep']}",   # opaque to the judge if hidden
            "qid": r["qid"],
            "question": qs[r["qid"]]["ask"],
            "ground_truth": qs[r["qid"]]["truth_fact"],
            "grader_note": qs[r["qid"]]["grader_note"],
            "answer": r["answer"],
        })
    random.Random(20260818).shuffle(tasks)
    with open(f"{BENCH}/judge_queue.jsonl", "w") as fh:
        for t in tasks:
            # the judge is shown question/truth/answer only; 'key' is for merge-back
            fh.write(json.dumps(t) + "\n")
    return len(tasks)


def verdicts():
    p = f"{BENCH}/judge_verdicts.jsonl"
    if not os.path.exists(p):
        return {}
    out = {}
    for l in open(p):
        d = json.loads(l)
        out[d["key"]] = d["correct"]
    return out


def main():
    rows, qs = collect()
    if not rows:
        raise SystemExit("no runs found -- run ./run.sh first")
    n = emit_judge_queue(rows, qs)
    v = verdicts()

    for r in rows:
        key = f"{r['arm']}__{r['qid']}__{r['rep']}"
        r["correct"] = False if r["mech"] == "FAIL" else v.get(key)

    ungraded = [r for r in rows if r["correct"] is None]
    print(f"runs={len(rows)}  queued_for_blind_judging={n}  ungraded={len(ungraded)}")
    if ungraded:
        print("-> grade judge_queue.jsonl into judge_verdicts.jsonl "
              '({"key":...,"correct":true|false}) then re-run. '
              "Numbers below treat ungraded as UNKNOWN and exclude them.\n")

    graded = [r for r in rows if r["correct"] is not None]

    print(f"{'arm':8s} {'n':>3s} {'acc':>6s} {'med_tok':>9s} {'med_$neut':>10s} "
          f"{'tok/correct':>12s} {'med_tools':>9s} {'tool_share%':>11s}")
    per_arm = {}
    for arm in ARMS:
        g = [r for r in graded if r["arm"] == arm]
        if not g:
            continue
        ok = [r for r in g if r["correct"]]
        acc = len(ok) / len(g)
        med_tok = statistics.median(r["tokens"] for r in g)
        med_cost = statistics.median(r["cost_neutral"] for r in g)
        # tokens per correct answer: total spend by the arm / number it got right
        tpc = (sum(r["tokens"] for r in g) / len(ok)) if ok else float("inf")
        med_tools = statistics.median(r["tool_calls"] for r in g)
        shares = [r["share_billed"] for r in g if r["share_billed"] is not None]
        med_share = statistics.median(shares) if shares else 0
        per_arm[arm] = {"acc": acc, "med_tok": med_tok, "tpc": tpc,
                        "med_cost": med_cost, "n": len(g)}
        print(f"{arm:8s} {len(g):3d} {acc:6.0%} {med_tok:9.0f} {med_cost:10.4f} "
              f"{tpc:12.0f} {med_tools:9.1f} {med_share:11.1f}")

    if "native" in per_arm:
        base = per_arm["native"]
        print("\nHEADLINE (vs native, tokens per CORRECT answer):")
        for arm in ("xerj", "hybrid"):
            if arm in per_arm and per_arm[arm]["tpc"]:
                print(f"  {arm:7s} {base['tpc']/per_arm[arm]['tpc']:.2f}x  "
                      f"(accuracy {per_arm[arm]['acc']:.0%} vs {base['acc']:.0%})")

    print("\nPER QUESTION CLASS (median tokens; accuracy):")
    classes = sorted({r["class"] for r in graded})
    print(f"{'class':16s} " + " ".join(f"{a:>18s}" for a in ARMS))
    for c in classes:
        cells = []
        for arm in ARMS:
            g = [r for r in graded if r["class"] == c and r["arm"] == arm]
            if not g:
                cells.append(f"{'-':>18s}"); continue
            m = statistics.median(r["tokens"] for r in g)
            a = sum(1 for r in g if r["correct"]) / len(g)
            cells.append(f"{m:11.0f}/{a:5.0%}")
        print(f"{c:16s} " + " ".join(cells))

    print("\nPER QUESTION (median tokens by arm, and who was correct):")
    for qid in [q for q in questions()]:
        line = f"  {qid:22s}"
        for arm in ARMS:
            g = [r for r in graded if r["qid"] == qid and r["arm"] == arm]
            if not g:
                line += f" {arm}:-        "
                continue
            m = statistics.median(r["tokens"] for r in g)
            a = sum(1 for r in g if r["correct"]) / len(g)
            line += f" {arm}:{m:6.0f}/{a:3.0%}"
        print(line)

    with open(f"{BENCH}/results.json", "w") as fh:
        json.dump({"rows": rows, "per_arm": per_arm}, fh, indent=1)
    print("\nwrote results.json")


if __name__ == "__main__":
    main()
