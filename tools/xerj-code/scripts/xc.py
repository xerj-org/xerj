#!/usr/bin/env python3
"""Retrieve reference passages from an indexed corpus.

This is the tool the agent calls before writing code. It returns ranked passages
with file:line provenance, and it refuses to answer from a stale index rather
than handing back code that no longer exists.
"""
import argparse
import http.client
import json
import os
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

ROOT = os.environ.get("XERJ_CODE_HOME", os.path.expanduser("~/.xerj-code"))
URL = os.environ.get("XERJ_URL", "http://localhost:9200")
STALE_DAYS = 30

# Reciprocal Rank Fusion constant (Cormack, Clarke & Buettcher, SIGIR 2009).
# k=60 is the paper's value and the universal default. It is NOT tuned here.
RRF_K = 60

# Licences you must not copy from, as substrings of what the manifest records.
# This MUST stay in step with `warn_licence()` in xc-corpus.sh: that one fires
# once at clone time, this one fires at the moment an agent is actually looking
# at the code and deciding whether to lift it, which is the one that counts.
#
# Warning on only ("GPL", "LGPL") — an exact match, as this did — was silent on
# every restricted repo the hub deliberately ships: elasticsearch (AGPL/SSPL/
# Elastic), meilisearch's Enterprise Edition parts (BUSL), and, once the
# detector was corrected to read MPL-2.0 rather than GPL, sonic as well. Fixing
# the detector must not quietly cost a repo its warning.
RESTRICTED = ("AGPL", "SSPL", "Elastic", "BUSL", "GPL", "LGPL", "MPL",
              "UNKNOWN", "NONE-FOUND")


def die(msg, code=2):
    print(f"xc: {msg}", file=sys.stderr)
    sys.exit(code)


def field_report_nudge():
    """One line, on STDERR, at most once a day: file the field report.

    The reference-coding loop had no path back to the maintainers — an agent
    retrieved, wrote code, and left, and the folder that asks for a short field
    report stayed empty. This is that missing path, surfaced where the agent
    already is. It is deliberately unobtrusive: it writes only to stderr, so it
    can NEVER corrupt the retrieval passages a caller parses on stdout; it fires
    at most once per day per machine via a marker under ROOT, so a session's many
    queries do not turn it into spam; and it is silenced entirely by
    XERJ_CODE_NO_NUDGE. A cache we cannot write is a reason to stay quiet, not to
    nag every query.
    """
    if os.environ.get("XERJ_CODE_NO_NUDGE"):
        return
    try:
        marker = os.path.join(ROOT, f".field-report-nudge-{datetime.now():%Y%m%d}")
        if os.path.exists(marker):
            return
        os.makedirs(ROOT, exist_ok=True)
        open(marker, "w").close()
    except OSError:
        return
    print(
        "xc: wrapping up? `xerj feedback --open-pr` files the one short field "
        "report XERJ asks every agent for (or --dry-run if sandboxed); it "
        "auto-fills the facts and a field-report-only PR skips the CLA gate. "
        "Silence with XERJ_CODE_NO_NUDGE=1.",
        file=sys.stderr,
    )


def load_state(corpus):
    path = os.path.join(ROOT, "state", f"{corpus}.json")
    if not os.path.exists(path):
        die(f"corpus '{corpus}' is not indexed — run xc-index.sh {corpus}")
    with open(path) as fh:
        return json.load(fh)


def live_index_count(prefix):
    """How many indices under `prefix*` actually exist on the target server.

    `state/<corpus>.json` records that a corpus was once indexed, and against
    which url. It is NOT proof the corpus is loaded on the server THIS process
    is querying: the state entry survives a data-dir swap, a server restart onto
    an empty dir, or an XERJ_URL pointed at a different node. When that happens
    the prefix resolves to zero indices and every query returns "no match" —
    indistinguishable from a genuinely bad query, which is the exact failure
    reference-coding exists to prevent (an agent concludes "no reference" and
    starts guessing). So the count is checked explicitly.

    Returns an int on success (0 meaning "in state/ but not loaded here"), or
    None when the server is unreachable/unparseable — in which case the caller
    must NOT claim "0 live indices": the ordinary search path will report the
    transport failure honestly instead.
    """
    req = urllib.request.Request(f"{URL}/_cat/indices/{prefix}*?format=json&h=index")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.load(resp)
    except urllib.error.HTTPError as e:
        # A 404 on a wildcard means "no such indices" -> 0 loaded. Any other
        # HTTP status is an ambiguous server condition, not a clean zero.
        return 0 if e.code == 404 else None
    except (urllib.error.URLError, ValueError):
        return None
    if not isinstance(data, list):
        return None
    return len(data)


