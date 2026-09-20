#!/usr/bin/env bash
# Index a corpus into the local XERJ instance.
#
#   xc-index.sh <corpus>           index, or update in place what an earlier run built
#   xc-index.sh <corpus> --fresh   rebuild from scratch: build a replacement BESIDE the
#                                  existing index, verify it, and only then retire the old one
set -euo pipefail

ROOT="${XERJ_CODE_HOME:-$HOME/.xerj-code}"
CORPORA="$ROOT/corpora"
URL="${XERJ_URL:-http://localhost:9200}"
XERJ_BIN="${XERJ_BIN:-xerj}"
# One autoindex --state-dir per build, owned by this script. See "WHAT --fresh
# MEANS" below for why the state directory is never left to autoindex's default.
STATE_ROOT="$ROOT/autoindex-state"

usage() { echo "usage: xc-index.sh <corpus-name> [--fresh]" >&2; exit 2; }

[ $# -ge 1 ] || usage
name="$1"; shift

# Reject what we do not understand rather than dropping it. The previous form
# was `[ "${1:-}" = "--fresh" ] && fresh="--fresh"`, which silently ignored a
# mistyped or extra flag: `xc-index.sh <corpus> --frsh` ran an INCREMENTAL
# index and exited 0.
fresh=false
while [ $# -gt 0 ]; do
  case "$1" in
    --fresh) fresh=true; shift ;;
    *) echo "xc-index: unknown argument '$1'" >&2; usage ;;
  esac
done

dir="$CORPORA/$name"
[ -d "$dir" ] || { echo "xc-index: no corpus '$name' — run xc-corpus.sh first" >&2; exit 2; }

command -v "$XERJ_BIN" >/dev/null 2>&1 || {
  echo "xc-index: '$XERJ_BIN' not on PATH. Set XERJ_BIN to the binary path." >&2; exit 2; }

command -v python3 >/dev/null 2>&1 || {
  echo "xc-index: python3 is required (xc.py needs it too)." >&2; exit 2; }

# An auth-enabled node needs the key on every request this script makes itself,
# not only on the ones autoindex makes (autoindex reads XERJ_API_KEY on its own).
auth=()
[ -n "${XERJ_API_KEY:-}" ] && auth=(-H "Authorization: ApiKey $XERJ_API_KEY")

# `${auth[@]+…}`, not a bare "${auth[@]}": under `set -u`, bash 3.2 — still what
# macOS ships as /bin/bash — treats expanding an EMPTY array as an unbound
# variable and exits, so every call would fail on a node without auth.
http() { curl -fsS -m 30 ${auth[@]+"${auth[@]}"} "$@"; }

http -m 5 "$URL/_cluster/health" >/dev/null 2>&1 || {
  echo "xc-index: no XERJ at $URL. Start it, or set XERJ_URL." >&2; exit 2; }

# ---------------------------------------------------------------------------
# WHAT --fresh MEANS (issue #930)
#
# This flag used to be forwarded to `xerj autoindex --fresh`, which is a
# different thing: it discards autoindex's resume journal and never touches the
# records already on the server, and it is REFUSED outright once a durable
# corpus generation exists. So `xc-index.sh <corpus> --fresh` failed for every
# corpus that had been indexed before — with "cannot become generation
# authority" for a state directory written before the generation format, and
# with "--fresh cannot discard committed corpus generation N" for one written
# since. autoindex's own advice in both messages is the same: build into a NEW
# --state-dir and a NEW --prefix, validate, then switch readers. A wrapper that
# owns the whole `xc-<corpus>` namespace can do exactly that, so it does:
#
#   1. build   the replacement under  xc-<corpus>-b<stamp>-*  with a state dir
#              of its own — nothing an earlier run left can be adopted or refuse;
#   2. verify  it: autoindex exited 0 or 3 AND  _count > 0;
#   3. switch  readers by rewriting state/<corpus>.json (atomic rename);
#   4. retire  the old indices, by exact name, plus their catalog documents and
#              their state directory — only now, never before step 2 passed.
#
# A failed build removes only what it created and leaves the old index, the old
# state file and the old state directory exactly as they were.
#
# The state file keeps  "prefix": "xc-<corpus>"  on purpose. `xc-<corpus>*`
# matches every build's indices, so any reader that globs on it keeps working
# across a rebuild. `index_prefix` is the exact build, and xc.py prefers it, so
# during the (deliberate) overlap between step 1 and step 4 it does not see the
# half-built replacement beside the index it is still serving.
# ---------------------------------------------------------------------------

