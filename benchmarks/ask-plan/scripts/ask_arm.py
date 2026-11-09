#!/usr/bin/env python3
"""ask_arm.py — fixture loader + the /_ask arm of the ask-plan harness.

STATUS: DESIGN, NOTHING MEASURED YET. POST /_ask does not exist in the engine
today (issue #1056 is the work item); when run.sh finds it absent it stops
after the fixture self-check and writes a status file that says so, with
measured:false. Nothing in here fabricates a result.

Three subcommands (python3 stdlib only):

  load    create the three ax-* indices with explicit mappings and bulk-load
          the committed raw snapshots, assigning _id from each dataset's own
          key column — the SAME id space data/gold/pairs.jsonl derives gold
          doc_ids from, so gold and index contents are tied to the same bytes.
  selfcheck   execute every pair's query_equivalent and require the hit-id
          set to equal gold doc_ids. This validates the FIXTURE against a live
          engine: a failure here means the fixture is wrong (or the engine's
          query semantics differ from the DSL we wrote), and it is reported as
          a fixture error, never folded into /_ask scores.
  ask     the measurement: for each pair, POST /_ask {index, prompt}, then
          (a) the returned query must execute — a 400 means invalid DSL out,
          exactly what #1056's zero-invalid-DSL line forbids; (b) its hit-id
          set is compared to gold for precision/recall/F1. Raw per-pair
          responses go to the run directory untouched; the summary is written
          next to them. It exits 0 and records measured:false if /_ask is
          absent (404/405/501).

Usage (run.sh drives this):
  XERJ_URL=http://127.0.0.1:9610 python3 scripts/ask_arm.py load
  XERJ_URL=... python3 scripts/ask_arm.py selfcheck --out DIR
  XERJ_URL=... python3 scripts/ask_arm.py ask --out DIR [--determinism N]
"""
import argparse
import csv
import json
import os
import pathlib
import sys
import time
import urllib.error
import urllib.request

HERE = pathlib.Path(__file__).resolve().parent
DATA = HERE.parent / "data"
U = os.environ.get("XERJ_URL", "http://127.0.0.1:9610").rstrip("/")

# _id is derived from the data's own key column — never file order — so gold
# doc_ids (derived from the same rows) match what the engine stores.
DATASETS = {
    "usgs-earthquakes": dict(
        file="usgs-earthquakes.csv", delimiter=",",
        id=lambda r: r["id"],
        mappings={
            "time": {"type": "date"}, "place": {"type": "keyword"},
            "mag": {"type": "double"}, "depth": {"type": "double"},
            "latitude": {"type": "double"}, "longitude": {"type": "double"},
            "magType": {"type": "keyword"}, "net": {"type": "keyword"},
            "nst": {"type": "long"}, "rms": {"type": "double"},
            "type": {"type": "keyword"}, "status": {"type": "keyword"},
        },
    ),
    "nasa-exoplanets": dict(
        file="nasa-exoplanets.csv", delimiter=",",
        id=lambda r: r["pl_name"],
        mappings={
            "hostname": {"type": "keyword"},
            "sy_snum": {"type": "long"}, "sy_pnum": {"type": "long"},
            "discoverymethod": {"type": "keyword"}, "disc_year": {"type": "long"},
            "pl_orbper": {"type": "double"}, "pl_rade": {"type": "double"},
            "pl_bmasse": {"type": "double"}, "pl_eqt": {"type": "double"},
            "st_teff": {"type": "double"}, "st_rad": {"type": "double"},
            "st_mass": {"type": "double"},
            "ra": {"type": "double"}, "dec": {"type": "double"},
        },
    ),
    "gapminder": dict(
        file="gapminder.tsv", delimiter="\t",
        id=lambda r: f"{r['country']}-{r['year']}",
        mappings={
            "country": {"type": "keyword"}, "continent": {"type": "keyword"},
            "year": {"type": "long"}, "lifeExp": {"type": "double"},
            "pop": {"type": "long"}, "gdpPercap": {"type": "double"},
        },
    ),
}


