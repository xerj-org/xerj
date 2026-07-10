#!/bin/bash
# usage: run_one.sh <a|b> <QN>
set -u
AGENT=$1; QN=$2
QTEXT=$(grep "^${QN}|" /tmp/xerj-autoindex/exam/questions.txt | cut -d'|' -f2-)
TDIR=/home/claude/ai/xerj-autoindex-wt/demo/usecases/autoindex/sim-transcripts
mkdir -p "$TDIR"
OUT="$TDIR/${QN}-agent-${AGENT}.jsonl"
if [ "$AGENT" = "a" ]; then
  CWD=/tmp/xerj-autoindex/sim-a
  PROMPT="A data folder was automatically indexed by 'xerj autoindex' into an Elasticsearch-compatible search engine at http://localhost:9274. You have NO filesystem access to the original folder — answer using ONLY the engine's HTTP API via curl. The indexed data lives in indices you can discover via GET /_cat/indices; there is also a catalog/data-map index describing every dataset and its fields. Question: ${QTEXT} Investigate with the API, then give a clear final answer."
else
  CWD=/tmp/xerj-discover/corpus
  PROMPT="You are in a data folder (your current working directory). Answer the question using ONLY the files under the current directory — you may use bash, grep, find, python3, or any local tool to read and analyze them. Do NOT access the network and do NOT read or modify anything outside the current directory. Question: ${QTEXT} Investigate the files, then give a clear final answer."
fi
cd "$CWD" || exit 9
START=$(date +%s.%N)
timeout 900 /home/claude/.local/bin/claude -p "$PROMPT" --output-format stream-json --verbose --max-turns 40 --allowedTools "Bash" --dangerously-skip-permissions > "$OUT" 2> "$OUT.err"
RC=$?
END=$(date +%s.%N)
echo "{\"agent\":\"$AGENT\",\"q\":\"$QN\",\"rc\":$RC,\"wall_s\":$(echo "$END $START" | awk '{printf "%.1f", $1-$2}')}" >> /tmp/xerj-autoindex/exam/runlog.jsonl
