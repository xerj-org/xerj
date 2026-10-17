#!/usr/bin/env bash
# The whole-hub end-to-end gate (CORPUS_HUB_V2 spec §12.4): run before tagging the
# release that advertises Hub v2.
#
#   whole_hub_e2e.sh --bin <xerj> --out <dir> [options]
#
# PRODUCTION MODE (the gate itself; the default)
#   --hub <url>          the hub (default https://hub.xerj.org). The client verifies
#                        the catalogue and every pack against the key compiled into
#                        <xerj>; XERJ_HUB_PUBKEY is never set.
#   --repo <owner/name>  where the pack releases live (default xerj-org/xerj); the
#                        catalogue's corpus count must equal the number of corpora
#                        with a published `pack-<slug>-<pin8>` release there.
#   --expect <n>         use n instead of counting releases (no GitHub API call).
#
# REHEARSAL MODE (before a signed catalogue is deployed)
#   --rehearse <tags>    comma list of published pack release tags. Their assets are
#                        downloaded and their REAL signatures checked against the
#                        embedded key (--pubkey); then, because the real seed is not
#                        available off CI, the packs are re-signed with a throwaway
#                        key, a catalogue of exactly those packs is built with
#                        `xerj hub build-catalogue` and served on loopback, and the
#                        client runs with XERJ_HUB_PUBKEY set to the throwaway key.
#   --registry <dir>     the hub registry (tools/xerj-code/hub on corpus-hub), and
#   --backlog <file>     its backlog-100.json: build-catalogue's inputs.
#   --assets <dir>       take the archives + .sig from here instead of downloading.
#
# BOTH MODES
#   --pubkey <hex>       the key the client embeds (default: HUB_PUBKEY_HEX from
#                        engine/crates/xerj-common/src/hub.rs in this checkout).
#   --questions <file>   the lz4 benchmark questions (default: e2e/lz4-questions.json).
#   --only <slugs>       restrict the per-corpus checks (a partial run never passes).
#   --batch <n>          corpora per fresh HOME + node (default 20).
#   --no-sandbox         run without bubblewrap (still an empty HOME and a cleared
#                        environment per phase). Default: every client phase runs in
#                        benchmarks/hub-pov/sandbox.sh (own PID namespace, read-only
#                        filesystem, a fresh HOME).
#   --run-id <id>        names the report rows (default: e2e-<UTC timestamp>).
#   --port <n>           loopback port for the local stand-in hub (default 19530;
#                        <n>+1 is used as a port nothing listens on).
#
# Some checks cannot use the real key in any mode: an over-the-cap entry and a
# tampered pack need a catalogue that says something the real one does not, so
# they run against copies of the verified catalogue edited and re-signed with a
# throwaway key (the `signatures` column of every check says which key it ran
# against). The tampered-CATALOGUE check uses the hub's own catalogue and
# signature, so in production mode it is a real-key check.
#
# OUTPUT (in --out)
#   checks.jsonl   one {check, result, signatures, detail} line per gate check
#   rows/*.json    one backfill-report row per catalogued corpus (hub_packs.py
#                  `report --rows-dir` folds them into benchmarks/hub-backfill/)
#   GATE.md        the human summary; homes/<phase>/ every phase's HOME and logs
# Exit 0 only when every check passed and every catalogued corpus passed.
#
# Never point --hub, XERJ_URL or anything else here at a node you care about: each
# client phase starts its own node (the automatic node, port >= 9300) in its own
# HOME, and stops exactly that node when the phase ends.
set -euo pipefail

SELF=$(realpath "$0")
HERE=$(dirname "$SELF")

# The messages each path must print, copied from the client (the source of truth
# until the user docs carry them; spec §5 documents only the hosted-off notice).
MSG_HOSTED_OFF="hosted answers are off in git repositories; xerj hub config hosted on" # ask.rs HOSTED_OFF_NOTICE
MSG_NO_HOSTED_CLIENT="hosted answers were requested but this build has no hosted client" # ask.rs NO_HOSTED_CLIENT
MSG_NO_CACHE="no cached copy to fall back to"                                            # catalogue.rs load_catalogue
MSG_KEEP_CACHE="keeping the cached copy"                                                 # catalogue.rs load_catalogue
MSG_HTML="not a signed catalogue; no catalogue is served at this address"               # catalogue.rs fetch_and_store
MSG_OFFLINE_NOCAT="offline and no usable cached catalogue"                               # catalogue.rs load_catalogue
MSG_OFFLINE_NOPACK="--offline forbids downloading it"                                    # ask.rs ask() step 5
MSG_OVERCAP_ASK="MB limit for pulling a pack, so it is not downloaded"                  # ask.rs ask() pull policy
MSG_OVERCAP_PULL="over the 52428800-byte auto-pull cap; not downloaded"                 # cli.rs plan_pull_entry
MSG_TAMPER_SHA="pack sha256 mismatch"                                                    # pack.rs read_and_verify_pack
MSG_TAMPER_SIG="pack signature"                                                          # pack.rs read_and_verify_pack
MSG_TAMPER_CAT="catalogue signature did not verify"                                      # catalogue.rs fetch_and_store
AUTO_PULL_MAX_BYTES=52428800                                                             # xerj-common hub.rs

