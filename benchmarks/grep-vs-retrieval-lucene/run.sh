#!/usr/bin/env bash
# run.sh -- execute the head-to-head. Usage:
#   ./run.sh <arm> <question_id|all> [repeat_index]
#   ./run.sh all all            # full matrix, 3 arms x 10 questions x REPEATS
#
# Arms:
#   native   grep/glob/read on the real checkout. No index. What a Claude Code user has today.
#   xerj     retrieval ONLY (xq). No file tools at all. Strict test of "can the index alone answer".
#   hybrid   both xq and file tools; the model chooses. The realistic product experience.
#
# Every run writes a raw stream-json transcript to runs/. Nothing is summarised here --
# summarising happens in analyze.py, from the transcripts, so the raw evidence survives.
set -uo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LUCENE=/home/claude/.xerj-code/corpora/lucene-meta/lucene
CLAUDE=/home/claude/.local/bin/claude
REPEATS="${REPEATS:-3}"
export PATH="$BENCH:$PATH"

# A neutral empty cwd for the xerj arm: it must NOT be able to fall back to the tree.
SANDBOX="$BENCH/.sandbox"; mkdir -p "$SANDBOX"

RULES='Answer the question about the Apache Lucene source. Rules: (1) Answer ONLY from what you actually retrieve or read in this session - do NOT answer from prior knowledge of Lucene, because this checkout may differ from the version you remember. (2) Cite file path and line number for every fact. (3) Be concise: the answer, then the citation. (4) If you cannot find it, say NOT FOUND.'

run_one () {
  local arm="$1" qid="$2" rep="$3"
  local q; q=$(python3 -c "
import json,sys
for l in open('$BENCH/questions.jsonl'):
    d=json.loads(l)
    if d['id']=='$qid': print(d['ask']); break
")
  [ -z "$q" ] && { echo "unknown question $qid" >&2; return 1; }
  local out="$BENCH/runs/${arm}__${qid}__r${rep}.jsonl"
  [ -s "$out" ] && { echo "skip (exists) $out"; return 0; }

  case "$arm" in
    native)
      ( cd "$LUCENE" && $CLAUDE -p "$RULES

QUESTION: $q" \
          --output-format stream-json --verbose \
          --allowedTools "Grep,Glob,Read" \
          --disallowedTools "Bash,WebSearch,WebFetch,Task" ) > "$out" 2>"$BENCH/logs/${arm}__${qid}__r${rep}.err"
      ;;
    xerj)
      ( cd "$SANDBOX" && $CLAUDE -p "$RULES

You have NO access to the source tree. You have exactly one retrieval tool, a shell command:
  xq \"<free text query>\" [k]
It searches a prebuilt symbol-level index of this Lucene checkout and returns matching
symbols as: path:line, kind, signature, and the symbol body. Call it via Bash. Query it
as many times as you need, refining your query. Note the index contains backward-codecs
and test copies of many classes, so check the path of each hit before trusting it.

QUESTION: $q" \
          --output-format stream-json --verbose \
          --allowedTools "Bash(xq:*)" \
          --disallowedTools "Grep,Glob,Read,WebSearch,WebFetch,Task" ) > "$out" 2>"$BENCH/logs/${arm}__${qid}__r${rep}.err"
      ;;
    hybrid)
      ( cd "$LUCENE" && $CLAUDE -p "$RULES

In addition to the normal file tools you have a prebuilt symbol index of this checkout,
queried by the shell command:
  xq \"<free text query>\" [k]
which returns path:line, kind, signature and body for matching symbols. Use whichever
approach you judge cheapest for this question.

QUESTION: $q" \
          --output-format stream-json --verbose \
          --allowedTools "Grep,Glob,Read,Bash(xq:*)" \
          --disallowedTools "WebSearch,WebFetch,Task" ) > "$out" 2>"$BENCH/logs/${arm}__${qid}__r${rep}.err"
      ;;
    *) echo "unknown arm $arm" >&2; return 1;;
  esac
  echo "wrote $out"
}

ARM="${1:-all}"; QID="${2:-all}"
ARMS=$([ "$ARM" = all ] && echo "native xerj hybrid" || echo "$ARM")
QIDS=$([ "$QID" = all ] && python3 -c "
import json
print(' '.join(json.loads(l)['id'] for l in open('$BENCH/questions.jsonl')))" || echo "$QID")

for r in $(seq 1 "$REPEATS"); do
  for a in $ARMS; do
    for q in $QIDS; do
      run_one "$a" "$q" "$r"
    done
  done
done
