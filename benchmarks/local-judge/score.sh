#!/bin/bash
# Score every pair file for the given tiers and datasets with `pair_score` (resumable).
#
# usage: RUNS=<runs_dir> PAIR_SCORE=<path to pair_score> score.sh <threads> <tier...> -- <dataset...>
#
# For each dataset D it scores, in this order, whichever of these exist in RUNS:
#   D.fit.w30.jsonl   held-out BM25 top-30 windows (for the calibration fit)
#   D.test.w30.jsonl  test BM25 top-30 windows
#   D.test.rest.jsonl every other test pair the 100-document and hybrid arms need
# and writes D.<split>.<tier>.<part>.logits.jsonl (+ .summary.json) beside them — the
# names eval.py reads. Build pair_score with:
#   cargo build --release -p xerj-ai --features neural --example pair_score
set -u
: "${RUNS:?set RUNS to the runs directory}"
: "${PAIR_SCORE:?set PAIR_SCORE to the pair_score binary}"
T=$1; shift
tiers=(); while [ "$1" != "--" ]; do tiers+=("$1"); shift; done; shift
for tier in "${tiers[@]}"; do
  for ds in "$@"; do
    for sp in fit.w30 test.w30 test.rest; do
      split=${sp%.*}; part=${sp#*.}; f=$ds.$split.$part
      [ -f "$RUNS/$f.jsonl" ] || { echo "skip $f (no pair file)"; continue; }
      echo "$(date +%T) $tier $f start"
      nice -n 5 "$PAIR_SCORE" --tier "$tier" --threads "$T" --in "$RUNS/$f.jsonl" \
        --out "$RUNS/$ds.$split.$tier.$part.logits.jsonl" \
        --summary "$RUNS/$ds.$split.$tier.$part.summary.json" > /dev/null 2>> "$RUNS/score.$tier.err" \
        || { echo "$(date +%T) $tier $f FAILED"; exit 1; }
      echo "$(date +%T) $tier $f done"
    done
  done
done
