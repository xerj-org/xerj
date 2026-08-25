#!/usr/bin/env bash
# Usage: run_arm.sh <arm: A|B|C|BASE> <question_id> <repeat_n>
set -euo pipefail
B="$(cd "$(dirname "$0")" && pwd)"
CORPUS=/home/claude/.xerj-code/corpora/lucene-meta/lucene
ARM="$1"; QID="${2:-}"; REP="${3:-1}"
SID=$(python3 -c "import uuid;print(uuid.uuid4())")

Q=$(python3 -c "
import json,sys
d=json.load(open('$B/questions.json'))
print(next(q['q'] for q in d['questions'] if q['id']=='$QID'))
" 2>/dev/null || echo "Reply with exactly: ok")

XERJ_PROMPT='You are answering questions about a Java codebase. You have NO filesystem access to it.
The ONLY way to see code is the XERJ index, an Elasticsearch-compatible endpoint at
http://localhost:9200/refsym/_search . Query it with curl and standard ES DSL.
Fields: repo, path, file, language, kind (method|class|interface|enum|...), name (keyword),
name_text (text), line, end_line, loc, sig, doc, code. The `code` field holds the full
symbol body, so a hit already contains the implementation - there is nothing to open afterwards.
Always request _source so you get path and line numbers to cite.
Answer from retrieval results only. If retrieval does not contain the answer, say so explicitly
rather than guessing.'

COMMON=(--output-format json --session-id "$SID" --permission-mode bypassPermissions
        --max-budget-usd 3 --model opus --effort medium)

case "$ARM" in
  A)  # native: the tools a real Claude Code user has, no XERJ, no hints
      cd "$CORPUS"
      claude -p "$Q" "${COMMON[@]}" \
        --allowedTools "Bash Read Grep Glob" \
        --disallowedTools "WebSearch WebFetch Edit Write" \
        > "$B/runs/${QID}_A_${REP}.json" ;;
  B)  # retrieval-only: XERJ is the sole source of code. No grep, no file reads.
      cd /home/claude
      claude -p "$Q" "${COMMON[@]}" \
        --append-system-prompt "$XERJ_PROMPT" \
        --allowedTools "Bash" \
        --disallowedTools "Read Grep Glob Edit Write WebSearch WebFetch" \
        --add-dir /home/claude \
        > "$B/runs/${QID}_B_${REP}.json" ;;
  C)  # realistic: XERJ available AND the normal toolset as fallback
      cd "$CORPUS"
      claude -p "$Q" "${COMMON[@]}" \
        --append-system-prompt "$XERJ_PROMPT Standard file tools are also available as a fallback." \
        --allowedTools "Bash Read Grep Glob" \
        --disallowedTools "WebSearch WebFetch Edit Write" \
        > "$B/runs/${QID}_C_${REP}.json" ;;
  BASE_A) cd "$CORPUS"
      claude -p "Reply with exactly: ok" "${COMMON[@]}" \
        --allowedTools "Bash Read Grep Glob" --disallowedTools "WebSearch WebFetch Edit Write" \
        > "$B/runs/baseline_A.json" ;;
  BASE_B) cd /home/claude
      claude -p "Reply with exactly: ok" "${COMMON[@]}" \
        --append-system-prompt "$XERJ_PROMPT" --allowedTools "Bash" \
        --disallowedTools "Read Grep Glob Edit Write WebSearch WebFetch" \
        > "$B/runs/baseline_B.json" ;;
esac
echo "$SID"
