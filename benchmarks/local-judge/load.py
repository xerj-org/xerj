"""Index one BEIR corpus into a XERJ node started with `--embed-mode neural`.
usage: load.py <dataset_dir> <index> [workers]   (XERJ_URL, XERJ_API_KEY from the environment)

`workers` concurrent `_bulk` requests (default 1). Neural indexing embeds every document on
the CPU and one bulk does not use every core, so several in flight is what makes a 57k-document
corpus finish in minutes rather than hours."""
import json, sys, time, threading, queue
from beirlib import call

ds, idx = sys.argv[1], sys.argv[2]
workers = int(sys.argv[3]) if len(sys.argv) > 3 else 1
print(call("DELETE", "/" + idx))
print(call("PUT", "/" + idx, {"mappings": {"properties": {
    "title": {"type": "text"}, "text": {"type": "text"}, "body": {"type": "semantic_text"}}}}))
docs = [json.loads(l) for l in open(ds + "/corpus.jsonl")]
B, t0 = 100, time.time()
jobs, failed, done = queue.Queue(), [], [0]
for i in range(0, len(docs), B):
    jobs.put(docs[i:i + B])


def work():
    while not failed:
        try:
            batch = jobs.get_nowait()
        except queue.Empty:
            return
        lines = []
        for d in batch:
            lines.append(json.dumps({"index": {"_index": idx, "_id": d["_id"]}}))
            lines.append(json.dumps({"title": d["title"], "text": d["text"], "body": d["title"] + ". " + d["text"]}))
        r = call("POST", "/_bulk", ("\n".join(lines) + "\n").encode(), "application/x-ndjson")
        if r.get("errors") or "_err" in r:
            failed.append(str(r)[:400]); return
        done[0] += len(batch)
        if done[0] % 2000 < B:
            print(f"indexed {done[0]}/{len(docs)}  {done[0] / (time.time() - t0):.1f} docs/s", flush=True)


threads = [threading.Thread(target=work) for _ in range(workers)]
[t.start() for t in threads]; [t.join() for t in threads]
if failed:
    print("bulk problem", failed[0]); sys.exit(1)
print(call("POST", "/" + idx + "/_refresh"), call("GET", "/" + idx + "/_count"))
print(f"total {time.time() - t0:.0f}s, {workers} worker(s)")
