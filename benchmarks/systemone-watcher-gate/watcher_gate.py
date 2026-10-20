"""#1108 — the wrong-and-confident gate, run through the REAL `_watcher` product
path, not a script that calls `/_decide` directly.

Issue #1108 asks for the unmeasured half of the rc.80 `_watcher` work (#1062):
on the classify-email boundary tier, the wrong-and-confident rate at confidence
>= 0.8 must be <= 5 %. The measurement has to go through the watcher's own
semantics — percolate prefilter -> `/_decide` -> fire into `.xerj_alert_fires` —
so that it tests what a deployed detection watch actually does, not a bare vote.

WHAT THIS DOES
  1. Index the 200-email train split of systemone-classify-email/corpus.json
     into the vote-history index (`decide_index`). That is the labelled history
     `/_decide` votes against — reused verbatim so the two benchmarks share it.
  2. Create the watched (input) index EMPTY, then PUT one detection watch per
     label (one-vs-rest): a real `_watcher` watch whose `condition.xerj_decide`
     fires when the vote's argmax == that watch's positive_label and the RAW
     confidence >= p_min. The watch baselines at the index's current max _seq_no
     at PUT time, so documents indexed AFTER the PUT are the ones it judges.
  3. Index the 120 boundary-tier candidates (gen_boundary.py) into the watched
     index, each carrying its ground-truth label in `gold`.
  4. Let the watcher tick; poll `.xerj_alert_fires` until the fire count stops
     growing.
  5. Read every fire, map `doc_id` -> candidate `gold`, and compute the one
     number the issue defines: wrong-and-confident = fires whose true label is
     not the fired positive label, among fires with confidence >= 0.8. The
     watcher fires on and records the RAW `/_decide` confidence as `p`; it also
     carries `p_cal` when the node has a fitted calibration. We report the rate
     on `p` (always present) and on `p_cal` (when the node calibrated).

This harness NEVER calls POST /_decide itself for the gate decision — the fire
records written by the node's own evaluator are the sole input to the number.

Usage:
  python3 gen_boundary.py
  XERJ_URL=http://localhost:9471 [XERJ_APIKEY=...] python3 watcher_gate.py
"""
import collections
import json
import os
import sys
import time
import urllib.error
import urllib.request

U = os.environ.get("XERJ_URL", "http://localhost:9471").rstrip("/")
APIKEY = os.environ.get("XERJ_APIKEY", "")
K = int(os.environ.get("K", "10"))
P_MIN = float(os.environ.get("P_MIN", "0.5"))   # raw-confidence fire threshold
CONF_GATE = float(os.environ.get("CONF_GATE", "0.8"))  # the issue's >= 0.8 bar
HISTORY = "email-history"
WATCHED = "mail-incoming"
FIRES = ".xerj_alert_fires"


def req(m, p, b=None, ct="application/json"):
    d = b if isinstance(b, (bytes, type(None))) else json.dumps(b).encode()
    h = {"content-type": ct}
    if APIKEY:
        h["authorization"] = f"ApiKey {APIKEY}"
    r = urllib.request.Request(U + p, data=d, method=m, headers=h)
    try:
        return json.loads(urllib.request.urlopen(r, timeout=180).read())
    except urllib.error.HTTPError as e:
        return {"_err": e.code, "body": e.read().decode()[:500]}
    except Exception as e:  # noqa: BLE001
        return {"_err": "exc", "body": str(e)[:500]}


def die(msg, ctx=None):
    print("FATAL:", msg)
    if ctx is not None:
        print("  ", str(ctx)[:500])
    sys.exit(1)


def index_history(train):
    req("DELETE", "/" + HISTORY)
    created = req("PUT", "/" + HISTORY, {"mappings": {"properties": {
        "label": {"type": "keyword"}, "text": {"type": "text"}}}})
    if "_err" in created:
        die("could not create history index", created)
    lines = []
    for i, r in enumerate(train):
        lines.append(json.dumps({"index": {"_index": HISTORY, "_id": f"h{i}"}}))
        lines.append(json.dumps(
            {"text": r["subject"] + "\n" + r["text"], "label": r["label"]}))
    resp = req("POST", "/_bulk", ("\n".join(lines) + "\n").encode(),
               "application/x-ndjson")
    if "_err" in resp or resp.get("errors"):
        die("history bulk failed", resp)
    req("POST", f"/{HISTORY}/_refresh")
    n = req("GET", f"/{HISTORY}/_count").get("count")
    print(f"history '{HISTORY}': {n} labelled emails")


