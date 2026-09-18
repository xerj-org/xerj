#!/usr/bin/env python3
"""Check a live XERJ node against the ground truth synthetic-takeout.py wrote.

Read-only: every request is a _search. Prints one JSON object.

For each planted needle (a token that exists in exactly one place in the
corpus) it asks the node for that token and records whether it came back
exactly once and from the right message. `pdf-attachment` needles are the ones
that matter most: they sit on a page INSIDE a PDF that is INSIDE a base64 MIME
part INSIDE the mbox, so a hit proves the whole chain ran in the real binary.
"""
import argparse
import collections
import json
import sys
import urllib.error
import urllib.request


def post(url, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        return {"error": "HTTP %d: %s" % (e.code, e.read()[:300].decode("utf-8", "replace"))}
    except Exception as e:  # noqa: BLE001 — a verifier reports, it does not crash
        return {"error": repr(e)}


def total(res):
    t = res.get("hits", {}).get("total", 0)
    return t.get("value", 0) if isinstance(t, dict) else t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--prefix", required=True)
    ap.add_argument("--brain", required=True)
    ap.add_argument("--truth", required=True)
    a = ap.parse_args()
    truth = json.load(open(a.truth, encoding="utf-8"))
    nodes = "%s/%s-*/_search" % (a.url, a.prefix)
    edges = "%s/.xerj-memory-%s-edges/_search" % (a.url, a.brain)
    count = lambda url, q: total(post(url, {"size": 0, "track_total_hits": True, "query": q}))  # noqa: E731

    out = {"node_docs": count(nodes, {"match_all": {}})}
    out["mbox_docs"] = count(nodes, {"term": {"ax_format": "mbox"}})
    out["attachment_docs"] = count(nodes, {"exists": {"field": "attachment_name"}})

    per = collections.defaultdict(lambda: {"planted": 0, "found_exactly_once": 0, "missing": 0, "duplicated": 0,
                                           "wrong_message": 0})
    examples = []
    for nd in truth["needles"]:
        kind = per[nd["where"]]
        kind["planted"] += 1
        res = post(nodes, {"size": 3, "track_total_hits": True, "query": {"match": {"body": nd["token"]}},
                           "_source": ["email_message_id", "ax_locator", "ax_path"]})
        n = total(res)
        if n == 1:
            src = res["hits"]["hits"][0].get("_source", {})
            if nd["message_id"] and src.get("email_message_id") != nd["message_id"]:
                kind["wrong_message"] += 1
            else:
                kind["found_exactly_once"] += 1
        elif n == 0:
            kind["missing"] += 1
            if len(examples) < 5:
                examples.append({"missing": nd, "response_error": res.get("error")})
        else:
            kind["duplicated"] += 1
            if len(examples) < 5:
                examples.append({"duplicated": nd, "hits": n})
    out["needles"] = dict(per)
    out["needle_examples_of_failure"] = examples

    out["edges"] = {
        "replies_to": {"written": count(edges, {"term": {"type": "replies_to"}}),
                       "truth_resolvable_replies": truth["replies_resolvable"]},
        "attachment_of": {"written": count(edges, {"term": {"type": "attachment_of"}}),
                          "attachment_docs": out["attachment_docs"]},
    }
    # The defect the e2e test found: an empty mbox entry indexed as a document.
    out["empty_no_subject_docs"] = count(nodes, {"bool": {"filter": [{"term": {"ax_format": "mbox"}}],
                                                          "must": [{"match_phrase": {"title": "(no subject)"}}],
                                                          "must_not": [{"exists": {"field": "email_message_id"}}]}})
    json.dump(out, sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
