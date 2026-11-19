#!/usr/bin/env bash
# =============================================================================
# mcp-schema-check.sh — pin the PUBLISHED MCP tool schema to the REAL binary
# =============================================================================
# `landing/docs/agents/schemas/mcp-tools.json` is what an agent reads to learn
# what XERJ's MCP server can do. It was hand-written, and it drifted: it
# advertised six tools while the binary served ten, missing every xerj_brain_*
# tool. Nothing compared them, so nothing failed.
#
# This script is the generator AND the gate. It boots the MCP server over
# stdio, issues a real `tools/list` JSON-RPC request, and either diffs the
# response against the published file (default) or writes it (--write).
#
# No XERJ node is required: `tools/list` is answered locally, before any
# proxying. That keeps this cheap enough to run on every CI push.
#
#   ./scripts/mcp-schema-check.sh            # check — non-zero on drift
#   ./scripts/mcp-schema-check.sh --write    # regenerate from the binary
#
# Per release (#3286): `mcp-tools.json` follows main, so an agent on an
# installed release read a tool list its binary did not serve. `--release`
# targets `mcp-tools-<version>.json` instead, where <version> is the
# `serverInfo.version` the binary itself reports on `initialize` — the file is
# named by the binary that produced it, never by a hand-typed argument.
#
#   ./scripts/mcp-schema-check.sh --release --write   # write mcp-tools-<v>.json
#   ./scripts/mcp-schema-check.sh --release           # check it against the binary
#
# A released file is frozen: `--release --write` refuses to replace an
# existing mcp-tools-<v>.json whose content differs (exit 1), because that
# release's binary cannot change. The release workflow runs both against the
# x86_64-unknown-linux-gnu archive it is about to publish.
#
# Environment:
#   XERJ_BIN   path to the main `xerj` binary (default:
#              engine/target/release/xerj). The server is driven as
#              `xerj mcp`, i.e. exactly the path an installed user has.
#   XERJ_MCP_BIN
#              path to the standalone `xerj-mcp` binary. If set, it is driven
#              instead, with no subcommand argument.
# =============================================================================
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
SCHEMA_DIR="$REPO/landing/docs/agents/schemas"
SCHEMA="$SCHEMA_DIR/mcp-tools.json"

MODE="check"
RELEASE=0
for arg in "$@"; do
  case "$arg" in
    --write) MODE="write" ;;
    --check) MODE="check" ;;
    --release) RELEASE=1 ;;
    *) echo "usage: $(basename "$0") [--check|--write] [--release]" >&2; exit 2 ;;
  esac
done

command -v jq >/dev/null 2>&1 || { echo "FAIL: jq is required" >&2; exit 2; }

# Drive the installed-shape path by default: the one binary, `xerj mcp`.
if [ -n "${XERJ_MCP_BIN:-}" ]; then
  BIN="$XERJ_MCP_BIN"; ARGS=()
  LABEL="$BIN (standalone)"
else
  BIN="${XERJ_BIN:-$REPO/engine/target/release/xerj}"; ARGS=(mcp)
  LABEL="$BIN mcp"
fi

if [ ! -x "$BIN" ]; then
  echo "FAIL: MCP server binary not found at $BIN"
  echo "      build it: (cd $REPO/engine && cargo build --release -j 12 -p xerj-server)"
  exit 2
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "── mcp schema $MODE · $LABEL ──"

# Line-delimited JSON-RPC over stdio. Closing stdin after the request is what
# shuts a stdio MCP server down, so no timeout-kill is needed for the exit —
# `timeout` stays as a hang guard only.
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"mcp-schema-check","version":"0"}}}'
  printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
# `${ARGS[@]+…}` — an empty array under `set -u` is an unbound-variable error
# on bash 3.2, which is still /bin/bash on macOS.
} | timeout 30 "$BIN" ${ARGS[@]+"${ARGS[@]}"} > "$WORK/out.jsonl" 2> "$WORK/err.log"

if [ ! -s "$WORK/out.jsonl" ]; then
  echo "FAIL: no JSON-RPC output from '$LABEL'; stderr:"
  sed 's/^/  /' "$WORK/err.log"
  exit 1
fi

