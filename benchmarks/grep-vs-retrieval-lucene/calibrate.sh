#!/usr/bin/env bash
# Measures chars-per-token for THIS corpus, by sending two identical prompts that
# differ only by a known number of characters of real Lucene source and diffing the
# cache_creation_input_tokens the CLI bills. No API key and no tokenizer needed.
#
# Result recorded 2026-08-18: 20000 chars -> 7181 tokens => 2.785 chars/token.
# (The common chars/4 rule understates Java source token counts by ~30%.)
set -euo pipefail
L=/home/claude/.xerj-code/corpora/lucene-meta/lucene/lucene
SRC=$L/core/src/java/org/apache/lucene/util/hnsw/HnswGraphBuilder.java
TMP=$(mktemp -d)
for n in 2000 22000; do
  { printf 'Reply with only the word OK. Ignore this reference material:\n<ref>\n'
    head -c $n "$SRC"; printf '\n</ref>\n'; } > "$TMP/p$n.txt"
  claude -p "$(cat "$TMP/p$n.txt")" --output-format json \
    | python3 -c "import sys,json;u=json.load(sys.stdin)['usage'];print($n,u['cache_creation_input_tokens'])"
done
echo "chars_per_token = (22000-2000) / (cc_22000 - cc_2000)"
