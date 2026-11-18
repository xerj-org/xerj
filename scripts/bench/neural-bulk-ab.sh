#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# neural-bulk-ab.sh — A/B harness for neural-embedding `_bulk` ingest.
#
#   scripts/bench/neural-bulk-ab.sh [--dry-run] CONTROL_SHA CANDIDATE_SHA [N=5]
#
# Builds the server at each SHA in its own git worktree, then runs
# control, candidate, control, candidate, ... (N each, interleaved) against a
# cold throwaway data dir per run, driving the real client
# (`xerj autoindex`) at a frozen corpus. Writes, per run,
#   benchmarks/ingest-neural-bulk/results/<date>/run-<arm>-<i>/
#     server.log client.log summary.json
# and, per invocation, meta.json + report.md in the same <date> directory.
# Column definitions and the control-vs-control precondition are in
# benchmarks/ingest-neural-bulk/README.md.
#
# Pass the same SHA twice for the control-vs-control precondition run.
#
# Environment (all optional):
#   SCRATCH            work dir (default: mktemp under /tmp)
#   RESULTS_DATE       results subdirectory name (default: today, UTC)
#   CONTROL_BIN / CANDIDATE_BIN   prebuilt `xerj` binaries; skips that build
#   HF_HOME            model cache (default: $HOME/.cache/huggingface)
#   RUN_TIMEOUT        seconds allowed per client run (default 3600)
#   PORT_MIN           first port tried (default 9560; 9200 is never used)
#   KEEP=1             keep each run's data dir (repro.sh knob)
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/repro.sh
source "$HERE/lib/repro.sh"

DRY=0
POS=()
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    -h | --help) sed -n '2,29p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) POS+=("$a") ;;
  esac
done
if [ "${#POS[@]}" -lt 2 ] || [ "${#POS[@]}" -gt 3 ]; then
  echo "usage: $0 [--dry-run] CONTROL_SHA CANDIDATE_SHA [N=5]" >&2
  exit 2
fi
CONTROL_REF="${POS[0]}"
CANDIDATE_REF="${POS[1]}"
N="${POS[2]:-5}"
case "$N" in '' | *[!0-9]* | 0) echo "N must be a positive integer, got '$N'" >&2; exit 2 ;; esac

BENCH_DIR="$REPO_ROOT/benchmarks/ingest-neural-bulk"
CORPUS_TAR="$BENCH_DIR/corpus/okta-bulk.tar.zst"
DATE="${RESULTS_DATE:-$(date -u +%F)}"
RESULTS="$BENCH_DIR/results/$DATE"
SCRATCH="${SCRATCH:-$(mktemp -u /tmp/neural-bulk-ab.XXXXXX)}"
HF_HOME="${HF_HOME:-$HOME/.cache/huggingface}"
RUN_TIMEOUT="${RUN_TIMEOUT:-3600}"
PORT_MIN="${PORT_MIN:-9560}"
CLK_TCK="$(getconf CLK_TCK)"

# run CMD...  executes, or prints the command in dry-run mode.
run() {
  if [ "$DRY" = 1 ]; then
    printf '+ '
    printf '%q ' "$@"
    printf '\n'
  else
    "$@"
  fi
}
say() { printf '%s\n' "$*"; }

CONTROL_SHA="$(git -C "$REPO_ROOT" rev-parse --verify "$CONTROL_REF^{commit}")"
CANDIDATE_SHA="$(git -C "$REPO_ROOT" rev-parse --verify "$CANDIDATE_REF^{commit}")"

# Isolation: user scope with a CPU quota and memory cap, else taskset.
ISOLATION=""
if systemd-run --user --scope -q -p CPUQuota=400% -- true >/dev/null 2>&1; then
  ISOLATION="systemd-scope"
elif command -v taskset >/dev/null 2>&1; then
  ISOLATION="taskset"
  echo "WARNING: systemd-run --user --scope unavailable; falling back to taskset -c 0-3" \
    "(no memory cap, no CPU quota)." >&2
else
  echo "ERROR: neither systemd-run --user --scope nor taskset is available." >&2
  exit 1
