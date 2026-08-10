#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# adoption-snapshot.sh — the honest adoption funnel for xerj, on demand.
#
# Prints three sections, in this order and on purpose:
#   1. numbers you can quote
#   2. numbers you must NOT quote, each with the reason it is contaminated
#   3. numbers that do not exist at all
#
# It exists because the repo-level totals GitHub shows on the front page
# (stars, forks, clones) are, for this repository, inflated by roughly 13x by a
# synthetic cohort, and quoting them is the fastest available way to lose
# technical credibility — the evidence is public and anyone can re-run the same
# queries. Section 2 is therefore not a disclaimer; it is the point.
#
# PRIVACY: read-only, public GitHub metadata via `gh api`. Nothing is stored,
# nothing is sent anywhere, no personal data is collected. Star/fork account
# names are counted, never printed.
#
# Usage:
#   scripts/adoption-snapshot.sh                 # full report
#   scripts/adoption-snapshot.sh --no-stars      # skip the star-cohort scan (~13 API calls)
#   scripts/adoption-snapshot.sh --repo o/n      # another repo
#
# Requires: gh (authenticated: `gh auth login`), jq.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="${XERJ_REPO:-xerj-org/xerj}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
SERIES="$ROOT/metrics/release-downloads.jsonl"
DO_STARS=1

# Accounts that are us. There is no API for this: xerj-org is a personal
# account, not an Organization, so /orgs/.../members 404s. Keep this list
# current or the external-contributor count silently drifts up.
FIRST_PARTY='["xerj-org","xerj-team"]'

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) REPO="$2"; shift 2 ;;
    --no-stars) DO_STARS=0; shift ;;
    -h|--help) sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

command -v gh >/dev/null || { echo "gh is required (https://cli.github.com)" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }
gh auth status >/dev/null 2>&1 || { echo "gh is not authenticated — run: gh auth login" >&2; exit 2; }

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
b() { printf '\033[1m%s\033[0m\n' "$*"; }
rule() { printf '%s\n' "────────────────────────────────────────────────────────────────────────"; }

echo
b "xerj adoption snapshot — ${REPO} — $(date -u +'%F %T UTC')"
rule

