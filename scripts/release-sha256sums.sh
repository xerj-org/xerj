#!/usr/bin/env bash
# Build one SHA256SUMS from a release's per-asset *.sha256 files (#3278).
#
# An agent that installs from a mirror (no route to github.com) needs one file
# to check against; the release only had one .sha256 per asset. This joins
# every *.sha256 the release (or a local dist/ directory) carries — binaries,
# .deb, and the separately uploaded model bundle — into `<hash>  <name>`
# lines sorted by name, so `sha256sum -c SHA256SUMS --ignore-missing` checks
# whatever subset was downloaded.
#
#   scripts/release-sha256sums.sh --dir dist            # write dist/SHA256SUMS
#   scripts/release-sha256sums.sh --tag v1.0.0-rc.102   # fetch, build, upload
#
# --tag needs a signed-in `gh` with write access; it replaces an existing
# SHA256SUMS asset (--clobber), so rerun it after uploading a late asset.
set -euo pipefail

repo="${XERJ_REPO:-xerj-org/xerj}"
mode=""; arg=""
case "${1:-}" in
  --dir) mode=dir; arg="${2:?--dir needs a directory}" ;;
  --tag) mode=tag; arg="${2:?--tag needs a release tag}" ;;
  *) echo "usage: $0 --dir <dist> | --tag <tag>" >&2; exit 2 ;;
esac

join_sums() {  # $1 = directory holding *.sha256
  local dir="$1" f line hash name
  shopt -s nullglob
  local files=("$dir"/*.sha256)
  [ "${#files[@]}" -gt 0 ] || { echo "no *.sha256 files in $dir" >&2; exit 1; }
  for f in "${files[@]}"; do
    # One "<hash>  <name>" line per file; PowerShell writes CRLF and may
    # write the name with a "*" binary marker.
    line=$(tr -d '\r' < "$f" | head -n1)
    hash=$(printf '%s' "$line" | awk '{print tolower($1)}')
    name=$(basename "$f" .sha256)
    case "$hash" in *[!0-9a-f]*|'') echo "malformed $f" >&2; exit 1 ;; esac
    [ "${#hash}" -eq 64 ] || { echo "malformed $f" >&2; exit 1; }
    printf '%s  %s\n' "$hash" "$name"
  done | LC_ALL=C sort -k2
}

if [ "$mode" = dir ]; then
  join_sums "$arg" > "$arg/SHA256SUMS"
  echo "wrote $arg/SHA256SUMS ($(wc -l < "$arg/SHA256SUMS") assets)"
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
gh release download "$arg" -R "$repo" -p '*.sha256' -D "$work"
join_sums "$work" > "$work/SHA256SUMS"
gh release upload "$arg" -R "$repo" --clobber "$work/SHA256SUMS"
echo "uploaded SHA256SUMS to $repo $arg ($(wc -l < "$work/SHA256SUMS") assets)"
