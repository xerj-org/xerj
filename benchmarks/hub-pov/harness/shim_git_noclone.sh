#!/usr/bin/env bash
# `git` shim for arm A2 (normal agent + web ONLY): PLAN.md §3 says A2 "may not
# `git clone` the corpus source". Refuses every verb that fetches a repository and
# logs the attempt to $HOME/.pov/git-refused.log (the contamination check reads it);
# anything else is passed to the real git. A2's prompt states the rule, so a refusal
# is the agent ignoring it, not a trap.
case " $* " in
  *" clone "*|*" fetch "*|*" pull "*|*" ls-remote "*|*" submodule "*|*" archive "*--remote*)
    mkdir -p "$HOME/.pov"; echo "$(date -u +%FT%TZ) $*" >> "$HOME/.pov/git-refused.log"
    echo "git: fetching repositories is not allowed in this environment (web only)" >&2
    exit 128 ;;
esac
exec /usr/bin/git "$@"