state_file="$ROOT/state/$name.json"

# Read one string field of the state file; empty when absent or unreadable.
state_get() {
  [ -f "$state_file" ] || return 0
  python3 - "$state_file" "$1" <<'PY' 2>/dev/null || true
import json, sys
try:
    value = json.load(open(sys.argv[1])).get(sys.argv[2], "")
except Exception:
    value = ""
print(value if isinstance(value, str) else "")
PY
}

# Every index under `xc-<corpus>-` that belongs to THIS corpus. The bare glob is
# not enough: `xc-battle-*` also matches the sibling corpus `battle-terse`, and
# retiring a sibling's indices is not a mistake this script gets to make. An
# index is a sibling's when it sits under `xc-<other>-` for a longer corpus name
# that extends this one — known from corpora/ and from state/.
# The filter is passed with -c, NOT as a heredoc on `python3 -`: a heredoc IS the
# process's stdin, so it would replace the pipe carrying the index list — the
# filter would read nothing, and curl would die writing into a closed pipe
# (exit 23), which `pipefail` + `set -e` turn into a silent exit of this script.
CORPUS_INDICES_PY='
import json, os, sys
name, corpora, state = sys.argv[1], sys.argv[2], sys.argv[3]
siblings = set()
for folder, strip in ((corpora, ""), (state, ".json")):
    try:
        for entry in os.listdir(folder):
            other = entry[: -len(strip)] if strip and entry.endswith(strip) else entry
            if other != name and other.startswith(name + "-"):
                siblings.add("xc-%s-" % other)
    except OSError:
        pass
try:
    rows = json.load(sys.stdin)
except Exception:
    rows = []
for row in rows if isinstance(rows, list) else []:
    index = row.get("index", "") if isinstance(row, dict) else ""
    if index.startswith("xc-%s-" % name) and not any(index.startswith(s) for s in siblings):
        print(index)
'
corpus_indices() {
  # A wildcard that matches nothing may answer 404; that is "no indices", not a
  # failure. Any other error also lists nothing, which errs toward keeping an
  # index (nothing is retired that was not listed), never toward deleting one.
  { http "$URL/_cat/indices/xc-$name-*?format=json&h=index" 2>/dev/null || true; } \
    | python3 -c "$CORPUS_INDICES_PY" "$name" "$CORPORA" "$ROOT/state"
}

# Records under one exact index prefix, asked ONCE. Three answers, and the
# difference between the last two is the whole point:
#   a number   the node counted them
#   0          the node answered 404 — the wildcard matches no index, so no records
#   (nothing)  the node DID NOT SAY: timeout, 5xx, connection refused, junk
# "Did not say" used to be folded into 0 by every caller, and 0 is what
# authorises the two destructive steps below. One 503 on one request was enough
# to (a) read a working 500-record index as "nothing to fall back to", keep a
# FAILED build over it and retire it, and (b) read a finished build as "empty"
# and delete it with its resume state. A node under memory pressure — the node
# a long build produces — answers exactly like that.
count_once() {
  local reply code
  # No -f: the status is read, not inferred. `|| return 0`: under `pipefail` +
  # `set -e` a failure inside `x="$(count_once …)"` would end the script at the
  # assignment instead of reporting "did not say".
  reply="$(curl -sS -m 30 ${auth[@]+"${auth[@]}"} -w '\n%{http_code}' "$URL/$1-*/_count" 2>/dev/null)" || return 0
  code="${reply##*$'\n'}"
  case "$code" in
    200) printf '%s\n' "${reply%$'\n'*}" \
           | sed -n 's/.*"count"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1 || true ;;
    404) echo 0 ;;
  esac
}

# The same, asked patiently: a busy node gets several chances before "did not
# say" is believed. Still prints nothing when it never answered — callers must
# treat that as UNKNOWN, never as zero.
COUNT_TRIES="${XC_COUNT_TRIES:-6}"
COUNT_PAUSE="${XC_COUNT_PAUSE:-5}"
count_under() {
  local attempt=1 value=""
  while :; do
    value="$(count_once "$1")"
    if [ -n "$value" ]; then printf '%s\n' "$value"; return 0; fi
    [ "$attempt" -lt "$COUNT_TRIES" ] || return 0
    attempt=$((attempt + 1))
    sleep "$COUNT_PAUSE"
  done
}