# ═════════════════════════════════════════════════════════════════════════════
# INNER: one client phase, run inside the sandbox with a fresh HOME. Everything it
# learns goes to $HOME/result (the only writable place in the jail).
# ═════════════════════════════════════════════════════════════════════════════
if [ "${1:-}" = "--inner" ]; then
  PHASE=$2; shift 2
  R="$HOME/result"; mkdir -p "$R/logs" "$R/rows" "$HOME/work"
  cd "$HOME/work"
  N=0
  stop_node() { "$BIN" hub stop >/dev/null 2>&1 || true; }
  trap stop_node EXIT

  # run <label> <cmd...>: stdout/stderr to $R/logs/<n>-<label>.{out,err}; sets RC,
  # OUTF, ERRF. Never aborts the phase.
  run() {
    local label=$1; shift
    N=$((N + 1))
    OUTF="$R/logs/$(printf %02d $N)-$label.out"; ERRF="${OUTF%.out}.err"
    printf '%q ' "$@" > "${OUTF%.out}.cmd"
    RC=0
    timeout "${E2E_CMD_TIMEOUT:-900}" "$@" > "$OUTF" 2> "$ERRF" || RC=$?
    echo "$RC" > "${OUTF%.out}.rc"
  }
  # check <name> <ok:0|1> <detail>
  check() {
    local res=fail; [ "$2" = 0 ] && res=pass
    jq -cn --arg c "$1" --arg r "$res" --arg s "$E2E_SIG" --arg d "$3" --arg p "$PHASE" \
      '{check: $c, result: $r, signatures: $s, phase: $p, detail: $d}' >> "$R/checks.jsonl"
    echo "  [$res] $1: $3"
  }
  has() { grep -qF -- "$1" "$2"; }
  # Nothing of <slug> may have been extracted or registered.
  nothing_extracted() {
    local slug=$1 found
    found=$(find "$HOME/.xerj/hub/packs" \( -name pack.json -o -name defs.jsonl -o -name SHA256SUMS \
              -o -name '*.tmp' -o -name snapshot \) 2>/dev/null | head -n 3 || true)
    [ -z "$found" ] && [ ! -e "$HOME/.xerj-code/state/$slug.json" ]
  }
  last_err() { tr '\n' ' ' < "$ERRF" | tr -s ' ' | cut -c1-200; }

  case "$PHASE" in
    sync)
      run sync "$BIN" hub sync
      if [ "$RC" = 0 ] && grep -q 'catalogue refreshed' "$OUTF"; then
        check catalogue-loads-and-verifies 0 "$(head -n 1 "$OUTF")"
      else
        check catalogue-loads-and-verifies 1 "xerj hub sync exit $RC: $(last_err)"
      fi
      ;;

    corpora)
      # §12.3 again, for each slug: pull, one --def, one question.
      for slug in "$@"; do
        row() { # result stage error [detail]
          local cat="$HOME/.xerj/hub/catalogue.json" base
          base=$(jq -cn --arg run "$E2E_RUN_ID" --arg date "$E2E_DATE" --arg slug "$slug" \
               --arg result "$1" --arg stage "$2" --arg error "$3" --arg detail "${4:-}" \
               '{run: $run, date: $date, kind: "e2e", slug: $slug, pin8: "", bytes: null, defs: null,
                 result: $result, tag: "", stage: $stage, error: $error, detail: $detail}')
          # pin8, bytes and the release tag from the catalogue entry, when there is one.
          jq -c --arg s "$slug" --argjson r "$base" '
            ([.corpora[]? | select(.slug == $s)][0]) as $e
            | if $e == null then $r else $r + {pin8: ($e.pin_sha[0:8]), bytes: $e.pack_bytes,
                tag: (($e.pack_url | capture("/(?<t>[^/]+)/[^/]+$").t) // "")} end' \
            "$cat" > "$R/rows/$slug.json" 2>/dev/null || echo "$base" > "$R/rows/$slug.json"
          echo "  $slug: $1${2:+ at $2}${3:+ ($3)}${4:+ $4}"
        }
        run "pull-$slug" "$BIN" hub pull "$slug"
        if [ "$RC" != 0 ]; then row failed pull "exit $RC: $(last_err)"; continue; fi
        pin8=$(jq -r --arg s "$slug" '.corpora[] | select(.slug == $s) | .pin_sha[0:8]' "$HOME/.xerj/hub/catalogue.json")
        defs="$HOME/.xerj/hub/packs/$slug/$pin8/defs.jsonl"
        DEF=$(jq -cn 'first(inputs | select(.symbol | test("^[A-Za-z_][A-Za-z0-9_]{3,}$"))) // empty' "$defs" 2>/dev/null || true)
        if [ -z "$DEF" ]; then row failed def "no identifier in the pack's defs.jsonl"; continue; fi
        SYM=$(jq -r .symbol <<<"$DEF")
        def_ok() { [ "$RC" = 0 ] && jq -e --argjson d "$DEF" '.answered_from == "local_pack"
              and ([.hits[]? | select(.file == $d.file and .line == $d.line)] | length) > 0' "$OUTF" >/dev/null 2>&1; }
        run "def-$slug" "$BIN" code "$slug" --def "$SYM" --json
        if ! def_ok; then
          why="--def $SYM did not return $(jq -r '"\(.file):\(.line)"' <<<"$DEF") from the local pack (exit $RC: $(last_err))"
          # Tell a broken pack from a client that cannot find the node the pull started
          # (`xerj code` resolves --url / XERJ_URL / localhost:9200 only): ask the
          # automatic node directly. The row still fails: a clean machine has no XERJ_URL.
          port=$(cat "$HOME/.xerj/hub/node.port" 2>/dev/null || true)
          if [ -n "$port" ]; then
            run "def-$slug-autonode" env XERJ_URL="http://127.0.0.1:$port" "$BIN" code "$slug" --def "$SYM" --json
            def_ok && why="$why; the same call answers with XERJ_URL=<the automatic node, port $port>: xerj code does not find the node xerj hub pull started"
          fi
          row failed def "$why"; continue
        fi
        ASK=$(jq -r --arg s "$slug" '.corpora[] | select(.slug == $s) | .ask_for' "$HOME/.xerj/hub/catalogue.json")
        WORDS=$(tr -cs 'A-Za-z' '\n' <<<"$ASK" | tr 'A-Z' 'a-z' | awk 'length >= 4' \
          | grep -vxE 'agent|agents|working|needing|queries|query|this|that|with|from|when|into|their|about|does' \
          | awk '{ print length, $0 }' | sort -rn -k1,1 -s | cut -d' ' -f2 | awk '!seen[$0]++' | head -n 3 || true)
        [ -n "$WORDS" ] || WORDS=$slug
        hit=""
        for w in $WORDS; do
          run "q-$slug" "$BIN" code "$slug" "$w" --json
          if [ "$RC" = 0 ] && jq -e '[.. | objects | select((.file? // .path? // "") != "" and (.line? // .start_line? // null) != null)] | length > 0' "$OUTF" >/dev/null 2>&1; then
            hit=$w; break
          fi
        done
        if [ -z "$hit" ]; then row failed question "no word of ask_for ($(echo $WORDS)) returned a file:line hit"; continue; fi
        loc=$(jq -r '[.. | objects | select((.file? // .path? // "") != "" and (.line? // .start_line? // null) != null)][0] | "\(.file // .path):\(.line // .start_line)"' "$OUTF")
        row "e2e passed" "" "" "--def $SYM -> $(jq -r '"\(.file):\(.line)"' <<<"$DEF"); \"$hit\" -> $loc"
      done
      ;;

    questions)
      # Spec step 2 "done when", asked the step-5 way: `xerj "<question>"`, no corpus
      # named, on a machine with no pack yet (the first question pulls it).
      total=$(jq '.tasks | length' "$E2E_QFILE"); ok=0; bad=""
      for i in $(seq 0 $((total - 1))); do
        q=$(jq -r ".tasks[$i].q" "$E2E_QFILE"); id=$(jq -r ".tasks[$i].id" "$E2E_QFILE")
        re=$(jq -r ".tasks[$i].re | join(\"|\")" "$E2E_QFILE")
        run "ask-$id" "$BIN" "$q" --json
        val=$(jq -r '[.hits[]? | .value // empty][0] // ""' "$OUTF" 2>/dev/null || true)
        where=$(jq -r '.hits[0] | "\(.file):\(.line)"' "$OUTF" 2>/dev/null || true)
        corpus=$(jq -r '.corpus // ""' "$OUTF" 2>/dev/null || true)
        # The registered checker's semantics: the regex is searched for in the answer.
        if [ "$RC" = 0 ] && [ "$corpus" = "$E2E_QCORPUS" ] && [[ $val =~ ($re) ]]; then
          ok=$((ok + 1)); echo "    $id ok: $corpus $where = $val"
        else
          note="exit $RC, corpus '$corpus', value '$val'"
          if [ "$RC" = 4 ]; then
            # Ambiguous: name the candidates, and say whether naming the corpus answers.
            note="exit 4, ambiguous between $(cut -d' ' -f1 "$OUTF" | tr '\n' ' ' | sed 's/ $//')"
            run "ask-$id-named" "$BIN" ask --corpus "$E2E_QCORPUS" "$q" --json
            v2=$(jq -r '[.hits[]? | .value // empty][0] // ""' "$OUTF" 2>/dev/null || true)
            [ "$RC" = 0 ] && [[ $v2 =~ ($re) ]] && note="$note; with --corpus $E2E_QCORPUS it answers $v2"
          fi
          bad="$bad $id($note)"; echo "    $id FAILED: $note"
        fi
      done
      check routed-questions "$([ "$ok" = "$total" ] && echo 0 || echo 1)" \
        "$ok/$total $E2E_QCORPUS questions routed with no corpus named and answered with the registered value${bad:+; failed:$bad}"

      q1=$(jq -r '.tasks[0].q' "$E2E_QFILE")
      # Hosted off: inside a git work tree `--hosted auto` resolves off, said once.
      mkdir -p "$HOME/work/repo"; git -C "$HOME/work/repo" init -q
      cd "$HOME/work/repo"
      run hosted-off-1 "$BIN" "$q1"; rc1=$RC; e1=$ERRF
      run hosted-off-2 "$BIN" "$q1"; rc2=$RC; e2=$ERRF
      cd "$HOME/work"
      if [ "$rc1" = 0 ] && has "$MSG_HOSTED_OFF" "$e1" && [ "$rc2" = 0 ] && ! has "$MSG_HOSTED_OFF" "$e2"; then
        check hosted-off 0 "in a git work tree: '$MSG_HOSTED_OFF' printed on the first question only; both answered"
      else
        check hosted-off 1 "exits $rc1/$rc2; notice first=$(has "$MSG_HOSTED_OFF" "$e1" && echo yes || echo no) second=$(has "$MSG_HOSTED_OFF" "$e2" && echo yes || echo no)"
      fi
      # Hosted on, but no hosted node exists (this release ships without one).
      run hosted-on "$BIN" ask --hosted on "$q1"
      if [ "$RC" = 0 ] && has "$MSG_NO_HOSTED_CLIENT" "$ERRF"; then
        check hosted-unavailable-answer 0 "--hosted on: '$MSG_NO_HOSTED_CLIENT…', answered from the local pack"
      else
        check hosted-unavailable-answer 1 "exit $RC: $(last_err)"
      fi
      # Hub unreachable / an HTML 200 with a stale cached catalogue: keep the cache, answer.
      cat_file="$HOME/.xerj/hub/catalogue.json"
      touch -d '3 days ago' "$cat_file"
      run stale-unreachable env XERJ_HUB_URL="$E2E_DEAD_HUB" "$BIN" "$q1"
      if [ "$RC" = 0 ] && has "$MSG_KEEP_CACHE" "$ERRF"; then
        check hub-unreachable-cached 0 "hub unreachable, catalogue cached: '…$MSG_KEEP_CACHE', answered"
      else
        check hub-unreachable-cached 1 "exit $RC: $(last_err)"
      fi
      touch -d '3 days ago' "$cat_file"
      run stale-html env XERJ_HUB_URL="$E2E_HTML_HUB" "$BIN" "$q1"
      if [ "$RC" = 0 ] && has "$MSG_HTML" "$ERRF" && has "$MSG_KEEP_CACHE" "$ERRF"; then
        check hub-html-200-cached 0 "HTML 200 for the catalogue, catalogue cached: '$MSG_HTML' + '$MSG_KEEP_CACHE', answered"
      else
        check hub-html-200-cached 1 "exit $RC: $(last_err)"
      fi
      # --offline with the pack cached: answers, and makes no request at all.
      run offline-cached env XERJ_HUB_URL="$E2E_PROBE_HUB" "$BIN" "$q1" --offline --json
      if [ "$RC" = 0 ] && [ "$(jq -r .answered_from "$OUTF" 2>/dev/null)" = local_pack ] \
         && ! grep -q '/offline-probe' "$E2E_HTTP_LOG"; then
        check offline-with-pack 0 "--offline with the pack cached: answered from the local pack, no request made"
      else
        check offline-with-pack 1 "exit $RC; requests: $(grep -c '/offline-probe' "$E2E_HTTP_LOG" || true): $(last_err)"
      fi
      ;;

    unreachable)
      run pull env XERJ_HUB_URL="$E2E_DEAD_HUB" "$BIN" hub pull "$E2E_TARGET"; p=$RC; pe=$ERRF
      run ask env XERJ_HUB_URL="$E2E_DEAD_HUB" "$BIN" ask --corpus "$E2E_TARGET" "$E2E_TARGET"
      if [ "$p" != 0 ] && has "cannot load catalogue" "$pe" && has "$MSG_NO_CACHE" "$pe" \
         && [ "$RC" = 1 ] && has "cannot load the hub catalogue" "$ERRF" && nothing_extracted "$E2E_TARGET"; then
        check hub-unreachable 0 "empty HOME, nothing listening: pull exit $p and ask exit 1, both '…$MSG_NO_CACHE'; nothing extracted"
      else
        check hub-unreachable 1 "pull exit $p, ask exit $RC: $(last_err)"
      fi
      ;;

    html)
      run pull env XERJ_HUB_URL="$E2E_HTML_HUB" "$BIN" hub pull "$E2E_TARGET"; p=$RC; pe=$ERRF
      run ask env XERJ_HUB_URL="$E2E_HTML_HUB" "$BIN" ask --corpus "$E2E_TARGET" "$E2E_TARGET"
      if [ "$p" != 0 ] && has "$MSG_HTML" "$pe" && [ "$RC" = 1 ] && has "$MSG_HTML" "$ERRF" \
         && [ ! -e "$HOME/.xerj/hub/catalogue.json" ] && nothing_extracted "$E2E_TARGET"; then
        check hub-html-200 0 "empty HOME, the site's HTML page (200) as the catalogue: '$MSG_HTML' from pull (exit $p) and ask (exit 1); nothing cached or extracted"
      else
        check hub-html-200 1 "pull exit $p, ask exit $RC: $(last_err)"
      fi
      ;;

    offline)
      run offline-empty "$BIN" ask --offline --corpus "$E2E_TARGET" "$E2E_TARGET"
      if [ "$RC" = 1 ] && has "$MSG_OFFLINE_NOCAT" "$ERRF"; then
        check offline-no-catalogue 0 "--offline, empty HOME: exit 1, '$MSG_OFFLINE_NOCAT'"
      else
        check offline-no-catalogue 1 "exit $RC: $(last_err)"
      fi
      run sync "$BIN" hub sync
      run offline-nopack env XERJ_HUB_URL="$E2E_PROBE_HUB" "$BIN" ask --offline --corpus "$E2E_TARGET" "$E2E_TARGET"
      if [ "$RC" = 1 ] && has "$MSG_OFFLINE_NOPACK" "$ERRF" && has "xerj hub pull $E2E_TARGET" "$ERRF" \
         && nothing_extracted "$E2E_TARGET" && ! grep -q '/offline-probe' "$E2E_HTTP_LOG"; then
        check offline-no-pack 0 "--offline, catalogue cached, no pack: exit 1, '$MSG_OFFLINE_NOPACK' with the pull hint; no request, nothing extracted"
      else
        check offline-no-pack 1 "exit $RC: $(last_err)"
      fi
      ;;

    overcap)
      run sync "$BIN" hub sync
      run ask "$BIN" ask --corpus "$E2E_TARGET" "$E2E_TARGET"; a=$RC; ae=$ERRF
      run pull "$BIN" hub pull "$E2E_TARGET"
      if [ "$a" = 1 ] && has "$MSG_OVERCAP_ASK" "$ae" && [ "$RC" != 0 ] && has "$MSG_OVERCAP_PULL" "$ERRF" \
         && nothing_extracted "$E2E_TARGET"; then
        check over-the-cap 0 "pack_bytes $((AUTO_PULL_MAX_BYTES + 1)): ask exit 1 '$(head -c 160 "$ae")', pull exit $RC '…$MSG_OVERCAP_PULL'; nothing downloaded"
      else
        check over-the-cap 1 "sync/ask/pull: ask exit $a, pull exit $RC: $(last_err)"
      fi
      ;;

    tamper-sha|tamper-sig)
      want=$MSG_TAMPER_SHA; [ "$PHASE" = tamper-sig ] && want=$MSG_TAMPER_SIG
      run sync "$BIN" hub sync
      run pull "$BIN" hub pull "$E2E_TARGET"
      if [ "$RC" = 3 ] && has "pack verification failed" "$ERRF" && has "$want" "$ERRF" && nothing_extracted "$E2E_TARGET"; then
        check "tampered-pack-${PHASE#tamper-}" 0 "one flipped byte: pull exit 3, '$want'; nothing extracted"
      else
        check "tampered-pack-${PHASE#tamper-}" 1 "pull exit $RC: $(last_err)"
      fi
      ;;

    tamper-catalogue)
      run sync "$BIN" hub sync; s=$RC; se=$ERRF
      run pull "$BIN" hub pull "$E2E_TARGET"; p=$RC
      run ask "$BIN" ask --corpus "$E2E_TARGET" "$E2E_TARGET"
      if [ "$s" != 0 ] && has "$MSG_TAMPER_CAT" "$se" && [ "$p" != 0 ] && [ "$RC" != 0 ] \
         && [ ! -e "$HOME/.xerj/hub/catalogue.json" ] && nothing_extracted "$E2E_TARGET"; then
        check tampered-catalogue 0 "one flipped byte: sync exit $s '$MSG_TAMPER_CAT', pull exit $p, ask exit $RC; nothing cached or extracted"
      else
        check tampered-catalogue 1 "sync exit $s, pull exit $p, ask exit $RC: $(tr '\n' ' ' < "$se" | cut -c1-200)"
      fi
      ;;

    *) echo "unknown phase $PHASE" >&2; exit 2 ;;
  esac
  exit 0
