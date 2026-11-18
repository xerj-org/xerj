#!/usr/bin/env bash
# Per-platform runtime gate for `xerj autoindex`. Two regressions live here:
#
#   1. the "Too many open files" (EMFILE) crash — described below;
#   2. #482, the `--no-graph` ERROR_ACCESS_DENIED abort on Windows — described
#      at its own section further down.
#   3. #2644, `xerj autoindex --skills` finding skill folders under HOME or
#      USERPROFILE, at its own section near the end.
#
# All three exist because nothing in CI used to RUN the binary on macOS/Windows.
#
# The crash: `xerj autoindex` on a large repo infers hundreds of datasets =>
# hundreds of indices. Before the fix each index pinned one WAL file descriptor
# PER ingest shard (a count that scales with CPU cores, ~8-16), so a few hundred
# datasets held thousands of fds open and aborted with EMFILE — fatal on macOS,
# whose default soft limit is 256.
#
# This gate constrains the descriptor budget so the regression is caught on ANY
# runner regardless of its `kern.maxfilesperproc`:
#   soft 256  — the macOS default; the server MUST raise it (raise_nofile_limit)
#   hard 4096 — between the pre-fix need (~6,400 for 400 datasets) and the
#               post-fix need (~600). Pre-fix => EMFILE; post-fix => fits.
# So the job fails if EITHER the limit-raise OR the per-index WAL-fd reduction
# regresses. Runs under bash on ubuntu / macOS / windows(git-bash).
set -uo pipefail

# Best-effort FD budget (no-op on Windows, which uses a different handle model).
ulimit -Hn 4096 2>/dev/null || true
ulimit -Sn 256  2>/dev/null || true
echo "platform: $(uname -s)   ulimit -Sn: $(ulimit -Sn 2>/dev/null || echo n/a)   -Hn: $(ulimit -Hn 2>/dev/null || echo n/a)"

# Overridable so CI can point at a cheaper profile than fat-LTO release;
# defaults to the release path for local use.
BIN="${XERJ_BIN:-engine/target/release/xerj}"
[ -f "$BIN.exe" ] && BIN="$BIN.exe" # windows

# ── generate 400 distinct-schema datasets (one index each) ─────────────────
# Each dataset must stay its OWN index, or the descriptor pressure this gate
# exists for disappears. Since #2422/#2430 one run aims for at most
# MAX_DATASETS_PER_RUN (32) data datasets: past that, CSV/JSONL/log clusters
# below DATASET_CAP_PROTECT_RECORDS (1024) rows merge into one shared dataset
# per scope and family (400 x 3-row CSVs became 32 indices). A cluster of
# 1024+ rows "never moves", so every folder gets ROWS >= 1024 rows. For CSV the
# cluster size is the SAMPLED row count (the extractor declares no length),
# so autoindex below runs with --sample SAMPLE > ROWS. Keep
# ROWS > DATASET_CAP_PROTECT_RECORDS in engine/crates/xerj-autoindex/src/dataset.rs.
N_DATASETS=400
ROWS=1050
SAMPLE=1100
ROOT="$(mktemp -d)"
CORPUS="$ROOT/corpus"
mkdir -p "$CORPUS"
for i in $(seq 1 "$N_DATASETS"); do mkdir -p "$CORPUS/ds_$i"; done
# Unique column names per folder => a distinct inferred dataset per folder.
# One awk process writes all files (cheap on git-bash, where forks are slow).
awk -v n="$N_DATASETS" -v rows="$ROWS" -v root="$CORPUS" 'BEGIN {
  split("alpha beta gamma delta", w, " ")
  for (i = 1; i <= n; i++) {
    f = root "/ds_" i "/data.csv"
    printf "k_%d_id,k_%d_name,k_%d_val\n", i, i, i > f
    for (r = 1; r <= rows; r++) printf "%d,%s,%d\n", r, w[(r % 4) + 1], r * 10 > f
    close(f)
  }
}' || { echo "::error::corpus generation failed"; exit 1; }
echo "generated $(find "$CORPUS" -name '*.csv' | wc -l | tr -d ' ') dataset files of $ROWS rows"