delete_indices() {   # names on stdin, one per line; exact names only, never a wildcard
  local failed=0 index
  while IFS= read -r index; do
    [ -n "$index" ] || continue
    http -X DELETE "$URL/$index" >/dev/null 2>&1 || { failed=$((failed+1)); echo "xc-index:   could not delete $index" >&2; }
  done
  return "$failed"
}

# The catalog is one global index shared by every corpus on the node, so its
# documents are removed by EXACT scope value: `corpus_scope` is a keyword, and a
# `term` on a legacy analyzed `prefix` cannot equal a hyphenated value at all —
# it under-deletes there, it never reaches a sibling corpus.
delete_catalog_scope() {
  http -X POST "$URL/autoindex-catalog/_delete_by_query?refresh=true" \
    -H 'Content-Type: application/json' \
    -d "{\"query\":{\"bool\":{\"minimum_should_match\":1,\"should\":[{\"term\":{\"corpus_scope\":\"$1\"}},{\"term\":{\"prefix\":\"$1\"}}]}}}" \
    >/dev/null 2>&1 || echo "xc-index:   could not clean autoindex-catalog for '$1' (harmless; \`xerj autoindex map\` may list retired datasets)" >&2
}

write_state() {   # rc salvaged build index_prefix state_dir
  mkdir -p "$ROOT/state"
  local tmp="$state_file.tmp.$$"
  python3 - "$tmp" "$name" "$URL" "$1" "$2" "$3" "$4" "$5" <<'PY'
import json, sys, datetime
path, corpus, url, rc, salvaged, build, index_prefix, state_dir = sys.argv[1:9]
state = {
    "corpus": corpus,
    "indexed_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "prefix": "xc-%s" % corpus,
    "url": url,
    "autoindex_exit": int(rc),
    "salvaged": salvaged == "true",
}
if build:
    state.update({"build": build, "index_prefix": index_prefix, "state_dir": state_dir})
with open(path, "w") as handle:
    json.dump(state, handle, separators=(",", ":"))
    handle.write("\n")
PY
  mv -f "$tmp" "$state_file"   # rename is atomic: a reader sees the old build or the new, never half
}

report() {   # rc docs
  case "$1" in
    0) echo "indexed cleanly" ;;
    3) echo "indexed (exit 3: some files skipped as junk — normal for real repos)" ;;
  esac
  echo "corpus '$name' searchable: $2 records"
  echo "next: xc.py $name \"<what you need>\""
}

old_build="$(state_get build)"
old_index_prefix="$(state_get index_prefix)"
old_state_dir="$(state_get state_dir)"
old_url="$(state_get url)"

# ── Which run is this? ──────────────────────────────────────────────────────
mode=legacy
if $fresh; then
  mode=build
elif [ -n "$old_build" ] && [ -n "$old_index_prefix" ] && [ -d "$old_state_dir" ] && [ "$old_url" = "$URL" ]; then
  live="$(count_under "$old_index_prefix")"
  if [ -n "$live" ] && [ "$live" -gt 0 ] 2>/dev/null; then
    mode=update
  else
    # The ledger names a build this node does not hold (data-dir swap, another
    # port). There is nothing to update and nothing to lose, so build.
    echo "xc-index: state/ records build $old_build but $URL holds no records for it — building it here"
    mode=build
  fi
elif [ ! -f "$state_file" ] && [ -z "$(corpus_indices)" ]; then
  mode=build   # first index of this corpus: same path as --fresh, nothing to retire
fi

echo "indexing corpus '$name' from $dir"

# --no-graph: reference code needs ranked passages, not a relationship map. The
# graph detectors cost real time on a large tree and nothing here consumes edges.
run_autoindex() {   # prefix [state-dir]
  local args=("$dir" --url "$URL" --prefix "$1" --no-graph)
  [ -n "${2:-}" ] && args+=(--state-dir "$2")
  set +e
  "$XERJ_BIN" autoindex "${args[@]}"
  rc=$?
  set -e
}

