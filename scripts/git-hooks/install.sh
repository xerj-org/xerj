#!/bin/sh
# Install the XERJ identity-guard hooks into this clone's shared .git/hooks.
#
# FOR xerj-org AGENT-FLEET MACHINES ONLY — enforces the xerj-org identity
# on every worktree of the clone (hooks are shared across worktrees when
# core.hooksPath is unset). Do not run on a personal contributor checkout.
#
# Usage: scripts/git-hooks/install.sh
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
git_common=$(git rev-parse --git-common-dir)

if hooks_path=$(git config --get core.hooksPath); then
    echo "install: core.hooksPath is set ($hooks_path); refusing to guess. Install manually." >&2
    exit 1
fi

for hook in pre-commit commit-msg; do
    dst="$git_common/hooks/$hook"
    if [ -e "$dst" ] && ! cmp -s "$here/$hook" "$dst"; then
        echo "install: $dst already exists and differs; not overwriting." >&2
        exit 1
    fi
    cp "$here/$hook" "$dst"
    chmod +x "$dst"
    echo "installed $dst"
done
