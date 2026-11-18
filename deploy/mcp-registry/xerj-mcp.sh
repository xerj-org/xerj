#!/bin/sh
# MCPB launcher for macOS and Linux: pick the bundled xerj binary for this
# CPU and run its MCP stdio server. Windows uses server/windows-x86_64/xerj.exe
# directly (manifest platform_overrides), so this script never runs there.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux) os=linux ;;
  *) echo "xerj-mcp: unsupported OS $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch=aarch64 ;;
  x86_64|amd64) arch=x86_64 ;;
  *) echo "xerj-mcp: unsupported CPU $(uname -m)" >&2; exit 1 ;;
esac
bin="$here/$os-$arch/xerj"
chmod +x "$bin" 2>/dev/null || true
exec "$bin" mcp "$@"
