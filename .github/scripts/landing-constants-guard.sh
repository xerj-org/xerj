#!/usr/bin/env bash
# landing-constants-guard.sh — fail when the website's hand-duplicated constants drift.
#
# WHY THIS EXISTS
#   landing/ is plain static HTML with no build step, so every published constant
#   (ES-YAML conformance count, canonical-operation count, release version, the
#   kNN exactness claim) is copy-pasted across dozens of files. A 2026-08-12
#   first-contact agent study found FOUR different conformance figures live on the
#   site at once, a page headlining "SEVEN canonical operations" whose own card said
#   "six", version footers frozen three releases back, and three machine-readable
#   schema files that nothing linked to. Individually cosmetic; collectively they
#   cost trust on a site whose entire pitch is verified numbers — and agents read
#   whichever copy they land on first.
#
#   Findings: demo/playbooks/FTX_PROBE_2026-08-12.md
#
# HISTORICAL CAPTURES ARE ALLOWED, BUT MUST SAY SO
#   Pages that publish a dated captured run (demo/index.html) legitimately carry an
#   old number — rewriting it would falsify a real measurement. Mark those with an
#   inline HTML comment immediately before the figure:
#
#       <!-- snapshot:YYYY-MM-DD -->1305 / 1329
#
#   and state the current value nearby in prose. The marker is what distinguishes
#   "deliberately historical" from "nobody updated this".
#
# USAGE
#   .github/scripts/landing-constants-guard.sh          # check
#   CONFORMANCE=1444/1454 .github/scripts/landing-constants-guard.sh
#
# ENV OVERRIDES
#   LANDING       root of the site tree            (default: landing)
#   CONFORMANCE   canonical "passed/total"         (default: read from below)
#   VERSION       expected release stamp, e.g. rc.15 (default: newest git tag)

set -uo pipefail

LANDING="${LANDING:-landing}"

# Canonical ES-YAML conformance. Source of truth is main's own CI job
# ("ES-compat YAML conformance"), NOT any doc — per CLAUDE.md the pass count grows
# as cases are added and only "0 failed" is invariant. Update this line when CI does.
CONFORMANCE="${CONFORMANCE:-1444/1454}"
CONF_PASS="${CONFORMANCE%%/*}"
CONF_TOTAL="${CONFORMANCE##*/}"

# Newest released tag, unless pinned. Note rc.N existing in CHANGELOG does NOT mean
# it is released — the 2026-08-12 study found rc.16 in the changelog while rc.15 was
# still the latest GitHub release, and footers claiming an unreleased version are
# exactly the kind of drift this guard is for.
if [ -z "${VERSION:-}" ]; then
  VERSION="$(git tag --list 'v1.0.0-rc.*' 2>/dev/null | sort -V | tail -1 | sed 's/^v//')"
fi

# The tree's own version (engine/Cargo.toml). A release PR stamps the footers
# with the version it is cutting BEFORE that version's tag exists, so requiring
# tag-equality on a release branch is unsatisfiable by construction — rc.76
# dodged it only by cutting directly on main, which skipped every release gate
# (#474). A footer matching the version of the tree it ships in is
# self-consistent, not drift; the stale-footer drift this check exists for is a
# footer matching NEITHER the tag NOR the tree. The allowed set is therefore
# {newest released tag, engine/Cargo.toml version} — which collapses to just
# the tag on every non-release branch.
TREE_VERSION="$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml 2>/dev/null || true)"