case "$mode" in
# ── build: first index, or --fresh ──────────────────────────────────────────
build)
  # Listed BEFORE the build so "old" can never include what this run creates.
  old_indices="$(corpus_indices)"
  # The build id has one-second resolution, and everything below keys on it: a
  # second --fresh inside the same second would reuse the prefix of the build it
  # is replacing, write into its indices, and then RETIRE them as "old". So the
  # id must be new — not the recorded build, not a state directory that exists,
  # not a prefix any live index already sits under.
  build="b$(date -u +%Y%m%d%H%M%S)"
  while [ "$build" = "$old_build" ] || [ -e "$STATE_ROOT/$name/$build" ] \
     || printf '%s\n' "$old_indices" | grep -q "^xc-$name-$build-"; do
    sleep 1
    build="b$(date -u +%Y%m%d%H%M%S)"
  done
  new_prefix="xc-$name-$build"
  new_state="$STATE_ROOT/$name/$build"
  # Is there a working index to protect? When the node cannot count it, the
  # answer is YES: presuming "none" is what lets a failed build be kept over it
  # and the old indices be retired.
  has_working_index=false
  if [ -n "$old_indices" ]; then
    old_docs="$(count_under "${old_index_prefix:-xc-$name}")"
    if [ -z "$old_docs" ]; then
      has_working_index=true; old_docs="an unknown number of"
      echo "xc-index: the node did not answer a record count for the existing index; treating it as" >&2
      echo "xc-index: a WORKING index — a failed build will not be kept over it." >&2
    elif [ "$old_docs" -gt 0 ] 2>/dev/null; then
      has_working_index=true
    fi
    echo "xc-index: --fresh — building $new_prefix-* beside the existing index ($old_docs records);"
    echo "xc-index: the existing index stays live until the replacement has been verified"
  fi
  mkdir -p "$new_state"
  run_autoindex "$new_prefix" "$new_state"

  docs="$(count_under "$new_prefix")"
  if [ -z "$docs" ]; then
    # UNKNOWN is not EMPTY. The cleanup below deletes this build's indices and
    # its resume state, so it may only run on a count the node actually gave.
    echo "xc-index: the node did not answer a record count for build $build ($COUNT_TRIES attempts;" >&2
    echo "xc-index: autoindex exit $rc). It cannot be verified, so NOTHING was deleted and NOTHING" >&2
    echo "xc-index: was switched: its indices ($new_prefix-*) and its state directory are kept." >&2
    if $has_working_index; then
      echo "xc-index: the existing index was NOT touched and is still what xc.py serves." >&2
      echo "xc-index: When the node answers again, re-run with --fresh; the unverified build is" >&2
      echo "xc-index: retired by the next build that verifies." >&2
    else
      # Nothing to protect and nothing else to serve: record it for what it is,
      # so xc.py warns on every query and a plain re-run resumes or confirms it.
      write_state "$rc" true "$build" "$new_prefix" "$new_state"
      echo "xc-index: There is no other index for '$name', so this build is recorded as UNVERIFIED" >&2
      echo "xc-index: (xc.py will say so). Re-run  xc-index.sh $name  to resume or confirm it." >&2
    fi
    [ "$rc" -ne 0 ] && [ "$rc" -ne 3 ] && exit "$rc"
    exit 1
  fi
  salvaged=false
  verified=false
  case $rc in
    0|3) [ "$docs" -gt 0 ] 2>/dev/null && verified=true ;;
    *)
      # autoindex can abort in finalisation AFTER every document was written
      # (#367), leaving a complete, queryable index behind. That is worth
      # keeping when the alternative is no corpus at all — and NOT worth
      # swapping a verified, working index out for.
      if [ "$docs" -gt 0 ] 2>/dev/null && ! $has_working_index; then
        verified=true; salvaged=true
        echo "xc-index: WARNING — autoindex exited $rc, but this build wrote $docs records and" >&2
        echo "xc-index: there is no working index to fall back to. Recording it as indexed with" >&2
        echo "xc-index: autoindex_exit=$rc; coverage is not guaranteed — please report the error above." >&2
      fi
      ;;
  esac

  if ! $verified; then
    echo "xc-index: build $build did not verify (autoindex exit $rc, $docs records)." >&2
    # Remove only what THIS run created; everything that existed before stays.
    comm -13 <(printf '%s\n' "$old_indices" | sort) <(corpus_indices | sort) \
      | grep "^$new_prefix-" | delete_indices || true
    delete_catalog_scope "$new_prefix"
    rm -rf "$new_state"
    if [ -n "$old_indices" ]; then
      echo "xc-index: the existing index was NOT touched and is still what xc.py serves." >&2
    fi
    [ "$rc" -ne 0 ] && [ "$rc" -ne 3 ] && exit "$rc"
    exit 1
  fi

  # Verified. Switch readers first, retire second: a crash between the two
  # leaves a duplicate, never a gap.
  write_state "$rc" "$salvaged" "$build" "$new_prefix" "$new_state"
  if [ -n "$old_indices" ]; then
    # Belt and braces for the invariant above: whatever else is true, an index
    # of the build that was just verified is never on the retire list.
    old_indices="$(printf '%s\n' "$old_indices" | grep -v "^$new_prefix-" || true)"
  fi
  if [ -n "$old_indices" ]; then
    echo "xc-index: replacement verified ($docs records) — retiring $(printf '%s\n' "$old_indices" | wc -l | tr -d ' ') old indices"
    if ! printf '%s\n' "$old_indices" | delete_indices; then
      echo "xc-index: WARNING — some old indices could not be deleted. The corpus is healthy and" >&2
      echo "xc-index: xc.py reads only $new_prefix-*; delete the leftovers by name when the node allows." >&2
    fi
    delete_catalog_scope "${old_index_prefix:-xc-$name}"
  fi
  # Earlier builds' state directories are dead weight once their indices are gone.
  if [ -d "$STATE_ROOT/$name" ]; then
    find "$STATE_ROOT/$name" -mindepth 1 -maxdepth 1 -type d ! -name "$build" -exec rm -rf {} +
  fi
  report "$rc" "$docs"
  ;;