def require_loaded(state):
    """Fail with a DISTINCT, actionable message when the corpus is not loaded.

    Kept deliberately separate from the empty-result path in main(): "0 live
    indices here" and "the corpus is loaded but nothing matched your query" are
    different diagnoses with different fixes, and collapsing them is the bug this
    guard closes.
    """
    prefix = state["prefix"]
    count = live_index_count(prefix)
    if count is None:
        return  # server unreachable/ambiguous — let the search path report it.
    if count == 0:
        name = state.get("corpus", "?")
        stamp = state.get("indexed_at", "?")
        against = state.get("url")
        hint = (f" It was indexed against {against}." if against and against != URL
                else " It was indexed against a different data dir or server.")
        die(f"corpus '{name}' is in state/ (indexed {stamp}) but has 0 live "
            f"indices ('{prefix}*') on {URL}.{hint} Re-run xc-index.sh {name}, "
            f"or set XERJ_URL to the node that has it. (This is NOT a 'no "
            f"match' — the corpus simply is not loaded on this server.)", code=3)


def check_fresh(state, stale_ok):
    """A stale index returns code that no longer exists, with false confidence."""
    stamp = state.get("indexed_at")
    if not stamp:
        return None
    when = datetime.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)
    age = datetime.now(timezone.utc) - when
    if age > timedelta(days=STALE_DAYS) and not stale_ok:
        die(f"index for '{state['corpus']}' is {age.days} days old. "
            f"Re-run xc-index.sh {state['corpus']} --fresh, or pass --stale-ok.")
    return age.days


# These exact fields and these exact (flat) weights were measured, not chosen.
# Swept 7 variants over 6 ground-truth queries on a 324-file Rust corpus:
#
#   body, defs, title  (flat)   top1 3/6   top3 6/6   <- this
#   title^3 defs body           top1 3/6   top3 6/6
#   defs^2 body                 top1 3/6   top3 4/6
#   defs^3 title^2 body         top1 2/6   top3 5/6
#   body alone                  top1 2/6   top3 3/6
#   title^2 body                top1 1/6   top3 2/6
#
# Boosting `defs` looks obviously right and measures worse: `defs` is a flat
# blob of every symbol name in the file, so a boost favours whichever file
# defines the most symbols — usually a test module, not the implementation.
# top3 is the metric that matters here; the agent reads k passages, not one.
#
# Two fields are deliberately absent:
#   `symbols.name` — `symbols` is an array of objects with no searchable `.name`
#     subpath. Including it makes the whole multi_match return ZERO hits with no
#     error at all. It took an Aho-Corasick query from 0 hits to 3 correct
#     source files just to remove it. A silent zero is the worst failure shape
#     available, so it is called out here rather than left to be rediscovered.
#   `"*"` — a bare wildcard flattens every score to the same value (measured:
#     every hit scored exactly 2.0), destroying the ranking entirely.
# `defs_expanded^0.5` — a low-weight RECALL field (present only on indexes built
# by the enrichment server): per-symbol signatures + identifier sub-words
# (`per_tick` -> `per tick`). It is boosted BELOW 1.0 on purpose, so it adds hits
# a behavioural query would otherwise miss WITHOUT overriding the precise name
# match in `defs`. The boost is measured, not chosen: see
# measure/SERVER_UPLIFT_SCORECARD.md.
#
# It is NOT safe to send unconditionally — see resolve_fields().
FIELDS = ["body", "defs", "title", "defs_expanded^0.5"]

