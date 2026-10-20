#!/usr/bin/env bash
# retire_v1_releases.sh — Corpus Hub v2, implementation spec §12.2 ("Binaries only").
#
# WHAT THIS RETIRES, AND NOTHING ELSE
#   The hub must host ONLY binaries: the signed `*.hubpack.tar.zst` (+ `.sig`)
#   pack releases and the single signed `catalogue.json` (+ `.sig`). The old
#   lane-B JSON "record pack" releases published by `.github/workflows/pack-publish.yml`
#   — the signed JSON/zip record archives for `rust-vulns` and
#   `agent-session-trajectories` — are the redundant lane and are retired here,
#   but ONLY after their v2 hubpacks exist (PR #1736). See §12.1/§12.2 of
#   docs/CORPUS_HUB_V2_IMPLEMENTATION.md and BINARIES_ONLY_12.2.md next to this file.
#
#   The old record-pack releases are tagged by DATE:
#       pack-rust-vulns-YYYY-MM-DD[-rN]
#       pack-agent-session-trajectories-YYYY-MM-DD[-rN]
#   and carry ONLY JSON/zip assets (`*-pack.zip`, `*-freshness.json`,
#   `*-SHA256SUMS`, `*-SHA256SUMS.sig`). The v2 hubpacks for the SAME two
#   corpora are tagged by content PIN (pack-<slug>-<8-hex-pin>[-rN]) and carry
#   `*.hubpack.tar.zst`. The date suffix and the asset set are BOTH checked so
#   this script can never touch a v2 pack, even though the slug prefix collides.
#
# SAFETY MODEL (why this is safe to hand to a human to run)
#   1. DRY RUN IS THE DEFAULT. Nothing is deleted without `--execute`.
#   2. Explicit allowlist: only the two record slugs below, only the date-tag
#      shape, only releases whose assets are the JSON/zip record-archive set and
#      contain NO `*.hubpack.tar.zst`. Anything else is skipped and reported.
#   3. A pre-delete assertion re-checks every selected release against the v2
#      pin shape and the hubpack-asset test; one failure aborts the whole run.
#   4. #1736 GUARD: `--execute` refuses unless a pin-based v2 hubpack release
#      already exists for BOTH record slugs (published, unless
#      `--allow-draft-replacement`). Retiring the live JSON packs before their
#      binary replacements are live would strand those two corpora.
#   5. Idempotent: a release already gone (HTTP 404) is counted "already retired".
#
#   This repository uses IMMUTABLE releases: deleting a release does NOT free its
#   tag for reuse (see the note in pack-publish.yml). That is fine here — these
#   dated tags are never reused. The git tag ref is left in place unless you pass
#   `--cleanup-tags`.
#
# USAGE
#   export GH_TOKEN=<token with contents:write on the repo>
#   tools/hub-packs/retire_v1_releases.sh                 # dry run (default)
#   tools/hub-packs/retire_v1_releases.sh --execute        # really delete (prompts)
#   tools/hub-packs/retire_v1_releases.sh --execute --yes  # no prompt (CI/cron)
#
# FLAGS
#   --execute                 Perform the deletions. Omit for a dry run.
#   --yes                     In --execute, skip the interactive confirmation.
#   --repo <owner/name>       Default: xerj-org/xerj.
#   --cleanup-tags            Also delete the git tag ref of each retired release
#                             (best effort; skipped for drafts). Default: off.
#   --allow-draft-replacement Let the #1736 guard pass on DRAFT v2 hubpacks.
#                             Default: the v2 record hubpacks must be PUBLISHED.
#   -h | --help               This help.
#
# EXIT CODES
#   0  dry run completed, or execute completed (incl. "already retired")
#   2  bad usage / missing dependency / API error
#   3  #1736 guard failed (v2 record hubpacks not present) — nothing deleted
#   4  pre-delete safety assertion failed — nothing deleted
set -euo pipefail

# ---- explicit allowlist (the ONLY corpora whose JSON record packs are retired)
RECORD_SLUGS=("rust-vulns" "agent-session-trajectories")

REPO="xerj-org/xerj"
EXECUTE=0
ASSUME_YES=0
CLEANUP_TAGS=0
ALLOW_DRAFT_REPLACEMENT=0

die() { echo "error: $*" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --execute) EXECUTE=1 ;;
    --yes|-y) ASSUME_YES=1 ;;
    --cleanup-tags) CLEANUP_TAGS=1 ;;
    --allow-draft-replacement) ALLOW_DRAFT_REPLACEMENT=1 ;;
    --repo) shift; REPO="${1:-}"; [ -n "$REPO" ] || die "--repo needs a value" ;;
    --repo=*) REPO="${1#--repo=}" ;;
    -h|--help) sed -n '2,70p' "$0"; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
  shift
done