# ── boot the server (inherits the constrained fd budget) ───────────────────
DATA="$ROOT/data"
mkdir -p "$DATA"
"$BIN" --insecure --data-dir "$DATA" > "$DATA/server.log" 2>&1 &
SPID=$!
up=0
for _ in $(seq 1 160); do
  if curl -fs -m1 localhost:9200/_cluster/health >/dev/null 2>&1; then up=1; break; fi
  kill -0 "$SPID" 2>/dev/null || { echo "server exited during boot"; cat "$DATA/server.log"; exit 1; }
  sleep 0.5
done
[ "$up" = 1 ] || { echo "server never became healthy"; cat "$DATA/server.log"; exit 1; }

# ── write/read round-trip on the platform's own filesystem ─────────────────
# Index creation runs the durable-publish chain (write tmp → fsync → rename →
# fsync parent dir), which is where per-platform filesystem semantics bite:
# on Windows `fsync_dir` used to fail with ERROR_ACCESS_DENIED for every call,
# so the server aborted at boot and no index could ever be created. Autoindex
# below would catch that too, but only as a vague "server exited" — assert the
# round-trip explicitly so the failure names itself.
curl -fs -m10 -XPUT localhost:9200/smoke-crud -H 'content-type: application/json' \
  -d '{"settings":{"number_of_shards":1}}' > "$DATA/crud.log" 2>&1 \
  || { echo "::error::create index failed on $(uname -s)"; cat "$DATA/crud.log"; cat "$DATA/server.log"; exit 1; }
curl -fs -m10 -XPOST 'localhost:9200/smoke-crud/_doc/1?refresh=true' -H 'content-type: application/json' \
  -d '{"title":"windows durability round trip"}' >> "$DATA/crud.log" 2>&1 \
  || { echo "::error::index doc failed on $(uname -s)"; cat "$DATA/crud.log"; cat "$DATA/server.log"; exit 1; }
HITS="$(curl -s -m10 'localhost:9200/smoke-crud/_search?q=durability' | tr -d ' \n' | sed -n 's/.*"value":\([0-9]*\).*/\1/p' | head -1)"
[ "${HITS:-0}" = "1" ] \
  || { echo "::error::search round-trip returned '${HITS:-}' hits, expected 1"; cat "$DATA/server.log"; exit 1; }
echo "write/read round-trip OK on $(uname -s)"

# ── autoindex the corpus ───────────────────────────────────────────────────
# --sample: see the corpus comment (CSV cluster size is the sampled count).
# --max-minutes 0: never stop at the estimate gate (exit 4) on a slow runner.
# --no-semantic: no embeddings for ~420k rows; FD pressure comes from the
# index count, not from semantic fields.
"$BIN" autoindex "$CORPUS" --sample "$SAMPLE" --max-minutes 0 --no-semantic \
  > "$DATA/ax.log" 2>&1
RC=$?
echo "autoindex exit=$RC"
tail -20 "$DATA/ax.log" || true

# ── assertions ─────────────────────────────────────────────────────────────
FAIL=0
if grep -qiE 'too many open files|os error 24|emfile' "$DATA/ax.log" "$DATA/server.log"; then
  echo "::error::FD-exhaustion regression — autoindex hit 'Too many open files'"
  FAIL=1
fi
# autoindex exit codes: 0 complete, 3 completed-with-junk (both fine).
if [ "$RC" != "0" ] && [ "$RC" != "3" ]; then
  echo "::error::autoindex exited $RC (expected 0 or 3)"
  FAIL=1
fi
CREATED="$(curl -s 'localhost:9200/_cat/indices?h=index' 2>/dev/null | grep -c 'ax-' || true)"
echo "ax-* indices created: $CREATED"
# One index per folder is the FD pressure; 300 leaves slack for the engine
# legitimately joining a few. Far fewer means the corpus no longer produces
# one index per dataset (e.g. the run cap merged them): fix the corpus, not
# the threshold, or the gate stops guarding EMFILE.
if [ "${CREATED:-0}" -lt 300 ]; then
  echo "::error::expected >=300 datasets indexed, got ${CREATED:-0} (are the datasets still >= DATASET_CAP_PROTECT_RECORDS rows?)"
  FAIL=1
