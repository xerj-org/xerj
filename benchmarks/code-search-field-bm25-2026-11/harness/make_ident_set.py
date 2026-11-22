import collections, os, random, re, sys
root, out = sys.argv[1], sys.argv[2]
skip = {"core", "core.tests", "test-framework", "benchmark", "benchmark-jmh", "luke"}
meth = re.compile(r"^\s*(?:public|protected)\s+(?:(?:static|final|abstract|synchronized|native)\s+)*[\w<>\[\],.? ]+?\s+([a-z][a-zA-Z0-9]+)\s*\(", re.M)
seen = collections.defaultdict(set)
lroot = os.path.join(root, "lucene")
for mod in sorted(os.listdir(lroot)):
    if mod in skip or not os.path.isdir(os.path.join(lroot, mod)):
        continue
    for d, _, files in sorted(os.walk(os.path.join(lroot, mod))):
        if "/src/java" not in d + "/":
            continue
        for f in sorted(files):
            if f.endswith(".java"):
                p = os.path.join(d, f)
                for m in meth.finditer(open(p, encoding="utf-8", errors="replace").read()):
                    seen[m.group(1)].add(os.path.relpath(p, root))
allfiles = collections.Counter()
for d, _, files in os.walk(root):
    for f in files:
        if f.endswith(".java"):
            txt = open(os.path.join(d, f), encoding="utf-8", errors="replace").read()
            for name in re.findall(r"\b([a-z][a-zA-Z0-9]+)\s*\(", txt):
                allfiles[name] += 1
rows = []
for name, paths in seen.items():
    words = re.findall(r"[A-Z]?[a-z]+|[0-9]+", name)
    if len(paths) != 1 or not 2 <= len(words) <= 5 or "".join(words) != name:
        continue
    if allfiles[name] > 3:
        continue
    rows.append((next(iter(paths)), " ".join(w.lower() for w in words), name))
rows.sort()
random.Random(1122).shuffle(rows)
rows = sorted(rows[:200])
with open(out + "-queries.tsv", "w") as q, open(out + "-qrels.tsv", "w") as r:
    r.write("query-id\tcorpus-id\tscore\n")
    for i, (path, words, name) in enumerate(rows):
        q.write(f"I{i:03d}\t{words}\n")
        r.write(f"I{i:03d}\t{path}\t1\n")
print(len(rows), "of", len(seen))
