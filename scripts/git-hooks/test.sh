#!/bin/sh
# Self-test for the XERJ identity-guard hooks.
#
# Builds a scratch repository, proves the failure mode exists WITHOUT the
# hooks (a leaked identity commits successfully), installs the hooks, and
# proves each guard: wrong author/committer refused, correct identity
# accepted, Claude co-author trailer refused.
#
# Exits 0 only if every assertion holds. No network, no shared state.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fails=0
check() { # check <desc> <expected 0|1> <actual-exit>
    if [ "$2" -eq 0 ] && [ "$3" -eq 0 ]; then echo "PASS: $1";
    elif [ "$2" -ne 0 ] && [ "$3" -ne 0 ]; then echo "PASS: $1";
    else echo "FAIL: $1 (expected exit $2, got $3)"; fails=$((fails+1)); fi
}

git init -q "$tmp/repo"
cd "$tmp/repo"
git config user.name xerj-org
git config user.email git@xerj.org

# 1. Without the hooks the leak is silent — this is the bug being fixed.
rc=0; git -c user.email=leaked@example.com -c user.name=xerj-org \
    commit -q --allow-empty -m "leak without hooks" || rc=$?
check "without hooks, a leaked identity commits (the pre-fix failure)" 0 "$rc"
leaked_email=$(git log -1 --format=%ae)
[ "$leaked_email" = "leaked@example.com" ] || { echo "FAIL: leak not recorded"; fails=$((fails+1)); }

# Install the hooks.
cp "$here/pre-commit" "$here/commit-msg" .git/hooks/
chmod +x .git/hooks/pre-commit .git/hooks/commit-msg

# 2. Wrong email via config — refused.
rc=0; git -c user.email=leaked@example.com commit -q --allow-empty -m x 2>/dev/null || rc=$?
check "wrong config email refused" 1 "$rc"

# 3. Wrong email via environment — refused.
rc=0; GIT_AUTHOR_EMAIL=leaked@example.com GIT_AUTHOR_NAME=xerj-org \
    git commit -q --allow-empty -m x 2>/dev/null || rc=$?
check "wrong env author refused" 1 "$rc"

# 4. Wrong committer via environment — refused.
rc=0; GIT_COMMITTER_EMAIL=leaked@example.com GIT_COMMITTER_NAME=xerj-org \
    git commit -q --allow-empty -m x 2>/dev/null || rc=$?
check "wrong env committer refused" 1 "$rc"

# 5. Wrong name, sanctioned email — refused.
rc=0; git -c user.name=somebody commit -q --allow-empty -m x 2>/dev/null || rc=$?
check "wrong name refused" 1 "$rc"

# 6. Claude co-author trailer — refused.
rc=0; git commit -q --allow-empty \
    -m "ok subject" -m "Co-Authored-By: Claude <noreply@anthropic.com>" 2>/dev/null || rc=$?
check "Claude co-author trailer refused" 1 "$rc"

# 7. Correct identity — accepted.
rc=0; git commit -q --allow-empty -m "good identity" || rc=$?
check "sanctioned identity accepted" 0 "$rc"

# 8. Noreply variant — accepted.
rc=0; git -c user.email=xerj-org@users.noreply.github.com \
    commit -q --allow-empty -m "noreply variant" || rc=$?
check "noreply variant accepted" 0 "$rc"

echo "---"
if [ "$fails" -eq 0 ]; then echo "identity-guard test: ALL PASS"; else
    echo "identity-guard test: $fails FAILURE(S)"; exit 1; fi
