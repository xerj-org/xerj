#!/usr/bin/env bash
# The per-pack consumer end-to-end gate (CORPUS_HUB_V2 spec §12.3).
#
#   consumer_e2e.sh <xerj> <slug> <pin8> <tag> <pack-dir> <seed-file>
#
# <pack-dir> holds <slug>-<pin8>.hubpack.tar.zst and its .sig: the bytes a
# client would download (hub-packs.yml passes the DRAFT release's assets,
# downloaded back from GitHub, or in a dry run the builder's output). The pack
# is served exactly as a client meets it:
#
#   1. a one-entry catalogue is built by `xerj hub build-catalogue` and signed
#      with <seed-file> (the hub key), its pack_url pointing at a release-shaped
#      copy of <pack-dir> (file://<work>/rel/<tag>/<file>), and served over HTTP
#      as <hub>/api/v1/catalogue.json + .sig;
#   2. a FRESH node (empty data dir) and an EMPTY $HOME;
#   3. `xerj hub pull <slug>`: catalogue signature, pack sha256 + signature,
#      snapshot restore — all against the key compiled into <xerj>;
#   4. `xerj code <slug> --def <symbol> --json`, the symbol taken from the pack's
#      own defs.jsonl, must return that symbol from the local pack;
#      4b. the same lookup with NO XERJ_URL must find the node from the ledger
#      `hub pull` wrote (not localhost:9200); skipped with 4 when no symbol;
#   5. `xerj code <slug> "<word from ask_for>" --json` must return a hit that
#      cites a file the way `xerj code` itself does (an ax_path/ax_file/path/file
#      provenance path; the line is optional, as the renderer never invents one).
#
# Any failure exits non-zero. Ports: E2E_NODE_PORT (default 19300; the node also
# takes the next two) and E2E_HUB_PORT (default 19310). REGISTRY and BACKLOG as in
# hub-packs.yml. The seed file is read only by step 1.
set -euo pipefail

if [ "$#" -ne 6 ]; then
  sed -n '2,25p' "$0" >&2
  exit 2
fi
BIN=$(realpath "$1"); SLUG=$2; PIN8=$3; TAG=$4; SRC=$(realpath "$5"); SEED=$6
: "${REGISTRY:?REGISTRY must name the registry directory}"
: "${BACKLOG:?BACKLOG must name the backlog file}"
NODE_PORT=${E2E_NODE_PORT:-19300}
HUB_PORT=${E2E_HUB_PORT:-19310}
FILE="$SLUG-$PIN8.hubpack.tar.zst"
REGISTRY=$(realpath "$REGISTRY"); BACKLOG=$(realpath "$BACKLOG")

WORK=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/hub-e2e.XXXXXX")
mkdir -p "$WORK/rel/$TAG" "$WORK/site/api/v1" "$WORK/home" "$WORK/node/data"
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT
fail() {
  echo "::error title=consumer e2e ($SLUG)::$*"
  for log in "$WORK/node/node.log" "$WORK/http.log"; do
    [ -f "$log" ] && { echo "--- tail $log"; tail -n 40 "$log"; }
  done
  exit 1
}

# 1. Catalogue + release layout.
cp "$SRC/$FILE" "$SRC/$FILE.sig" "$WORK/rel/$TAG/"
printf '%s\t%s\t%s\n' "$SLUG" "$PIN8" "$TAG" > "$WORK/tags.tsv"
"$BIN" hub build-catalogue --registry "$REGISTRY" --backlog "$BACKLOG" \
  --packs "$WORK/rel/$TAG" --tags "$WORK/tags.tsv" --release-base "file://$WORK/rel" \
  --out "$WORK/site/api/v1/catalogue.json" --sign-key "$SEED" \
  || fail "build-catalogue failed"
python3 -m http.server "$HUB_PORT" --bind 127.0.0.1 --directory "$WORK/site" \
  > "$WORK/http.log" 2>&1 &
PIDS+=("$!")

# 2. A fresh node and an empty HOME. The node is started the way a user starts
#    one — no config file, no snapshot allowlist: a server restores from the pack
#    cache of the user it runs as by default (limits.hub_pack_restore), and this
#    gate is what proves `xerj hub pull` works against such a node.
export HOME="$WORK/home"
# XERJ_HUB_PUBKEY is deliberately NOT unset: CI never sets it, so the key compiled
# into <xerj> governs there; a local rehearsal with a throwaway key sets it.
unset XERJ_CODE_HOME XERJ_HUB_HOME XERJ_API_KEY
"$BIN" --insecure --bind 127.0.0.1 --port "$NODE_PORT" --data-dir "$WORK/node/data" \
  > "$WORK/node/node.log" 2>&1 &
