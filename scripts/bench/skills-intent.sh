#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# skills-intent.sh: can an agent find the right skill by task, among 388?
#
#   bash scripts/bench/skills-intent.sh
#
# Issue #2644's eval. Downloads one pinned commit of a public MIT skill
# library (alirezarezvani/claude-skills), installs its 388 distinct skills
# into a throwaway home's `.claude/skills`, runs `xerj autoindex --skills`
# with HOME and USERPROFILE pointed there, then asks
# `xerj search --prefix skills` each labelled query in
# benchmarks/skills-intent/queries.tsv and records the expected skill's rank.
# Results: benchmarks/skills-intent/results/<date>.tsv, every row kept.
#
# Environment (all optional): XERJ_BIN, PORT, KEEP=1 (repro.sh knobs);
# RESULTS_DATE (default: today, UTC).
# Needs bash, curl, python3, sha256sum (or shasum) and tar.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib/repro.sh
source "$HERE/lib/repro.sh"

FIXTURE_REPO="alirezarezvani/claude-skills"
FIXTURE_SHA="e4a1dc3695908dd5a6c811cc76ab7aa4cf95f2f0"
FIXTURE_TAR_SHA256="2ee7153dc1cf418baffc902de130df89db36d2bbfc42c02be54f7c1e23d4060f"
BENCH="$REPO_ROOT/benchmarks/skills-intent"
RESULTS="$BENCH/results/${RESULTS_DATE:-$(date -u +%F)}.tsv"

repro_find_binary
SCRATCH="$(mktemp -d /tmp/skills-intent.XXXXXX)"
cleanup() {
  repro_node_stop
  if [ "${KEEP:-0}" = 1 ]; then echo "kept $SCRATCH" >&2; else rm -rf "$SCRATCH"; fi
}
trap cleanup EXIT INT TERM

repro_header "fixture"
curl -fsSL --retry 3 --max-time 300 "https://codeload.github.com/$FIXTURE_REPO/tar.gz/$FIXTURE_SHA" -o "$SCRATCH/src.tar.gz"
got="$( (sha256sum "$SCRATCH/src.tar.gz" 2>/dev/null || shasum -a 256 "$SCRATCH/src.tar.gz") | cut -d' ' -f1)"
if [ "$got" != "$FIXTURE_TAR_SHA256" ]; then
  echo "fixture checksum mismatch: got $got, want $FIXTURE_TAR_SHA256" >&2
  exit 1
fi
mkdir -p "$SCRATCH/src" "$SCRATCH/home" "$SCRATCH/cwd"
tar xzf "$SCRATCH/src.tar.gz" -C "$SCRATCH/src" --strip-components 1
repro_note "$FIXTURE_REPO@$FIXTURE_SHA (MIT)"
python3 -I "$BENCH/install_fixture.py" "$SCRATCH/src" "$SCRATCH/home"

repro_free_port 9330
repro_node_start -
repro_node_wait
URL="http://127.0.0.1:$REPRO_PORT"

repro_header "xerj autoindex --skills"
# cwd has no .claude/skills, so only the home folder is found.
set +e
(cd "$SCRATCH/cwd" && HOME="$SCRATCH/home" USERPROFILE="$SCRATCH/home" \
  XERJ_DISABLE_FEEDBACK=true "$XERJ_BIN" autoindex --skills --url "$URL" \
  --max-minutes 0) >"$SCRATCH/autoindex.log" 2>&1
rc=$?
set -e
grep -E '^skills:|xerj-done' "$SCRATCH/autoindex.log" || true
case "$rc" in
  0 | 3) ;;
  *) tail -20 "$SCRATCH/autoindex.log" >&2; echo "autoindex exited $rc" >&2; exit 1 ;;
esac

repro_header "queries"
mkdir -p "$(dirname "$RESULTS")"
python3 -I "$BENCH/score.py" "$XERJ_BIN" "$URL" "$BENCH/queries.tsv" "$RESULTS"
repro_note "every rank: ${RESULTS#"$REPO_ROOT"/}"
repro_env_box
