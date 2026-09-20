#!/usr/bin/env python3
"""Why did BM25 return nothing for some queries? Count the lexical gap.

A BM25 arm that returns zero hits is either an engine defect or a query whose
words occur nowhere in the corpus. This separates the two WITHOUT a running
node: it tokenises the way a no-stemming standard analyzer does (lowercase,
split on non-alphanumerics) and counts the test queries that share no token
with any document's title or text.

    python3 lexical_gap.py nfcorpus

If the count equals the `empty=` figure eval.py printed for the BM25 arm, the
empty result lists are the dataset's lexical gap, not a bug.
"""
import csv, json, re, sys

def tok(s):
    return set(re.findall(r"[a-z0-9]+", s.lower()))

def main(root):
    vocab, docs = set(), 0
    for line in open(f"{root}/corpus.jsonl"):
        d = json.loads(line)
        docs += 1
        vocab |= tok(d.get("title", "")) | tok(d.get("text", ""))
    rows = list(csv.reader(open(f"{root}/qrels/test.tsv"), delimiter="\t"))[1:]
    test_ids = {r[0] for r in rows}
    queries = {}
    for line in open(f"{root}/queries.jsonl"):
        q = json.loads(line)
        if q["_id"] in test_ids:
            queries[q["_id"]] = q["text"]
    gap = sorted((i, q) for i, q in queries.items() if not (tok(q) & vocab))
    print(f"docs={docs}  vocab={len(vocab)}  test_queries={len(queries)}")
    print(f"test queries sharing NO token with any document: {len(gap)}")
    for i, q in gap:
        print(f"  {i}\t{q}")

if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "nfcorpus")
