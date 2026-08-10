#!/usr/bin/env bash
# Append today's per-asset release download counts to metrics/release-downloads.jsonl.
#
# WHY THIS EXISTS: GitHub reports `download_count` per asset as a running
# total and keeps no history. Nothing was snapshotting it, so the only
# uninflated adoption number this project has existed solely as a
# point-in-time reading — you could say "145 downloads" but never "how many
# last week". One line a day turns that scalar into a series, at zero
# infrastructure cost.
#
# PRIVACY: this reads public repository metadata only. GitHub exposes no IP,
# no user agent and no identity per download; there is nothing personal in
# this data and nothing here that could make it personal.
#
#   .github/scripts/snapshot-release-downloads.sh [--out FILE] [--repo OWNER/NAME] [--dry-run]
#
# Needs: gh (authenticated), jq. Re-running on the same day REPLACES that
# day's line rather than appending a second one, so it is safe to run by hand.
set -euo pipefail

REPO="${XERJ_REPO:-xerj-org/xerj}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="$ROOT/metrics/release-downloads.jsonl"
DRY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --out)     OUT="$2"; shift 2 ;;
    --repo)    REPO="$2"; shift 2 ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

command -v gh >/dev/null || { echo "gh is required" >&2; exit 2; }
command -v jq >/dev/null || { echo "jq is required" >&2; exit 2; }

DAY="$(date -u +%F)"
NOW="$(date -u +%FT%TZ)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# API payloads travel via files, not argv: the full /releases response is
# hundreds of KB (every asset carries an embedded uploader object) and
# --argjson blew past ARG_MAX. Trim to the fields we keep, up front.
#
# --paginate emits either one merged array or one array per page depending on
# the gh version; `jq -s 'flatten(1)'` normalises both to a single array.
gh api --paginate "repos/${REPO}/releases?per_page=100" \
  | jq -s 'flatten(1) | map({tag_name, published_at, prerelease,
                             assets: ((.assets // []) | map({name, download_count}))})' \
  > "$WORK/releases.json"

# Traffic clones/views are a 14-DAY ROLLING WINDOW: GitHub drops the oldest day
# every day, so any day not snapshotted is gone for good. That is the strongest
# reason to run this daily. The endpoint needs push access; when the token has
# not got it we store null rather than guessing, and the series stays honest.
gh api "repos/${REPO}/traffic/clones" 2>/dev/null | jq '{count, uniques}' > "$WORK/clones.json" || echo null > "$WORK/clones.json"
gh api "repos/${REPO}/traffic/views"  2>/dev/null | jq '{count, uniques}' > "$WORK/views.json"  || echo null > "$WORK/views.json"
[ -s "$WORK/clones.json" ] || echo null > "$WORK/clones.json"
[ -s "$WORK/views.json" ]  || echo null > "$WORK/views.json"

snapshot="$(jq -cn \
  --arg date "$DAY" --arg ts "$NOW" --arg repo "$REPO" \
  --slurpfile releases_in "$WORK/releases.json" \
  --slurpfile clones_in "$WORK/clones.json" \
  --slurpfile views_in "$WORK/views.json" '
  ($releases_in[0]) as $releases |
  ($clones_in[0])   as $clones   |
  ($views_in[0])    as $views    |
  # An asset name ending in .sha256 is a checksum. The split matters: the
  # installer fetches exactly one binary and one checksum per run, so a
  # binary:checksum ratio near 1:1 is installer-shaped traffic and a ratio
  # far below 1 is something else (direct fetches, mirrors, scanners).
  def is_sum: (.name | endswith(".sha256"));
  ($releases | map(.assets // []) | flatten(1)) as $assets |
  {
    date: $date,
    collected_at: $ts,
    repo: $repo,
    totals: {
      releases:  ($releases | length),
      binary:    ($assets | map(select(is_sum | not) | .download_count) | add // 0),
      checksum:  ($assets | map(select(is_sum)       | .download_count) | add // 0),
      all:       ($assets | map(.download_count) | add // 0)
    },
    releases: ($releases | map({
      key: .tag_name,
      value: {
        published_at: .published_at,
        prerelease: .prerelease,
        assets: (.assets // [] | map({key: .name, value: .download_count}) | from_entries)
      }
    }) | from_entries),
    # 14-day rolling windows, not lifetime totals. Never sum these across days:
    # consecutive snapshots overlap by 13 days.
    traffic_14d: { clones: $clones, views: $views }
  }')"

if [ "$DRY" = 1 ]; then
  printf '%s\n' "$snapshot" | jq .
  exit 0
fi

mkdir -p "$(dirname "$OUT")"
touch "$OUT"
# Idempotent: drop any existing line for today, then append the fresh one.
jq -c --arg date "$DAY" 'select(.date != $date)' "$OUT" > "$WORK/out.jsonl" || true
printf '%s\n' "$snapshot" >> "$WORK/out.jsonl"
mv "$WORK/out.jsonl" "$OUT"

echo "wrote $DAY -> $OUT"
jq -c '{date, totals}' <<<"$snapshot"