def req(method, path, body=None, ctype="application/json", timeout=120):
    data = body.encode() if isinstance(body, str) else body
    r = urllib.request.Request(U + path, data=data, method=method)
    if data is not None:
        r.add_header("content-type", ctype)
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:800]
        try:
            detail = json.loads(detail)
        except json.JSONDecodeError:
            pass
        return e.code, detail
    except urllib.error.URLError as e:
        raise SystemExit(f"engine unreachable at {U}: {e}")


def typed(row, mappings):
    """Coerce CSV strings to the mapped JSON type; blank -> omit (missing)."""
    doc = {}
    for k, v in row.items():
        m = mappings.get(k)
        if v is None or v == "":
            continue
        if m and m["type"] in ("double", "long"):
            f = float(v)
            doc[k] = int(f) if m["type"] == "long" else f
        else:
            doc[k] = v
    return doc


def cmd_load():
    for name, spec in DATASETS.items():
        idx = f"ax-{name}"
        code, body = req("DELETE", f"/{idx}")
        code, body = req("PUT", f"/{idx}", json.dumps(
            {"mappings": {"properties": dict(spec["mappings"], **{
                # a text projection of the row for any semantic arm later; not
                # used by any current pair
                "row_text": {"type": "text"},
            })}}))
        if code not in (200, 201):
            print(f"PUT {idx} -> {code}: {json.dumps(body)[:300]}")
            return 1
        rows = list(csv.DictReader(open(DATA / "raw" / name / spec["file"]),
                                   delimiter=spec["delimiter"]))
        nd = ""
        for r in rows:
            _id = spec["id"](r)
            nd += json.dumps({"index": {"_index": idx, "_id": _id}}) + "\n"
            nd += json.dumps(typed(r, spec["mappings"])) + "\n"
        code, body = req("POST", "/_bulk?refresh=true", nd, "application/x-ndjson", timeout=600)
        if code != 200 or body.get("errors"):
            print(f"bulk {idx} -> {code} errors={body.get('errors')}: {json.dumps(body)[:400]}")
            return 1
        code, body = req("GET", f"/{idx}/_count")
        print(f"loaded {idx}: {body.get('count')} docs ({len(rows)} rows in raw)")
        if body.get("count") != len(rows):
            print(f"::error::count mismatch on {idx}")
            return 1
    return 0


def search_ids(index, query, size=10000):
    body = json.dumps({"query": query, "size": size, "_source": False})
    code, resp = req("POST", f"/{index}/_search", body)
    if code != 200:
        return None, code, resp
    hits = resp.get("hits", {})
    total = hits.get("total")
    total = total.get("value") if isinstance(total, dict) else total
    ids = {h["_id"] for h in hits.get("hits", [])}
    return ids, code, (total, len(ids))


def pairs():
    return [json.loads(l) for l in open(DATA / "gold" / "pairs.jsonl")]


def cmd_selfcheck(out):
    bad = 0
    for p in pairs():
        ids, code, detail = search_ids(p["gold"]["index"], p["gold"]["query_equivalent"])
        if ids is None:
            print(f"FIXTURE-ERROR {p['id']}: query returned HTTP {code}: {json.dumps(detail)[:200]}")
            bad += 1
        elif ids != set(p["gold"]["doc_ids"]):
            missing = sorted(set(p["gold"]["doc_ids"]) - ids)[:4]
            extra = sorted(ids - set(p["gold"]["doc_ids"]))[:4]
            print(f"FIXTURE-ERROR {p['id']}: engine {detail[0]} hits vs {len(p['gold']['doc_ids'])} gold"
                  f" (missing e.g. {missing}, extra e.g. {extra})")
            bad += 1
    if bad:
        print(f"::error::ask-plan selfcheck: {bad} pair(s) do not reproduce their gold set —"
              " the FIXTURE (or query semantics) is wrong; fix before measuring anything")
        return 1
    print("selfcheck: every query_equivalent reproduces its gold doc-id set")
    return 0


def f1(pred, gold):
    if not pred and not gold:
        return 1.0
    tp = len(pred & gold)
    if not tp:
        return 0.0
    p = tp / len(pred)
    r = tp / len(gold)
    return 2 * p * r / (p + r)


def ask_once(prompt, index):
    body = json.dumps({"index": index, "prompt": prompt})
    t0 = time.perf_counter()
    code, resp = req("POST", "/_ask", body, timeout=300)
    ms = (time.perf_counter() - t0) * 1000.0
    return code, resp, ms