def make_watched_index():
    req("DELETE", "/" + WATCHED)
    created = req("PUT", "/" + WATCHED, {"mappings": {"properties": {
        "text": {"type": "text"}, "gold": {"type": "keyword"}}}})
    if "_err" in created:
        die("could not create watched index", created)
    req("POST", f"/{WATCHED}/_refresh")


def put_watch(label):
    wid = "detect-" + label.replace("-", "_")
    body = {
        "trigger": {"schedule": {"interval": "1s"}},
        "input": {"search": {
            "request": {"indices": [WATCHED]},
            "body": {"query": {"match_all": {}}},
        }},
        "condition": {"xerj_decide": {
            "question": "{{text}}",
            "positive_label": label,
            "text_field": "text",
            "p_min": P_MIN,
            "index": HISTORY,
            "k": K,
        }},
        "actions": {"index_alert": {"level": "info"}},
    }
    resp = req("PUT", f"/_watcher/watch/{wid}", body)
    if "_err" in resp:
        die(f"watch {wid} refused", resp)
    print(f"  watch {wid}: positive_label={label!r} p_min={P_MIN} -> {resp.get('watcher',{}).get('evaluation')}")
    return wid


def index_candidates(cands):
    lines = []
    for i, r in enumerate(cands):
        lines.append(json.dumps({"index": {"_index": WATCHED, "_id": f"c{i}"}}))
        lines.append(json.dumps(
            {"text": r["subject"] + "\n" + r["text"], "gold": r["label"]}))
    resp = req("POST", "/_bulk", ("\n".join(lines) + "\n").encode(),
               "application/x-ndjson")
    if "_err" in resp or resp.get("errors"):
        die("candidate bulk failed", resp)
    req("POST", f"/{WATCHED}/_refresh")
    print(f"watched '{WATCHED}': {len(cands)} boundary candidates indexed after baseline")


def gold_by_doc(cands):
    return {f"c{i}": r["label"] for i, r in enumerate(cands)}


def fire_count():
    req("POST", f"/{FIRES}/_refresh")
    r = req("GET", f"/{FIRES}/_count")
    return r.get("count", 0) if "_err" not in r else 0


def wait_for_fires(expected_candidates):
    # Each candidate fires at most once (under the watch matching its argmax
    # label, if confident enough). Poll until the count is stable across
    # several ticks; the watcher interval is 1s.
    print("waiting for the watcher to evaluate (1s ticks)...")
    last, stable = -1, 0
    for _ in range(90):
        time.sleep(2)
        n = fire_count()
        if n == last:
            stable += 1
            if stable >= 4:
                print(f"  fires stable at {n}")
                return n
        else:
            print(f"  fires={n}")
            stable = 0
            last = n
    print(f"  stopped waiting at {last} fires")
    return last


def read_fires():
    req("POST", f"/{FIRES}/_refresh")
    r = req("GET", f"/{FIRES}/_search",
            {"size": 10000, "query": {"match_all": {}}})
    if "_err" in r:
        die("could not read fires", r)
    return [h["_source"] for h in r.get("hits", {}).get("hits", [])]


def rate(num, den):
    return (num / den) if den else float("nan")


