#!/usr/bin/env python3
"""Build nested corpus scales S1<S2<S3<S4 from the Lucene tree.

Exactly one variable changes across scales: the NUMBER OF FILES visible.
The 9 answer-bearing files are pinned into every scale, so every question is
answerable at every scale. Distractors are sampled with a fixed seed from the
real tree (not synthesized), so decoy density at S4 is the true density.
"""
import os, random, shutil, json, sys

ROOT = "/home/claude/.xerj-code/corpora/lucene-meta/lucene"
OUT  = "/home/claude/tasks/speakers-FTS-vector-search/outreach/bench/scales"
SEED = 20260814

ANSWER_FILES = [
 "lucene/core/src/java/org/apache/lucene/index/TieredMergePolicy.java",
 "lucene/core/src/java/org/apache/lucene/search/AbstractKnnVectorQuery.java",
 "lucene/core/src/java/org/apache/lucene/codecs/lucene90/IndexedDISI.java",
 "lucene/core/src/java/org/apache/lucene/index/ConcurrentMergeScheduler.java",
 "lucene/core/src/java/org/apache/lucene/index/LeafMetaData.java",
 "lucene/core/src/java/org/apache/lucene/search/LRUQueryCache.java",
 "lucene/core/src/java/org/apache/lucene/util/fst/FSTCompiler.java",
 "lucene/core/src/java/org/apache/lucene/index/IndexWriterConfig.java",
 "lucene/core/src/java/org/apache/lucene/util/quantization/ScalarQuantizer.java",
]
SIZES = {"S1": 64, "S2": 256, "S3": 1024, "S4": None}   # None = whole tree

def main():
    allj = []
    for dp, dn, fn in os.walk(ROOT):
        for f in fn:
            if f.endswith(".java"):
                allj.append(os.path.relpath(os.path.join(dp, f), ROOT))
    allj.sort()
    for a in ANSWER_FILES:
        assert os.path.exists(os.path.join(ROOT, a)), "missing answer file: " + a
    pool = [p for p in allj if p not in set(ANSWER_FILES)]
    random.Random(SEED).shuffle(pool)

    manifest = {}
    prev = list(ANSWER_FILES)
    for name in ["S1", "S2", "S3", "S4"]:
        n = SIZES[name]
        if n is None:
            files = allj
        else:
            need = n - len(ANSWER_FILES)
            files = sorted(set(ANSWER_FILES) | set(pool[:need]))
        assert set(prev).issubset(set(files)), name + " is not a superset of the previous scale"
        prev = files
        dest = os.path.join(OUT, name)
        if os.path.exists(dest):
            shutil.rmtree(dest)
        for rel in files:
            d = os.path.join(dest, rel)
            os.makedirs(os.path.dirname(d), exist_ok=True)
            shutil.copy2(os.path.join(ROOT, rel), d)
        manifest[name] = {"n_files": len(files), "dir": dest, "files": files}
        print("%s: %d files -> %s" % (name, len(files), dest))
    with open(os.path.join(OUT, "manifest.json"), "w") as fh:
        json.dump({"seed": SEED, "root": ROOT, "answer_files": ANSWER_FILES,
                   "scales": {k: {"n_files": v["n_files"], "files": v["files"]}
                              for k, v in manifest.items()}}, fh, indent=1)
    print("manifest written")

if __name__ == "__main__":
    main()