fails=0
note() { printf '  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }
pass() { printf 'ok    %s\n' "$1"; }

if [ ! -d "$LANDING" ]; then
  echo "FAIL  no such directory: $LANDING (run from the repo root)"
  exit 1
fi

echo "landing-constants-guard"
echo "  tree=$LANDING conformance=$CONF_PASS/$CONF_TOTAL version=${VERSION:-<unknown>}"
echo

# ---------------------------------------------------------------- conformance ---
# Any "NNNN / NNNN" pair in the 1300-1399 range is treated as a conformance claim.
# Accepts thousands separators and "of" as the separator. Snapshot-marked lines are
# exempt: the marker is a deliberate assertion that the figure is historical.
conf_bad=0
while IFS= read -r hit; do
  [ -z "$hit" ] && continue
  case "$hit" in *"<!-- snapshot:"*) continue ;; esac
  file="${hit%%:*}"
  rest="${hit#*:}"
  line="${rest%%:*}"
  text="${rest#*:}"
  # Pull each conformance-shaped pair out of the line and compare against canonical.
  echo "$text" | grep -oE '1,?3[0-9]{2}[[:space:]]*(/|of)[[:space:]]*1,?3[0-9]{2}' | while read -r fig; do
    got_pass="$(echo "$fig" | sed -E 's/^(1,?3[0-9]{2}).*/\1/' | tr -d ,)"
    got_total="$(echo "$fig" | sed -E 's/.*(1,?3[0-9]{2})$/\1/' | tr -d ,)"
    # "N / N executed cases of the M-case suite" is the honest passed/executed form,
    # not a passed/total claim — accept it when N is the canonical pass count.
    if [ "$got_pass" = "$CONF_PASS" ] && [ "$got_total" = "$CONF_PASS" ]; then
      continue
    fi
    if [ "$got_pass" != "$CONF_PASS" ] || [ "$got_total" != "$CONF_TOTAL" ]; then
      printf '%s:%s: %s\n' "$file" "$line" "$fig"
    fi
  done
done < <(grep -rnE '1,?3[0-9]{2}[[:space:]]*(/|of)[[:space:]]*1,?3[0-9]{2}' \
           "$LANDING" --include='*.html' --include='*.txt' --include='*.json' 2>/dev/null) > /tmp/.lcg_conf.$$ || true

if [ -s /tmp/.lcg_conf.$$ ]; then
  fail "stale ES-YAML conformance figures (canonical: $CONF_PASS / $CONF_TOTAL)"
  while IFS= read -r l; do note "$l"; done < /tmp/.lcg_conf.$$
  note "fix: update to the canonical figure, or mark a dated capture with <!-- snapshot:YYYY-MM-DD -->"
  conf_bad=1
else
  pass "ES-YAML conformance figures agree ($CONF_PASS / $CONF_TOTAL)"
fi
rm -f /tmp/.lcg_conf.$$

# -------------------------------------------------------------------- version ---
# Footer stamps must not advertise a version other than the newest released tag.
#
# Scoped to the site footer (the "XERJ.AI ·" brand line) ON PURPOSE. Version strings
# elsewhere are usually measurement provenance — "XERJ v1.0.0-rc.6 vs Elasticsearch
# 8.13.4" on the benchmarks page names the build that was actually benchmarked, and
# the demo page's captured transcript really did run on rc.1. Rewriting those would
# falsify a real run. Only the footer claims "this is the current release".
if [ -n "${VERSION:-}" ]; then
  vre="$(echo "$VERSION" | sed 's/\./\\./g')"
  tre="$(printf '%s' "$TREE_VERSION" | sed 's/\./\\./g')"
  allow_ver="V${vre}([^0-9]|$)"
  if [ -n "$tre" ] && [ "$tre" != "$vre" ]; then
    allow_ver="V(${vre}|${tre})([^0-9]|$)"
  fi
  bad_ver="$(grep -rniE 'XERJ\.AI[^<]*V1\.0\.0-RC\.?[0-9]+' "$LANDING" --include='*.html' 2>/dev/null \
             | grep -viE "$allow_ver" \
             | grep -v '<!-- snapshot:' || true)"
  if [ -n "$bad_ver" ]; then
    fail "version stamps disagree with the newest released tag (v$VERSION) and the tree's own version (v$TREE_VERSION)"
    echo "$bad_ver" | head -20 | while IFS= read -r l; do note "${l:0:160}"; done
    n="$(echo "$bad_ver" | wc -l)"; [ "$n" -gt 20 ] && note "... and $((n - 20)) more"
  else
    pass "version stamps match v$VERSION (tree: v$TREE_VERSION)"
  fi

  # The agent-facing llms*.txt carry a prose "Current release: **vX.Y.Z**"
  # line. That is a claim about the current release, not measurement
  # provenance, and it drifted three releases behind (rc.17 while rc.19 had
  # shipped) exactly the way the footers used to — an agent that reads llms.txt
  # is handed the wrong version (#515 / #520). Hold it to the newest tag too.
  # Snapshot-marked lines stay exempt.
  allow_llms="v${vre}([^0-9]|$)"
  if [ -n "$tre" ] && [ "$tre" != "$vre" ]; then
    allow_llms="v(${vre}|${tre})([^0-9]|$)"
  fi
  bad_llms="$(grep -rniE 'current release:[^<]*v1\.0\.0-rc\.?[0-9]+' "$LANDING" --include='llms*.txt' 2>/dev/null \
              | grep -viE "$allow_llms" \
              | grep -v '<!-- snapshot:' || true)"
  if [ -n "$bad_llms" ]; then
    fail "llms*.txt 'Current release' disagrees with the newest released tag (v$VERSION) and the tree's own version (v$TREE_VERSION)"
    echo "$bad_llms" | while IFS= read -r l; do note "${l:0:160}"; done
  else
    pass "llms 'Current release' matches v$VERSION (tree: v$TREE_VERSION)"
  fi
