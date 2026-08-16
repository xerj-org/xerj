#!/usr/bin/env bash
# check-roadmap-issue-states.sh — compare every issue ROADMAP.md names against
# its live state on GitHub.
#
# This exists because the same defect shipped three times in one release cycle.
# ROADMAP.md is prose full of claims about issue state, `docs_capability_lists`
# can only check the review line and the next-release heading, and CI cannot call
# GitHub. So the claims went unchecked, and independent verification of the
# rc.18 cut found, across two rounds:
#
#   * #275 and #295 closed but published as open
#   * #362 listed as "retired by this RC" though closed `not_planned` with the
#     closing comment reading "Not closing as fixed"
#   * #399 and #403 and #384 open, but deleted from the file
#   * a milestone the file linked that did not exist
#   * "I queried all 24 issues the file names" — the file named 30
#
# Every one of those is mechanically detectable. This script detects them.
# It is NOT a CI gate: it needs network and a token, and a release should not be
# blocked by GitHub being slow. Run it before cutting an RC, and paste the output
# into the release PR.
#
# usage:  scripts/check-roadmap-issue-states.sh [ROADMAP.md]
# needs:  gh (authenticated), python3
set -uo pipefail

ROADMAP="${1:-ROADMAP.md}"
REPO="${REPO:-xerj-org/xerj}"

[ -f "$ROADMAP" ] || { echo "no such file: $ROADMAP" >&2; exit 2; }
command -v gh >/dev/null || { echo "gh not found" >&2; exit 2; }

echo "roadmap:  $ROADMAP"
echo "repo:     $REPO"
echo

refs=$(grep -oE '#[0-9]+' "$ROADMAP" | tr -d '#' | sort -un)
total=$(echo "$refs" | grep -c .)
echo "$refs" > /tmp/.roadmap-refs.$$

# Fetch state for each reference. `issues/N` answers for PRs too, and carries
# `pull_request` so the two can be told apart — the file mixes both, and a PR
# described as an issue is its own kind of wrong.
: > /tmp/.roadmap-state.$$
while read -r n; do
  [ -z "$n" ] && continue
  gh api "repos/$REPO/issues/$n" \
     --jq '[.number,(if .pull_request then "PR" else "ISSUE" end),.state,(.state_reason//"-"),(.title|.[0:60])]|@tsv' \
     2>/dev/null >> /tmp/.roadmap-state.$$ || echo -e "$n\tMISSING\t-\t-\t(not found)" >> /tmp/.roadmap-state.$$
done < /tmp/.roadmap-refs.$$

python3 - "$ROADMAP" /tmp/.roadmap-state.$$ <<'PY'
import re, sys, subprocess, json

roadmap, statefile = sys.argv[1], sys.argv[2]
text = open(roadmap).read()
rows = [l.rstrip("\n").split("\t") for l in open(statefile) if l.strip()]

issues = {int(r[0]): r for r in rows if r[1] == "ISSUE"}
prs    = {int(r[0]): r for r in rows if r[1] == "PR"}
missing= [r[0] for r in rows if r[1] == "MISSING"]

print(f"named: {len(rows)} refs = {len(issues)} issues + {len(prs)} PRs"
      + (f" + {len(missing)} NOT FOUND" if missing else ""))
print()

# Which section is each reference in? "retired"/"Items it retired" implies closed;
# the open-defects list implies open. Anything else is unclassified and only
# checked for existence.
def section_of(pos):
    heads = [(m.start(), m.group(0)) for m in re.finditer(r'^#{2,3} .*$', text, re.M)]
    cur = "(preamble)"
    for start, h in heads:
        if start > pos: break
        cur = h.strip()
    return cur

RETIRED = re.compile(r'items it retired', re.I)
OPENSEC = re.compile(r'open defects', re.I)
# A third form, and the one that recurred: an inline "still open" list outside
# either section. #275 sat under "Known members still open" on the GA gate and
# was missed by a classifier that only knew the two headings above.
STILL_OPEN = re.compile(r'(still open|members still open|remains? open)[^.\n]{0,400}$', re.I | re.S)

