#!/usr/bin/env python3
"""Split semantic requests into phases from a server log written with
XERJ_TRACE_SEMANTIC_PHASES=1.

    python3 phases.py server.log "semantic|2026-09-18T08:06:47|2026-09-18T08:07:14" ...

Each extra argument is "label|start|end" in the log's own (UTC) clock; use the
ARM_START / ARM_END stamps latency.py prints. Requests answered by the HNSW
graph have no `scored` phase and are reported with embed + total only.
"""
import datetime
import re
import statistics
import sys

log, windows = sys.argv[1], [w.split("|") for w in sys.argv[2:]]


def ts(s):
    return datetime.datetime.strptime(s[:26], "%Y-%m-%dT%H:%M:%S.%f")


ANSI = re.compile(r"\x1b\[[0-9;]*m")
reqs, cur = [], None
for line in open(log, errors="replace"):
    line = ANSI.sub("", line)  # a TTY-less server still colours its log unless NO_COLOR is set
    if "semantic_phase=" not in line:
        continue
    phase = re.search(r"semantic_phase=(\w+)", line).group(1)
    kv = dict(re.findall(r"(\w+)=(\d+)\b", line))
    if phase == "embed_start":
        cur = {"t": ts(line.split()[0].rstrip("Z"))}
    if cur is None:
        continue
    if phase == "embed_complete":
        cur["embed"] = int(kv["elapsed_ms"])
    if phase == "scored":
        cur["collect"], cur["scored_at"], cur["n"] = int(kv["collect_ms"]), int(kv["elapsed_ms"]), int(kv["scored"])
    if phase == "complete":
        cur["brute_total"] = int(kv["elapsed_ms"])
    if phase == "request_complete":
        cur["total"] = int(kv["elapsed_ms"])
        reqs.append(cur)
        cur = None


def med(v):
    return statistics.median(v) if v else float("nan")


for name, a, b in windows:
    lo = datetime.datetime.strptime(a, "%Y-%m-%dT%H:%M:%S")
    hi = datetime.datetime.strptime(b, "%Y-%m-%dT%H:%M:%S")
    rs = [r for r in reqs if lo <= r["t"] <= hi]
    rs = rs[3:] if len(rs) > 10 else rs  # the first requests pay the cold stored-value cache
    brute = [r for r in rs if "collect" in r]
    print(f"{name:32s} n={len(rs):3d} exact-scan={len(brute):3d}  embed p50={med([r['embed'] for r in rs]):6.1f}"
          f"  collect p50={med([r['collect'] for r in brute]):6.1f}"
          f"  score p50={med([r['scored_at'] - r['collect'] for r in brute]):6.1f}"
          f"  topk+hits p50={med([r['brute_total'] - r['scored_at'] for r in brute]):6.1f}"
          f"  total p50={med([r['total'] for r in rs]):6.1f}"
          f"  scored_docs p50={med([r['n'] for r in brute]):.0f}")