command -v gh  >/dev/null 2>&1 || die "gh (GitHub CLI) is required"
command -v python3 >/dev/null 2>&1 || die "python3 is required"
gh auth status >/dev/null 2>&1 || [ -n "${GH_TOKEN:-}${GITHUB_TOKEN:-}" ] \
  || die "authenticate gh, or export GH_TOKEN"

echo "== §12.2 retire old JSON record-pack releases =="
echo "repo:            $REPO"
echo "record slugs:    ${RECORD_SLUGS[*]}"
echo "mode:            $([ "$EXECUTE" = 1 ] && echo EXECUTE || echo 'DRY RUN (default; nothing will be deleted)')"
echo

# ---- fetch every release once, as one combined JSON array -------------------
WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
RELJSON="$WORK/releases.json"
if ! gh api --paginate --slurp "repos/$REPO/releases?per_page=100" > "$RELJSON" 2>"$WORK/err"; then
  cat "$WORK/err" >&2; die "could not list releases (need contents:read on $REPO)"
fi
# `--slurp` wraps the paginated pages in an extra array layer: flatten it.
python3 -I - "$RELJSON" <<'PY' > "$WORK/flat.json"
import json,sys
d=json.load(open(sys.argv[1]))
if d and isinstance(d[0],list):
    d=[r for page in d for r in page]
json.dump(d,open(sys.stdout.fileno(),"w"))
PY
RELJSON="$WORK/flat.json"
echo "releases fetched: $(python3 -I -c 'import json,sys;print(len(json.load(open(sys.argv[1]))))' "$RELJSON")"
echo

# ---- classify + guard, in one isolated pass (logic mirrors §12.2) -----------
# Emits:  $WORK/retire.tsv  (id<TAB>tag<TAB>draft) for the releases to retire
#         $WORK/guard.txt   ("OK" or a human reason), and a printed summary.
SLUGS_CSV="$(IFS=,; echo "${RECORD_SLUGS[*]}")"
python3 -I - "$RELJSON" "$SLUGS_CSV" "$WORK" "$ALLOW_DRAFT_REPLACEMENT" <<'PY'
import json,re,sys,os
rel   = json.load(open(sys.argv[1]))
slugs = sys.argv[2].split(",")
work  = sys.argv[3]
allow_draft = sys.argv[4] == "1"

alt       = "|".join(re.escape(s) for s in slugs)
date_re   = re.compile(r"^pack-(?:%s)-\d{4}-\d{2}-\d{2}(?:-r\d+)?$" % alt)
pin_re    = re.compile(r"^pack-[A-Za-z0-9._-]+-[0-9a-f]{8}(?:-r\d+)?$")
def names(r): return [a.get("name","") for a in r.get("assets",[])]
def is_hubpack(r): return any(n.endswith(".hubpack.tar.zst") for n in names(r))
def is_record_archive(r):
    ns = names(r)
    return any(n.endswith("-pack.zip") for n in ns) or any(n.endswith("-SHA256SUMS") for n in ns)

retire, skipped_pin, skipped_other = [], [], []
for r in rel:
    tag = r["tag_name"]
    if date_re.match(tag) and is_record_archive(r) and not is_hubpack(r):
        retire.append(r)
    elif tag.startswith("pack-") and any(tag.startswith("pack-%s-" % s) for s in slugs):
        # same slug prefix but NOT a retire-able date/JSON release → report why kept
        (skipped_pin if (pin_re.match(tag) or is_hubpack(r)) else skipped_other).append(r)

retire.sort(key=lambda r: r["tag_name"])
with open(os.path.join(work,"retire.tsv"),"w") as f:
    for r in retire:
        f.write("%s\t%s\t%s\n" % (r["id"], r["tag_name"], str(r["draft"]).lower()))

print("RETIRE-ABLE (old JSON record-pack releases): %d" % len(retire))
for r in retire:
    print("  - %-45s id=%-10s draft=%s  assets=%s"
          % (r["tag_name"], r["id"], str(r["draft"]).lower(), ",".join(names(r))))
print()
print("KEPT — v2 hubpacks for the same slugs (never retired): %d" % len(skipped_pin))
for r in skipped_pin:
    print("  + %-45s (pin/hubpack: v2 binary pack)" % r["tag_name"])
if skipped_other:
    print("KEPT — same-slug releases that are neither v2 nor retire-able (review): %d" % len(skipped_other))
    for r in skipped_other:
        print("  ? %-45s assets=%s" % (r["tag_name"], ",".join(names(r))))
print()

# ---- #1736 guard: a pin-based v2 hubpack must exist for BOTH record slugs ----
guard_lines, ok = [], True
for s in slugs:
    pin = re.compile(r"^pack-%s-[0-9a-f]{8}(?:-r\d+)?$" % re.escape(s))
    hits = [r for r in rel if pin.match(r["tag_name"]) and is_hubpack(r)]
    pub  = [r for r in hits if not r["draft"]]
    usable = hits if allow_draft else pub
    tag_list = ", ".join(r["tag_name"] for r in (usable[:5])) or "(none)"
    guard_lines.append("  %-30s v2 hubpack releases: %d (published %d) -> %s"
                       % (s, len(hits), len(pub), tag_list))
    if not usable:
        ok = False
