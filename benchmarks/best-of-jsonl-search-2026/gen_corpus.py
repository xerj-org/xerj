#!/usr/bin/env python3
"""Deterministic synthetic JSONL log corpus for the best-of JSONL/CSV search page.
300,000 records; planted terms live only in `msg` so a line grep and a field
query count the same records."""
import json, random, sys
N = int(sys.argv[2]) if len(sys.argv) > 2 else 300_000
rnd = random.Random(20261019)
words = ("request handler cache shard replica upstream worker queue retry backoff payload "
         "session token header client server socket buffer flush commit segment merge "
         "latency budget window batch stream record parser schema field value cursor").split()
services = ["api", "billing", "auth", "search", "ingest", "mailer", "scheduler", "gateway"]
levels = ["INFO"] * 14 + ["WARN"] * 4 + ["ERROR"] * 2
with open(sys.argv[1], "w") as out:
    for i in range(N):
        msg = " ".join(rnd.choice(words) for _ in range(rnd.randint(8, 20)))
        r = rnd.random()
        if r < 0.001: msg += " kestrel"                 # rare term, ~0.1 %
        elif r < 0.006: msg += " disk quota exceeded"   # phrase, ~0.5 %
        if rnd.random() < 0.05: msg = "timeout " + msg  # common term, ~5 %
        out.write(json.dumps({"ts": f"2026-10-{1 + i % 18:02d}T{i % 24:02d}:{i % 60:02d}:{(i * 7) % 60:02d}Z",
                              "level": rnd.choice(levels), "service": rnd.choice(services),
                              "user": f"u{rnd.randint(1, 5000)}", "msg": msg}) + "\n")
