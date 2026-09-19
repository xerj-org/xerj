#!/usr/bin/env python3
"""Regression tests for the state-ledger guard in xc.py.

The bug (dogfood 2026-08-18): a corpus can be listed in `state/` while having
ZERO live indices on the server xc.py is querying — it was indexed against a
different data dir / node. Before this guard, querying such a corpus returned
the SAME "No passage matches — corpus is likely wrong, fall back" message as a
genuinely bad query, so an agent concluded "no reference exists" and started
guessing: the exact failure reference-coding exists to prevent.

These tests pin the three outcomes, distinguishing them by exit code AND by
message, with the HTTP layer (urllib) mocked so no live server is needed:

  1. in state/ but 0 live indices  -> exit 3, DISTINCT "not loaded here" error
  2. loaded, query matches nothing  -> exit 1, ordinary "No passage ... matches"
  3. loaded, query matches          -> exit 0, normal answer with provenance

Offline by design: XERJ_CODE_HOME points at a throwaway dir and urlopen is
replaced, so nothing touches the network or the real ~/.xerj-code.
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
import urllib.error
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
XC_PATH = os.path.join(HERE, "..", "scripts", "xc.py")

PREFIX = "xc-fixture"
CORPUS = "fixture"


class FakeResp:
    """Minimal context-manager response json.load() can read."""
    def __init__(self, payload):
        self._data = json.dumps(payload).encode()

    def read(self, *a):
        return self._data

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def make_urlopen(indices, search_hits):
    """A urlopen stand-in dispatching on the request URL.

    `indices` is the list _cat/indices returns (its length is the live count).
    `search_hits` is the hits array _search returns.
    """
    def fake_urlopen(req, timeout=None):
        url = req.full_url if hasattr(req, "full_url") else str(req)
        if "/_cat/indices/" in url:
            return FakeResp(indices)
        if "/_mapping" in url:
            # One index, mapping `body`+`defs`+`title` so resolve_fields keeps
            # the multi-field query and semantic_indices finds no semantic_text.
            return FakeResp({
                f"{PREFIX}-1": {"mappings": {"properties": {
                    "body": {"type": "text"},
                    "defs": {"type": "text"},
                    "title": {"type": "text"},
                }}}
            })
        if "/_search" in url:
            return FakeResp({"hits": {"total": {"value": len(search_hits)},
                                      "hits": search_hits}})
        raise urllib.error.URLError(f"unexpected url in test: {url}")
    return fake_urlopen


def load_xc():
    """Import xc.py fresh so module-level ROOT/URL pick up the test env."""
    for m in [k for k in sys.modules if k == "xc_under_test"]:
        del sys.modules[m]
    spec = importlib.util.spec_from_file_location("xc_under_test", XC_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def write_state(home):
    os.makedirs(os.path.join(home, "state"), exist_ok=True)
    os.makedirs(os.path.join(home, "corpora", CORPUS), exist_ok=True)
    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    with open(os.path.join(home, "state", f"{CORPUS}.json"), "w") as fh:
        json.dump({"corpus": CORPUS, "indexed_at": now, "prefix": PREFIX,
                   "url": "http://localhost:9999"}, fh)
    with open(os.path.join(home, "corpora", CORPUS, "corpus.json"), "w") as fh:
        json.dump({"repos": [{"repo": "acme", "licence": "MIT"}]}, fh)


def run_query(mod, indices, search_hits, argv):
    """Run xc.main() with urlopen mocked; return (exit_code, stdout, stderr)."""
    mod.urllib.request.urlopen = make_urlopen(indices, search_hits)
    out, err = io.StringIO(), io.StringIO()
    code = 0
    saved = sys.argv
    sys.argv = ["xc.py"] + argv
    try:
        with redirect_stdout(out), redirect_stderr(err):
            mod.main()
    except SystemExit as e:
        code = e.code if isinstance(e.code, int) else 1
    finally:
        sys.argv = saved
    return code, out.getvalue(), err.getvalue()


def main():
    passed = failed = 0

    def check(name, cond, detail=""):
        nonlocal passed, failed
        if cond:
            passed += 1
            print(f"  ok   — {name}")
        else:
            failed += 1
            print(f"  FAIL — {name}  {detail}")

    home = tempfile.mkdtemp(prefix="xc-ledger-test-")
    os.environ["XERJ_CODE_HOME"] = home
    os.environ["XERJ_URL"] = "http://localhost:9200"
    write_state(home)
    mod = load_xc()

    HIT = [{"_index": f"{PREFIX}-1", "_id": "d1", "_score": 12.3,
            "_source": {"body": "fn parse_two_way() { /* the algorithm */ }",
                        "path": "acme/src/lib.rs", "line": 42,
                        "symbols": [{"name": "parse_two_way", "kind": "fn",
                                     "line": 1}]}}]

    # Case 1: in state/ but ZERO live indices -> distinct "not loaded here".
    code, out, err = run_query(mod, indices=[], search_hits=[],
                               argv=[CORPUS, "two way string search"])
    blob = out + err
    check("case1 exit code is 3 (distinct, not 1)", code == 3, f"got {code}")
    check("case1 message says 0 live indices", "0 live" in blob, repr(blob))
    check("case1 message is explicitly NOT a no-match",
          "NOT a 'no match'" in blob, repr(blob))
    check("case1 does NOT emit the ordinary no-match line",
          "No passage in" not in blob, repr(blob))
    check("case1 is actionable (re-run xc-index.sh)",
          "xc-index.sh" in blob, repr(blob))

    # Case 2: loaded (1 index) but the query matches nothing -> ordinary miss.
    code, out, err = run_query(mod, indices=[{"index": f"{PREFIX}-1"}],
                               search_hits=[],
                               argv=[CORPUS, "nonexistent xyzzy plugh"])
    blob = out + err
    check("case2 exit code is 1 (ordinary empty-result)", code == 1, f"got {code}")
    check("case2 uses the no-match wording", "No passage in" in blob, repr(blob))
    check("case2 does NOT claim the corpus is unloaded",
          "0 live" not in blob and "not loaded" not in blob, repr(blob))

    # Case 3: loaded and the query matches -> normal answer with provenance.
    code, out, err = run_query(mod, indices=[{"index": f"{PREFIX}-1"}],
                               search_hits=HIT,
                               argv=[CORPUS, "two way parse_two_way"])
    blob = out + err
    check("case3 exit code is 0 (success)", code == 0, f"got {code}")
    check("case3 shows provenance file:line", "acme/src/lib.rs:42" in blob,
          repr(blob))
    check("case3 shows the retrieved definition",
          "parse_two_way" in blob, repr(blob))

    # Case 4: --list distinguishes loaded from not-loaded per entry.
    code, out, err = run_query(mod, indices=[], search_hits=[], argv=["--list"])
    blob = out + err
    check("list exit code is 0", code == 0, f"got {code}")
    check("list flags the fixture as NOT loaded",
          "NOT loaded here" in blob, repr(blob))

    # --json must be parseable for a genuine miss as well as a match. Keep the
    # existing exit codes and the raw response (including BM25 metadata).
    for mode, extra in (("bm25", []), ("hybrid", []), ("semantic", []),
                        ("bm25", ["--meatl"])):
        label = mode + (" +meatl" if extra else "")
        code, out, err = run_query(
            mod, indices=[{"index": f"{PREFIX}-1"}], search_hits=[],
            argv=[CORPUS, "nonexistent xyzzy plugh", "--json", "--mode", mode]
                 + extra)
        expected = {"hits": {"hits": []}}
        if mode == "bm25":
            expected["hits"]["total"] = {"value": 0}
        try:
            payload = json.loads(out)
        except ValueError:
            payload = None
        check(f"json {label} miss keeps exit code 1", code == 1, f"got {code}")
        check(f"json {label} miss emits the empty response",
              payload == expected, repr(out))
        check(f"json {label} miss has no stderr", err == "", repr(err))

    code, out, err = run_query(
        mod, indices=[{"index": f"{PREFIX}-1"}], search_hits=HIT,
        argv=[CORPUS, "two way parse_two_way", "--json"])
    check("json match keeps exit code 0", code == 0, f"got {code}")
    check("json match preserves the raw response",
          json.loads(out) == {"hits": {"total": {"value": 1}, "hits": HIT}},
          repr(out))

    code, out, err = run_query(mod, indices=[], search_hits=[],
                               argv=[CORPUS, "two way string search", "--json"])
    check("json unloaded corpus keeps exit code 3", code == 3, f"got {code}")
    check("json unloaded corpus has no search response", out == "", repr(out))
    check("json unloaded corpus keeps its actionable stderr",
          "0 live indices" in err and "xc-index.sh" in err, repr(err))

    code, out, err = run_query(
        mod, indices=[{"index": f"{PREFIX}-1"}], search_hits=[],
        argv=[CORPUS, "nonexistent xyzzy plugh", "--meatl"])
    check("meatl miss keeps exit code 1", code == 1, f"got {code}")
    check("meatl miss keeps its no-match record",
          out == '@no q="nonexistent xyzzy plugh" why=no-match-in-corpus\n',
          repr(out))

    print(f"\n{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
