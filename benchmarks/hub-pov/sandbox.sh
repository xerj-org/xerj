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
#
# WHAT THE 2026-10-17 SMOKE RUN ADDED (SMOKE.md "harness defects"), each one a way
# an arm could leak, be contaminated, or touch the host that the first version left
# open:
#   * PRIVATE NETWORK NAMESPACE (default `--net isolated`). The jail runs inside
#     `pasta` (user-mode networking: the arm keeps internet access) with NO port
#     forwarding in either direction and no gateway->host-loopback mapping, plus an
#     nftables rule rejecting RFC1918/CGNAT/link-local destinations. Without this an
#     arm shares the host's loopback: `xerj ask` probes http://localhost:9200 FIRST
#     and uses whatever answers — on this host that is the user's reference-coding
#     node, so a v2 arm would have restored a pack INTO the user's node and answered
#     from its corpora (contamination and a write to a node we must never touch).
#     Inside the namespace :9200 is the arm's own, so XERJ arms can bind it safely.
#   * /home and /root are MASKED (tmpfs); only the HOME, the auth dir and the paths
#     passed with --ro are visible. With a bare `--ro-bind / /` every arm could read
#     the gold answers (benchmarks/corpus-tasks, prior run dirs), the local corpus
#     clones in ~/.xerj-code, other sessions' transcripts, and secrets in .env files.
#   * The CLAUDE_CONFIG_DIR is bound writable (token refresh) but everything in it
#     except `.credentials.json` and `.claude.json` is masked: the user's global
#     CLAUDE.md, skills, plugins, hooks, transcripts and history would otherwise be
#     loaded into or readable by every arm, and settings.json `env` would inject the
#     user's GH_TOKEN into the arm despite --clearenv. settings.json is replaced by `{}`.
#   * Inbound bytes are counted per jail (nft counters): all non-loopback bytes, and
#     the subset from the model API (Anthropic, 160.79.104.0/21) so the harness can
#     report "bytes downloaded" net of model traffic. Written to
#     $HOME/.sandbox/net.json when the jail exits.
#
# USAGE:
#   benchmarks/hub-pov/sandbox.sh [--home DIR] [--rm-home] [--ro PATH]...
#                                 [--net isolated|host] -- CMD [ARGS...]
#     --home DIR   use DIR as the arm's fresh HOME (default: a new `mktemp -d`).
#                  Must NOT be the real user's home; the script refuses that.
#     --rm-home    delete the HOME on exit (default: keep it so the harness can
#                  collect the transcript/artifacts, then clean up itself).
#     --ro PATH    expose a host PATH read-only at the same path (repeatable). Needed
#                  for anything under /home (the claude binary, the xerj binary, shim
#                  dirs, the catalogue override). NEVER pass a dir holding gold answers.
#     --net MODE   `isolated` (default; needs pasta + nft) or `host` (the old shared
#                  network — only for debugging the jail itself, never for an arm).
#   Everything after `--` is the command run inside the jail.
#   The resolved HOME is printed to stderr as `sandbox-home: <dir>` for the harness.
#
# ENV (optional, read from the host before the jail clears the environment):
#   CLAUDE_CONFIG_DIR     auth/config dir, bound writable (default /home/claude/.claude).
#                         Bound writable because the CLI refreshes its token in place;
#                         do NOT copy .credentials.json into the HOME instead — a copy
#                         can refresh out from under the main session and log it out.
#   XERJ_POV_PATH_PREFIX  dir(s, ':'-separated) prepended to PATH inside the jail (e.g.
#                         the logging `xerj` shim). Each is exposed read-only.
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
NET=isolated
IN_NETNS=0
RO_PATHS=()
ORIG_ARGS=("$@")

# ---- parse args ----------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --home)    HOME_DIR="${2:?--home needs a dir}"; shift 2 ;;
    --rm-home) RM_HOME=1; shift ;;
    --ro)      RO_PATHS+=("${2:?--ro needs a path}"); shift 2 ;;
    --net)     NET="${2:?--net needs isolated|host}"; shift 2 ;;
    --_in-netns) IN_NETNS=1; shift ;;   # internal: re-exec'd inside pasta
    --)        shift; break ;;
    *) echo "sandbox.sh: unknown arg '$1' (did you forget '--' before the command?)" >&2; exit 2 ;;
  esac
done
if [ $# -eq 0 ]; then
  echo "sandbox.sh: no command after '--'" >&2; exit 2
fi
case "$NET" in isolated|host) ;; *) echo "sandbox.sh: --net must be isolated or host" >&2; exit 2 ;; esac

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
  / | /home | /home/ | /home/claude | /home/claude/ | /root | /root/ | "${HOME:-/nonexistent}")
    echo "sandbox.sh: refusing to use '$HOME_DIR' as a scratch HOME (not a fresh dir)" >&2; exit 3 ;;
esac

# ---- network namespace (re-exec under pasta) ------------------------------
if [ "$NET" = isolated ] && [ "$IN_NETNS" -eq 0 ]; then
  command -v pasta >/dev/null 2>&1 || { echo "sandbox.sh: pasta not found — refusing to run an arm on the host network (use --net host only to debug the jail)" >&2; exit 3; }
  command -v nft >/dev/null 2>&1 || { echo "sandbox.sh: nft not found — refusing to run an arm without the private-range block" >&2; exit 3; }
  # -t/-u none: nothing forwarded host->ns.  -T/-U none: nothing forwarded ns->host
  # loopback (pasta's default `auto` would forward every port the host has bound on
  # 127.0.0.1, i.e. exactly the reference node).  --no-map-gw: the gateway address
  # does not lead to the host either.
  # pasta puts us in a user namespace as uid 0; remember the real ids so the jail
  # maps back to them (claude refuses --dangerously-skip-permissions as root).
  export SANDBOX_HOST_UID="$(id -u)" SANDBOX_HOST_GID="$(id -g)"
  exec pasta --config-net -q -t none -u none -T none -U none --no-map-gw \
    -- bash "$(realpath "$0")" --_in-netns --home "$HOME_DIR" "${ORIG_ARGS[@]}"
