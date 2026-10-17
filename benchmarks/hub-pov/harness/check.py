#!/usr/bin/env python3
"""Grade one run's ANSWER line against the task's regex set."""
import json, re, sys
task = json.load(open(sys.argv[1]))
if "tasks" in task:  # whole suite passed: argv[2] = task id, argv[3] = run
    task = next(t for t in task["tasks"] if t["id"] == sys.argv[2])
    run_json = json.load(open(sys.argv[3]))
else:
    run_json = json.load(open(sys.argv[2]))
text = (run_json.get("result") or "") + "\n" + run_json.get("_raw", "")
m = re.findall(r"ANSWER:\s*(.+)", text)
ans = m[-1] if m else text[-800:]
need = task.get("need", 1)
hits = sum(1 for pat in task["re"] if re.search(pat, ans, re.I))
print(json.dumps({"id": task["id"], "answered": bool(m), "hits": hits,
                  "need": need, "pass": hits >= need,
                  "answer": ans.strip()[:300]}))
