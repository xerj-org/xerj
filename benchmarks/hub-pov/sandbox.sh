#!/usr/bin/env bash
# sandbox.sh — the bubblewrap jail every Corpus-Hub-v2 proof-of-value arm runs in.
#
# WHY THIS EXISTS (read before changing it):
#   On 2026-10-13 an UNSANDBOXED headless benchmark agent, cleaning up the node it
#   had started, ran `pkill -x xerj`. Because it shared the host PID namespace, that
#   also killed the user's long-running reference-index node (ports 9297-9299),
#   which other sessions depend on. This recipe makes that impossible:
#     * --unshare-pid gives the arm its OWN PID namespace, so `pgrep`/`pkill` inside
#       see only the arm's own processes — never a host process.
#     * --ro-bind / / makes the whole filesystem READ-ONLY except the one fresh HOME
#       we bind writable, so an arm cannot modify the host, the repo, or another arm.
#     * --die-with-parent tears the jail down if the harness dies.
#   Every `claude -p` arm (A0-A7 in PLAN.md) MUST be launched through this script.
#   Network is deliberately NOT unshared — arms need it (git clone, web fetch, the
#   hub pull, the search APIs). Isolation here is of the filesystem and the process
#   table, not the network.
#
# USAGE:
#   benchmarks/hub-pov/sandbox.sh [--home DIR] [--rm-home] -- CMD [ARGS...]
#     --home DIR   use DIR as the arm's fresh HOME (default: a new `mktemp -d`).
#                  Must NOT be the real user's home; the script refuses that.
#     --rm-home    delete the HOME on exit (default: keep it so the harness can
#                  collect the transcript/artifacts, then clean up itself).
#   Everything after `--` is the command run inside the jail, e.g.
#     sandbox.sh --home "$cell" -- claude -p --output-format stream-json "$prompt"
#   The resolved HOME is printed to stderr as `sandbox-home: <dir>` for the harness.
#
# ENV (optional, read from the host before the jail clears the environment):
#   CLAUDE_CONFIG_DIR     auth/config dir, bound writable (default /home/claude/.claude).
#                         Bound writable because the CLI refreshes its token in place;
#                         do NOT copy .credentials.json into the HOME instead — a copy
#                         can refresh out from under the main session and log it out.
#   XERJ_POV_PATH_PREFIX  dir prepended to PATH inside the jail (e.g. the logging
#                         `xerj` shim that counts invocations for the XERJ arms).
#   No host secret is forwarded: the jail starts with --clearenv and we set back only
#   HOME, CLAUDE_CONFIG_DIR, PATH, TERM, LANG. GH_TOKEN / ANTHROPIC_API_KEY are NOT
#   passed in — arms authenticate via CLAUDE_CONFIG_DIR, and the LLM judge runs
#   OUTSIDE the sandbox (see PLAN.md §6), so the judge key never enters an arm.
set -euo pipefail

CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-/home/claude/.claude}"
XERJ_POV_PATH_PREFIX="${XERJ_POV_PATH_PREFIX:-}"
SANDBOX_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
HOME_DIR=""
RM_HOME=0

# ---- parse args ----------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --home)    HOME_DIR="${2:?--home needs a dir}"; shift 2 ;;
    --rm-home) RM_HOME=1; shift ;;
    --)        shift; break ;;
    *) echo "sandbox.sh: unknown arg '$1' (did you forget '--' before the command?)" >&2; exit 2 ;;
  esac
done
if [ $# -eq 0 ]; then
  echo "sandbox.sh: no command after '--'" >&2; exit 2
fi

# ---- preconditions (fail closed) -----------------------------------------
command -v bwrap >/dev/null 2>&1 || { echo "sandbox.sh: bwrap (bubblewrap) not found — refusing to run an arm unsandboxed" >&2; exit 3; }

if [ -z "$HOME_DIR" ]; then
  HOME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/hub-pov-home.XXXXXX")"
fi
mkdir -p "$HOME_DIR"
HOME_DIR="$(cd "$HOME_DIR" && pwd -P)"   # absolute, resolved

# Guard: the HOME must be a fresh dir, never the real home or the filesystem root.
# Binding the real home writable would defeat the whole point of --ro-bind / /.
case "$HOME_DIR" in
  / | /home/claude | /home/claude/ | /root | /root/ | "${HOME:-/nonexistent}")
    echo "sandbox.sh: refusing to use '$HOME_DIR' as a scratch HOME (not a fresh dir)" >&2; exit 3 ;;
esac
mkdir -p "$HOME_DIR/work"
echo "sandbox-home: $HOME_DIR" >&2

PATH_IN="$SANDBOX_PATH"
[ -n "$XERJ_POV_PATH_PREFIX" ] && PATH_IN="$XERJ_POV_PATH_PREFIX:$PATH_IN"

# ---- the jail ------------------------------------------------------------
# ro-bind the whole host read-only; carve out exactly two writable spots:
#   - the fresh HOME (the arm's whole writable world, cwd = $HOME/work)
#   - the auth/config dir (token refresh writes here; kept as narrow as possible)
# Fresh tmpfs /tmp and minimal /dev /proc; --unshare-pid for the private PID ns;
# --clearenv so no host env (secrets, GH_TOKEN) leaks in, then set back the minimum.
BWRAP=(bwrap
  --ro-bind / /
  --dev /dev
  --proc /proc
  --tmpfs /tmp
  --bind "$HOME_DIR" "$HOME_DIR"
  --bind "$CLAUDE_CONFIG_DIR" "$CLAUDE_CONFIG_DIR"
  --unshare-pid
  --die-with-parent
  --new-session
  --chdir "$HOME_DIR/work"
  --clearenv
  --setenv HOME "$HOME_DIR"
  --setenv CLAUDE_CONFIG_DIR "$CLAUDE_CONFIG_DIR"
  --setenv PATH "$PATH_IN"
  --setenv TERM xterm
  --setenv LANG C.UTF-8)

# When --rm-home is set we must run bwrap as a child (not exec) so the EXIT trap
# that deletes the HOME can fire; otherwise exec for clean signal propagation.
if [ "$RM_HOME" -eq 1 ]; then
  trap 'rm -rf "$HOME_DIR"' EXIT
  rc=0; "${BWRAP[@]}" "$@" || rc=$?
  exit "$rc"
else
  exec "${BWRAP[@]}" "$@"
fi