fi
# Every row indexed, read from autoindex's own summary line.
WANT_RECORDS=$((N_DATASETS * ROWS))
RECORDS="$(sed -n 's/.*xerj-done .*[[:space:]]records=\([0-9]*\).*/\1/p' "$DATA/ax.log" | tail -1)"
echo "records indexed: ${RECORDS:-?} (want $WANT_RECORDS)"
if [ "${RECORDS:-0}" -lt "$WANT_RECORDS" ]; then
  echo "::error::expected $WANT_RECORDS records indexed, got ${RECORDS:-none}"
  FAIL=1
fi

# ── the `--no-graph` generated path (#482) ─────────────────────────────────
# The default (graph) path above never touches `sync-snapshots/`. `--no-graph`
# does: it seals a durable source snapshot under the state directory and
# fsyncs the directories it just published. Sealing used `File::open(dir)
# .sync_all()`, a Unix-only idiom that returns ERROR_ACCESS_DENIED (os error
# 5) on EVERY Windows call, so `xerj autoindex <folder> --no-graph` aborted on
# Windows right after the journal was written, before one document was
# indexed — reported against rc.17 and invisible to this job because it only
# ever ran the graph path.
#
# Two runs, on purpose. The first exercises the seal (create_snapshot_inner);
# the second exercises snapshot GC over an existing `sync-snapshots/`
# directory, which was a second, unconditional open of the same kind — so
# once a Windows user had run `--no-graph` once, EVERY later run over that
# state directory died too.
NG_CORPUS="$ROOT/nograph"
mkdir -p "$NG_CORPUS"
printf '# beacon report\n\nA short markdown note mentioning a trojan.\n' > "$NG_CORPUS/t.md"
printf 'ng_id,ng_name,ng_val\n1,alpha,10\n2,beta,20\n' > "$NG_CORPUS/rows.csv"
NG_STATE="$ROOT/nograph-state"

for attempt in 1 2; do
  "$BIN" autoindex "$NG_CORPUS" --no-graph --max-minutes 0 \
    --state-dir "$NG_STATE" --prefix ng > "$DATA/ng-$attempt.log" 2>&1
  NGRC=$?
  echo "autoindex --no-graph (run $attempt) exit=$NGRC"
  tail -20 "$DATA/ng-$attempt.log" || true
  if [ "$NGRC" != "0" ] && [ "$NGRC" != "3" ]; then
    echo "::error::--no-graph autoindex run $attempt exited $NGRC (expected 0 or 3) on $(uname -s)"
    FAIL=1
  fi
  # The localised Windows message ("Acceso denegado", "Zugriff verweigert", …)
  # is why this matches the os error NUMBER and not the text.
  if grep -qiE 'os error 5([^0-9]|$)' "$DATA/ng-$attempt.log" "$DATA/server.log"; then
    echo "::error::--no-graph run $attempt hit os error 5 (ACCESS_DENIED) — #482 regression"
    FAIL=1
  fi
done

# A silent no-op must not pass. The seal is the operation that aborted, so
# assert its output exists: a sealed snapshot directory holding a manifest.
SEALED="$(find "$NG_STATE/sync-snapshots" -name manifest.json 2>/dev/null | head -1)"
if [ -z "$SEALED" ]; then
  echo "::error::--no-graph sealed no snapshot manifest under $NG_STATE/sync-snapshots"
  FAIL=1
else
  echo "sealed snapshot manifest: $SEALED"
fi
NG_CREATED="$(curl -s 'localhost:9200/_cat/indices?h=index' 2>/dev/null | grep -cE '(^|[[:space:]])ng-' || true)"
echo "ng-* indices created: $NG_CREATED"
if [ "${NG_CREATED:-0}" -lt 1 ]; then
  echo "::error::--no-graph created no ng-* index"
  FAIL=1