else
  note "skipped version check: no v1.0.0-rc.* tag found"
fi

# ------------------------------------------------------------- operation count ---
# The canonical-operation count must not contradict itself across the agent pages.
ops_six="$(grep -rniE '\b(all )?six (canonical )?(agent )?operations?\b' "$LANDING" --include='*.html' --include='*.txt' 2>/dev/null || true)"
ops_seven="$(grep -rniE '\bseven (canonical )?(agent )?operations?\b' "$LANDING" --include='*.html' --include='*.txt' 2>/dev/null || true)"
if [ -n "$ops_six" ] && [ -n "$ops_seven" ]; then
  fail "the site claims both six and seven canonical agent operations"
  echo "$ops_six" | head -5 | while IFS= read -r l; do note "six:   ${l:0:140}"; done
  echo "$ops_seven" | head -5 | while IFS= read -r l; do note "seven: ${l:0:140}"; done
  note "note: xerj_autoindex is the seventh and is CLI-only — it has no HTTP route"
else
  pass "canonical-operation count is self-consistent"
fi

# ----------------------------------------------------------------- kNN honesty ---
# Unfiltered kNN is HNSW-served (approximate, exact-rescored). Claiming it is exact
# is an honest-claims violation. Filtered / aggs-bearing / small-corpus kNN really
# IS exact brute-force, so only flag the unqualified "exact nearest-neighbor" form.
knn_bad="$(grep -rniE 'exact nearest[- ]neighbou?r' "$LANDING" \
            --include='*.html' --include='*.txt' --include='*.json' 2>/dev/null \
           | grep -viE 'rescor|filtered|brute|when|otherwise|below|small' || true)"
if [ -n "$knn_bad" ]; then
  fail "unqualified 'exact nearest-neighbor' claim (unfiltered kNN is HNSW-served)"
  echo "$knn_bad" | while IFS= read -r l; do note "${l:0:160}"; done
else
  pass "kNN exactness claims are qualified"
fi

# ------------------------------------------------------- orphaned agent artifacts ---
# Machine-readable artifacts an agent cannot discover are equivalent to absent ones.
# The 2026-08-12 study found all three tool-schema files reachable only by guessing
# the URL — an agent found them solely by enumerating a local mirror directory.
for artifact in "$LANDING"/docs/agents/schemas/*.json; do
  [ -e "$artifact" ] || continue
  base="$(basename "$artifact")"
  refs="$(grep -rl "$base" "$LANDING" --include='*.html' --include='*.xml' --include='*.txt' 2>/dev/null | grep -v "^$artifact$" || true)"
  if [ -z "$refs" ]; then
    fail "orphaned machine-readable artifact: $artifact (no inbound link; a crawler cannot find it)"
  else
    pass "$base linked from: $(echo "$refs" | tr '\n' ' ')"
  fi
done

echo
if [ "$fails" -gt 0 ]; then
  echo "landing-constants-guard: $fails check(s) failed"
  exit 1
fi
echo "landing-constants-guard: all checks passed"