_FIELDS_CACHE = {}


def resolve_fields(prefix):
    """Drop query fields that no index under `prefix` actually maps.

    This engine does NOT ignore an unmapped field in `multi_match` the way ES
    does — including one silently collapses a MULTI-TOKEN query to ZERO hits.
    Measured 2026-08-06, one index, exact totals (`relation: eq`):

        query "log merge policy segment size buckets"
        fields=["body"]                                 -> 673 hits
        fields=["body","defs"]                          -> 673 hits
        fields=["body","defs","title"]                  -> 673 hits
        fields=["body","defs","title","defs_expanded"]  ->   0 hits

    `defs_expanded` was mapped in exactly 0 of the corpus's 219 indices, so
    every multi-word query returned "no passage matches". Mapped fields of any
    type (keyword, numeric) are harmless; only unmapped ones zero the query,
    and only when the query has more than one token.

    The old comment here asserted the opposite — "a no-op there, multi_match
    ignores a missing field" — which was never true on this engine. Until the
    engine is fixed, resolve the field list against the real mapping first.

    Full reproducer and the separate memtable/segment divergence it uncovered:
    measure/MULTIMATCH_DEFECT.md.
    """
    if prefix in _FIELDS_CACHE:
        return _FIELDS_CACHE[prefix]
    present = None
    try:
        req = urllib.request.Request(f"{URL}/{prefix}*/_mapping")
        with urllib.request.urlopen(req, timeout=60) as resp:
            present = set()
            for _idx, m in json.load(resp).items():
                present |= set(m.get("mappings", {}).get("properties", {}).keys())
    except Exception:
        present = None  # mapping unreadable: send the list unchanged rather
        # than silently narrowing it on a transport hiccup.
    if present is None:
        out = list(FIELDS)
    else:
        out = [f for f in FIELDS if f.split("^", 1)[0] in present]
        out = out or ["body"]  # never send an empty field list
    _FIELDS_CACHE[prefix] = out
    return out