fi

# ── `xerj autoindex --skills` finds skill folders on this platform (#2644) ──
# The skill folders are found under HOME, else USERPROFILE (Windows). A fake
# home holds two of the folders `--skills` looks for (`.claude/skills`,
# `.agents/skills`); `.codex/skills` is left missing on purpose, and the run
# starts in an empty folder so no project `.claude/skills` is found. Both
# folders must be indexed, a search must find a skill, and the missing folder
# must not be mentioned at all.
SK_HOME="$ROOT/skills-home"
SK_CWD="$ROOT/skills-cwd"
mkdir -p "$SK_HOME/.claude/skills/kiln-log" "$SK_HOME/.agents/skills/tide-chart" "$SK_CWD"
printf -- '---\nname: kiln-log\ndescription: Record pottery kiln firings and their temperatures.\n---\n\n# Kiln log\n\nWrite one line per firing with the cone reached.\n' \
  > "$SK_HOME/.claude/skills/kiln-log/SKILL.md"
printf -- '---\nname: tide-chart\ndescription: Read a harbour tide chart before a boat launch.\n---\n\n# Tide chart\n\nFind the next high water for the harbour.\n' \
  > "$SK_HOME/.agents/skills/tide-chart/SKILL.md"
# On Windows (git-bash and MSYS, where cygpath exists) HOME is emptied so the
# run takes the folder from USERPROFILE as a Windows path, the way a program
# started from PowerShell or cmd, where HOME is usually unset, finds it.
SK_HOME_VAR="$SK_HOME"
SK_PROFILE="$SK_HOME"
if command -v cygpath >/dev/null 2>&1; then
  SK_HOME_VAR=""
  SK_PROFILE="$(cygpath -w "$SK_HOME")"
fi
BIN_ABS="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
(cd "$SK_CWD" && HOME="$SK_HOME_VAR" USERPROFILE="$SK_PROFILE" \
  "$BIN_ABS" autoindex --skills --max-minutes 0) > "$DATA/skills.log" 2>&1
SKRC=$?
echo "autoindex --skills exit=$SKRC"
tail -20 "$DATA/skills.log" || true
if [ "$SKRC" != "0" ] && [ "$SKRC" != "3" ]; then
  echo "::error::autoindex --skills exited $SKRC (expected 0 or 3) on $(uname -s)"
  FAIL=1
fi
for want in claude agents; do
  if ! curl -s 'localhost:9200/_cat/indices?h=index' 2>/dev/null | grep -qE "(^|[[:space:]])skills-$want-"; then
    echo "::error::--skills created no skills-$want-* index on $(uname -s)"
    FAIL=1
  fi
done
if grep -q '\.codex' "$DATA/skills.log"; then
  echo "::error::--skills mentioned the missing .codex/skills folder on $(uname -s)"
  FAIL=1
fi
# One search per folder; each query shares words with one skill only.
for pair in "kiln-log|pottery kiln firing temperatures" "tide-chart|harbour tide chart boat launch"; do
  skill="${pair%%|*}"
  "$BIN_ABS" search --prefix skills "${pair#*|}" > "$DATA/skills-search.log" 2>&1
  if ! grep -qE "$skill[/\\\\]SKILL\.md" "$DATA/skills-search.log"; then
    echo "::error::a --skills search did not find $skill/SKILL.md on $(uname -s)"
    cat "$DATA/skills-search.log"
    FAIL=1
  else
    echo "--skills search found $skill/SKILL.md"
  fi
done

kill "$SPID" 2>/dev/null || true
if [ "$FAIL" = 0 ]; then
  echo "AUTOINDEX SMOKE PASSED on $(uname -s) ($CREATED datasets, no EMFILE; \
--no-graph sealed and reconciled, no ACCESS_DENIED; --skills found 2 folders)"
else
  echo "AUTOINDEX SMOKE FAILED on $(uname -s)"
  exit 1
fi