# ── data pulls ───────────────────────────────────────────────────────────────
gh api "repos/${REPO}" > "$W/repo.json"
gh api --paginate "repos/${REPO}/releases?per_page=100" \
  | jq -s 'flatten(1) | map({tag_name, published_at,
                             assets: ((.assets // []) | map({name, download_count}))})' \
  > "$W/releases.json"
gh api --paginate "repos/${REPO}/pulls?state=all&per_page=100" \
  | jq -s 'flatten(1) | map({user: (.user.login // ""), created_at, merged_at})' > "$W/pulls.json"
gh api --paginate "repos/${REPO}/issues?state=all&per_page=100" \
  | jq -s 'flatten(1) | map(select(.pull_request == null) | {user: (.user.login // ""), created_at})' > "$W/issues.json"

# ═════════════════════════ 1. NUMBERS YOU CAN QUOTE ══════════════════════════
b "1. QUOTABLE — nothing has inflated these"
echo

jq -r --argjson fp "$FIRST_PARTY" '
  def is_sum: (.name | endswith(".sha256"));
  (map(.assets) | flatten(1)) as $a |
  ($a | map(select(is_sum | not) | .download_count) | add // 0) as $bin |
  ($a | map(select(is_sum)       | .download_count) | add // 0) as $sum |
  # Paired ceiling: the installer fetches exactly one binary and exactly one
  # matching .sha256 per run (landing/get keeps that order deliberately), so
  # min(binary, its checksum) is an UPPER BOUND on installer runs for that
  # asset. Unpaired binary downloads cannot be installer runs.
  ( map(. as $r
        | ($r.assets | map(select(is_sum | not))) as $bins
        | ($r.assets | map(select(is_sum)) | map({key: (.name|rtrimstr(".sha256")), value: .download_count}) | from_entries) as $sums
        | ($bins | map([.download_count, ($sums[.name] // 0)] | min) | add // 0))
    | add // 0 ) as $paired |
  # A release where every one of its 8 platform archives AND all 8 checksums
  # were downloaded is an automated all-platform sweep, not eight humans: no
  # machine is eight platforms at once. Subtract one sweep from each.
  ( map(select((.assets | map(select(is_sum|not) | select(.download_count > 0)) | length) >= 8
           and (.assets | map(select(is_sum)     | select(.download_count > 0)) | length) >= 8))
    | length ) as $swept |
  "  release asset downloads      \($bin + $sum)   (\($bin) binaries + \($sum) checksums, all-time)",
  "  binary:checksum ratio        \(if $sum == 0 then "n/a" else ((($bin*100/$sum)|floor)/100) end)   (installer traffic pairs 1:1; far from 1.0 = something else)",
  "  installer runs (ceiling)     ≤ \($paired)   matched binary+checksum pairs",
  "  minus one sweep per swept release  ≤ \($paired - ($swept * 8))   (\($swept) release\(if $swept == 1 then "" else "s" end) show all 16 assets downloaded)",
  "",
  "  A ceiling is not a count. It counts fetch pairs, not people, and says",
  "  nothing about whether the archive was ever extracted or run. This is the",
  "  mechanical rule; the 2026-08-08 hand audit applied two extra judgements",
  "  (capping one 15-download outlier, and excluding targets the installer",
  "  could not then request) and landed at ≤28. Expect this figure to sit a",
  "  little above any hand-adjusted one — that is the direction a ceiling",
  "  should err."
' "$W/releases.json"

echo
# Trend, straight out of the committed series.
if [ -s "$SERIES" ]; then
  lines=$(wc -l < "$SERIES")
  jq -rs --argjson n "$lines" '
    sort_by(.date) as $s |
    ($s | last) as $now |
    "  download series               \($n) daily snapshot\(if $n == 1 then "" else "s" end) in metrics/release-downloads.jsonl",
    "  latest                        \($now.date): \($now.totals.all) assets (\($now.totals.binary) binaries)",
    (if $n >= 2 then
       ($s[-2]) as $prev |
       "  change since \($prev.date)      \(if ($now.totals.binary - $prev.totals.binary) >= 0 then "+" else "" end)\($now.totals.binary - $prev.totals.binary) binaries"
     else
       "  change                        not computable yet — one snapshot only. The series starts the day this lands."
     end)
  ' "$SERIES"
else
  echo "  download series               absent — run .github/scripts/snapshot-release-downloads.sh"
fi

echo
jq -rs --argjson fp "$FIRST_PARTY" '
  .[0] as $pulls | .[1] as $issues |
  (($pulls + $issues) | map(.user)
    | map(select(. != "" and (IN($fp[]) | not) and (endswith("[bot]") | not)))
    | unique) as $ext |
  ($pulls | map(.user) | map(select(. != "" and (IN($fp[]) | not) and (endswith("[bot]") | not))) | unique) as $extpr |
  "  external humans, PR or issue  \($ext | length)   distinct accounts, all-time",
  "  └ of which opened a PR        \($extpr | length)",
  "  first-party PRs               \($pulls | map(select(.user | IN($fp[]))) | length) of \($pulls | length)",
  "",
  "  This is the community. At this size it is a list of people, not a metric."
' "$W/pulls.json" "$W/issues.json"

echo
rule
# ═══════════════════════ 2. NUMBERS YOU MUST NOT QUOTE ═══════════════════════
b "2. CONTAMINATED — do not put these in a README, a deck, or a post"
echo

jq -r '
  "  stars (raw)                  \(.stargazers_count)",
  "  forks (raw)                  \(.forks_count)",
  "  watchers                     \(.subscribers_count)"
' "$W/repo.json"

if [ "$DO_STARS" = 1 ]; then
  # starred_at needs the star+json media type. ~1 call per 100 stars.
  gh api --paginate -H "Accept: application/vnd.github.star+json" \
    "repos/${REPO}/stargazers?per_page=100" \
    | jq -s 'flatten(1) | map(.starred_at // .created_at // "")' > "$W/stars.json" 2>/dev/null || echo '[]' > "$W/stars.json"

  jq -r '
    map(select(. != "") | .[0:10]) | sort as $days |
    ($days | length) as $total |
    (reduce $days[] as $d ({}; .[$d] += 1)) as $hist |
    ($hist | to_entries | max_by(.value)) as $peak |
    if $total == 0 then "  star timestamps              unavailable"
    else
      "  ── star arrival ───────────────────────────────────────────────────",
      "  busiest single day           \($peak.key): \($peak.value) stars  (\(($peak.value * 100 / $total) | floor)% of all stars)",
      (if ($peak.value * 4) > $total then
         ($days | map(select(. < $peak.key)) | length) as $before |
         "  stars BEFORE that day        \($before)   ← the defensible number",
         "  stars on/after that day      \($total - $before)   ← a single-day arrival of \(($peak.value * 100 / $total)|floor)% of all",
         "                               stars is not organic. Composition analysis of that",
         "                               cohort (account age, followers, username shape) is in",
         "                               docs/gtm/ADOPTION.md. It is not an abuse determination,",
         "                               but it is not adoption either."
       else
         "  no single-day spike dominates — the raw star count may be usable here."
       end)
    end
  ' "$W/stars.json"
fi

echo
echo "  ── why each of the above is contaminated ──────────────────────────"
echo "  stars    a synthetic-looking cohort arrived in one day; see the split above."
echo "  forks    a fork is free and costs nothing to abandon. The only fork number"
echo "           worth anything is 'forks with a commit after forking', which for"
echo "           this repo was 4 of 351 at the last audit (98.9% never touched)."
echo "  clones   14-DAY ROLLING window, not a lifetime total, and it counts our own"
echo "           CI and indexing runs. ~11 clone events per unique cloner is machine"
echo "           traffic, not 11 developers. Never sum consecutive snapshots: they"
echo "           overlap by 13 days."
echo "  views    same window, sampled the same way, and self-referral inflates it."
echo

echo "  ── /get and /get.ps1 requests ─────────────────────────────────────"
echo "  Instrumented since 2026-08-08 by functions/get.js (a Pages Function that"
echo "  counts the request and serves the script). It is NOT readable from here:"
echo "  the counts live in R2 and come out of the token-guarded export —"
echo "    curl -s 'https://xerj.org/get?token=\$INSTALLS_TOKEN&days=30' | jq ."
echo "  Read it with its own caveat: a request to /get is a request. It does not"
echo "  prove a download, a checksum match, or that the binary was ever run."
echo

rule
# ══════════════════════════ 3. NUMBERS THAT DO NOT EXIST ═════════════════════
b "3. BLIND — no number exists, and none should be invented"
echo
cat <<'BLIND'
  Whether an installed binary was ever executed, even once.
      Deliberate. There is no telemetry in the xerj binary and none is planned:
      it is an Apache-2.0 tool that engineers run on their own hardware, and a
      silent phone-home would cost more credibility than the data is worth.
      This gap is a choice, not an oversight — do not close it silently.

  Site visits.
      The Cloudflare RUM beacon stopped recording on 2026-07-22. Until it is
      re-injected, the analytics dashboard reads zero and that zero is false.

  Install failures.
      A run that dies on an unsupported platform, a missing curl, or (since
      2026-08-08) a missing SHA-256 tool leaves no trace. Every install number
      here counts successes; the denominator is unknown.

  Attribution.
      Nothing links a visit, a clone, a download and an install. Any funnel
      conversion rate computed across those rows is fiction.
BLIND
echo
rule
echo "  Source of the contamination analysis: docs/gtm/ADOPTION.md (internal)."
echo "  Instrumentation: functions/get.js, .github/workflows/release-metrics.yml"
echo
