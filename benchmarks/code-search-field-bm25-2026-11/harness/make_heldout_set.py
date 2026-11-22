import os, random, re, sys
root, out = sys.argv[1], sys.argv[2]
decl = re.compile(r"/\*\*(.*?)\*/\s*(?:@\w+(?:\([^)]*\))?\s*)*public\s+(?:(?:final|abstract|sealed|static)\s+)*(?:class|interface|enum|record)\s+(\w+)", re.S)
skip = {"core", "core.tests", "test-framework", "benchmark", "benchmark-jmh", "luke"}
rows = []
lroot = os.path.join(root, "lucene")
for mod in sorted(os.listdir(lroot)):
    if mod in skip or not os.path.isdir(os.path.join(lroot, mod)):
        continue
    for d, _, files in sorted(os.walk(os.path.join(lroot, mod))):
        if "/src/java" not in d + "/":
            continue
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
                rows.append((os.path.relpath(p, root), sent, m.group(2)))
random.Random(20261122).shuffle(rows)
rows = sorted(rows[:200])
named = 0
with open(out + "-queries.tsv", "w") as q, open(out + "-qrels.tsv", "w") as r, open(out + "m-queries.tsv", "w") as qm:
    r.write("query-id\tcorpus-id\tscore\n")
    for i, (path, sent, cls) in enumerate(rows):
        pat = re.compile(r"\b" + re.escape(cls) + r"\b", re.I)
        named += bool(pat.search(sent))
        masked = " ".join(pat.sub(" ", sent).split())
        q.write(f"H{i:03d}\t{sent}\n")
        qm.write(f"H{i:03d}\t{masked}\n")
        r.write(f"H{i:03d}\t{path}\t1\n")
print(len(rows), "queries; class name in sentence:", named)
