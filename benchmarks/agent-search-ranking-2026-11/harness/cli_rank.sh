#!/bin/bash
# usage: cli_rank.sh <binary> <prefix> <query> [k]
export XERJ_DISABLE_FEEDBACK=true
"$1" search --url http://localhost:22110 --prefix "$2" -k "${4:-6}" --json "$3" 2>/dev/null | python3 -I "$(dirname "$0")/fmt_hits.py"
