#!/usr/bin/env python3
"""Time first-time queries on a node that has just opened an existing index.

Read-only: every request is a _search. For each needle kind in the truth file
it draws `--per-kind` needles with a fixed `--seed`, skipping the first
`--skip` of each kind (the ones verify.py's old default window checked, so a
query cache warmed by that run cannot answer them), sends one
`match body <needle>` per needle, and prints one line per query plus a JSON
summary. A needle counts as ok only when it comes back as exactly one document
from its own message.

  python3 latency_sample.py --url http://127.0.0.1:12120 --truth tree.truth.json \
      --seed 20260919        # pass 1
  python3 latency_sample.py ... --seed 777                               # pass 2: other needles

The key of a node with auth on comes from --api-key or $XERJ_API_KEY.
"""
import argparse
import json
import os
import random
import time
import urllib.request


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--truth", required=True)
    ap.add_argument("--prefix", default="bench")
    ap.add_argument("--seed", type=int, default=20260919)
    ap.add_argument("--per-kind", type=int, default=8)
    ap.add_argument("--skip", type=int, default=60)
    ap.add_argument("--api-key", default=os.environ.get("XERJ_API_KEY", ""))
    a = ap.parse_args()
    headers = {"content-type": "application/json"}
    if a.api_key:
        headers["Authorization"] = "ApiKey " + a.api_key
    by_kind = {}
    for nd in json.load(open(a.truth, encoding="utf-8"))["needles"]:
        by_kind.setdefault(nd["where"], []).append(nd)
    rng = random.Random(a.seed)
    picks = []
    for _, needles in sorted(by_kind.items()):
        rest = needles[a.skip:]
        if rest:
            picks += rng.sample(rest, min(a.per_kind, len(rest)))
    ok = 0
    lat = []
    for nd in picks:
        body = {"size": 2, "track_total_hits": True, "query": {"match": {"body": nd["token"]}},
                "_source": ["email_message_id", "ax_locator"]}
        req = urllib.request.Request("%s/%s-*/_search" % (a.url, a.prefix), data=json.dumps(body).encode(),
                                     headers=headers)
        started = time.monotonic()
        res = json.load(urllib.request.urlopen(req, timeout=600))
        dt = time.monotonic() - started
        n = res["hits"]["total"]["value"]
        good = n == 1 and res["hits"]["hits"][0]["_source"].get("email_message_id") == nd["message_id"]
        ok += good
        lat.append(dt)
        print("%-28s %9.1f ms  hits=%d %s" % (nd["where"], dt * 1000, n, "ok" if good else "BAD"), flush=True)
    lat.sort()
    print(json.dumps({"checked": len(picks), "ok": ok, "slow_over_1s": sum(1 for x in lat if x > 1),
                      "median_ms": round(lat[len(lat) // 2] * 1000, 1), "max_s": round(lat[-1], 2),
                      "total_s": round(sum(lat), 1)}))


if __name__ == "__main__":
    main()