fi

# ═════════════════════════════════════════════════════════════════════════════
# OUTER: prepare, run each phase in a fresh sandbox, fold the results.
# ═════════════════════════════════════════════════════════════════════════════
usage() { sed -n '2,60p' "$SELF" >&2; exit 2; }
BIN="" OUT="" HUB="https://hub.xerj.org" REPO="xerj-org/xerj" EXPECT="" REHEARSE="" REGISTRY="" BACKLOG=""
ASSETS="" PUBKEY="" QFILE="$HERE/e2e/lz4-questions.json" ONLY="" BATCH=20 SANDBOX=1 RUN_ID="" PORT=19530
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --hub) HUB=${2%/}; shift 2 ;;
    --repo) REPO=$2; shift 2 ;;
    --expect) EXPECT=$2; shift 2 ;;
    --rehearse) REHEARSE=$2; shift 2 ;;
    --registry) REGISTRY=$2; shift 2 ;;
    --backlog) BACKLOG=$2; shift 2 ;;
    --assets) ASSETS=$2; shift 2 ;;
    --pubkey) PUBKEY=$2; shift 2 ;;
    --questions) QFILE=$2; shift 2 ;;
    --only) ONLY=$2; shift 2 ;;
    --batch) BATCH=$2; shift 2 ;;
    --no-sandbox) SANDBOX=0; shift ;;
    --run-id) RUN_ID=$2; shift 2 ;;
    --port) PORT=$2; shift 2 ;;
    -h|--help) usage ;;
    *) echo "unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$BIN" ] && [ -n "$OUT" ] || usage
