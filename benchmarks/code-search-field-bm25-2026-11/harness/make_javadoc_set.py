"""usage: make_javadoc_set.py <lucene-checkout> <out-prefix>

A natural-language code question set from Lucene (Apache-2.0), built
deterministically and frozen before it was run against any arm: for each
top-level public class/interface/enum under lucene/core/src/java with a class
Javadoc, the query is the Javadoc's first sentence ({@link X} -> X, tags and
HTML dropped) and the relevant document is the file that declares it.
Sentences of 5-25 words only; 200 sampled with random.Random(1788).
Writes <out-prefix>-queries.tsv and <out-prefix>-qrels.tsv."""
import os
import random
import re
import sys

root, out = sys.argv[1], sys.argv[2]
decl = re.compile(r"/\*\*(.*?)\*/\s*(?:@\w+(?:\([^)]*\))?\s*)*public\s+(?:(?:final|abstract|sealed|static)\s+)*(?:class|interface|enum|record)\s+(\w+)", re.S)
rows = []
base = os.path.join(root, "lucene/core/src/java")
for d, _, files in sorted(os.walk(base)):
    for f in sorted(files):
        if not f.endswith(".java") or f == "package-info.java":
            continue
        p = os.path.join(d, f)
        text = open(p, encoding="utf-8", errors="replace").read()
        m = decl.search(text)
        if not m or m.group(2) != f[:-5]:
            continue
        doc = re.sub(r"^\s*\*", " ", m.group(1), flags=re.M)
        doc = re.sub(r"\{@(?:link|linkplain|code)\s+([^}\s]+)[^}]*\}", lambda x: x.group(1).split("#")[-1], doc)
        doc = re.sub(r"<[^>]+>", " ", doc)
        doc = re.sub(r"@\w+.*", "", doc, flags=re.S)
        doc = " ".join(doc.split())
        sent = re.split(r"(?<=[.!?])\s", doc, maxsplit=1)[0].rstrip(".")
        n = len(sent.split())
        if 5 <= n <= 25 and "\t" not in sent:
            rows.append((os.path.relpath(p, root), sent))
random.Random(1788).shuffle(rows)
rows = sorted(rows[:200])
with open(out + "-queries.tsv", "w") as q, open(out + "-qrels.tsv", "w") as r:
    r.write("query-id\tcorpus-id\tscore\n")
    for i, (path, sent) in enumerate(rows):
        q.write(f"J{i:03d}\t{sent}\n")
        r.write(f"J{i:03d}\t{path}\t1\n")
print(len(rows), "queries")