def main():
    corpus = json.load(open("../systemone-classify-email/corpus.json"))
    train = [r for r in corpus if r["split"] == "train"]
    cands = json.load(open("boundary.json"))
    labels = sorted({r["label"] for r in cands})

    print(f"XERJ_URL={U}  k={K}  p_min={P_MIN}  conf_gate>={CONF_GATE}")
    index_history(train)
    make_watched_index()
    print("creating one detection watch per label (one-vs-rest):")
    for lab in labels:
        put_watch(lab)
    index_candidates(cands)
    wait_for_fires(len(cands))

    fires = read_fires()
    gold = gold_by_doc(cands)

    # Deduplicate: one fire per (doc_id, rule). A candidate's argmax is one
    # label, so it should fire under exactly one watch; keep the highest p if
    # the node retried.
    best = {}
    for f in fires:
        did = f.get("doc_id")
        if did not in gold:
            continue  # a fire from some other run's docs; ignore
        key = did
        if key not in best or f.get("p", 0) > best[key].get("p", 0):
            best[key] = f
    fires = list(best.values())

    rows = []
    any_cal = False
    for f in fires:
        did = f["doc_id"]
        fired = f.get("label")
        g = gold[did]
        p = f.get("p")
        pc = f.get("p_cal")
        if f.get("calibrated") and isinstance(pc, (int, float)):
            any_cal = True
        rows.append({"doc_id": did, "fired": fired, "gold": g,
                     "correct": fired == g, "p": p, "p_cal": pc,
                     "calibrated": bool(f.get("calibrated"))})

    total_fires = len(rows)
    # The number, on RAW p (what the watcher fires on and records):
    conf_raw = [r for r in rows if isinstance(r["p"], (int, float)) and r["p"] >= CONF_GATE]
    wrong_conf_raw = [r for r in conf_raw if not r["correct"]]
    wac_raw = rate(len(wrong_conf_raw), len(conf_raw))

    # On p_cal, only if the node calibrated:
    conf_cal = [r for r in rows if r["calibrated"]
                and isinstance(r["p_cal"], (int, float)) and r["p_cal"] >= CONF_GATE]
    wrong_conf_cal = [r for r in conf_cal if not r["correct"]]
    wac_cal = rate(len(wrong_conf_cal), len(conf_cal))

    correct_all = sum(r["correct"] for r in rows)
    confusion = collections.Counter((r["gold"], r["fired"]) for r in rows)

    print("\n" + "=" * 64)
    print("RESULT — wrong-and-confident gate via the _watcher path")
    print("=" * 64)
    print(f"candidates evaluated : {len(cands)}")
    print(f"total fires          : {total_fires} "
          f"(a candidate fires when its vote argmax clears p_min={P_MIN})")
    print(f"fired correctly      : {correct_all}/{total_fires} "
          f"= {rate(correct_all, total_fires):.4f}")
    print(f"calibration present  : {'yes (p_cal on fires)' if any_cal else 'no — p_cal absent, reporting raw p'}")
    print()
    print(f"confidence compared at >= {CONF_GATE}: RAW p  (always present on a fire)")
    print(f"  confident fires        : {len(conf_raw)}")
    print(f"  wrong AND confident    : {len(wrong_conf_raw)}")
    print(f"  WRONG-AND-CONFIDENT RATE: {wac_raw:.4f}  "
          f"({'PASS' if wac_raw <= 0.05 else 'FAIL'} vs <= 0.05 bar)")
    if any_cal:
        print(f"\nconfidence compared at >= {CONF_GATE}: p_cal (calibrated)")
        print(f"  confident fires        : {len(conf_cal)}")
        print(f"  wrong AND confident    : {len(wrong_conf_cal)}")
        print(f"  WRONG-AND-CONFIDENT RATE: {wac_cal:.4f}  "
              f"({'PASS' if wac_cal <= 0.05 else 'FAIL'} vs <= 0.05 bar)")

    print("\nwrong-and-confident fires (raw p >= %.2f):" % CONF_GATE)
    for r in sorted(wrong_conf_raw, key=lambda r: -r["p"]):
        print(f"  doc={r['doc_id']:>4}  gold={r['gold']:<12} fired={r['fired']:<12} "
              f"p={r['p']:.3f} p_cal={r['p_cal']}")

    out = {
        "url": U, "k": K, "p_min": P_MIN, "conf_gate": CONF_GATE,
        "n_candidates": len(cands), "n_fires": total_fires,
        "fired_correct": correct_all,
        "calibration_present": any_cal,
        "raw": {"confident_fires": len(conf_raw),
                "wrong_and_confident": len(wrong_conf_raw),
                "rate": wac_raw, "pass": bool(wac_raw <= 0.05)},
        "calibrated": ({"confident_fires": len(conf_cal),
                        "wrong_and_confident": len(wrong_conf_cal),
                        "rate": wac_cal, "pass": bool(wac_cal <= 0.05)}
                       if any_cal else None),
        "confusion": {f"{g}->{p}": c for (g, p), c in confusion.items()},
        "fires": rows,
    }
    os.makedirs("results", exist_ok=True)
    json.dump(out, open("results/run.json", "w"), indent=1)
    print("\nwrote results/run.json")


if __name__ == "__main__":
    main()
