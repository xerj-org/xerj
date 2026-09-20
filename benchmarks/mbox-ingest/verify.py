#!/usr/bin/env python3
"""Check a live XERJ node against the ground truth synthetic-takeout.py wrote.

Read-only: every request is a _search. Prints one JSON object.

For each planted needle (a token that exists in exactly one place in the
corpus) it asks the node for that token and records whether it came back
exactly once and from the right message. `pdf-attachment` needles are the ones
that matter most: they sit on a page INSIDE a PDF that is INSIDE a base64 MIME
part INSIDE the mbox, so a hit proves the whole chain ran in the real binary.

It also checks what the docs teach, against the node rather than by assumption:
the LIVE `replies_to` / `attachment_of` edge counts (the brain is bi-temporal,
so superseded edges are counted apart), a `term` filter on
`email_from_address` for sampled senders, and `match_phrase` on `body` for
sampled subjects. Every request's latency is recorded.
"""
import argparse
import collections
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

HEADERS = {"Content-Type": "application/json"}
LATENCIES = []  # seconds, one per request - a restarted node answers some of these in tens of seconds


def post(url, body):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=HEADERS)
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            out = json.load(r)
            LATENCIES.append(time.monotonic() - started)
            return out
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
    ap.add_argument("--per-kind", type=int, default=0,
                    help="needles checked per kind (default 0 = EVERY needle). n > 0 checks the FIRST n in "
                         "planting order, i.e. the start of the mailbox only - a sample, and the output says "
                         "so in each kind's `checked`. All 2,530 needles of the 1 GB tree took 58 s on a "
                         "settled node; against a node that is still merging it once took over 15 minutes.")
    ap.add_argument("--api-key", default=os.environ.get("XERJ_API_KEY", ""),
                    help="admin key of a node running with auth on (<data_dir>/admin.key); default $XERJ_API_KEY")
    ap.add_argument("--field-samples", type=int, default=40,
                    help="distinct subjects / sender addresses sampled for the subject and sender checks")
    a = ap.parse_args()
    if a.api_key:
        HEADERS["Authorization"] = "ApiKey " + a.api_key
    truth = json.load(open(a.truth, encoding="utf-8"))
    nodes = "%s/%s-*/_search" % (a.url, a.prefix)
    edges = "%s/.xerj-memory-%s-edges/_search" % (a.url, a.brain)
    count = lambda url, q: total(post(url, {"size": 0, "track_total_hits": True, "query": q}))  # noqa: E731

    out = {"node_docs": count(nodes, {"match_all": {}})}
    out["mbox_docs"] = count(nodes, {"term": {"ax_format": "mbox"}})
    out["attachment_docs"] = count(nodes, {"exists": {"field": "attachment_name"}})

    per = collections.defaultdict(lambda: {"planted": 0, "found_exactly_once": 0, "found_in_overlapping_sections": 0,
                                           "missing": 0, "duplicated": 0, "wrong_message": 0})
    examples = []
    for nd in truth["needles"]:
        kind = per[nd["where"]]
        kind["planted"] += 1
        if a.per_kind and kind["planted"] > a.per_kind:
            continue
        kind["checked"] = kind.get("checked", 0) + 1
        # `body` OR `text`: mail is always `body`, but a Drive document of the export that is sniffed
        # as line-oriented text carries its words in `text`, and a body-only query misses it.
        res = post(nodes, {"size": 10, "track_total_hits": True,
                           "query": {"multi_match": {"query": nd["token"], "fields": ["body", "text"]}},
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
            # More than one DOCUMENT is not yet more than one MESSAGE. A long body is cut into
            # sections that overlap by a paragraph (so a phrase on a cut is still findable whole), and a
            # token that sits in the shared paragraph is in both neighbours: `m601-msg-s1` and
            # `m601-msg-s2`. Same file, same container slot, same part, right message -> counted apart
            # from `duplicated`, which stays reserved for a token found in two different places.
            hits = [h.get("_source", {}) for h in res["hits"]["hits"]]
            slots = {(h.get("ax_path"), re.sub(r"-s\d+$", "", h.get("ax_locator", ""))) for h in hits}
            right = all(not nd["message_id"] or h.get("email_message_id") == nd["message_id"] for h in hits)
            if len(hits) == n and len(slots) == 1 and right:
                kind["found_in_overlapping_sections"] += 1
            else:
                kind["duplicated"] += 1
                if len(examples) < 5:
                    examples.append({"duplicated": nd, "hits": n})
    out["needles"] = dict(per)
    out["needle_examples_of_failure"] = examples

    # LIVE edges only. The brain is bi-temporal: an incremental re-run of a changed mailbox keeps the
    # superseded edges with `invalid_at` set, so a bare type count doubles after one re-run.
    def live(edge_type):
        return {"bool": {"filter": [{"term": {"type": edge_type}}], "must_not": [{"exists": {"field": "invalid_at"}}]}}

    out["edges"] = {
        "replies_to": {"live": count(edges, live("replies_to")),
                       "stored_including_invalidated": count(edges, {"term": {"type": "replies_to"}}),
                       "truth_resolvable_replies": truth["replies_resolvable"]},
        "attachment_of": {"live": count(edges, live("attachment_of")),
                          "stored_including_invalidated": count(edges, {"term": {"type": "attachment_of"}}),
                          "attachment_docs": out["attachment_docs"]},
    }

    # The two queries the docs teach, checked against the node rather than assumed (both were broken
    # in the first cut of this feature and no needle kind could see it):
    #  - a SENDER filter: `term email_from_address` must find what the display-form header holds;
    #  - SUBJECT words: the subject opens the searchable body of a message's first section.
    first_section = {"bool": {"should": [{"term": {"ax_locator": "msg-s0"}}, {"wildcard": {"ax_locator": "m*-msg-s0"}}],
                              "minimum_should_match": 1}}

    def buckets(field):
        res = post(nodes, {"size": 0, "aggs": {"f": {"terms": {"field": field, "size": a.field_samples}}}})
        return [b["key"] for b in res.get("aggregations", {}).get("f", {}).get("buckets", [])]

    senders = {"sampled": 0, "term_matches_display_form": 0, "mismatch": []}
    for addr in buckets("email_from_address"):
        senders["sampled"] += 1
        by_addr = count(nodes, {"term": {"email_from_address": addr}})
        by_display = count(nodes, {"wildcard": {"email_from": "*" + addr + "*"}})
        if by_addr > 0 and by_addr == by_display:
            senders["term_matches_display_form"] += 1
        elif len(senders["mismatch"]) < 5:
            senders["mismatch"].append({"address": addr, "term": by_addr, "display_form": by_display})
    out["sender_filter"] = senders

    subjects = {"sampled": 0, "subject_found_in_body": 0, "mismatch": []}
    for subject in buckets("email_subject"):
        if not subject.strip() or len(subject) > 200:
            continue  # the body carries at most the first 512 characters of a subject
        subjects["sampled"] += 1
        scope = [{"term": {"email_subject": subject}}, first_section]
        messages = count(nodes, {"bool": {"filter": scope}})
        in_body = count(nodes, {"bool": {"filter": scope, "must": [{"match_phrase": {"body": subject}}]}})
        if messages > 0 and messages == in_body:
            subjects["subject_found_in_body"] += 1
        elif len(subjects["mismatch"]) < 5:
            subjects["mismatch"].append({"subject": subject, "messages": messages, "match_phrase_body": in_body})
    out["subject_search"] = subjects
    # The defect the e2e test found: an empty mbox entry indexed as a document.
    out["empty_no_subject_docs"] = count(nodes, {"bool": {"filter": [{"term": {"ax_format": "mbox"}}],
                                                          "must": [{"match_phrase": {"title": "(no subject)"}}],
                                                          "must_not": [{"exists": {"field": "email_message_id"}}]}})
    lat = sorted(LATENCIES)
    if lat:
        out["query_latency_seconds"] = {
            "requests": len(lat), "total": round(sum(lat), 2), "median": round(lat[len(lat) // 2], 4),
            "p95": round(lat[min(len(lat) - 1, int(len(lat) * 0.95))], 4), "max": round(lat[-1], 3),
            "over_1s": sum(1 for x in lat if x > 1.0), "over_10s": sum(1 for x in lat if x > 10.0)}
    json.dump(out, sys.stdout, indent=1, ensure_ascii=False)
    print()


if __name__ == "__main__":
    main()
