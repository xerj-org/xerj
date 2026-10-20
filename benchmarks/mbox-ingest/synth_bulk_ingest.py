#!/usr/bin/env python3
"""Mailbox-shaped synthetic bulk load against a running xerj node (issue #948).

Generates the record shape that dominates a Google-Takeout mailbox ingest
(~60 text/keyword fields, a multi-KB body per doc) and drives the ES-compat
`_bulk` API with the same 8 MB request bodies `xerj autoindex` uses, WITHOUT
autoindex's catalog/brain/edge traffic — so the numbers isolate the engine's
ingest-side memory behaviour.

  synth_bulk_ingest.py --url http://127.0.0.1:9341 --docs 20000 --body-kb 10 \
      [--index bench-docs] [--bulk-mb 8] [--key FILE] [--json]

Every doc is unique (`msg-<i>`); `--seed` makes bodies reproducible. Always
writes one JSON line to stdout (`--json` is accepted but changes nothing) with
docs, wall seconds, and the node's own `_count`. Memory figures are read by the caller from /proc.
"""
import argparse
import json
import random
import sys
import time
import urllib.request

WORDS = (
    "deployment window roadmap vendor renewal sync notes thread reply "
    "forward attached please review the following document as discussed "
    "earlier this week let me know when convenient regards team update "
    "quarterly planning budget review milestone delivery customer feedback"
).split()


def body(rng: random.Random, kb: int) -> str:
    # kb=0 means "tiny body" (100 B) for the per-doc vs per-byte bisection —
    # the field stays present so doc SHAPE is constant across variants.
    target = max(kb * 1024, 100)
    parts: list[str] = []
    while sum(len(p) + 1 for p in parts) < target:
        sent = " ".join(rng.choice(WORDS) for _ in range(rng.randint(6, 14)))
        parts.append(f"{rng.randint(1, 999_999):06d} {sent}.")
    return "\n".join(parts)[:target]


def doc(rng: random.Random, i: int, kb: int) -> dict:
    d = {
        "_id": f"msg-{i}",
        "_source": {
            "message_id": f"<{i:08d}@takeout.example>",
            "from": f"user{i % 97}@example.org",
            "to": f"peer{i % 89}@example.net",
            "subject": " ".join(rng.choice(WORDS) for _ in range(rng.randint(3, 9))),
            "date": f"2024-{1 + i % 12:02d}-{1 + i % 28:02d}T10:{i % 60:02d}:00Z",
            "body": body(rng, kb),
            "labels": [rng.choice(WORDS) for _ in range(3)],
            "attachment_name": f"report-{i % 500}.pdf",
            "attachment_content_type": "application/pdf",
            "attachment_bytes": rng.randint(10_000, 900_000),
            "ax_paths": [f"Takeout/Mail/{i % 40}.mbox"],
            "kind": "email",
        },
    }
    return d


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--docs", type=int, default=20_000)
    ap.add_argument("--body-kb", type=int, default=10)
    ap.add_argument("--index", default="bench-docs")
    ap.add_argument("--bulk-mb", type=float, default=8.0)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--body-type", choices=["text", "keyword"], default="text",
                    help="mapping for the body field: text (postings) or keyword (none)")
    ap.add_argument("--key", help="file with the admin API key")
    ap.add_argument("--json", action="store_true",
                    help="no-op, kept for old command lines: the JSON summary is always printed")
    ap.add_argument("--auto-id", action="store_true",
                    help="omit _id from bulk actions (turbo-raw path) instead of "
                         "explicit ids (per-doc path)")
    ap.add_argument("--retry-rounds", type=int, default=300,
                    help="bulk rounds to re-offer 429-rejected records (like autoindex)")
    args = ap.parse_args()

    rng = random.Random(args.seed)
    headers = {"Content-Type": "application/x-ndjson"}
    if args.key:
        headers["Authorization"] = "ApiKey " + open(args.key).read().strip()

    req = urllib.request.Request(
        f"{args.url}/{args.index}",
        data=json.dumps({"mappings": {"properties": {
            "body": {"type": args.body_type}, "subject": {"type": "text"},
            "message_id": {"type": "keyword"}, "from": {"type": "keyword"},
            "to": {"type": "keyword"}, "date": {"type": "date"},
            "labels": {"type": "keyword"}, "attachment_name": {"type": "keyword"},
            "attachment_content_type": {"type": "keyword"},
            "attachment_bytes": {"type": "long"}, "ax_paths": {"type": "keyword"},
            "kind": {"type": "keyword"},
        }}}).encode(), headers=headers, method="PUT")
    try:
        urllib.request.urlopen(req).read()
    except urllib.error.HTTPError as e:
        if b"resource_already_exists" not in e.read():
            raise

    t0 = time.time()
    sent = 0
    bulk_bytes = int(args.bulk_mb * 1024 * 1024)

    def send(items: list[tuple[str, str]]) -> list[tuple[str, str]]:
        """POST one bulk batch; return the (action, line) pairs to retry (429s)."""
        payload = ("\n".join(a + "\n" + l for a, l in items) + "\n").encode()
        r = urllib.request.Request(f"{args.url}/_bulk", data=payload,
                                   headers=headers, method="POST")
        try:
            with urllib.request.urlopen(r) as resp:
                out = json.load(resp)
        except urllib.error.HTTPError as e:
            if e.code == 429:
                return items  # whole-request admission rejection: retry all
            raise
        if not out.get("errors"):
            return []
        retry: list[tuple[str, str]] = []
        for item, (a, l) in zip(out["items"], items):
            op = item.get("index") or item.get("create") or {}
            status = op.get("status", 500)
            if status == 429:
                retry.append((a, l))
            elif status >= 400:
                print(f"non-429 bulk error: {json.dumps(op)[:200]}", file=sys.stderr)
        return retry

    retries = 0
    pending: list[tuple[str, str]] = []
    for i in range(args.docs):
        d = doc(rng, i, args.body_kb)
        line = json.dumps(d, separators=(",", ":"))
        if args.auto_id:
            action = json.dumps({"index": {"_index": args.index}},
                                separators=(",", ":"))
        else:
            action = json.dumps({"index": {"_index": args.index, "_id": d["_id"]}},
                                separators=(",", ":"))
        pending.append((action, line))
        sent += 1
        if sum(len(a) + len(l) for a, l in pending) >= bulk_bytes or i == args.docs - 1:
            batch, pending = pending, []
            for attempt in range(args.retry_rounds):
                batch = send(batch)
                if not batch:
                    break
                retries += 1
                time.sleep(min(2.0 * (attempt + 1), 10.0))
            if batch:
                print(f"giving up on {len(batch)} records after {args.retry_rounds} retry rounds",
                      file=sys.stderr)
                return 2
    wall = time.time() - t0

    q = urllib.request.Request(
        f"{args.url}/{args.index}/_count",
        data=json.dumps({"query": {"match_all": {}}}).encode(),
        headers=headers, method="POST")
    with urllib.request.urlopen(q) as resp:
        count = json.load(resp)["count"]
    print(json.dumps({
        "docs_requested": args.docs, "docs_sent": sent, "node_count": count,
        "body_kb": args.body_kb, "bulk_mb": args.bulk_mb, "retry_rounds": retries,
        "wall_s": round(wall, 1),
        "docs_per_s": round(sent / wall, 1) if wall else None,
    }))
    return 0 if count == args.docs else 3


if __name__ == "__main__":
    sys.exit(main())
