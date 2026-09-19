"""Turn first-stage lists into pair files for `pair_score`.
usage: make_pairs.py <dataset_dir> <first_stage.json> <out_prefix> [max_doc_chars] [windows]

Writes <out_prefix>.w30.jsonl   — BM25 top-30 per query, one group per query: one group is one
                                  30-document rerank call, which is what the latency figures time
       <out_prefix>.rest.jsonl  — every other (query, document) pair any arm needs, scored once:
                                  hybrid top-W and BM25 top-W for the widest window asked for
`windows` is a comma list (default "30,100"); "30" alone skips the 100-document arms."""
import json, sys
from beirlib import queries, corpus

ds, fs, prefix = sys.argv[1:4]
clip = int(sys.argv[4]) if len(sys.argv) > 4 else 4000
windows = [int(w) for w in (sys.argv[5] if len(sys.argv) > 5 else "30,100").split(",")]
W = max(windows)
run, Q, C = json.load(open(fs)), queries(ds), corpus(ds)


def doc(did):
    title, text = C[did]
    # Exactly `xerj_rerank::local::document_text`: title, newline, text, each clipped.
    return (title[:clip] + "\n" + text[:clip]) if title.strip() else text[:clip]


n30 = nrest = 0
with open(prefix + ".w30.jsonl", "w") as a, open(prefix + ".rest.jsonl", "w") as b:
    for qid in sorted(run["bm25"]):
        first = run["bm25"][qid][:30]
        for did in first:
            a.write(json.dumps({"g": qid, "id": did, "a": Q[qid], "b": doc(did)}) + "\n"); n30 += 1
        seen = set(first)
        for did in run["bm25"][qid][:W] + run["hybrid"][qid][:W]:
            if did not in seen:
                seen.add(did)
                b.write(json.dumps({"g": qid, "id": did, "a": Q[qid], "b": doc(did)}) + "\n"); nrest += 1
print(f"{prefix}: w30 pairs={n30} rest pairs={nrest} (windows {windows})")