if ! jq -s -e '[.[] | select(.id == 2)] | length == 1' "$WORK/out.jsonl" >/dev/null 2>&1; then
  echo "FAIL: no tools/list response (id 2) in the stream:"
  sed 's/^/  /' "$WORK/out.jsonl"
  exit 1
fi

# The published envelope is {"tools":[...]} — preserve it exactly.
#
# Key order: the server serialises with serde_json's default BTreeMap ordering,
# so it emits `description` before `name`. Lift name/description/inputSchema to
# the front for a reader, losslessly — `{…} + .` keeps every value from the
# original object and appends any key not named here, so a new field can never
# be dropped by this reshaping. Order is cosmetic to both checkers (the diff
# below canonicalises with `jq -S`; the Rust test compares parsed values).
jq -s --indent 2 '{
    tools: ([.[] | select(.id == 2)] | .[0].result.tools
            | map({ name: .name, description: .description, inputSchema: .inputSchema } + .))
  }' "$WORK/out.jsonl" > "$WORK/live.json" || { echo "FAIL: could not extract tools"; exit 1; }

COUNT=$(jq '.tools | length' "$WORK/live.json")
echo "  served: $COUNT tools — $(jq -r '.tools[].name' "$WORK/live.json" | tr '\n' ' ')"

if [ "$COUNT" -lt 1 ]; then
  echo "FAIL: the server listed no tools"
  exit 1
fi

SCHEMA_LABEL="landing/docs/agents/schemas/mcp-tools.json"
if [ "$RELEASE" = 1 ]; then
  VERSION=$(jq -rs '[.[] | select(.id == 1)] | .[0].result.serverInfo.version // empty' "$WORK/out.jsonl")
  # A version becomes part of a published path: semver-shaped only.
  if ! printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$'; then
    echo "FAIL: the binary reported no usable serverInfo.version on initialize (got '${VERSION}')"
    exit 1
  fi
  SCHEMA="$SCHEMA_DIR/mcp-tools-$VERSION.json"
  SCHEMA_LABEL="landing/docs/agents/schemas/mcp-tools-$VERSION.json"
  echo "  release: $VERSION -> $SCHEMA_LABEL"
  if [ "$MODE" = "write" ] && [ -f "$SCHEMA" ]; then
    if [ "$(jq -S . "$SCHEMA" 2>/dev/null)" = "$(jq -S . "$WORK/live.json")" ]; then
      echo "RESULT: PASS — $SCHEMA_LABEL already matches the binary ($COUNT tools)"
      exit 0
    fi
    echo "FAIL: $SCHEMA_LABEL already exists and differs from this binary."
    echo "      A released schema is frozen; a binary reporting $VERSION but serving other"
    echo "      tools is a different build. Delete the file by hand only if it was never released."
    exit 1
  fi
fi

if [ "$MODE" = "write" ]; then
  # jq already terminates its output with a newline — do not add a second one.
  cp "$WORK/live.json" "$SCHEMA"
  echo "  wrote $SCHEMA"
  echo "RESULT: WROTE ($COUNT tools)"
  exit 0
fi

if [ ! -f "$SCHEMA" ]; then
  echo "FAIL: published schema missing at $SCHEMA — regenerate with --write"
  exit 1
fi

# Compare canonically (sorted keys) so formatting is never the thing that
# fails, but every name, description and schema field is.
jq -S . "$SCHEMA"          > "$WORK/a.json" 2>/dev/null || { echo "FAIL: $SCHEMA is not valid JSON"; exit 1; }
jq -S . "$WORK/live.json"  > "$WORK/b.json"

if diff -u "$WORK/a.json" "$WORK/b.json" > "$WORK/diff.txt"; then
  echo "RESULT: PASS — published schema matches the binary ($COUNT tools)"
  exit 0
fi

echo "FAIL: $SCHEMA_LABEL has DRIFTED from the binary."
echo "      (-published  +served)"
sed -n '1,120p' "$WORK/diff.txt" | sed 's/^/  /'
if [ "$RELEASE" = 1 ]; then
  echo "      a released schema is frozen: check which binary this is before changing it"
else
  echo "      regenerate: ./scripts/mcp-schema-check.sh --write"
fi
echo "RESULT: FAIL"
exit 1