def post(path, body, fatal=True):
    """POST a search body; fatal=False returns HTTP/transport errors as data.

    The non-fatal path exists for the vector arm of hybrid retrieval: a corpus
    where no index supports `semantic` must degrade to BM25, not abort.
    """
    req = urllib.request.Request(
        f"{URL}/{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.load(resp)
    except urllib.error.HTTPError as e:
        if not fatal:
            # The status already means fallback. Reading an optional error
            # body can itself time out or fail on an interrupted response.
            e.close()
            return {"_xc_error": f"{e.code}: {e.reason}"}
        try:
            msg = e.read()[:200].decode(errors="replace")
        except (OSError, http.client.HTTPException) as read_error:
            msg = f"{e.reason}; error response could not be read: {read_error}"
        finally:
            e.close()
        die(f"search failed ({e.code}): {msg}")
    except (OSError, http.client.HTTPException) as e:
        # URLError is an OSError; response reads can also raise socket or
        # HTTP errors directly, without urllib wrapping them.
        msg = f"cannot reach XERJ at {URL}: {getattr(e, 'reason', str(e))}"
        if not fatal:
            return {"_xc_error": msg}
        die(msg)


def bm25_query(query, lang, fields=None):
    must = [{"multi_match": {"query": query, "fields": fields or FIELDS}}]
    if lang:
        must.append({"match": {"language": lang}})
    return {"bool": {"must": must}}


def search(prefix, query, k, lang, highlight=False):
    """BM25 search.

    `highlight` defaults OFF because on this engine a highlight block CHANGES
    THE RANKING (issue #177). Measured on a 6-query labelled set over a
    valkey+memcached corpus: identical query, top-1 6/6 without highlight and
    1/6 with it, every score shifted (12.68 -> 20.43) and the result sets
    disjoint. Since `--full` now returns the matching definition from
    `symbols[]`, the highlighter is not needed for the passage anyway — so the
    cost of requesting it is pure ranking damage.
    """
    body = {"size": k, "query": bm25_query(query, lang, resolve_fields(prefix))}
    if highlight:
        body["highlight"] = {
            "fields": {"*": {"fragment_size": 320, "number_of_fragments": 2}},
        }
    return post(f"{prefix}*/_search", body)


def semantic_indices(prefix, field="body", fatal=False):
    """Indices under `prefix*` whose `field` is mapped as semantic_text.

    A `semantic` query against an index where the field is plain `text` does not
    return fewer hits — it fails the WHOLE search with a 400, taking every other
    index in the wildcard down with it. Measured on this corpus: 3 of 9
    `xc-rust-text-*` indices carry semantic_text; querying the wildcard errors
    outright. So the capable set is discovered from the mapping, and the vector
    arm is aimed only at those indices.

    Returns (capable, total). An unreachable or unparseable mapping yields an
    empty capable set, which degrades to BM25 rather than failing. Standalone
    semantic retrieval uses fatal=True because it has no BM25 result to keep.
    """
    req = urllib.request.Request(f"{URL}/{prefix}*/_mapping")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            mapping = json.load(resp)
    except (OSError, http.client.HTTPException, ValueError) as e:
        if isinstance(e, urllib.error.HTTPError):
            e.close()
        if fatal:
            die(f"semantic mapping lookup failed at {URL}: {e}")
        return [], 0
    if not isinstance(mapping, dict):
        return [], 0
    capable = [
        name for name, m in sorted(mapping.items())
        if (m.get("mappings", {}).get("properties", {})
             .get(field, {}).get("type") == "semantic_text")
    ]
    return capable, len(mapping)


def semantic_search(indices, query, k, lang, field="body", fatal=False):
    """Vector search; errors are optional only when BM25 fallback is allowed."""
    if not indices:
        return []
    q = {"semantic": {"field": field, "query": query}}
    if lang:
        q = {"bool": {"must": [q, {"match": {"language": lang}}]}}
    res = post(f"{','.join(indices)}/_search", {"size": k, "query": q}, fatal=fatal)
    if "_xc_error" in res:
        return []
    return res.get("hits", {}).get("hits", [])


def rrf(lists, size, k=RRF_K):
    """Reciprocal Rank Fusion: score(d) = sum over lists of 1/(k + rank(d)).

    Ranks are 1-based. A document missing from a list contributes nothing —
    that is the whole point of RRF: it needs only the orderings, so it never has
    to reconcile a BM25 score of 13.0 with a cosine-ish score of 0.81, which are
    not on the same scale and cannot be added.

    Ties are broken by BM25 rank — it is the arm the FIELDS sweep was measured
    on and the one hybrid falls back to. Without a stated rule the order would
    depend on dict iteration and the measurement would not reproduce. Exact ties
    are not hypothetical: on "two-way substring search algorithm" the top two
    documents both score 0.032522 (ranks 1,2 and 2,1), and this rule decides
    which one the agent reads first. It was fixed before the scoring run, not
    after — a tie-break picked once its effect on the score is known is not a
    measurement.
    """
    fused = {}
    for li, hits in enumerate(lists):
        for rank, h in enumerate(hits, start=1):
            key = (h.get("_index"), h.get("_id"))
            slot = fused.setdefault(key, {"hit": h, "rrf": 0.0, "ranks": {}})
            slot["rrf"] += 1.0 / (k + rank)
            slot["ranks"][li] = rank
            # Keep the BM25 copy of the document: it carries the highlights.
            if li == 0:
                slot["hit"] = h
    out = []
    for slot in sorted(fused.values(),
                       key=lambda s: (-s["rrf"], s["ranks"].get(0, 10**6))):
        h = dict(slot["hit"])
        h["_score"] = slot["rrf"]
        h["_rrf_ranks"] = slot["ranks"]
        out.append(h)
    return out[:size]


def hybrid_search(prefix, query, k, lang, depth=None):
    """BM25 + vector, fused by RRF.

    `depth` is how many candidates each arm contributes before fusion. Fusing
    only the k the caller asked for would mean a document ranked k+1 by BM25 and
    1 by the vector arm could never surface, which is precisely the case fusion
    exists to catch.

    Returns (hits, note). `note` is never None: the caller must be able to say
    which arms actually ran, because a silently BM25-only "hybrid" result would
    be indistinguishable from a working one.
    """
    depth = depth or max(4 * k, 20)
    bm = search(prefix, query, depth, lang).get("hits", {}).get("hits", [])
    if not bm:
        # BM25 finding nothing is the corpus's only honest "no". The vector arm
        # cannot supply one: the built-in embedder is LEXICAL, and it returns
        # its nearest neighbours for any input whatsoever. Measured: the query
        # "xyzzy plugh frobnicate quuxbaz" returns 0 BM25 hits and 3 confident
        # vector hits. Passing those through would convert an explicit miss into
        # three irrelevant files at exit 0 — the exact silent failure the
        # no-match branch below exists to prevent.
        return [], ("no lexical match in this corpus — vector nearest-neighbours "
                    "are not evidence of a match, so this is reported as a miss")
    capable, total = semantic_indices(prefix)
    if not capable:
        return bm[:k], (f"BM25 only — no usable semantic_text mapping for `body` "
                        f"could be discovered under '{prefix}*'")
    sem = semantic_search(capable, query, depth, lang)
    if not sem:
        return bm[:k], (f"BM25 only — vector search failed or returned no hits from the "
                        f"{len(capable)} semantic_text index(es)")
    note = (f"hybrid RRF(k={RRF_K}) — BM25 over {total} index(es), vector over "
            f"{len(capable)} of {total}")
    return rrf([bm, sem], k), note


def symbol_spans(src):
    """[(name, kind, start_line, end_line)] from the record's own `symbols`.

    A definition runs until the next definition starts, which is a coarse but
    honest bound: it never claims a range the index did not report, and it is
    exactly right for the common case of top-level functions laid out in order.
    """
    syms = src.get("symbols") or []
    if not isinstance(syms, list):
        return []
    got = sorted(
        ((s.get("name"), s.get("kind"), int(s["line"]))
         for s in syms
         if isinstance(s, dict) and s.get("name") and isinstance(s.get("line"), int)),
        key=lambda t: t[2],
    )
    out = []
    for i, (name, kind, line) in enumerate(got):
        end = got[i + 1][2] - 1 if i + 1 < len(got) else None
        out.append((name, kind, line, end))
    return out


def symbol_passage(body, src, query, width):
    """Return the DEFINITION that best matches the query, not a byte window.

    This is the difference between "search found the file" and "the agent got
    the code". Measured on a real valkey corpus: a query about null replies
    ranks networking.c correctly, but `addReplyNull` is at line 1460 — roughly
    40 KB in — so any head- or window-based slice of a 200 KB file is a coin
    flip on whether the answer is inside it. The record already carries
    {name, kind, line} for all 278 definitions in that file; using them turns a
    guess into a lookup.

    Returns (text, label) or None when the record has no usable symbols.
    """
    spans = symbol_spans(src)
    if not spans:
        return None
    lines = body.splitlines()
    if not lines:
        return None
    terms = [t.lower() for t in re.findall(r"[A-Za-z_][A-Za-z0-9_]{2,}", query)]
    if not terms:
        return None

    best = None
    for name, kind, start, end in spans:
        lo = max(0, start - 1)
        hi = min(len(lines), end if end else len(lines))
        if hi <= lo:
            continue
        text = "\n".join(lines[lo:hi])
        low = text.lower()
        # Name match is the strongest signal an agent can act on: a query
        # mentioning `addReplyNull` should return addReplyNull, not whichever
        # unrelated span happens to repeat a common word most often.
        nm = (name or "").lower()
        score = sum(low.count(t) for t in terms)
        score += 25 * sum(1 for t in terms if t in nm)
        if score <= 0:
            continue
        if best is None or score > best[0]:
            best = (score, name, kind, start, text)
    if best is None:
        return None
    _, name, kind, start, text = best
    if len(text) > width:
        text = text[:width] + f"\n    ... [{kind} {name} truncated at {width} chars]"
    return text, f"{kind} {name} @ line {start}"


def best_window(body, query, width):
    """The densest sampled `width`-char slice, snapped to lines when safe.

    Taking the HEAD of the file instead is the single worst bug this tool had.
    Measured on a real valkey+memcached corpus: retrieval ranked the correct
    files (`memcached/proto_text.c` #1 for a memcached protocol query), then
    handed back 21,461 chars containing ZERO occurrences of `$-1`, `addReply`,
    `STORED` or `CRLF` — the entire reason to retrieve those files. `addReplyNull`
    lives at networking.c:1261, thousands of chars past a head-truncation, so the
    model received licence banners and #include lines and nothing else.

    A file-level record means retrieval can only tell you WHICH file. Choosing
    the passage is this function's job, and skipping it makes the whole reference
    block worthless while still looking substantial.

    Returns (window, start_offset, total_len).
    """
    total = len(body)
    if total <= width:
        return body, 0, total
    terms = [t.lower() for t in re.findall(r"[A-Za-z_][A-Za-z0-9_]{2,}", query)]
    if not terms:
        return body[:width], 0, total
    # Score every candidate start on a coarse stride: dense term hits win. The
    # stride keeps this linear-ish on multi-hundred-KB sources.
    stride = max(1, width // 8)
    best_start, best_score = 0, -1
    for start in range(0, total - width + stride, stride):
        # Slice before lowercasing: Unicode lowercase can expand a character,
        # so offsets in a lowercased copy need not be offsets in the source.
        chunk = body[start:start + width].lower()
        score = sum(chunk.count(t) for t in terms)
        if score > best_score:
            best_start, best_score = start, score
    if best_score <= 0:
        return body[:width], 0, total
    # Prefer complete lines, but never discard query evidence just to align
    # them. The final partial line may hold the match, or the previous newline
    # may be far away in a source line longer than the requested width.
    nl = body.rfind("\n", 0, best_start)
    start = nl + 1 if nl != -1 else best_start
    end = body.rfind("\n", start, start + width)
    end = end if end > start else min(total, start + width)
    snapped = body[start:end].lower()
    if sum(snapped.count(t) for t in terms) < best_score:
        return body[best_start:best_start + width], best_start, total
    return body[start:end], start, total


def provenance(src):
    """file:line, or the best locator the record actually carries.

    Never invent a line number — a fabricated citation is worse than none,
    because it survives review by looking checkable.
    """
    path = (src.get("ax_path") or src.get("ax_file")
            or src.get("path") or src.get("file") or "?")
    line = src.get("line") or src.get("start_line") or src.get("lineno")
    return f"{path}:{line}" if line else path


def list_corpora():
    """Show every state/ entry and whether it is actually loaded HERE.

    The whole point: freshness in state/ does not imply the corpus is queryable
    on this server. This prints the live index count per corpus so an agent can
    see the real, loadable corpus set without hitting _cat/indices by hand — and
    without mistaking an over-advertised ledger for the truth.
    """
    state_dir = os.path.join(ROOT, "state")
    if not os.path.isdir(state_dir):
        die(f"no state directory at {state_dir} — nothing indexed yet")
    entries = sorted(f[:-5] for f in os.listdir(state_dir) if f.endswith(".json"))
    if not entries:
        die(f"no corpora in {state_dir} — run xc-index.sh <corpus>")
    print(f"corpora in state/ (queried against {URL}):")
    loaded = 0
    for name in entries:
        try:
            with open(os.path.join(state_dir, f"{name}.json")) as fh:
                st = json.load(fh)
        except (OSError, ValueError):
            print(f"  {name:<18} !! unreadable state file")
            continue
        prefix = st.get("prefix", f"xc-{name}")
        stamp = (st.get("indexed_at") or "?")[:10]
        count = live_index_count(prefix)
        if count is None:
            status = "server unreachable/ambiguous"
        elif count == 0:
            status = "NOT loaded here (0 indices) — stale/other-server"
        else:
            loaded += 1
            status = f"loaded — {count} index(es)"
        print(f"  {name:<18} {stamp}  {prefix:<18} {status}")
    print(f"\n{loaded} of {len(entries)} corpora are actually loaded on {URL}.")


def main():
    ap = argparse.ArgumentParser(description="retrieve reference passages")
    ap.add_argument("corpus", nargs="?")
    ap.add_argument("query", nargs="?")
    ap.add_argument("-k", type=int, default=5, help="max passages (default 5)")
    ap.add_argument("--lang", help="restrict to a file extension, e.g. rs")
    ap.add_argument("--stale-ok", action="store_true", help="answer from an old index anyway")
    ap.add_argument("--meatl", action="store_true", help="emit MEATL instead of prose")
    ap.add_argument("--json", action="store_true", help="emit raw JSON")
    # Reference coding needs the actual implementation, not a keyword-in-context
    # snippet. A 320-char highlight tells the agent a file is relevant; it does
    # not tell it how the algorithm works.
    # Defaulted, not opt-in. This used to be None, which meant the documented
    # invocation `xc.py <corpus> "<query>"` skipped the symbol path entirely and
    # fell through to a branch that prints body[:400] — the licence header, on
    # every Apache/BSD/MIT file in every corpus (issue #368). Retrieving the
    # definition IS the feature; capping its length is the knob.
    ap.add_argument("--full", type=int, metavar="CHARS", default=800,
                    help="cap each passage at CHARS of real source (default 800; "
                         "0 prints only the file head)")
    ap.add_argument("--no-symbol", action="store_true",
                    help="use a raw byte window instead of the matching definition")
    # Default is BM25, decided on TWO corpora after an earlier default (hybrid)
    # was chosen on one and turned out to be corpus-specific.
    #
    #                  rust-text        kv-oss (C)       combined
    #   mode        top1   top3      top1   top3      top1    top3
    #   bm25         3/6    6/6       6/6    6/6      9/12   12/12   <- this
    #   hybrid       5/6    6/6       2/6    4/6      7/12   10/12
    #   semantic     4/6    4/6       2/6    3/6      6/12    7/12
    #
    # The adoption rule, fixed before the numbers were in: top-3 is the operative
    # metric, because the agent reads k passages rather than one. BM25 is perfect
    # on it (12/12) and is never the worst arm on either corpus. Hybrid's only
    # win is top-1 on rust-text; on the C corpus it loses badly, because the
    # vector arm can reach just 5 of 407 indices there (issue #173), so RRF
    # fuses a good ranking with one that cannot see 98.8% of the corpus.
    #
    # BM25 is also ~5x faster: no vector round trip and no mapping lookup.
    # Use --mode hybrid when a corpus has broad semantic_text coverage AND you
    # care about top-1 specifically; measure before trusting it.
    ap.add_argument("--mode", choices=("bm25", "semantic", "hybrid"), default="bm25",
                    help="retrieval arm (default bm25 — best top-3 on both corpora)")
    ap.add_argument("--hybrid", action="store_const", const="hybrid", dest="mode",
                    help="shorthand for --mode hybrid (lexical + vector, RRF-fused)")
    ap.add_argument("--list", action="store_true",
                    help="list state/ corpora and whether each is loaded HERE, "
                         "then exit (no corpus/query needed)")
    args = ap.parse_args()

    if args.list:
        list_corpora()
        return
    if not args.corpus or not args.query:
        ap.error("the following arguments are required: corpus, query "
                 "(or pass --list)")

    state = load_state(args.corpus)
    age = check_fresh(state, args.stale_ok)
    # A corpus can be in state/ yet have zero live indices on THIS server (it
    # was indexed against another data dir / node). Catch that here with a
    # distinct, actionable error — never let it fall through to the generic
    # "no passage matches" path, which reads as a bad query and sends the agent
    # off to guess. This is the fix for the state-ledger trust trap.
    require_loaded(state)

    note = None
    if args.mode == "hybrid":
        hits, note = hybrid_search(state["prefix"], args.query, args.k, args.lang)
        res = {"hits": {"hits": hits}}
    elif args.mode == "semantic":
        capable, total = semantic_indices(state["prefix"], fatal=True)
        hits = semantic_search(capable, args.query, args.k, args.lang, fatal=True)
        res = {"hits": {"hits": hits}}
        note = (f"vector only over {len(capable)} of {total} index(es)" if capable
                else f"no index under '{state['prefix']}*' maps `body` as semantic_text")
    else:
        res = search(state["prefix"], args.query, args.k, args.lang)
        hits = res.get("hits", {}).get("hits", [])

    # Say which arms ran, every time. A hybrid call that silently fell back to
    # BM25 must not read like a hybrid result.
    if note and not args.json:
        print(f'@mode {note}' if args.meatl else f"[{note}]")

    if args.json:
        # Empty results must remain machine-readable without turning a miss
        # into a successful retrieval (the exit-code contract is unchanged).
        print(json.dumps(res, indent=1))
        if not hits:
            sys.exit(1)
        return

    if not hits:
        # Say so explicitly. A silent miss makes the next agent re-run the same
        # dead query; this is the line that stops the loop.
        print(f'@no q="{args.query}" why=no-match-in-corpus' if args.meatl
              else f"No passage in '{args.corpus}' matches: {args.query}\n"
                   f"The corpus is likely wrong for this task — fall back to "
                   f"normal work rather than forcing a bad match.")
        sys.exit(1)

    licences = {}
    man = os.path.join(ROOT, "corpora", args.corpus, "corpus.json")
    if os.path.exists(man):
        with open(man) as fh:
            licences = {r["repo"]: r["licence"] for r in json.load(fh)["repos"]}

    for h in hits:
        src = h.get("_source", {})
        loc = provenance(src)
        score = h.get("_score") or 0.0
        repo = loc.split("/")[0] if "/" in loc else ""
        lic = licences.get(repo, "")

        # An RRF score lives around 0.03 and two decimals would print every hit
        # as "0.03". Show the arms that produced it instead — that is the part
        # a reader can act on.
        ranks = h.get("_rrf_ranks")
        if ranks:
            arms = ", ".join(f"{('bm25', 'vec')[li]} #{r}"
                             for li, r in sorted(ranks.items()))
            shown = f"rrf {score:.4f} [{arms}]"
        else:
            shown = f"score {score:.2f}"

        if args.meatl:
            print(f'@ok f={loc} {shown.replace(" ", "=", 1)}'
                  + (f' why={lic}' if lic else ""))
            continue

        print(f"\n─── {loc}  ({shown}{', ' + lic if lic else ''})")
        if args.full:
            body = str(src.get("body") or "")
            # Prefer the matching DEFINITION over a byte window. Fall back to the
            # window only when the record carries no usable symbols (a data file,
            # or a language whose extractor found nothing — see issue #170).
            sp = None if args.no_symbol else symbol_passage(body, src, args.query, args.full)
            if sp:
                text, label = sp
                print(f"    [{label} — {len(text):,} of {len(body):,} chars]")
                print(text)
            else:
                window, start, total = best_window(body, args.query, args.full)
                if total > len(window):
                    print(f"    [no symbol match; showing {len(window):,} of {total:,} "
                          f"chars from offset {start:,}, window centred on the match]")
                print(window)
        else:
            # Only reachable via `--full 0`. `highlight` is never requested
            # (issue #177 — it reorders hits), so this is the file head and is
            # labelled as such rather than passed off as a matching passage.
            body = str(src.get("body") or "")
            head = body[:400]
            if len(body) > len(head):
                print(f"    [file head; {len(head):,} of {len(body):,} chars — "
                      f"pass --full N for the matching definition]")
            print("    " + head.replace("\n", "\n    "))
        if lic and any(r in lic for r in RESTRICTED):
            print(f"    !! {lic}: adapt the APPROACH, do not copy the code")

    if not args.meatl:
        print(f"\n{len(hits)} passages from '{args.corpus}'"
              + (f" (index {age}d old)" if age else ""))
        print("Cite file:line for anything you rely on.")

    # Stderr-only, once a day: the loop back to the maintainers. Kept out of the
    # retrieval output on stdout on purpose (see field_report_nudge).
    field_report_nudge()


if __name__ == "__main__":
    main()