# ── update: reconcile the recorded build in place ───────────────────────────
update)
  run_autoindex "$old_index_prefix" "$old_state_dir"
  case $rc in
    0|3) ;;
    *)
      echo "xc-index: updating build $old_build failed with exit $rc. The index is unchanged." >&2
      echo "xc-index: re-run with --fresh to build a replacement beside it (the existing index" >&2
      echo "xc-index: stays live until the replacement verifies)." >&2
      exit "$rc"
      ;;
  esac
  docs="$(count_under "$old_index_prefix")"; [ -n "$docs" ] || docs="?"
  write_state "$rc" false "$old_build" "$old_index_prefix" "$old_state_dir"
  report "$rc" "$docs"
  ;;

# ── legacy: a corpus indexed before builds existed, updated the way it was ──
legacy)
  # Recorded before the run so a failure can tell "this run wrote records" apart
  # from "an earlier run's records are still lying around". Salvaging the latter
  # would date stale data to now, which SKILL.md calls worse than no index.
  docs_before="$(count_under "xc-$name")"
  before_known=true
  [ -n "$docs_before" ] || { before_known=false; docs_before=0; }
  run_autoindex "xc-$name"
  docs=""
  salvaged=false
  case $rc in
    0|3) ;;
    *)
      docs="$(count_under "xc-$name")"
      # Without a count from BEFORE the run, "more records than before" cannot be
      # shown, and salvaging would date an earlier run's records to now.
      if $before_known && [ -n "$docs" ] && [ "$docs" -gt "$docs_before" ] 2>/dev/null; then
        salvaged=true
        echo "xc-index: WARNING — autoindex exited $rc, but this run wrote records" >&2
        echo "xc-index: ($docs_before -> $docs). The corpus is queryable and is being" >&2
        echo "xc-index: recorded as indexed, with autoindex_exit=$rc in its state file." >&2
        echo "xc-index: Coverage is not guaranteed — please report the error above." >&2
      else
        echo "xc-index: autoindex failed with exit $rc and wrote no new records." >&2
        echo "xc-index: If the error above says the state directory cannot become generation" >&2
        echo "xc-index: authority, or that --fresh is refused, run:  xc-index.sh $name --fresh" >&2
        echo "xc-index: It builds a replacement beside the existing index and retires the old one" >&2
        echo "xc-index: only after the new one verifies." >&2
        exit "$rc"
      fi
      ;;
  esac
  [ -n "$docs" ] || docs="$(count_under "xc-$name")"
  [ -n "$docs" ] || docs="?"
  write_state "$rc" "$salvaged" "" "" ""
  report "$rc" "$docs"
  ;;
esac
