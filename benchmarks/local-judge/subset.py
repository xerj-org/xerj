"""A seeded query subset of the pair files, for tiers too slow to score in full on a CPU.

usage: subset.py <runs_dir> <dataset> <n> [seed] [split] [parts]
  split  "test" (default): writes <dataset>.sub<n>.*   |  "fit": writes <dataset>.fitsub<n>.*
  parts  comma list of pair-file parts to subset (default "w30,rest" for test, "w30" for fit).
         Pass "w30,hyb30" to cover the BM25 top-30 and hybrid top-30 arms without paying for
         the top-100 ones — `hyb30` is what `make_pairs.py <…> 4000 30` writes as `.rest`.

eval.py reports every arm — BM25, hybrid and every tier — on the same subset, so the
comparison stays paired; it never mixes a subset score with a full-set one."""
import json, random, sys

runs, ds, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
seed = int(sys.argv[4]) if len(sys.argv) > 4 else 7
split = sys.argv[5] if len(sys.argv) > 5 else "test"
name = f"sub{n}" if split == "test" else f"fitsub{n}"
default_parts = "w30,rest" if split == "test" else "w30"
parts = [p for p in (sys.argv[6] if len(sys.argv) > 6 else default_parts).split(",") if p]
qids = sorted({json.loads(l)["g"] for l in open(f"{runs}/{ds}.{split}.w30.jsonl")})
random.Random(seed).shuffle(qids)
keep = set(qids[:n])
json.dump({"seed": seed, "n": len(keep), "of": len(qids), "split": split, "parts": parts,
           "qids": sorted(keep)}, open(f"{runs}/{ds}.{name}.qids.json", "w"))
for part in parts:
    k = 0
    with open(f"{runs}/{ds}.{name}.{part}.jsonl", "w") as out:
        for line in open(f"{runs}/{ds}.{split}.{part}.jsonl"):
            if json.loads(line)["g"] in keep:
                out.write(line); k += 1
    print(f"{ds}.{name}.{part}: {k} pairs from {len(keep)} of {len(qids)} {split} queries (seed {seed})")
