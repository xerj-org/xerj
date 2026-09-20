#!/bin/bash
# Throughput scoring: N pair_score processes, each with T threads, over shards of one pair
# file; then the shard outputs are appended to the file eval.py reads. candle's CPU
# kernels scale poorly with pool width on a busy machine, so several narrow processes
# finish a bulk run sooner than one wide one. Latency is NOT measured here (latency.sh).
#
# usage: RUNS=<runs_dir> PAIR_SCORE=<path> score_parallel.sh <n> <threads> <tier> <dataset.split.part...>
set -u
: "${RUNS:?}"; : "${PAIR_SCORE:?}"
N=$1; T=$2; tier=$3; shift 3
here=$(cd "$(dirname "$0")" && pwd)
for f in "$@"; do
  ds=${f%%.*}; rest=${f#*.}; split=${rest%%.*}; part=${rest#*.}
  final=$RUNS/$ds.$split.$tier.$part.logits.jsonl
  [ -f "$RUNS/$f.jsonl" ] || { echo "skip $f (no pair file)"; continue; }
  python3 "$here/shard.py" "$RUNS/$f.jsonl" "$final" "$N" "$RUNS/shard.$tier.$f"
  pids=()
  for i in $(seq 0 $((N - 1))); do
    s=$RUNS/shard.$tier.$f.s${i}of$N
    [ -s "$s.jsonl" ] || continue
    nice -n 10 "$PAIR_SCORE" --tier "$tier" --threads "$T" --in "$s.jsonl" --out "$s.logits.jsonl" \
      > /dev/null 2>> "$RUNS/score.$tier.err" &
    pids+=($!)
  done
  ok=1; for p in "${pids[@]}"; do wait "$p" || ok=0; done
  [ $ok = 1 ] || { echo "$(date +%T) $tier $f FAILED (shards kept for resume)"; exit 1; }
  for i in $(seq 0 $((N - 1))); do
    s=$RUNS/shard.$tier.$f.s${i}of$N
    [ -f "$s.logits.jsonl" ] && cat "$s.logits.jsonl" >> "$final"
    rm -f "$s.jsonl" "$s.logits.jsonl"
  done
  echo "$(date +%T) $tier $f done ($(wc -l < "$final") pairs scored)"
done
