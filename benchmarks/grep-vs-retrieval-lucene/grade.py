#!/usr/bin/env python3
"""Deterministic oracle. No LLM judge: substring/regex only, case-insensitive."""
import json, sys, os, re
B = os.path.dirname(os.path.abspath(__file__))
QS = {q["id"]: q for q in json.load(open(f"{B}/questions.json"))["questions"]}

def grade(qid, answer):
    q = QS[qid]; a = answer.lower()
    missing = [e for e in q["expect"] if e.lower() not in a]
    tripped = [f for f in q.get("forbid", []) if f.lower() in a]
    return {"qid": qid, "correct": not missing and not tripped,
            "missing_expected": missing, "tripped_forbidden": tripped}

if __name__ == "__main__":
    out = []
    for p in sys.argv[1:]:
        qid = os.path.basename(p).split("_A_")[0].split("_B_")[0].split("_C_")[0]
        d = json.load(open(p))
        out.append({**grade(qid, d.get("result", "")), "file": os.path.basename(p)})
    print(json.dumps(out, indent=2))