problems = []
# Only the SUBJECT of a bullet is a claim about that issue's state. A number
# appearing mid-sentence is a cross-reference — "acceptance criteria on #423" is
# not an assertion that #423 is retired — and treating those as claims produced
# false positives on the first run of this script.
lines = text.split("\n")
offsets, acc = [], 0
for ln in lines:
    offsets.append(acc); acc += len(ln) + 1

def bullet_subject(pos):
    i = max(k for k, off in enumerate(offsets) if off <= pos)
    # walk back to the start of this bullet (a line beginning with '- ')
    j = i
    while j >= 0 and not lines[j].lstrip().startswith("- "):
        if lines[j].strip() == "" or lines[j].startswith("#"):
            return None
        j -= 1
    if j < 0:
        return None
    head = " ".join(lines[j:i + 1])
    col = pos - offsets[j] if i == j else len(head)
    first = re.search(r'#(\d+)', head)
    return first and first.start() >= 0 and pos - offsets[j] <= offsets[i] - offsets[j] + 200 and \
           re.match(r'\s*-\s', lines[j]) is not None and \
           (re.search(r'#(\d+)', head).group(1) == str(int(re.search(r'#(\d+)', text[pos:pos+12]).group(1))))

for m in re.finditer(r'#(\d+)', text):
    n = int(m.group(1))
    if n not in issues:
        continue
    _, _, state, reason, title = issues[n]
    # An inline "still open: ... #N" is a direct claim about #N wherever it sits,
    # including mid-bullet where #N is not the subject. #275 was exactly that
    # shape and slipped past two reviews, so this is checked BEFORE the
    # cross-reference filter rather than after it.
    if state == "closed" and STILL_OPEN.search(text[max(0, m.start()-400):m.start()]):
        problems.append(("CLOSED-BUT-CALLED-STILL-OPEN", n, state, reason, title))
        continue

    if not bullet_subject(m.start()):
        continue          # cross-reference, not a state claim
    ctx = text[max(0, m.start()-1400):m.start()]
    in_retired = bool(RETIRED.search(ctx)) and not OPENSEC.search(ctx.split("Items it retired")[-1])
    in_open    = bool(OPENSEC.search(ctx)) and OPENSEC.search(ctx).start() > (RETIRED.search(ctx).start() if RETIRED.search(ctx) else -1)

    if in_open and state == "closed":
        problems.append(("CLOSED-BUT-LISTED-OPEN", n, state, reason, title))
    elif in_retired and state == "open":
        problems.append(("OPEN-BUT-LISTED-RETIRED", n, state, reason, title))
    elif in_retired and state == "closed" and reason == "not_planned":
        problems.append(("RETIRED-BUT-CLOSED-not_planned", n, state, reason, title))

seen = set()
uniq = [p for p in problems if not (p[1] in seen or seen.add(p[1]))]

if missing:
    print("REFERENCES THAT DO NOT RESOLVE:")
    for n in missing: print(f"  #{n}")
    print()

if uniq:
    print("MISMATCHES:")
    for kind, n, state, reason, title in uniq:
        print(f"  #{n:<5} {kind:<32} state={state}/{reason}  {title}")
    print()
else:
    print("no state mismatches detected in classified sections")
    print()

# Open issues the roadmap does not name at all. Not automatically wrong — the
# file is a roadmap, not an issue tracker — but the rc.18 cut dropped three open
# issues silently, so the number is worth seeing.
try:
    allopen = json.loads(subprocess.run(
        ["gh","api","repos/xerj-org/xerj/issues?state=open&per_page=100","--paginate",
         "--jq",'[.[]|select(.pull_request|not)|.number]'],
        capture_output=True, text=True, timeout=60).stdout or "[]")
    named = {int(x) for x in re.findall(r'#(\d+)', text)}
    absent = sorted(set(allopen) - named)
    print(f"open issues: {len(allopen)}   named here: {len(set(allopen)&named)}   ABSENT: {len(absent)}")
    if absent:
        print("  " + " ".join(f"#{n}" for n in absent))
except Exception as e:
    print(f"(could not enumerate open issues: {e})")

sys.exit(1 if (uniq or missing) else 0)
PY
rc=$?
rm -f /tmp/.roadmap-refs.$$ /tmp/.roadmap-state.$$
exit $rc