fi

mkdir -p "$HOME_DIR/work" "$HOME_DIR/.sandbox"
echo "sandbox-home: $HOME_DIR" >&2

if [ "$IN_NETNS" -eq 1 ]; then
  # We are root in pasta's user+net namespace: block private destinations (another
  # route to a host service bound on 0.0.0.0, e.g. via docker0) and count bytes.
  nft -f - <<'EOF'
table inet povjail {
  counter rx_all {}
  counter rx_model_api {}
  chain out {
    type filter hook output priority 0; policy accept;
    oifname "lo" accept
    ip daddr { 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 100.64.0.0/10, 169.254.0.0/16 } reject
    ip6 daddr { fc00::/7, fe80::/10 } reject
  }
  chain in {
    type filter hook input priority 0; policy accept;
    iifname "lo" accept
    counter name rx_all
    ip saddr 160.79.104.0/21 counter name rx_model_api
  }
}
EOF
  # The host's resolv.conf points at systemd-resolved on 127.0.0.53, which is the
  # HOST loopback and unreachable from here: give the jail public resolvers.
  printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' > "$HOME_DIR/.sandbox/resolv.conf"
fi

PATH_IN="$SANDBOX_PATH"
[ -n "$XERJ_POV_PATH_PREFIX" ] && PATH_IN="$XERJ_POV_PATH_PREFIX:$PATH_IN"

# ---- the jail ------------------------------------------------------------
# ro-bind the whole host read-only; mask /home and /root; carve out:
#   - the fresh HOME (the arm's whole writable world, cwd = $HOME/work)
#   - the auth/config dir (token refresh writes here), with its contents masked
#   - the --ro paths and the PATH-prefix dirs, read-only
# Fresh tmpfs /tmp and minimal /dev /proc; --unshare-pid for the private PID ns;
# --clearenv so no host env (secrets, GH_TOKEN) leaks in, then set back the minimum.
BWRAP=(bwrap
  --ro-bind / /
  --dev /dev
  --proc /proc
  --tmpfs /tmp
  --tmpfs /home
  --tmpfs /root)
if [ "$IN_NETNS" -eq 1 ]; then
  BWRAP+=(--ro-bind "$HOME_DIR/.sandbox/resolv.conf" "$(realpath /etc/resolv.conf)")
fi
BWRAP+=(--bind "$HOME_DIR" "$HOME_DIR"
  --bind "$CLAUDE_CONFIG_DIR" "$CLAUDE_CONFIG_DIR")
# Mask the config dir's contents except the two files auth needs.
printf '{}\n' > "$HOME_DIR/.sandbox/settings.json"
: > "$HOME_DIR/.sandbox/empty"   # a regular empty file: a /dev/null bind reads as EACCES here
for entry in "$CLAUDE_CONFIG_DIR"/* "$CLAUDE_CONFIG_DIR"/.[!.]*; do
  [ -e "$entry" ] || continue
  case "$(basename "$entry")" in
    .credentials.json|.claude.json) continue ;;
    settings.json) BWRAP+=(--ro-bind "$HOME_DIR/.sandbox/settings.json" "$entry") ;;
    *) if [ -d "$entry" ] && [ ! -L "$entry" ]; then BWRAP+=(--tmpfs "$entry")
       elif [ -f "$entry" ] && [ ! -L "$entry" ]; then BWRAP+=(--ro-bind "$HOME_DIR/.sandbox/empty" "$entry"); fi ;;
  esac
done
IFS=: read -r -a prefix_dirs <<< "$XERJ_POV_PATH_PREFIX"
for p in "${RO_PATHS[@]}" "${prefix_dirs[@]}"; do
  [ -n "$p" ] || continue
  [ -e "$p" ] || { echo "sandbox.sh: --ro path does not exist: $p" >&2; exit 2; }
  BWRAP+=(--ro-bind "$p" "$p")
done
if [ "$IN_NETNS" -eq 1 ]; then
  BWRAP+=(--unshare-user --uid "${SANDBOX_HOST_UID:?}" --gid "${SANDBOX_HOST_GID:?}")
fi
BWRAP+=(--unshare-pid
  --die-with-parent
  --new-session
  --chdir "$HOME_DIR/work"
  --clearenv
  --setenv HOME "$HOME_DIR"
  --setenv CLAUDE_CONFIG_DIR "$CLAUDE_CONFIG_DIR"
  --setenv PATH "$PATH_IN"
  --setenv TERM xterm
  --setenv LANG C.UTF-8)

write_counters() {
  [ "$IN_NETNS" -eq 1 ] || return 0
  nft -j list counters table inet povjail > "$HOME_DIR/.sandbox/net.json" 2>/dev/null || true
}

# Run bwrap as a child (not exec) when we must act after it: delete the HOME
# (--rm-home) or read the namespace's byte counters.
if [ "$RM_HOME" -eq 1 ] || [ "$IN_NETNS" -eq 1 ]; then
  [ "$RM_HOME" -eq 1 ] && trap 'rm -rf "$HOME_DIR"' EXIT
  rc=0; "${BWRAP[@]}" "$@" || rc=$?
  write_counters
  exit "$rc"
else
  exec "${BWRAP[@]}" "$@"
fi