print("#1736 GUARD — v2 record hubpacks present%s:"
      % ("" if allow_draft else " and PUBLISHED"))
print("\n".join(guard_lines))
open(os.path.join(work,"guard.txt"),"w").write("OK" if ok else "MISSING")

# ---- defense-in-depth: assert nothing selected looks like a v2 pack ---------
bad = [r["tag_name"] for r in retire if pin_re.match(r["tag_name"]) or is_hubpack(r)]
open(os.path.join(work,"assert.txt"),"w").write("OK" if not bad else "FAIL:"+",".join(bad))
PY
echo

GUARD="$(cat "$WORK/guard.txt" 2>/dev/null || echo MISSING)"
ASSERT="$(cat "$WORK/assert.txt" 2>/dev/null || echo FAIL)"
RETIRE_TSV="$WORK/retire.tsv"
N_RETIRE="$(wc -l < "$RETIRE_TSV" | tr -d ' ')"

# ---- pre-delete safety assertion (always, even in dry run) ------------------
if [ "$ASSERT" != "OK" ]; then
  echo "SAFETY ASSERTION FAILED: a selected release looks like a v2 pack: ${ASSERT#FAIL:}" >&2
  echo "Refusing to go further. No release was touched." >&2
  exit 4
fi

# ---- dry run stops here -----------------------------------------------------
if [ "$EXECUTE" != 1 ]; then
  echo "DRY RUN — nothing was deleted. $N_RETIRE release(s) would be retired."
  if [ "$GUARD" = OK ]; then
    echo "The #1736 guard is satisfied: re-run with --execute to perform the deletions."
  else
    echo "The #1736 guard is NOT satisfied yet (v2 record hubpacks missing): --execute"
    echo "would refuse. This is expected until PR #1736 lands and publishes them."
  fi
  echo
  echo "Commands --execute would run (by release id, so drafts are handled):"
  while IFS=$'\t' read -r id tag draft; do
    [ -n "$id" ] || continue
    echo "    gh api -X DELETE repos/$REPO/releases/$id   # $tag"
    if [ "$CLEANUP_TAGS" = 1 ] && [ "$draft" != "true" ]; then
      echo "    gh api -X DELETE repos/$REPO/git/refs/tags/$tag   # (immutable: name stays burned)"
    fi
  done < "$RETIRE_TSV"
  exit 0
fi

# ---- execute: enforce the #1736 guard BEFORE any deletion -------------------
if [ "$GUARD" != OK ]; then
  echo "REFUSING TO EXECUTE: PR #1736's v2 record hubpacks are not present yet" >&2
  echo "(need a pin-based pack-<slug>-<8hex> *.hubpack.tar.zst release for" >&2
  echo " each of: ${RECORD_SLUGS[*]}$([ "$ALLOW_DRAFT_REPLACEMENT" = 1 ] || echo '  — PUBLISHED')." >&2
  echo "Retiring the JSON record packs now would strand those corpora. Nothing deleted." >&2
  exit 3
fi

if [ "$N_RETIRE" = 0 ]; then
  echo "Nothing to retire — already binaries-only. Done."
  exit 0
fi

if [ "$ASSUME_YES" != 1 ]; then
  printf 'About to DELETE %s old JSON record-pack release(s) on %s. Type "retire" to proceed: ' \
    "$N_RETIRE" "$REPO"
  read -r reply < /dev/tty || reply=""
  [ "$reply" = "retire" ] || { echo "Aborted; nothing deleted."; exit 0; }
fi

deleted=0; already=0; failed=0
while IFS=$'\t' read -r id tag draft; do
  [ -n "$id" ] || continue
  if gh api -X DELETE "repos/$REPO/releases/$id" >/dev/null 2>"$WORK/derr"; then
    echo "retired: $tag (id=$id)"; deleted=$((deleted+1))
  elif grep -q '"status": *"404"' "$WORK/derr" 2>/dev/null || grep -qi 'not found' "$WORK/derr" 2>/dev/null; then
    echo "already gone: $tag (id=$id)"; already=$((already+1))
  else
    echo "FAILED: $tag (id=$id)"; sed 's/^/    /' "$WORK/derr" >&2; failed=$((failed+1)); continue
  fi
  if [ "$CLEANUP_TAGS" = 1 ] && [ "$draft" != "true" ]; then
    if gh api -X DELETE "repos/$REPO/git/refs/tags/$tag" >/dev/null 2>"$WORK/derr"; then
      echo "  tag ref deleted: $tag"
    else
      echo "  tag ref left in place: $tag (immutable tag name stays burned; harmless)"
    fi
  fi
done < "$RETIRE_TSV"

echo
echo "Done. retired=$deleted already-gone=$already failed=$failed"
[ "$failed" = 0 ] || exit 2