for t in jq curl zstd tar openssl xxd python3 git timeout sha256sum; do
  command -v "$t" >/dev/null || { echo "whole_hub_e2e.sh: '$t' is required" >&2; exit 2; }
done
BIN=$(realpath "$BIN"); QFILE=$(realpath "$QFILE")
REPO_ROOT=$(cd "$HERE/../.." && pwd)
SANDBOX_SH="$REPO_ROOT/benchmarks/hub-pov/sandbox.sh"
ED="$HERE/ed25519.sh"
if [ -z "$PUBKEY" ]; then
  PUBKEY=$(sed -n 's/^pub const HUB_PUBKEY_HEX: &str = "\([0-9a-f]\{64\}\)";.*/\1/p' \
           "$REPO_ROOT/engine/crates/xerj-common/src/hub.rs")
fi
[[ $PUBKEY =~ ^[0-9a-f]{64}$ ]] || { echo "no hub public key (--pubkey)" >&2; exit 2; }
case "$OUT" in /tmp|/tmp/*) echo "--out must not be under /tmp: the sandbox mounts a fresh /tmp, so the client could not read file:// pack URLs there" >&2; exit 2 ;; esac
mkdir -p "$OUT"; OUT=$(realpath "$OUT")
[ -z "$(ls -A "$OUT")" ] || { echo "--out $OUT is not empty" >&2; exit 2; }
# The questions must start outside any git work tree (`--hosted auto` turns off in one).
if git -C "$OUT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "--out $OUT is inside a git work tree; use a directory outside any repository" >&2; exit 2
fi
mkdir -p "$OUT/logs" "$OUT/rows" "$OUT/homes" "$OUT/site" "$OUT/lab" "$OUT/sandbox-cfg"
RUN_ID=${RUN_ID:-e2e-$(date -u +%Y%m%dT%H%M%SZ)}
DATE=$(date -u +%F)
CHECKS="$OUT/checks.jsonl"; : > "$CHECKS"
DEAD_PORT=$((PORT + 1))

check() { # name ok detail [signatures]
  local res=fail; [ "$2" = 0 ] && res=pass
  jq -cn --arg c "$1" --arg r "$res" --arg s "${4:-$SIG}" --arg d "$3" \
    '{check: $c, result: $r, signatures: $s, phase: "outer", detail: $d}' >> "$CHECKS"
  echo "  [$res] $1: $3"
}
PIDS=()
cleanup() { for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT

if curl -s -m 2 -o /dev/null "http://127.0.0.1:$DEAD_PORT/"; then
  echo "port $DEAD_PORT answers; pick another --port" >&2; exit 2
fi
# The stand-in hub must be OURS: a sentinel only this run wrote proves it (another
# server already on the port would otherwise answer every check).
SENTINEL=$(openssl rand -hex 16); echo "$SENTINEL" > "$OUT/site/sentinel"
python3 -I "$HERE/e2e/static_hub.py" --root "$OUT/site" --port "$PORT" 2> "$OUT/logs/hub-http.log" &
PIDS+=("$!")
LOCAL="http://127.0.0.1:$PORT"
up=""
for _ in $(seq 1 50); do
  [ "$(curl -s -m 1 "$LOCAL/sentinel" 2>/dev/null)" = "$SENTINEL" ] && { up=1; break; }
  kill -0 "${PIDS[-1]}" 2>/dev/null || break
  sleep 0.2
done
[ -n "$up" ] || { cat "$OUT/logs/hub-http.log" >&2; echo "the stand-in hub did not start on port $PORT (in use?); pass --port" >&2; exit 2; }

umask 077; openssl rand -hex 32 > "$OUT/lab/seed"; umask 022
LAB_PUB=$(bash "$ED" derive "$OUT/lab/seed")
sign() { bash "$ED" sign "$OUT/lab/seed" "$1" > "$2"; }
flip_byte() { python3 -I -c 'import sys; b=bytearray(open(sys.argv[1],"rb").read()); i=len(b)//2; b[i]^=1; open(sys.argv[2],"wb").write(b)' "$1" "$2"; }

# ── the hub under test ──────────────────────────────────────────────────────
if [ -n "$REHEARSE" ]; then
  MODE=rehearsal; SIG=lab; CLIENT_KEY=$LAB_PUB
  [ -n "$REGISTRY" ] && [ -n "$BACKLOG" ] || { echo "--rehearse needs --registry and --backlog" >&2; exit 2; }
  echo "== rehearsal: packs ${REHEARSE//,/ } re-signed with a throwaway key, served on $LOCAL/main"
  mkdir -p "$OUT/rel" "$OUT/lab/packs"; : > "$OUT/lab/tags.tsv"
  real_ok=0; real_bad=""; n_tags=0; EXPECT_SLUGS=""
  for tag in ${REHEARSE//,/ }; do
    [[ $tag =~ ^pack-(.+)-([0-9a-f]{8})(-r[0-9]+)?$ ]] || { echo "not a pack tag: $tag" >&2; exit 2; }
    slug=${BASH_REMATCH[1]}; pin8=${BASH_REMATCH[2]}; file="$slug-$pin8.hubpack.tar.zst"
    n_tags=$((n_tags + 1)); EXPECT_SLUGS="$EXPECT_SLUGS $slug"
    mkdir -p "$OUT/rel/$tag" "$OUT/assets/$tag"
    if [ -n "$ASSETS" ]; then
      cp "$ASSETS/$file" "$ASSETS/$file.sig" "$OUT/assets/$tag/"
    else
      for f in "$file" "$file.sig"; do
        curl -fsSL --retry 3 -o "$OUT/assets/$tag/$f" "https://github.com/$REPO/releases/download/$tag/$f"
      done
    fi
    if bash "$ED" verify "$PUBKEY" "$OUT/assets/$tag/$file" "$OUT/assets/$tag/$file.sig" >/dev/null 2>&1; then
      real_ok=$((real_ok + 1))
    else
      real_bad="$real_bad $tag"
    fi
    cp "$OUT/assets/$tag/$file" "$OUT/rel/$tag/$file"
    sign "$OUT/rel/$tag/$file" "$OUT/rel/$tag/$file.sig"
    ln -sf "$OUT/rel/$tag/$file" "$OUT/lab/packs/$file"; ln -sf "$OUT/rel/$tag/$file.sig" "$OUT/lab/packs/$file.sig"
    printf '%s\t%s\t%s\n' "$slug" "$pin8" "$tag" >> "$OUT/lab/tags.tsv"
  done
  check published-pack-signatures-real-key "$([ -z "$real_bad" ] && echo 0 || echo 1)" \
    "$real_ok/$n_tags published archives verify against the embedded key $PUBKEY${real_bad:+; FAILED:$real_bad}" real
  mkdir -p "$OUT/site/main/api/v1"
  XERJ_HUB_PUBKEY=$LAB_PUB "$BIN" hub build-catalogue --registry "$REGISTRY" --backlog "$BACKLOG" \
    --packs "$OUT/lab/packs" --tags "$OUT/lab/tags.tsv" --release-base "file://$OUT/rel" \
    --out "$OUT/site/main/api/v1/catalogue.json" --sign-key "$OUT/lab/seed" > "$OUT/logs/build-catalogue.log" 2>&1 \
    || { cat "$OUT/logs/build-catalogue.log"; echo "build-catalogue failed" >&2; exit 1; }
  HUB_UNDER_TEST="$LOCAL/main"; EXPECT=$n_tags
else
  MODE=production; SIG=real; CLIENT_KEY=""
  HUB_UNDER_TEST=$HUB
  echo "== production: $HUB, verified against the key compiled into $BIN"
  if [ -z "$EXPECT" ]; then
    auth=(); [ -n "${GH_TOKEN:-}" ] && auth=(-H "Authorization: Bearer $GH_TOKEN")
    : > "$OUT/lab/releases.jsonl"
    for page in $(seq 1 50); do
      body=$(curl -fsSL "${auth[@]}" "https://api.github.com/repos/$REPO/releases?per_page=100&page=$page")
      [ "$(jq length <<<"$body")" = 0 ] && break
      jq -c '.[]' <<<"$body" >> "$OUT/lab/releases.jsonl"
    done
    EXPECT_SLUGS=$(jq -r 'select(.draft | not) | select(.tag_name | test("^pack-.+-[0-9a-f]{8}(-r[0-9]+)?$"))
        | .assets[].name | capture("^(?<s>.+)-[0-9a-f]{8}[.]hubpack[.]tar[.]zst$").s' "$OUT/lab/releases.jsonl" | sort -u)
    EXPECT=$(wc -w <<<"$EXPECT_SLUGS")
  fi
fi

# ── one phase = one fresh HOME in one sandbox ───────────────────────────────
phase() { # phase-name home-name [env...] -- [args...]
  local name=$1 home="$OUT/homes/$2"; shift 2
  local envs=()
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do envs+=("$1"); shift; done
  [ "${1:-}" = "--" ] && shift
  mkdir -p "$home"
  local base=(env BIN="$BIN" E2E_SIG="$SIG" E2E_RUN_ID="$RUN_ID" E2E_DATE="$DATE"
    XERJ_HUB_URL="$HUB_UNDER_TEST" XERJ_URL="http://127.0.0.1:$DEAD_PORT"
    E2E_TARGET="${TARGET:-}" E2E_QFILE="$QFILE" E2E_QCORPUS="${QCORPUS:-}"
    E2E_DEAD_HUB="http://127.0.0.1:$DEAD_PORT" E2E_HTML_HUB="$LOCAL/html"
    E2E_PROBE_HUB="$LOCAL/offline-probe" E2E_HTTP_LOG="$OUT/logs/hub-http.log"
    MSG_HOSTED_OFF="$MSG_HOSTED_OFF" MSG_NO_HOSTED_CLIENT="$MSG_NO_HOSTED_CLIENT" MSG_NO_CACHE="$MSG_NO_CACHE"
    MSG_KEEP_CACHE="$MSG_KEEP_CACHE" MSG_HTML="$MSG_HTML" MSG_OFFLINE_NOCAT="$MSG_OFFLINE_NOCAT"
    MSG_OFFLINE_NOPACK="$MSG_OFFLINE_NOPACK" MSG_OVERCAP_ASK="$MSG_OVERCAP_ASK"
    MSG_OVERCAP_PULL="$MSG_OVERCAP_PULL" MSG_TAMPER_SHA="$MSG_TAMPER_SHA" MSG_TAMPER_SIG="$MSG_TAMPER_SIG"
    MSG_TAMPER_CAT="$MSG_TAMPER_CAT" AUTO_PULL_MAX_BYTES="$AUTO_PULL_MAX_BYTES")
  [ -n "$CLIENT_KEY" ] && base+=(XERJ_HUB_PUBKEY="$CLIENT_KEY")
  base+=("${envs[@]}")
  echo "-- phase $name (HOME $home)"
  local rc=0
  if [ "$SANDBOX" = 1 ]; then
    CLAUDE_CONFIG_DIR="$OUT/sandbox-cfg" bash "$SANDBOX_SH" --home "$home" -- \
      "${base[@]}" bash "$SELF" --inner "$name" "$@" 2> >(grep -v '^sandbox-home:' >&2) || rc=$?
  else
    env -i HOME="$home" PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin LANG=C.UTF-8 \
      "${base[@]}" bash "$SELF" --inner "$name" "$@" || rc=$?
  fi
  [ -f "$home/result/checks.jsonl" ] && cat "$home/result/checks.jsonl" >> "$CHECKS"
  [ "$rc" = 0 ] || check "phase-$name" 1 "the phase itself exited $rc"
}
if [ "$SANDBOX" = 1 ]; then
  command -v bwrap >/dev/null || { echo "bwrap is not installed: install bubblewrap, or pass --no-sandbox" >&2; exit 2; }
  echo "== every client phase runs in $SANDBOX_SH (own PID namespace, read-only /, fresh HOME)"
else
  echo "== --no-sandbox: phases run with a cleared environment and a fresh HOME only"
fi

# 1. The catalogue loads and verifies; its corpus count equals the published packs.
phase sync sync
CAT="$OUT/homes/sync/.xerj/hub/catalogue.json"
if [ ! -s "$CAT" ]; then
  echo "no verified catalogue: nothing else can be checked" >&2
else
  cp "$CAT" "$OUT/catalogue.json"; cp "$CAT.sig" "$OUT/catalogue.json.sig" 2>/dev/null || true
  key=$PUBKEY; [ "$MODE" = rehearsal ] && key=$LAB_PUB
  if bash "$ED" verify "$key" "$OUT/catalogue.json" "$OUT/catalogue.json.sig" >/dev/null 2>&1; then
    check catalogue-signature-independent 0 "the cached catalogue verifies under $key with openssl (ed25519.sh), not only inside the client"
  else
    check catalogue-signature-independent 1 "the cached catalogue does NOT verify under $key with openssl"
  fi
  got=$(jq '.corpora | length' "$OUT/catalogue.json")
  CAT_SLUGS=$(jq -r '.corpora[].slug' "$OUT/catalogue.json" | sort -u)
  detail="catalogue lists $got corpora; $EXPECT corpora have a published pack"
  if [ -n "${EXPECT_SLUGS:-}" ]; then
    miss=$(comm -13 <(echo "$CAT_SLUGS") <(tr ' ' '\n' <<<"$EXPECT_SLUGS" | grep . | sort -u) | tr '\n' ' ')
    extra=$(comm -23 <(echo "$CAT_SLUGS") <(tr ' ' '\n' <<<"$EXPECT_SLUGS" | grep . | sort -u) | tr '\n' ' ')
    [ -n "$miss" ] && detail="$detail; published but not catalogued: $miss"
    [ -n "$extra" ] && detail="$detail; catalogued without a published pack: $extra"
  fi
  check catalogue-count-equals-published "$([ "$got" = "$EXPECT" ] && [ -z "${miss:-}${extra:-}" ] && echo 0 || echo 1)" "$detail"

  # 2. Every catalogued corpus: pull + one --def + one question (§12.3 again).
  SLUGS=$CAT_SLUGS
  if [ -n "$ONLY" ]; then SLUGS=$(tr ',' '\n' <<<"$ONLY" | grep -xF -f <(echo "$CAT_SLUGS") || true); fi
  mapfile -t ALL <<<"$SLUGS"
  b=0
  for ((i = 0; i < ${#ALL[@]}; i += BATCH)); do
    b=$((b + 1))
    phase corpora "corpora-$b" -- "${ALL[@]:i:BATCH}"
    cp "$OUT/homes/corpora-$b/result/rows/"*.json "$OUT/rows/" 2>/dev/null || true
    # A batch's restored indices are not needed again; keep its logs, drop the bulk.
    rm -rf "$OUT/homes/corpora-$b/.xerj/hub/node" "$OUT/homes/corpora-$b/.xerj/hub/packs"
  done
  for s in $SLUGS; do
    [ -f "$OUT/rows/$s.json" ] || jq -cn --arg run "$RUN_ID" --arg date "$DATE" --arg slug "$s" \
      '{run: $run, date: $date, kind: "e2e", slug: $slug, pin8: "", bytes: null, defs: null, result: "no result",
        tag: "", stage: "", error: "the e2e phase recorded no result"}' > "$OUT/rows/$s.json"
  done
  n_rows=$(ls "$OUT/rows" | wc -l)
  n_pass=$(cat "$OUT/rows/"*.json 2>/dev/null | jq -s '[.[] | select(.result == "e2e passed")] | length')
  partial=""; [ -n "$ONLY" ] && partial=" (PARTIAL: --only $ONLY; the gate is every catalogued corpus)"
  check every-corpus-pull-def-question "$([ "$n_pass" = "$got" ] && [ -z "$ONLY" ] && echo 0 || echo 1)" \
    "$n_pass of $got catalogued corpora passed pull + --def + one question$partial"

  # 3. `xerj "<question>"`, no corpus named: the lz4 benchmark questions.
  QCORPUS=$(jq -r .corpus "$QFILE")
  if jq -e --arg s "$QCORPUS" '.corpora[] | select(.slug == $s)' "$OUT/catalogue.json" >/dev/null; then
    phase questions questions
  else
    check routed-questions 1 "'$QCORPUS' is not in the catalogue, so its benchmark questions cannot be asked"
  fi

  # 4/5. The documented failure paths and the tamper checks, each from an empty HOME.
  TARGET=$(jq -r --arg s "$QCORPUS" '([.corpora[] | select(.slug == $s)] + (.corpora | sort_by(.pack_bytes)))[0].slug' "$OUT/catalogue.json")
  E=$(jq -c --arg s "$TARGET" '.corpora[] | select(.slug == $s)' "$OUT/catalogue.json")
  file=$(basename "$(jq -r .pack_url <<<"$E")")
  mkdir -p "$OUT/lab/target"
  case "$(jq -r .pack_url <<<"$E")" in
    file://*) cp "$(jq -r .pack_url <<<"$E" | sed 's#^file://##')" "$OUT/lab/target/$file" ;;
    *) curl -fsSL --retry 3 -o "$OUT/lab/target/$file" "$(jq -r .pack_url <<<"$E")" ;;
  esac
  sign "$OUT/lab/target/$file" "$OUT/lab/target/$file.sig"
  flip_byte "$OUT/lab/target/$file" "$OUT/lab/target/tampered-$file"
  bad_sha=$(sha256sum "$OUT/lab/target/tampered-$file" | cut -d' ' -f1)
  lab_catalogue() { # name jq-filter: the verified catalogue, edited, signed with the lab key
    mkdir -p "$OUT/site/$1/api/v1"
    jq -c --arg s "$TARGET" "$2" "$OUT/catalogue.json" > "$OUT/site/$1/api/v1/catalogue.json"
    sign "$OUT/site/$1/api/v1/catalogue.json" "$OUT/site/$1/api/v1/catalogue.json.sig"
  }
  T="file://$OUT/lab/target"
  lab_catalogue overcap "(.corpora[] | select(.slug == \$s) | .pack_bytes) = $((AUTO_PULL_MAX_BYTES + 1))"
  lab_catalogue tamper-sha "(.corpora[] | select(.slug == \$s)) |= (.pack_url = \"$T/tampered-$file\" | .sig_url = \"$T/$file.sig\")"
  lab_catalogue tamper-sig "(.corpora[] | select(.slug == \$s)) |= (.pack_url = \"$T/tampered-$file\" | .sig_url = \"$T/$file.sig\" | .pack_sha256 = \"$bad_sha\")"
  mkdir -p "$OUT/site/tampered/api/v1"
  flip_byte "$OUT/catalogue.json" "$OUT/site/tampered/api/v1/catalogue.json"
  cp "$OUT/catalogue.json.sig" "$OUT/site/tampered/api/v1/catalogue.json.sig"
  echo "== failure paths and tamper checks against '$TARGET'"
  phase unreachable unreachable
  phase html html
  phase offline offline
  SIG=lab phase overcap overcap XERJ_HUB_URL="$LOCAL/overcap" XERJ_HUB_PUBKEY="$LAB_PUB"
  SIG=lab phase tamper-sha tamper-sha XERJ_HUB_URL="$LOCAL/tamper-sha" XERJ_HUB_PUBKEY="$LAB_PUB"
  SIG=lab phase tamper-sig tamper-sig XERJ_HUB_URL="$LOCAL/tamper-sig" XERJ_HUB_PUBKEY="$LAB_PUB"
  phase tamper-catalogue tamper-catalogue XERJ_HUB_URL="$LOCAL/tampered"
fi

# ── fold ────────────────────────────────────────────────────────────────────
n_fail=$(jq -s '[.[] | select(.result != "pass")] | length' "$CHECKS")
n_all=$(jq -s length "$CHECKS")
{
  echo "# Hub v2 whole-hub end-to-end (spec §12.4)"
  echo
  echo "- run: \`$RUN_ID\` ($DATE), mode: **$MODE**, hub: \`$HUB_UNDER_TEST\`"
  echo "- binary: \`$("$BIN" --version 2>/dev/null | head -n 1 || echo "$BIN")\`, embedded key \`$PUBKEY\`"
  [ "$MODE" = rehearsal ] && echo "- rehearsal: the client verified a throwaway key \`$LAB_PUB\` (packs re-signed); \`signatures: real\` rows used the real key"
  echo "- sandbox: $([ "$SANDBOX" = 1 ] && echo "benchmarks/hub-pov/sandbox.sh, a fresh HOME per phase" || echo "none (--no-sandbox); a fresh HOME and cleared env per phase")"
  echo
  echo "**$((n_all - n_fail)) of $n_all checks passed.**"
  echo
  echo "| check | result | signatures | detail |"
  echo "|---|---|---|---|"
  jq -r '"| `\(.check)` | \(.result) | \(.signatures) | \(.detail | gsub("\\|"; "\\\\|")) |"' "$CHECKS"
  echo
  echo "## Per corpus"
  echo
  echo "| slug | pin8 | bytes | result | detail |"
  echo "|---|---|---:|---|---|"
  cat "$OUT/rows/"*.json 2>/dev/null | jq -r -s 'sort_by(.slug)[] | "| `\(.slug)` | `\(.pin8)` | \(.bytes // "") | \(.result)\(if .stage != "" then " at `\(.stage)`" else "" end) | \((if .error != "" then .error else (.detail // "") end) | gsub("\\|"; "\\\\|")) |"'
} > "$OUT/GATE.md"
echo
cat "$OUT/GATE.md"
[ "$n_fail" = 0 ]
