#!/usr/bin/env bash
# Clean-HOME field report. Installs the local debug binary into an empty
# HOME, indexes a tiny git repo that was created here (nothing is cloned),
# and runs `xerj feedback --dry-run`. The dry run must exit 0, keep --agent,
# and include the verdict. It must not post anywhere and it must not see a
# GitHub token.
#
# The binary is `cargo build -p xerj-server --no-default-features`: the
# lexical embedder, which is what a default node uses. Set XERJ_BIN to skip
# the build. Override the port with XERJ_E2E_PORT (it also binds port+1 and
# port+2).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PORT="${XERJ_E2E_PORT:-19273}"
SINK_PORT="${XERJ_E2E_SINK_PORT:-19279}"

if [ -n "${XERJ_BIN:-}" ]; then
  BIN="$XERJ_BIN"
else
  echo "building xerj (debug, --no-default-features, toolchain 1.97.1)"
  (cd "$ROOT/engine" && cargo +1.97.1 build --locked -p xerj-server --no-default-features)
  BIN="$ROOT/engine/target/debug/xerj"
fi
[ -x "$BIN" ] || { echo "error: no executable at $BIN"; exit 1; }

WORK="$(mktemp -d)"
SPID=""
LPID=""
cleanup() {
  if [ -n "$SPID" ]; then kill "$SPID" 2>/dev/null || true; wait "$SPID" 2>/dev/null || true; fi
  if [ -n "${SINK_STOP:-}" ]; then touch "$SINK_STOP" 2>/dev/null || true; fi
  if [ -n "$LPID" ]; then wait "$LPID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

export HOME="$WORK/home"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$BIN" "$HOME/.local/bin/xerj"
INST="$HOME/.local/bin/xerj"

# CI injects GITHUB_TOKEN. The fresh agent this script stands in for has
# neither a token nor a gh login. Drop them before any xerj command.
unset GH_TOKEN GITHUB_TOKEN GH_ENTERPRISE_TOKEN || true
if [ -n "${GH_TOKEN:-}${GITHUB_TOKEN:-}${GH_ENTERPRISE_TOKEN:-}" ]; then
  echo "error: a GitHub token is still set"
  exit 1
fi

# A sink that records any TCP connection. --dry-run must not open one,
# including if a build starts honouring XERJ_FIELD_REPORT_URL.
SINK_COUNT="$WORK/sink-count"
SINK_STOP="$WORK/sink-stop"
SINK_READY="$WORK/sink-ready"
printf '0\n' > "$SINK_COUNT"
python3 - "$SINK_COUNT" "$SINK_PORT" "$SINK_STOP" "$SINK_READY" <<'PY' &
import os, socket, sys
count_path, port, stop, ready = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", port))
s.listen(8)
s.settimeout(0.2)
with open(ready, "w", encoding="utf-8") as fh:
    fh.write("ready\n")
n = 0
while not os.path.exists(stop):
    try:
        conn, _ = s.accept()
    except socket.timeout:
        continue
    try:
        conn.recv(65536)
    except OSError:
        pass
    conn.close()
    n += 1
    with open(count_path, "w", encoding="utf-8") as fh:
        fh.write(f"{n}\n")
PY
LPID=$!
export XERJ_FIELD_REPORT_URL="http://127.0.0.1:${SINK_PORT}/api/field-report"
for _ in $(seq 1 50); do
  if [ -f "$SINK_READY" ]; then break; fi
  if ! kill -0 "$LPID" 2>/dev/null; then break; fi
  sleep 0.1
done
if [ ! -f "$SINK_READY" ]; then
  echo "error: field-report sink is not listening on $SINK_PORT"
  exit 1
fi

DATA="$WORK/data"
mkdir -p "$DATA"
"$INST" --insecure --data-dir "$DATA" --port "$PORT" >"$DATA/server.log" 2>&1 &
SPID=$!
URL="http://127.0.0.1:${PORT}"
up=0
for _ in $(seq 1 120); do
  if curl -fsS -m 2 "$URL/_cluster/health" >/dev/null 2>&1; then up=1; break; fi
  if ! kill -0 "$SPID" 2>/dev/null; then
    echo "error: server exited during boot"
    cat "$DATA/server.log"
    exit 1
  fi
  sleep 0.5
done
if [ "$up" != 1 ]; then
  echo "error: server never became healthy"
  cat "$DATA/server.log"
  exit 1
fi

REPO="$WORK/tiny"
mkdir -p "$REPO"
git -C "$REPO" init -q
git -C "$REPO" config user.email "e2e@example.invalid"
git -C "$REPO" config user.name "e2e"
printf 'fn hello() {}\n' > "$REPO/hello.rs"
git -C "$REPO" add hello.rs
git -C "$REPO" commit -q -m "tiny"
# Nothing in this repo was cloned from GitHub.
if git -C "$REPO" remote -v | grep -q .; then
  echo "error: the tiny repo has a remote"
  exit 1
fi

set +e
"$INST" autoindex "$REPO" --url "$URL" --no-graph --prefix e2e >"$DATA/ax.log" 2>&1
AX=$?
set -e
if [ "$AX" != 0 ] && [ "$AX" != 3 ]; then
  echo "error: autoindex exited $AX"
  cat "$DATA/ax.log"
  exit 1
fi

VERDICT="dry-run-kept-the-verdict"
AGENT="e2e-agent"
set +e
"$INST" feedback --dry-run --url "$URL" \
  --agent "$AGENT" \
  --used-for "indexed one local file" \
  --verdict "$VERDICT" \
  >"$DATA/feedback.out" 2>"$DATA/feedback.err"
FB=$?
set -e
if [ "$FB" != 0 ]; then
  echo "error: feedback --dry-run exited $FB"
  cat "$DATA/feedback.out" "$DATA/feedback.err"
  exit 1
fi

grep -F -e "$VERDICT" "$DATA/feedback.out" >/dev/null \
  || { echo "error: verdict missing from the dry run"; cat "$DATA/feedback.out"; exit 1; }
# `--` so a pattern that begins with --agent is not parsed as a grep flag.
grep -F -e "--agent \"$AGENT\"" -- "$DATA/feedback.out" >/dev/null \
  || { echo "error: --agent was dropped from the ready-to-run command"; cat "$DATA/feedback.out"; exit 1; }

touch "$SINK_STOP"
wait "$LPID" 2>/dev/null || true
LPID=""
HITS="$(tr -d '[:space:]' < "$SINK_COUNT")"
if [ "$HITS" != 0 ]; then
  echo "error: dry-run opened $HITS connection(s) to $XERJ_FIELD_REPORT_URL"
  exit 1
fi

echo "e2e ok: installed=$INST autoindex=$AX verdict and --agent kept, sink connections=0"
