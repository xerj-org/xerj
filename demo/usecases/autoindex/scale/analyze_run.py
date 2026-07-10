#!/usr/bin/env python3
"""Summarize a scale run: wall time, MB/s, rec/s, client+server RSS stats."""
import csv, json, sys

tag = sys.argv[1] if len(sys.argv) > 1 else "runA"
base = "/tmp/xerj-autoindex"

t0 = int(open(f"{base}/{tag}-start.txt").read().strip())
t1 = int(open(f"{base}/{tag}-end.txt").read().strip())
wall = t1 - t0
m = json.load(open("/home/claude/xerj-autoindex-scale/manifest.json"))
gb = m["total_bytes"] / 2**30
recs = m["total_records"]

def rss_stats(path, label):
    rows = []
    for r in csv.reader(open(path)):
        try:
            rows.append((float(r[0]), int(r[1])))
        except (ValueError, IndexError):
            pass
    if not rows:
        print(f"{label}: no samples"); return
    peak = max(v for _, v in rows) / 1024
    t_first = rows[0][0]
    n = len(rows)
    print(f"{label}: {n} samples, peak {peak:,.0f} MB")
    for i in range(0, n, max(1, n // 10)):
        t, v = rows[i]
        print(f"  t+{t-t_first:6.0f}s {v/1024:9.1f} MB")
    print(f"  t+{rows[-1][0]-t_first:6.0f}s {rows[-1][1]/1024:9.1f} MB (last)")

print(f"corpus: {gb:.2f} GiB, {recs:,} records")
print(f"wall: {wall}s  ->  {m['total_bytes']/2**20/wall:,.1f} MB/s, {recs/wall:,.0f} records/s")
rss_stats(f"{base}/ax-rss-{tag}.csv", "client(autoindex) RSS")
rss_stats(f"{base}/server-rss-{tag}.csv", "server RSS")
