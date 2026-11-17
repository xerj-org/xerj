#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# repro.sh — shared plumbing for the scripts/bench one-command reproductions.
#
# Every scripts/bench/<name>.sh reproduces ONE headline number that public copy
# quotes (landing/benchmarks/*, docs/*, demo/playbooks/SCORECARD.md) against a
# throwaway local node on a PRIVATE port (93xx, never :9200 — the runner's
# cleanup_indices does DELETE /_all and would wipe anything living there).
#
# Sourced, never executed directly:
#   source "$(dirname "${BASH_SOURCE[0]}")/lib/repro.sh"
#
# Provides:
#   repro_find_binary   -> sets $XERJ_BIN (existing release binary)
#   repro_free_port MIN -> sets $REPRO_PORT (first free base in MIN..MIN+19,
#                          checking base/base+1/base+2 — the node claims all 3)
#   repro_node_start [extra args...]   -> starts $XERJ_BIN on $REPRO_PORT, with
#                                         the whole-response query cache OFF
#   repro_node_wait                     -> poll GET / until it answers
#   repro_node_stop                     -> kill + remove the throwaway data dir
#   repro_claim / repro_measured        -> the aligned CLAIM vs MEASURED lines
#   repro_query_cache_report            -> cache hits/misses the node counted
#
# Environment knobs (all optional):
#   XERJ_BIN    path to a built server binary (default: engine/target/release/xerj)
#   PORT        force a specific base port (still must be free)
#   KEEP=1      keep the throwaway data dir + server log after the run
# ─────────────────────────────────────────────────────────────────────────────

# Repo root from THIS file's location (scripts/bench/lib/repro.sh -> repo root),
# never from the caller's — sourcing contexts vary.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
export REPO_ROOT

# ── binary ──────────────────────────────────────────────────────────────────
repro_find_binary() {
  if [ -n "${XERJ_BIN:-}" ]; then
    if [ ! -x "$XERJ_BIN" ]; then
      echo "repro: XERJ_BIN=$XERJ_BIN is not executable" >&2
      return 1
    fi
    export XERJ_BIN
    return 0
  fi
  local cand
  for cand in \
    "$REPO_ROOT/engine/target/release/xerj" \
    "$REPO_ROOT/target/release/xerj"; do
    if [ -x "$cand" ]; then
      export XERJ_BIN="$cand"
      return 0
    fi
  done
  echo "repro: no server binary found — build one first:" >&2
  echo "  cd engine && cargo build --release -p xerj-server" >&2
  echo "  or point XERJ_BIN at an existing binary." >&2
  return 1
}

# ── ports (private 93xx only) ───────────────────────────────────────────────
repro_port_free() {
  python3 - "$1" <<'PY'
import socket, sys
base = int(sys.argv[1])
for p in (base, base + 1, base + 2):
    s = socket.socket()
    try:
        s.bind(("127.0.0.1", p))
    except OSError:
        sys.exit(1)
    finally:
        s.close()
PY
}

repro_free_port() {
  local min="${1:-9310}" p
  if [ -n "${PORT:-}" ]; then
    if repro_port_free "$PORT"; then
      REPRO_PORT="$PORT"
      return 0
    fi
    echo "repro: PORT=$PORT (or its +1/+2 neighbours) is busy" >&2
    return 1
  fi
  for p in $(seq "$min" $((min + 19))); do
    if repro_port_free "$p"; then
      REPRO_PORT="$p"
      return 0
    fi
  done
  echo "repro: no free port in $min..$((min + 19))" >&2
  return 1
}

