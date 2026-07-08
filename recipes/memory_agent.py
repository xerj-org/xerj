#!/usr/bin/env python3
"""A security-triage agent with persistent semantic memory — zero dependencies.

A small SOC (security operations) agent that works through *real* attack
traffic — the public OpenSSH log capture from logpai/loghub that ships in
this repo — and remembers what it decided, using XERJ's Agent-Memory API
(`/_memory/{namespace}`) as its long-term memory:

  1. It groups the raw sshd events into per-attacker incidents.
  2. Before deciding, it RECALLS similar past incidents. Text recall is
     relevance-ranked (BM25); add `"semantic": true` to have XERJ embed the
     query server-side and recall by vector similarity (no client-side
     embedding), or pass your own `vector` for BYO-embedding kNN.
  3. It decides (block / rate-limit / watch), explains whether memory
     changed the decision, and STORES the outcome as a new memory.

Memory is backed by a real index, so it survives restarts: run this
script twice and the second run recalls the first run's decisions.

Run it:
    xerj --insecure --data-dir ./data &        # start XERJ
    python3 recipes/memory_agent.py            # first run: cold memory
    python3 recipes/memory_agent.py            # second run: warm memory

Optional env:
    XERJ_URL   (default http://localhost:9200)
"""

import collections
import json
import os
import pathlib
import urllib.request

XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")
LOGS = pathlib.Path(__file__).resolve().parent.parent / "engine" / "demo-data" / "ssh_one.ndjson"
NAMESPACE = "soc-agent"
EVENTS_TO_READ = 80_000   # first slice of the real capture
INCIDENTS_TO_TRIAGE = 8


def call(method: str, path: str, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        f"{XERJ}{path}", data=data,
        headers={"Content-Type": "application/json"} if body else {},
        method=method,
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


# ── 1. Build incidents from the real sshd capture ───────────────────────────
def load_incidents():
    attackers = collections.defaultdict(lambda: {"events": 0, "users": set(), "kinds": set()})
    with LOGS.open() as f:
        for i, line in enumerate(f):
            if i >= EVENTS_TO_READ:
                break
            ev = json.loads(line)
            ip = ev.get("src_ip")
            if not ip or ev.get("event") not in {
                "invalid_user", "failed_password", "possible_break_in", "auth_failure",
            }:
                continue
            a = attackers[ip]
            a["events"] += 1
            a["kinds"].add(ev["event"])
            if ev.get("user"):
                a["users"].add(ev["user"])

    ranked = sorted(attackers.items(), key=lambda kv: -kv[1]["events"])
    incidents = []
    for ip, a in ranked[:INCIDENTS_TO_TRIAGE]:
        users = sorted(a["users"])
        incidents.append({
            "src_ip": ip,
            "attempts": a["events"],
            "kinds": sorted(a["kinds"]),
            "summary": (
                f"host {ip} produced {a['events']} failed ssh authentications "
                f"({', '.join(sorted(a['kinds']))}) trying accounts like "
                f"{', '.join(users[:5]) or 'root'}"
            ),
        })
    return incidents


# ── 2. The agent: recall → decide → remember ────────────────────────────────
def decide(incident, recalled):
    # Memory-informed path: a strongly relevant past incident that was
    # blocked means this is a known pattern — block immediately.
    # (Recall scores are BM25 relevance — unbounded, higher is better.)
    for m in recalled:
        if m["score"] > 1.0 and m["metadata"].get("action") == "block":
            return "block", f"recalled similar blocked incident ({m['metadata']['src_ip']})"
    # Cold-start policy on the raw numbers.
    if incident["attempts"] >= 200:
        return "block", "fresh analysis: sustained brute force"
    if incident["attempts"] >= 30:
        return "rate-limit", "fresh analysis: repeated failures"
    return "watch", "fresh analysis: low volume"


def main():
    before = call("GET", f"/_memory/{NAMESPACE}").get("count", 0)
    print(f"agent memory `{NAMESPACE}`: {before} memories at start\n")

    for inc in load_incidents():
        recalled = call("POST", f"/_memory/{NAMESPACE}/_recall", {
            "query": inc["summary"],
            "k": 3,
        }).get("hits", [])

        action, why = decide(inc, recalled)
        print(f"incident  {inc['src_ip']:<16} {inc['attempts']:>5} attempts  → {action:<10} ({why})")
        for m in recalled[:1]:
            print(f"          remembered: [{m['score']:.2f}] {m['text'][:90]}…")

        call("POST", f"/_memory/{NAMESPACE}", {
            "text": f"{inc['summary']} — decision: {action}",
            "metadata": {
                "src_ip": inc["src_ip"],
                "action": action,
                "attempts": inc["attempts"],
            },
        })

    after = call("GET", f"/_memory/{NAMESPACE}").get("count", 0)
    print(f"\nagent memory `{NAMESPACE}`: {after} memories at end "
          f"(+{after - before} this run)")
    if before:
        print("second run used warm memory — decisions above cite recalled incidents")
    else:
        print("run me again: the next run recalls these memories semantically")


if __name__ == "__main__":
    main()
