"""Split a pair file into N shards by query group, skipping groups already scored.

usage: shard.py <pairs.jsonl> <done_logits.jsonl> <n> <out_prefix>
Writes <out_prefix>.s<i>of<n>.jsonl. A group is never split across shards, so each
shard's calls are still whole rerank windows. Groups whose every pair is already in
<done_logits.jsonl> are left out (resume). Throughput only: latency is measured by
latency.sh with one process, never from a sharded run."""
import json, sys

src, done_path, n, prefix = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
done = set()
try:
    for line in open(done_path):
        r = json.loads(line)
        done.add((r["g"], r["id"]))
except FileNotFoundError:
    pass
groups, order = {}, []
for line in open(src):
    if not line.strip():
        continue
    r = json.loads(line)
    if r["g"] not in groups:
        groups[r["g"]] = []
        order.append(r["g"])
    groups[r["g"]].append(line)
todo = [g for g in order if not all((g, json.loads(l)["id"]) in done for l in groups[g])]
outs = [open(f"{prefix}.s{i}of{n}.jsonl", "w") for i in range(n)]
for k, g in enumerate(todo):
    outs[k % n].writelines(groups[g])
print(f"{src}: {len(order)} groups, {len(order) - len(todo)} already scored, {len(todo)} into {n} shards")