# ── node lifecycle ──────────────────────────────────────────────────────────
# repro_node_start <config.toml|-> [env VAR=VAL ...] — env pairs come as
# VAR=VAL words after the config path ("-" for none). The node is started
# detached with its own session, logs into $REPRO_LOG, data in $REPRO_DATA.
#
# XERJ_DISABLE_QUERY_CACHE=1 is the default here. The node caches whole search
# responses by default, so a repeated identical query is answered from memory
# (#3020 measured 0.70 ms cached against 7.80 ms executed) and a benchmark that
# repeats queries would time the cache, not search. A script that really means
# to measure the cache passes XERJ_DISABLE_QUERY_CACHE=0 itself.
repro_node_start() {
  local config="$1"
  shift
  local env_pair
  local cache_set=0
  for env_pair in "$@"; do
    case "$env_pair" in XERJ_DISABLE_QUERY_CACHE=*) cache_set=1 ;; esac
  done
  REPRO_DATA="$(mktemp -d /tmp/xerj-bench.XXXXXX)"
  REPRO_LOG="$REPRO_DATA/server.log"
  export REPRO_DATA REPRO_LOG
  local args=("$XERJ_BIN" --insecure --port "$REPRO_PORT" --data-dir "$REPRO_DATA")
  [ "$config" != "-" ] && args+=(-c "$config")
  local envs=()
  [ "$cache_set" = 1 ] || envs+=(XERJ_DISABLE_QUERY_CACHE=1)
  for env_pair in "$@"; do envs+=("$env_pair"); done
  # Direct child of this shell (no setsid/pgrep — neither is guaranteed in a
  # minimal sandbox), log to file, pid remembered for the EXIT trap.
  env "${envs[@]}" "${args[@]}" >"$REPRO_LOG" 2>&1 &
  REPRO_PID=$!
  export REPRO_PID
  sleep 0.4
  if ! kill -0 "$REPRO_PID" 2>/dev/null; then
    echo "repro: node failed to start; log:" >&2
    tail -20 "$REPRO_LOG" >&2 || true
    return 1
  fi
}

repro_node_wait() {
  local i
  for i in $(seq 1 150); do
    if curl -sf -o /dev/null "http://127.0.0.1:$REPRO_PORT/"; then
      return 0
    fi
    if ! kill -0 "$REPRO_PID" 2>/dev/null; then
      echo "repro: node died while waiting; log:" >&2
      tail -20 "$REPRO_LOG" >&2 || true
      return 1
    fi
    sleep 0.2
  done
  echo "repro: node did not answer on :$REPRO_PORT within 30s" >&2
  return 1
}

repro_node_stop() {
  if [ -n "${REPRO_PID:-}" ]; then
    kill "$REPRO_PID" 2>/dev/null || true
    local i
    for i in $(seq 1 50); do
      kill -0 "$REPRO_PID" 2>/dev/null || break
      sleep 0.1
    done
    kill -9 "$REPRO_PID" 2>/dev/null || true
  fi
  if [ -n "${REPRO_DATA:-}" ] && [ "${KEEP:-0}" != "1" ]; then
    rm -rf "$REPRO_DATA"
  elif [ -n "${REPRO_DATA:-}" ]; then
    echo "repro: KEEP=1 — data dir and log kept at $REPRO_DATA" >&2
  fi
}

# ── output helpers ──────────────────────────────────────────────────────────
repro_claim() {
  printf '  CLAIMED   %s\n' "$1"
}

repro_measured() {
  printf '  MEASURED  %s\n' "$1"
}

repro_note() {
  printf '  note      %s\n' "$1"
}

repro_header() {
  printf '\n== %s ==\n' "$1"
}

# The node's own whole-response cache counters (xerj_query_cache_hits /
# _misses on /v1/metrics). They are per-index counters summed over the indexes
# open at scrape time: a deleted index takes its hits with it, so a script that
# deletes the index it timed before calling this can read 0 with the cache on.
# The current scripts report while their timed index still exists. With the
# cache off both stay 0: no request is even looked up. A non-zero hit count
# means some measured responses were served from the cache: a warning.
repro_query_cache_report() {
  local metrics hits misses
  metrics="$(curl -sf "http://127.0.0.1:$REPRO_PORT/v1/metrics" 2>/dev/null)" || {
    printf '  cache     unknown (GET /v1/metrics did not answer)\n'
    return 0
  }
  hits="$(printf '%s\n' "$metrics" | awk '$1 == "xerj_query_cache_hits" {print $2}')"
  misses="$(printf '%s\n' "$metrics" | awk '$1 == "xerj_query_cache_misses" {print $2}')"
  printf '  cache     whole-response query cache: %s hit(s), %s miss(es) (/v1/metrics)\n' \
    "${hits:-?}" "${misses:-?}"
  if [ -n "$hits" ] && [ "$hits" != 0 ]; then
    printf '  WARNING   %s response(s) came from the query cache: latency here is partly cache time\n' "$hits"
  fi
}

repro_env_box() {
  # what this run used — so a stranger can tell whether their numbers compare
  printf '\nrun environment\n'
  printf '  binary    %s (%s)\n' "$XERJ_BIN" "$("$XERJ_BIN" --version | head -1)"
  printf '  port      %s (private, throwaway data dir)\n' "$REPRO_PORT"
  repro_query_cache_report
  printf '  host      %s\n' "$(uname -srm 2>/dev/null || echo unknown)"
  printf '  nproc     %s\n' "$(nproc 2>/dev/null || echo '?')"
  printf '  date      %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
