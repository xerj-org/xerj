"""Prototype for discussion #1012: label documents at autoindex time from a
local labelled-history index, via the already-shipped POST /_decide endpoint
(engine/crates/xerj-api/src/systemone_api.rs) — no outbound call, no key,
nothing added to the wire client. This measures whether that existing local
mechanism is good enough at the classification the discussion asked about
("invoice / newsletter / personal / needs-reply"), before any autoindex code
is written.

Corpus: gen_corpus.py's synthetic, originally-authored emails (see that
file's docstring for why synthetic, not a real mailbox). k = 10, matching the
k used in the published Banking77 / SMS-spam decisions-as-retrieval numbers.

Usage:
  python3 gen_corpus.py                 # writes corpus.json (once)
  XERJ_URL=http://localhost:9420 python3 run.py
"""
import collections
import json
import os
import sys
import time
import urllib.request

U = os.environ.get("XERJ_URL", "http://localhost:9420")
K = int(os.environ.get("K", "10"))
INDEX = "email-classify-proto"


def req(m, p, b=None, ct="application/json"):
    d = b if isinstance(b, (bytes, type(None))) else json.dumps(b).encode()
    r = urllib.request.Request(U + p, data=d, method=m, headers={"content-type": ct})
    try:
        return json.loads(urllib.request.urlopen(r, timeout=120).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:400]}


def load_train(rows):
    train = [r for r in rows if r["split"] == "train"]
    req("DELETE", "/" + INDEX)
    created = req("PUT", "/" + INDEX, {"mappings": {"properties": {
        "label": {"type": "keyword"}, "text": {"type": "text"}}}})
    if "_err" in created:
        print("index create failed:", created)
        sys.exit(1)
    lines = []
    for i, r in enumerate(train):
        lines.append(json.dumps({"index": {"_index": INDEX, "_id": str(i)}}))
        lines.append(json.dumps({"text": r["subject"] + "\n" + r["text"], "label": r["label"]}))
    resp = req("POST", "/_bulk", ("\n".join(lines) + "\n").encode(), "application/x-ndjson")
    if resp.get("errors") or "_err" in resp:
        print("bulk problem:", str(resp)[:400])
        sys.exit(1)
    req("POST", f"/{INDEX}/_refresh")
    count = req("GET", f"/{INDEX}/_count").get("count")
    print(f"{INDEX}: indexed {len(train)} train emails, count={count}")


def decide(text):
    return req("POST", "/_decide", {"index": INDEX, "question": text, "k": K})


def confusion_table(labels, cm):
    w = max(len(l) for l in labels) + 1
    header = " " * w + "".join(f"{l[:10]:>11s}" for l in labels) + "  <- predicted"
    print(header)
    for gold in labels:
        row = f"{gold:<{w}}" + "".join(f"{cm[gold][pred]:11d}" for pred in labels)
        print(row)


def evaluate(name, rows, labels):
    cm = {g: collections.Counter() for g in labels}
    correct = 0
    abstained = 0
    confidences = []
    t0 = time.time()
    per_item = []
    for r in rows:
        text = r["subject"] + "\n" + r["text"]
        resp = decide(text)
        if "_err" in resp:
            print("decide failed:", resp)
            sys.exit(1)
        pred = resp.get("label")
        conf = resp.get("confidence", 0.0)
        if resp.get("abstain"):
            abstained += 1
        cm[r["label"]][pred if pred else "(abstain)"] += 1
        confidences.append(conf)
        ok = pred == r["label"]
        correct += ok
        per_item.append({"gold": r["label"], "pred": pred, "confidence": conf,
                          "ok": ok, "subject": r["subject"]})
    took_ms = (time.time() - t0) * 1000 / len(rows)

    print(f"\n== {name}: {len(rows)} emails, k={K}, index={INDEX}\n")
    print(f"accuracy: {correct}/{len(rows)} = {correct/len(rows):.4f}")
    print(f"abstained (below decisions.min_confidence, default 0.0 = never): {abstained}")
    print(f"mean confidence: {sum(confidences)/len(confidences):.4f}")
    print(f"latency: {took_ms:.1f} ms/item (local /_decide call, no egress)\n")
    confusion_table(labels, cm)

    for label in labels:
        tp = cm[label][label]
        fn = sum(v for k, v in cm[label].items() if k != label)
        fp = sum(cm[g][label] for g in labels if g != label)
        p = tp / max(tp + fp, 1)
        r_ = tp / max(tp + fn, 1)
        f1 = 2 * p * r_ / max(p + r_, 1e-9)
        print(f"  {label:<12s} P={p:.3f} R={r_:.3f} F1={f1:.3f}")

    misses = sorted((i for i in per_item if not i["ok"]), key=lambda i: i["confidence"])
    print(f"\nmisses ({len(misses)}):")
    for m in misses:
        print(f"  \"{m['subject'][:40]}\" gold={m['gold']:<12s} "
              f"pred={str(m['pred']):<12s} conf={m['confidence']:.3f}")

    return {"accuracy": correct / len(rows), "abstained": abstained,
            "mean_confidence": sum(confidences) / len(confidences),
            "ms_per_item": took_ms, "k": K, "n": len(rows),
            "confusion": {g: dict(cm[g]) for g in labels}, "per_item": per_item}


def main():
    rows = json.load(open("corpus.json"))
    load_train(rows)
    labels = sorted({r["label"] for r in rows})

    easy = evaluate("held-out test (templated, same generator as train)",
                     [r for r in rows if r["split"] == "test"], labels)
    hard = evaluate("hard set (hand-authored, boundary-crossing vocabulary)",
                     [r for r in rows if r["split"] == "hard"], labels)

    json.dump({"easy": easy, "hard": hard},
               open("results/run.json", "w"), indent=1)
    print("\nwrote results/run.json")


if __name__ == "__main__":
    main()
