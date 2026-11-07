"""Records how memvid-sdk's default lexical find() treats a natural-language question
versus the same key words. Writes probes/memvid_and_probe.json."""
import json, os, sys, tempfile
os.environ["MEMVID_TELEMETRY"] = "0"
import memvid_sdk
import common
d = json.load(open(os.path.join(common.HERE, "dataset.json")))
tmp = tempfile.mkdtemp(dir="/dev/shm")
mv = memvid_sdk.create(os.path.join(tmp, "probe.mv2"))
for m in d["memories"]:
    mv.put(title="memory", label="note", text=m["text"])
mv.commit()
probes = ["What is the main database for Lark?", "main database Lark", "main database for Lark",
          "Which package manager should I use for JS work?", "pnpm", "Lark API port", "What port does the Lark API listen on in production?"]
out = []
for q in probes:
    r = mv.find(q, k=5)
    hits = [h.get("snippet", "")[:120] for h in r.get("hits", [])]
    out.append({"query": q, "n_hits": len(hits), "top": hits[:2]})
    print(len(hits), q)
json.dump({"sdk": "memvid-sdk " + getattr(memvid_sdk, "__version__", "2.0.160"), "probes": out},
          open(os.path.join(common.HERE, "probes", "memvid_and_probe.json"), "w"), indent=1)