def cmd_ask(out, determinism_runs):
    out = pathlib.Path(out)
    out.mkdir(parents=True, exist_ok=True)

    # endpoint presence — the honest stopping point while #1056 is open
    code, resp, _ = ask_once("probe: every event", "ax-usgs-earthquakes")
    if code in (404, 405, 501):
        status = {"endpoint": "POST /_ask", "http": code, "measured": False,
                  "note": "POST /_ask is not implemented (issue #1056 open); "
                          "the harness ran zero measurement passes"}
        (out / "status.json").write_text(json.dumps(status, indent=1) + "\n")
        print(f"POST /_ask -> {code}: not implemented; nothing measured, nothing fabricated")
        return 0

    raw = out / "ask-raw.jsonl"
    per = []
    invalid = 0
    with open(raw, "w") as fh:
        for p in pairs():
            codes, resps = [], []
            for _ in range(max(1, determinism_runs)):
                c, r, ms = ask_once(p["prompt"], p["gold"]["index"])
                codes.append(c)
                resps.append(r)
            deterministic = all(json.dumps(x, sort_keys=True) == json.dumps(resps[0], sort_keys=True)
                                for x in resps[1:])
            code = codes[0]
            rec = {"id": p["id"], "http": codes, "deterministic": deterministic,
                   "response": resps[0]}
            got, pair_invalid, score = set(), False, None
            q = (resps[0] or {}).get("query") if isinstance(resps[0], dict) else None
            if code == 200 and isinstance(q, (dict, str)):
                if isinstance(q, str):
                    try:
                        q = json.loads(q)
                    except json.JSONDecodeError:
                        q = None
                if q:
                    # #1056's own hard line: every returned plan passes
                    # xerj_query::parse_request. Executing the query over the
                    # wire is the observable form of that: 400 => invalid DSL.
                    ids, scode, sresp = search_ids(p["gold"]["index"], q)
                    if ids is None:
                        pair_invalid = True
                        rec["exec_http"] = scode
                        rec["exec_error"] = json.dumps(sresp)[:300]
                    else:
                        got = ids
                        score = f1(got, set(p["gold"]["doc_ids"]))
                else:
                    pair_invalid = True
            elif code != 200:
                # a 422 naming an unresolved phrase is correct behaviour for a
                # prompt this fixture does not contain — but every pairs.jsonl
                # prompt names values that DO exist, so anything but 200 is a
                # failure for the F1 gate; record it and score zero.
                score = 0.0
            else:
                # 200 with no usable query field: wrong response shape — the
                # zero-invalid-DSL line counts it as invalid DSL out.
                pair_invalid = True
            invalid += int(pair_invalid)
            rec.update(score=score, invalid_dsl=pair_invalid,
                       got_n=len(got), gold_n=len(p["gold"]["doc_ids"]))
            per.append(rec)
            fh.write(json.dumps(rec) + "\n")

    # all-pair macro-F1: an invalid plan (score None) counts as 0.0, not skipped
    macro = (sum((r["score"] or 0.0) for r in per) / len(per)) if per else 0.0
    summary = {
        "endpoint": "POST /_ask",
        "pairs": len(per),
        "measured": True,
        "invalid_dsl_out": invalid,
        "macro_f1": round(macro, 4),
        "non_deterministic": sum(1 for r in per if not r["deterministic"]),
        "raw": str(raw),
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps(summary, indent=1))
    print("(numbers above are from THIS run only; the README gate table stays empty"
          " until maintainers bless a recorded run)")
    return 0


def main():
    ap = argparse.ArgumentParser(description="ask-plan fixture + /_ask arm")
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("load")
    sc = sub.add_parser("selfcheck")
    sc.add_argument("--out", default="results/tmp")
    ak = sub.add_parser("ask")
    ak.add_argument("--out", required=True)
    ak.add_argument("--determinism", type=int, default=1,
                    help="N passes per pair; all N responses must be identical (issue: 3)")
    args = ap.parse_args()
    if args.cmd == "load":
        return cmd_load()
    if args.cmd == "selfcheck":
        return cmd_selfcheck(args.out)
    return cmd_ask(args.out, args.determinism)


if __name__ == "__main__":
    sys.exit(main())