fi
if [ ! -d "$HF_HOME" ]; then
  echo "WARNING: HF_HOME=$HF_HOME does not exist; run 1 would include the model download." >&2
fi

say "control   $CONTROL_SHA"
say "candidate $CANDIDATE_SHA"
say "runs      $N per arm, interleaved; isolation=$ISOLATION; results=$RESULTS"
[ "$DRY" = 1 ] && say "(dry run: commands are printed, nothing is built, started or written)"

# ── cleanup: stop the server, drop arm worktrees ────────────────────────────
cleanup() {
  set +e
  repro_node_stop
  local w
  for w in "$SCRATCH"/wt-*; do
    [ -e "$w/.git" ] && git -C "$REPO_ROOT" worktree remove --force "$w" >/dev/null 2>&1
  done
  git -C "$REPO_ROOT" worktree prune
  return 0
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ── corpus ──────────────────────────────────────────────────────────────────
CORPUS_DIR="$SCRATCH/corpus"
run mkdir -p "$SCRATCH" "$CORPUS_DIR"
if [ "$DRY" = 1 ]; then
  say "+ (cd $BENCH_DIR/corpus && sha256sum -c SHA256SUMS)"
  say "+ zstd -dc $CORPUS_TAR | tar -x -C $CORPUS_DIR"
  say "+ verify extracted files against corpus/MANIFEST.tsv (sha256 column)"
else
  (cd "$BENCH_DIR/corpus" && sha256sum -c --quiet SHA256SUMS)
  zstd -dc "$CORPUS_TAR" | tar -x -C "$CORPUS_DIR"
  (cd "$CORPUS_DIR" && tail -n +2 "$BENCH_DIR/corpus/MANIFEST.tsv" \
    | awk -F'\t' '{print $6 "  " $4}' | sha256sum -c --quiet)
fi
CORPUS="$CORPUS_DIR/okta-bulk"

# ── build each distinct SHA once ────────────────────────────────────────────
# build_arm SHA PREBUILT -> prints path of the binary to use
build_arm() {
  local sha="$1" prebuilt="$2" short wt out
  short="${sha:0:9}"
  out="$SCRATCH/bin/xerj-$short"
  if [ -n "$prebuilt" ]; then
    echo "$prebuilt"
    return
  fi
  if [ ! -x "$out" ] || [ "$DRY" = 1 ]; then
    wt="$SCRATCH/wt-$short"
    run git -C "$REPO_ROOT" worktree add --detach "$wt" "$sha" >&2
    run bash -c "cd '$wt/engine' && cargo build --release -j 2 -p xerj-server" >&2
    run mkdir -p "$SCRATCH/bin" >&2
    run cp "$wt/engine/target/release/xerj" "$out" >&2
  fi
  echo "$out"
}
# BIN_<arm> are read indirectly in run_one.
# shellcheck disable=SC2034
BIN_control="$(build_arm "$CONTROL_SHA" "${CONTROL_BIN:-}")"
if [ "$CANDIDATE_SHA" = "$CONTROL_SHA" ] && [ -z "${CANDIDATE_BIN:-}" ]; then
  # shellcheck disable=SC2034
  BIN_candidate="$BIN_control"
else
  # shellcheck disable=SC2034
  BIN_candidate="$(build_arm "$CANDIDATE_SHA" "${CANDIDATE_BIN:-}")"
fi

# ── server launcher (isolation wrapper; XERJ_BIN points at this) ───────────
LAUNCH="$SCRATCH/launch.sh"
if [ "$DRY" != 1 ]; then
  if [ "$ISOLATION" = systemd-scope ]; then
    cat >"$LAUNCH" <<'EOF'
#!/usr/bin/env bash
exec systemd-run --user --scope -q -p CPUQuota=400% -p MemoryMax=16G -- "$REAL_BIN" "$@"
EOF
  else
    cat >"$LAUNCH" <<'EOF'
#!/usr/bin/env bash
exec taskset -c 0-3 "$REAL_BIN" "$@"
EOF
  fi
  chmod +x "$LAUNCH"
fi

# proc_stat PID -> "utime_ticks stime_ticks" (comm may contain spaces)
proc_cpu_ticks() {
  local s
  s="$(cat "/proc/$1/stat")"
  s="${s##*) }"
  # shellcheck disable=SC2086
  set -- $s
  echo "${12} ${13}"
}

# resolve_server_pid: the pid whose exe is the server binary (the launcher
# execs, so this is normally REPRO_PID itself).
resolve_server_pid() {
  local real="$1" p c
  local -a kids
  p="$REPRO_PID"
  if [ "$(readlink -f "/proc/$p/exe" 2>/dev/null)" = "$(readlink -f "$real")" ]; then
    echo "$p"
    return
  fi
  for c in /proc/"$p"/task/*/children; do
    read -r -a kids <"$c" 2>/dev/null || continue
    for p in "${kids[@]}"; do
      if [ "$(readlink -f "/proc/$p/exe" 2>/dev/null)" = "$(readlink -f "$real")" ]; then
        echo "$p"
        return
      fi
    done
  done
  echo "$REPRO_PID"
}

# ── one run ─────────────────────────────────────────────────────────────────
run_one() {
  local arm="$1" i="$2" bin rundir state port_msg
  local binvar="BIN_$arm"
  bin="${!binvar}"
  rundir="$RESULTS/run-$arm-$i"
  state="$SCRATCH/state-$arm-$i"
  say ""
  say "== run $arm #$i  (binary $bin) =="
  if [ "$DRY" = 1 ]; then
    say "+ mkdir -p $rundir $state"
    say "+ port=\$(repro_free_port $PORT_MIN)   # never 9200"
    say "+ env REAL_BIN=$bin XERJ_NUM_CPUS=4 XERJ_PROF=1 XERJ_EMBED_MODE=neural HF_HOME=$HF_HOME \\"
    if [ "$ISOLATION" = systemd-scope ]; then
      say "    systemd-run --user --scope -q -p CPUQuota=400% -p MemoryMax=16G -- $bin --insecure --port <port> --data-dir <fresh mktemp dir> &"
    else
      say "    taskset -c 0-3 $bin --insecure --port <port> --data-dir <fresh mktemp dir> &"
    fi
    say "+ wait for GET / on <port>"
    say "+ timeout $RUN_TIMEOUT $bin autoindex $CORPUS --url http://127.0.0.1:<port> --prefix bench --bulk-mb 8 --workers 4 --progress json --state-dir $state --approve proceed > $rundir/client.log 2>&1"
    say "+ read VmHWM from /proc/<pid>/status and utime+stime from /proc/<pid>/stat"
    say "+ copy server log to $rundir/server.log; stop server; write $rundir/summary.json"
    return
  fi
  if [ -e "$rundir" ]; then
    echo "ERROR: $rundir already exists; set RESULTS_DATE to a new name." >&2
    return 1
  fi
  mkdir -p "$rundir" "$state"

  repro_free_port "$PORT_MIN"
  if [ "$REPRO_PORT" = 9200 ]; then echo "refusing port 9200" >&2; return 1; fi
  XERJ_BIN="$LAUNCH"
  export XERJ_BIN
  repro_node_start - "REAL_BIN=$bin" XERJ_NUM_CPUS=4 XERJ_PROF=1 \
    XERJ_EMBED_MODE=neural "HF_HOME=$HF_HOME"
  repro_node_wait

  local spid t0 t1 rc=0 hwm cpu_ticks
  spid="$(resolve_server_pid "$bin")"
  t0="$(date +%s.%N)"
  timeout "$RUN_TIMEOUT" "$bin" autoindex "$CORPUS" \
    --url "http://127.0.0.1:$REPRO_PORT" --prefix bench \
    --bulk-mb 8 --workers 4 --progress json \
    --state-dir "$state" --approve proceed >"$rundir/client.log" 2>&1 || rc=$?
  t1="$(date +%s.%N)"

  hwm="$(awk '/^VmHWM:/ {print $2}' "/proc/$spid/status" 2>/dev/null || true)"
  cpu_ticks="$(proc_cpu_ticks "$spid" 2>/dev/null || true)"
  cp "$REPRO_LOG" "$rundir/server.log"
  repro_node_stop
  port_msg="$REPRO_PORT"
  REPRO_PID=""

  python3 -I - "$rundir" "$rc" "$(awk -v a="$t0" -v b="$t1" 'BEGIN{printf "%.3f", b-a}')" \
    "${hwm:-}" "${cpu_ticks:-}" "$CLK_TCK" "$arm" "$i" "$port_msg" <<'PY'
import json, math, re, sys

(rundir, rc, wrapper_s, hwm, ticks, clk, arm, idx, port) = sys.argv[1:10]


def pct(vals, p):
    if not vals:
        return None
    s = sorted(vals)
    return s[max(0, math.ceil(p * len(s)) - 1)]


def read(path):
    try:
        with open(path, errors="replace") as f:
            return f.read().splitlines()
    except OSError:
        return []


client = read(rundir + "/client.log")
server = read(rundir + "/server.log")

done = None
for line in client:
    line = line.strip()
    if line.startswith("{"):
        try:
            o = json.loads(line)
        except ValueError:
            continue
        if o.get("event") == "done":
            done = o
if done is None:
    for line in client:
        if line.startswith("xerj-done "):
            done = {}
            for tok in line.split()[1:]:
                if "=" in tok:
                    k, v = tok.split("=", 1)
                    done[k] = v
            if "wall" in done:
                done["wall_s"] = done["wall"].rstrip("s")


def num(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


bulk, embed = [], []
for line in server:
    if "XERJ_PROF" not in line:
        continue
    m = re.search(r"XERJ_PROF bulk .*?\btotal_ms=(\d+)", line)
    if m:
        bulk.append(float(m.group(1)))
    m = re.search(r"\bembed_ms=(\d+)", line)
    if m:
        embed.append(float(m.group(1)))

retry_item = sum("semantic embedding window failed; retrying per item" in l for l in server)
retry_embed = sum("embed retry" in l for l in server)
r429 = sum(bool(re.search(r"\b429\b", l)) for l in server + client)

wall = num(done.get("wall_s")) if done else None
records = num(done.get("records")) if done else None
cpu_s = None
if ticks.strip():
    a, b = ticks.split()
    cpu_s = round((int(a) + int(b)) / int(clk), 2)

summary = {
    "arm": arm,
    "run": int(idx),
    "port": int(port),
    "ok": bool(done and done.get("ok") in (True, "true") and rc == "0"),
    "client_exit": int(rc),
    "wall_s": wall,
    "wall_wrapper_s": num(wrapper_s),
    "records": int(records) if records is not None else None,
    "records_per_s": round(records / wall, 2) if records and wall else None,
    "bulk_total_ms_n": len(bulk),
    "bulk_total_ms_p50": pct(bulk, 0.50),
    "bulk_total_ms_p95": pct(bulk, 0.95),
    "embed_ms_n": len(embed),
    "embed_ms_p50": pct(embed, 0.50),
    "embed_ms_p95": pct(embed, 0.95),
    "retries_window_failed": retry_item,
    "retries_embed": retry_embed,
    "client_bulk_retries": int(num(done.get("bulk_retries")) or 0) if done else None,
    "http_429": r429,
    "peak_rss_kb": int(hwm) if hwm.strip() else None,
    "cpu_s": cpu_s,
}
summary["retries"] = retry_item + retry_embed + (summary["client_bulk_retries"] or 0)
with open(rundir + "/summary.json", "w") as f:
    json.dump(summary, f, indent=2)
    f.write("\n")
print(json.dumps(summary))
PY
}

# ── interleave ──────────────────────────────────────────────────────────────
run mkdir -p "$RESULTS"
if [ "$DRY" != 1 ]; then
  python3 -I - "$RESULTS/meta.json" "$CONTROL_SHA" "$CANDIDATE_SHA" "$N" "$ISOLATION" <<'PY'
import json, platform, os, sys, time
path, c, k, n, iso = sys.argv[1:6]
json.dump({
    "control_sha": c, "candidate_sha": k, "n": int(n), "isolation": iso,
    "started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "host": platform.platform(), "nproc": os.cpu_count(),
}, open(path, "w"), indent=2)
PY
fi
for i in $(seq 1 "$N"); do
  run_one control "$i"
  run_one candidate "$i"
done

# ── report ──────────────────────────────────────────────────────────────────
if [ "$DRY" = 1 ]; then
  say ""
  say "+ write $RESULTS/report.md (median and p95 per arm; control max-min spread; delta flagged only if > spread)"
  exit 0
fi
python3 -I - "$RESULTS" <<'PY'
import glob, json, math, statistics, sys

res = sys.argv[1]
meta = json.load(open(res + "/meta.json"))
runs = {"control": [], "candidate": []}
for p in sorted(glob.glob(res + "/run-*/summary.json")):
    s = json.load(open(p))
    runs[s["arm"]].append(s)

METRICS = [
    ("wall_s", "wall_s"),
    ("records_per_s", "records/s"),
    ("bulk_total_ms_p50", "bulk total_ms p50"),
    ("bulk_total_ms_p95", "bulk total_ms p95"),
    ("embed_ms_p50", "embed_ms p50"),
    ("embed_ms_p95", "embed_ms p95"),
    ("retries", "retries"),
    ("http_429", "http_429"),
    ("peak_rss_kb", "peak_rss_kb"),
    ("cpu_s", "cpu_s"),
]


def vals(arm, key):
    return [r[key] for r in runs[arm] if r.get("ok") and r.get(key) is not None]


def p95(v):
    s = sorted(v)
    return s[max(0, math.ceil(0.95 * len(s)) - 1)]


def f(x):
    return "-" if x is None else ((f"{x:.2f}" if abs(x) < 10 else f"{x:.1f}") if isinstance(x, float) else str(x))


out = []
out.append(f"# neural-bulk A/B report {meta['started_utc']}\n")
out.append(f"- control   `{meta['control_sha']}`")
out.append(f"- candidate `{meta['candidate_sha']}`")
out.append(f"- runs per arm requested {meta['n']}; isolation {meta['isolation']}; host {meta['host']}; nproc {meta['nproc']}")
for arm in runs:
    good = sum(1 for r in runs[arm] if r.get("ok"))
    bad = [f"#{r['run']}(exit {r['client_exit']})" for r in runs[arm] if not r.get("ok")]
    out.append(f"- {arm}: {good} ok runs" + (f", EXCLUDED failed runs {', '.join(bad)}" if bad else ""))
out.append("")
out.append("Spread is the control arm's max-min over its ok runs. A delta is flagged only if |delta| exceeds it.")
out.append("p95 over N runs is the nearest-rank value (the max when N <= 20).\n")
out.append("| metric | control median | control p95 | control spread | candidate median | candidate p95 | delta (cand - ctrl median) | verdict |")
out.append("|---|---|---|---|---|---|---|---|")
for key, label in METRICS:
    c, k = vals("control", key), vals("candidate", key)
    if not c or not k:
        out.append(f"| {label} | {f(statistics.median(c)) if c else '-'} | - | - | {f(statistics.median(k)) if k else '-'} | - | - | no data |")
        continue
    cm, km = statistics.median(c), statistics.median(k)
    spread = max(c) - min(c)
    d = km - cm
    verdict = "EXCEEDS SPREAD" if abs(d) > spread else "within noise"
    pctd = f" ({100 * d / cm:+.1f}%)" if cm else ""
    out.append(f"| {label} | {f(cm)} | {f(p95(c))} | {f(spread)} | {f(km)} | {f(p95(k))} | {f(d)}{pctd} | {verdict} |")

if meta["control_sha"] == meta["candidate_sha"]:
    c, k = vals("control", "bulk_total_ms_p95"), vals("candidate", "bulk_total_ms_p95")
    out.append("")
    if c and k:
        cm, km = statistics.median(c), statistics.median(k)
        diff = abs(km - cm) / cm * 100 if cm else float("inf")
        out.append(f"Control-vs-control precondition: bulk total_ms p95 medians {f(cm)} vs {f(km)}, "
                   f"difference {diff:.1f}% -> {'PASS (<10%)' if diff < 10 else 'FAIL (>=10%): no candidate number may be published from this host/setup'}")
    else:
        out.append("Control-vs-control precondition: no data (runs failed or XERJ_PROF lines missing).")
open(res + "/report.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
PY