PIDS+=("$!")
export XERJ_URL="http://127.0.0.1:$NODE_PORT"
export XERJ_HUB_URL="http://127.0.0.1:$HUB_PORT"
up=""
for _ in $(seq 1 120); do
  if curl -fs -m1 "$XERJ_URL/_cluster/health" >/dev/null 2>&1 \
     && curl -fs -m1 "$XERJ_HUB_URL/api/v1/catalogue.json" >/dev/null 2>&1; then
    up=1; break
  fi
  sleep 0.5
done
[ -n "$up" ] || fail "the consumer node or the catalogue server did not come up"

# 3. Pull.
"$BIN" hub pull "$SLUG" || fail "xerj hub pull $SLUG failed (exit $?)"

# 4. --def for a symbol from the pack's own defs.jsonl: the first identifier of
#    four or more characters (a stable, deterministic choice).
#
#    Not every corpus HAS identifiers. The awesome-* lists, docs-only, dataset
#    and regulation corpora are published with an EMPTY defs.jsonl (bytes:N
#    defs:0) — valid packs that answer natural-language questions but declare no
#    code symbols. For those the --def step has nothing to look up, so it is
#    SKIPPED and the pack is gated on the question check (step 5) alone. A pack
#    that DOES carry a usable symbol still MUST satisfy both --def and the
#    question (the full §12.3 gate, with the #1735 fix); only a pack with no
#    usable symbol falls back to question-only. A pack that answers nothing —
#    no defs AND no question hit — still fails (in step 5).
#    A record pack (build-pack --from-pack) may have no plain identifier: its
#    definitions are record ids (RUSTSEC-2021-0003, nst-...) and paths
#    (smallvec::SmallVec::insert_many), so the first such symbol is the fallback.
zstd -dc "$WORK/rel/$TAG/$FILE" | tar -xOf - defs.jsonl > "$WORK/defs.jsonl"
# defs.jsonl is one JSON DefRecord per line, exactly as the builder wrote it
# (xerj-hub::pack::to_jsonl, newline-terminated); an empty file is zero records.
# Count the non-blank lines, then pick the first "usable" symbol: a `.symbol`
# that is an identifier of four or more characters. "No usable symbol" covers
# every defs-0 shape — a zero-byte file, blank lines, or records whose symbols
# are all too short or not identifiers.
DEFS_RECORDS=$(grep -c '[^[:space:]]' "$WORK/defs.jsonl" || true)
DEF=$(jq -cn 'first(inputs | select(.symbol | test("^[A-Za-z_][A-Za-z0-9_]{3,}$"))) // empty' \
      "$WORK/defs.jsonl")
[ -n "$DEF" ] || DEF=$(jq -cn 'first(inputs | select(.symbol | test("^[A-Za-z0-9_][A-Za-z0-9_:.-]{3,}$"))) // empty' \
      "$WORK/defs.jsonl")
SYM=""
if [ -n "$DEF" ]; then
  SYM=$(jq -r .symbol <<<"$DEF")
  echo "defs present: $DEFS_RECORDS record(s); --def $SYM is required"
  "$BIN" code "$SLUG" --def "$SYM" --json > "$WORK/def.json" \
    || fail "xerj code $SLUG --def $SYM failed"
  # An answer's hits carry file and line, not the symbol: the definition the pack
  # recorded must be among them, answered from the local pack.
  jq -e --argjson d "$DEF" '
    .answered_from == "local_pack"
    and ([.hits[]? | select(.file == $d.file and .line == $d.line)] | length) > 0' \
    "$WORK/def.json" >/dev/null \
    || { cat "$WORK/def.json"; fail "--def $SYM did not return $(jq -r '"\(.file):\(.line)"' <<<"$DEF") from the local pack"; }
  echo "def ok: $SYM -> $(jq -r '"\(.file):\(.line)"' <<<"$DEF") (answered_from local_pack)"
else
  # defs:0 — a docs/list/dataset/regulation pack with no code symbols. The --def
  # assertion has nothing to look up, so it is skipped; the question check below
  # is the whole gate for this pack. (If it too finds nothing, step 5 fails.)
  echo "no defs ($DEFS_RECORDS record(s), none a usable symbol); question-only check"
fi

