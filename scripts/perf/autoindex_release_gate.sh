#!/usr/bin/env bash
# Autoindex/search release performance gate. See docs/perf-release-gate.md.
#
#   scripts/perf/autoindex_release_gate.sh --bin target/release/xerj [--tier 1gb]
#       [--size-mb N] [--seed 2421] [--corpus-dir DIR] [--work DIR] [--out DIR]
#       [--baseline PREV_XERJ_BINARY] [--engine-baseline es-baseline.json]
#       [--port 9400] [--autoindex-args "..."] [--keep]
#
# Generates (or reuses) the deterministic synthetic corpus, runs the candidate
# binary (and optionally the previous release binary) through gate.py on a
# throwaway node with a fixed memory cap, evaluates scripts/perf/budgets.toml
# and writes OUT/results.json + OUT/summary.md. Exit 0 = every budget met,
# 1 = at least one budget missed, 2 = usage / environment error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="" TIER="1gb" SIZE_MB="" SEED=2421 CORPUS="" WORK="" OUT="" BASE="" ENG="" PORT=9400 KEEP=0 AI_ARGS=""
BUDGETS="$HERE/budgets.toml"
MIN_FREE_GB="${MIN_FREE_GB:-8}"

die() { echo "perf-gate: $*" >&2; exit 2; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) BIN="$2"; shift 2;;
    --tier) TIER="$2"; shift 2;;
    --size-mb) SIZE_MB="$2"; shift 2;;
    --seed) SEED="$2"; shift 2;;
    --corpus-dir) CORPUS="$2"; shift 2;;
    --work) WORK="$2"; shift 2;;
    --out) OUT="$2"; shift 2;;
    --baseline) BASE="$2"; shift 2;;
    --engine-baseline) ENG="$2"; shift 2;;
    --budgets) BUDGETS="$2"; shift 2;;
    --port) PORT="$2"; shift 2;;
    --autoindex-args) AI_ARGS="$2"; shift 2;;
    --keep) KEEP=1; shift;;
    -h|--help) sed -n '2,16p' "$0"; exit 0;;
    *) die "unknown argument: $1";;
  esac
done
[[ -n "$BIN" && -x "$BIN" ]] || die "--bin must point at an executable xerj binary"
[[ -z "$BASE" || -x "$BASE" ]] || die "--baseline must point at an executable xerj binary"
command -v python3 >/dev/null || die "python3 (3.11+) required"
python3 -c 'import tomllib' 2>/dev/null || die "python3 >= 3.11 required (tomllib)"

tier_val() { python3 -c 'import sys,tomllib;print(tomllib.load(open(sys.argv[1],"rb"))["tier"][sys.argv[2]][sys.argv[3]])' "$BUDGETS" "$TIER" "$1"; }
tier_val corpus_mb >/dev/null 2>&1 || die "tier '$TIER' not in $BUDGETS"
SIZE_MB="${SIZE_MB:-$(tier_val corpus_mb)}"
CAP_MB="$(tier_val node_memory_cap_mb)"
TIMEOUT_MIN="$(tier_val autoindex_timeout_min)"

WORK="${WORK:-${RUNNER_TEMP:-/tmp}/xerj-perf-gate}"
OUT="${OUT:-$WORK/out}"
CORPUS="${CORPUS:-$WORK/corpus-${SIZE_MB}mb-s${SEED}}"
mkdir -p "$WORK" "$OUT"
WORK="$(cd "$WORK" && pwd)"; OUT="$(cd "$OUT" && pwd)"

# Disk guard: corpus + per-run data/state can reach ~4x the corpus on a bad
# build (rc.99 wrote 1.6x state + 2x index). Refuse rather than fill the disk.
free_gb() { df -Pk "$1" | awk 'NR==2{printf "%d", $4/1048576}'; }
need_gb=$(( (SIZE_MB * 4 + 1023) / 1024 ))
[[ -d "$CORPUS" ]] && need_gb=$(( (SIZE_MB * 3 + 1023) / 1024 ))
have=$(free_gb "$WORK")
if (( have - need_gb < MIN_FREE_GB )); then
  die "need ~${need_gb} GB scratch + ${MIN_FREE_GB} GB reserve on $(df -P "$WORK" | awk 'NR==2{print $6}'), only ${have} GB free (set MIN_FREE_GB or --size-mb)"
fi

echo "perf-gate: tier=$TIER corpus=${SIZE_MB}MB seed=$SEED cap=${CAP_MB}MB timeout=${TIMEOUT_MIN}min bin=$BIN"
if [[ -f "$CORPUS.manifest.json" && -f "$CORPUS.queries.json" ]] && \
   python3 -c 'import json,sys;m=json.load(open(sys.argv[1]));sys.exit(0 if float(m.get("size_mb_requested",-1))==float(sys.argv[2]) and m.get("seed")==int(sys.argv[3]) else 1)' "$CORPUS.manifest.json" "$SIZE_MB" "$SEED"; then
  echo "perf-gate: reusing corpus $CORPUS"
else
  if [[ -e "$CORPUS" ]]; then die "$CORPUS exists but is not a seed=$SEED size=${SIZE_MB}MB corpus; remove it or pick another --corpus-dir"; fi
  t0=$(date +%s)
  python3 "$HERE/gen_corpus.py" --out "$CORPUS" --size-mb "$SIZE_MB" --seed "$SEED"
  echo "perf-gate: corpus generated in $(( $(date +%s) - t0 ))s"
fi

export XERJ_DISABLE_FEEDBACK=true
run_one() {  # label bin port
  local label="$1" bin="$2" port="$3"
  rm -rf "$WORK/run-$label"
  python3 "$HERE/gate.py" run --bin "$bin" --label "$label" --corpus "$CORPUS" --queries "$CORPUS.queries.json" \
    --work "$WORK/run-$label" --out "$OUT/$label.json" --port "$port" --cap-mb "$CAP_MB" \
    --timeout-min "$TIMEOUT_MIN" --autoindex-args "$AI_ARGS"
  cp "$WORK/run-$label/autoindex.stderr" "$OUT/$label.autoindex.stderr" 2>/dev/null || true
  for f in node-ingest.log node-search.log; do
    [[ -f "$WORK/run-$label/$f" ]] && tail -c 2000000 "$WORK/run-$label/$f" > "$OUT/$label.$f" || true
  done
  (( KEEP )) || rm -rf "$WORK/run-$label"
}

if [[ -n "$BASE" ]]; then run_one baseline "$BASE" $((PORT + 10)); fi
run_one candidate "$BIN" "$PORT"

eval_args=(--budgets "$BUDGETS" --tier "$TIER" --candidate "$OUT/candidate.json" --md "$OUT/summary.md" --json "$OUT/results.json")
[[ -n "$BASE" ]] && eval_args+=(--baseline "$OUT/baseline.json")
[[ -n "$ENG" ]] && eval_args+=(--engine-baseline "$ENG")
set +e
python3 "$HERE/gate.py" eval "${eval_args[@]}"
rc=$?
set -e
[[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && cat "$OUT/summary.md" >> "$GITHUB_STEP_SUMMARY"
echo "perf-gate: results in $OUT (results.json, summary.md); exit $rc"
exit $rc