if [ -n "$SYM" ]; then
  # 4b. The same lookup BARE: no XERJ_URL, no --url — the command `hub pull` tells
  #     a user to run next. It must find the node the pack was restored into from
  #     the ledger, not fall back to localhost:9200 (where nothing listens here).
  env -u XERJ_URL "$BIN" code "$SLUG" --def "$SYM" --json > "$WORK/def-bare.json" \
    || { cat "$WORK/def-bare.json"; fail "bare xerj code $SLUG --def $SYM (no XERJ_URL) failed"; }
  jq -e --argjson d "$DEF" '
    .answered_from == "local_pack"
    and ([.hits[]? | select(.file == $d.file and .line == $d.line)] | length) > 0' \
    "$WORK/def-bare.json" >/dev/null \
    || { cat "$WORK/def-bare.json"; fail "bare --def $SYM (no XERJ_URL) did not find the pulled pack's node"; }
  echo "bare def ok: $SYM with no XERJ_URL found the pulled pack's node"
fi

# 5. One question: a word from the catalogue entry's ask_for. Words the template
#    sentence ("an agent working on ... queries this for ...") always carries say
#    nothing about the corpus; the longest remaining words are tried, at most three.
ASK=$(jq -r --arg s "$SLUG" '.corpora[] | select(.slug == $s) | .ask_for' \
      "$WORK/site/api/v1/catalogue.json")
WORDS=$(tr -cs 'A-Za-z' '\n' <<<"$ASK" | tr 'A-Z' 'a-z' | awk 'length >= 4' \
        | grep -vxE 'agent|agents|working|needing|queries|query|this|that|with|from|when|into|their|about|does' \
        | awk '{ print length, $0 }' | sort -rn -k1,1 -s | cut -d' ' -f2 | awk '!seen[$0]++' | head -n 3 || true)
# ask_for is a hand-written description, so its longest words need not occur in
# the corpus at all (ipsum-blocklist: "reputation aggregated blocklists" against
# an IP list; retext-passive/-profanities: "constructions", "rephrasings"). The
# slug's own words ("ipsum", "passive", "profanities") name what the corpus is
# and are tried after them, so such a pack is still asked a real question.
SLUG_WORDS=$(tr -cs 'A-Za-z' '\n' <<<"$SLUG" | tr 'A-Z' 'a-z' | awk 'length >= 4' || true)
WORDS=$(printf '%s\n%s\n' "$WORDS" "$SLUG_WORDS" | awk 'NF && !seen[$0]++')
[ -n "$WORDS" ] || WORDS=$SLUG
# A hit is "real and citable" when its `_source` yields a provenance path the
# way `xerj code` itself cites one: the first non-empty of ax_path / ax_file /
# path / file (xccode::passage::provenance). The line is deliberately NOT
# required: the ask_for words are prose, so they rank the pack's FULL-TEXT
# records first (file-level code docs, markdown, notebooks), whose citation
# field is `ax_path` with no per-hit line. `provenance` never invents a line,
# so a path-only hit IS a real hit a user can open and cite. LOCATED is the
# shape-agnostic form of the same test (any object with a file/path and a
# line, or an ax_path, anywhere in the answer), so a RECORD corpus
# (rust-vulns, agent-session-trajectories) addressed by `ax_path` also passes.
CITE='[.hits?.hits?[]?._source? | objects | (.ax_path // .ax_file // .path // .file // "") | select(. != "")]'
LOCATED='[.. | objects | select(((.file? // .path? // "") != "" and (.line? // .start_line? // null) != null) or (.ax_path? // "") != "")]'
hit=""
for w in $WORDS; do
  if "$BIN" code "$SLUG" "$w" --json > "$WORK/q.json" 2> "$WORK/q.err" \
     && jq -e "($CITE | length > 0) or ($LOCATED | length > 0)" "$WORK/q.json" >/dev/null 2>&1; then
    hit=$w; break
  fi
done
[ -n "$hit" ] || { cat "$WORK/q.json" "$WORK/q.err" 2>/dev/null | head -c 4000; fail "no question from ask_for or the slug ($WORDS) returned a citable hit"; }
echo "question ok: \"$hit\" -> $(jq -r "($CITE[0]) // ($LOCATED"'[0] | (.file // .path // .ax_path) + (if (.line // .start_line) then ":" + ((.line // .start_line) | tostring) else "" end))' "$WORK/q.json")"
echo "consumer e2e passed for $SLUG @ $PIN8"
