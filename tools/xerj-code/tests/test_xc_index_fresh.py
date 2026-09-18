#!/usr/bin/env python3
"""Regression tests for `xc-index.sh <corpus> --fresh` (issue #930).

The bug: `--fresh` was forwarded to `xerj autoindex --fresh`, which is a
different thing (it discards autoindex's resume journal and is REFUSED once a
durable corpus generation exists), and a state directory written before the
`generation-v1` format cannot be adopted at all. So the documented rebuild —
the one xc.py itself prints when an index is older than 30 days — failed for
every corpus that had ever been indexed.

The fix is a build-verify-swap in the wrapper. These tests pin its contract:

  * --fresh really is fresh: a NEW --state-dir and a NEW index prefix, and the
    `--fresh` flag is never forwarded to autoindex
  * the state file's `prefix` stays `xc-<corpus>` (every `xc-<corpus>*` reader
    keeps working); `index_prefix` names the one verified build
  * the old index is retired ONLY after the replacement verified (count > 0),
    by exact name, never by wildcard
  * a failed or empty build leaves the old index AND the old state file exactly
    as they were, and removes only what it created
  * a sibling corpus (`battle` vs `battle-terse`) is never touched
  * two --fresh runs inside one second cannot retire the build just verified
  * an interrupted FIRST build (no index to fall back to) is kept, recorded as
    salvaged with its real exit code, and resumed by a plain re-run
  * a record count the node does not ANSWER is never read as zero: it cannot
    get a working index retired, nor a finished build deleted

Offline: a fake node (http.server) and a fake `xerj` binary that behaves like
the real one where it matters — it refuses `--fresh` and refuses a legacy state
directory, with exit 1 and no records, exactly as in the #930 repro.
"""
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import threading
from fnmatch import fnmatchcase
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "..", "scripts", "xc-index.sh")


class Node:
    """The slice of the ES-compatible surface xc-index.sh talks to."""

    def __init__(self):
        self.indices = {}          # name -> record count
        self.log = []              # (method, path) in arrival order
        self.catalog_deletes = []  # delete-by-query bodies
        self.count_outage = 0      # the next N `_count` requests answer 503
        self.count_mute = set()    # `_count` patterns that NEVER answer (503)
        self.lock = threading.Lock()

    def matching(self, pattern):
        return sorted(n for n in self.indices if fnmatchcase(n, pattern))


def make_handler(node):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def reply(self, status, payload):
            body = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def body(self):
            length = int(self.headers.get("Content-Length") or 0)
            return self.rfile.read(length) if length else b""

        def do_GET(self):
            path = self.path.split("?")[0]
            with node.lock:
                node.log.append(("GET", path))
                if path == "/_cluster/health":
                    return self.reply(200, {"status": "green"})
                if path.startswith("/_cat/indices/"):
                    pattern = path[len("/_cat/indices/"):]
                    return self.reply(200, [{"index": n} for n in node.matching(pattern)])
                if path.endswith("/_count"):
                    # A busy node: it is up, it lists indices, it cannot count.
                    if path[1:-len("/_count")] in node.count_mute:
                        return self.reply(503, {"error": "busy"})
                    if node.count_outage > 0:
                        node.count_outage -= 1
                        return self.reply(503, {"error": "busy"})
                    pattern = path[1:-len("/_count")]
                    return self.reply(200, {"count": sum(node.indices[n] for n in node.matching(pattern))})
            self.reply(404, {"error": "not found"})

        def do_DELETE(self):
            name = self.path.split("?")[0].lstrip("/")
            with node.lock:
                node.log.append(("DELETE", "/" + name))
                if "*" in name or "," in name or name.startswith("_"):
                    return self.reply(400, {"error": "wildcard delete refused"})
                if node.indices.pop(name, None) is None:
                    return self.reply(404, {"error": "no such index"})
            self.reply(200, {"acknowledged": True})

        def do_POST(self):
            path = self.path.split("?")[0]
            raw = self.body()
            with node.lock:
                node.log.append(("POST", path))
                if path == "/__test/create":
                    spec = json.loads(raw)
                    node.indices[spec["index"]] = spec["docs"]
                    return self.reply(200, {})
                if path == "/__test/outage":
                    node.count_outage = json.loads(raw)["requests"]
                    return self.reply(200, {})
                if path == "/autoindex-catalog/_delete_by_query":
                    node.catalog_deletes.append(json.loads(raw))
                    return self.reply(200, {"deleted": 1})
            self.reply(404, {"error": "not found"})

    return Handler


FAKE_XERJ = r'''#!/usr/bin/env bash
# Stand-in for `xerj autoindex`. Records its argv, then behaves as configured.
set -u
fake="$FAKE_DIR"
printf '%s\n' "$*" >> "$fake/argv.log"
url="" prefix="" state="$fake/default-state" fresh=0
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[$i]}" in
    --url) url="${args[$((i + 1))]}" ;;
    --prefix) prefix="${args[$((i + 1))]}" ;;
    --state-dir) state="${args[$((i + 1))]}" ;;
    --fresh) fresh=1 ;;
  esac
done
# The two refusals of issue #930, as the real binary makes them: exit 1, no
# remote mutation.
if [ "$fresh" = 1 ]; then
  echo "error: this attempt made no remote mutations. \`--fresh\` cannot discard committed corpus generation 1 under the same destination" >&2
  exit 1
fi
if [ -e "$state/LEGACY" ]; then
  echo "error: this state directory contains a legacy nonempty plan that cannot become generation authority" >&2
  exit 1
fi
rc="$(cat "$fake/rc" 2>/dev/null || echo 0)"
docs="$(cat "$fake/docs" 2>/dev/null || echo 7)"
if [ "$docs" -gt 0 ]; then
  for dataset in docs code; do
    curl -fsS -X POST "$url/__test/create" -H 'Content-Type: application/json' \
      -d "{\"index\":\"$prefix-$dataset\",\"docs\":$docs}" >/dev/null
  done
fi
mkdir -p "$state" && : > "$state/journal.ndjson"
if [ -f "$fake/outage" ]; then
  curl -fsS -X POST "$url/__test/outage" -H 'Content-Type: application/json' \
    -d "{\"requests\":$(cat "$fake/outage")}" >/dev/null
fi
exit "$rc"
'''


class Rig:
    def __init__(self):
        self.home = tempfile.mkdtemp(prefix="xc-index-fresh-")
        self.fake = os.path.join(self.home, "fake")
        os.makedirs(self.fake)
        self.xerj = os.path.join(self.fake, "xerj")
        with open(self.xerj, "w") as handle:
            handle.write(FAKE_XERJ)
        os.chmod(self.xerj, os.stat(self.xerj).st_mode | stat.S_IXUSR)
        self.node = Node()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.node))
        self.url = "http://127.0.0.1:%d" % self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def corpus(self, name):
        os.makedirs(os.path.join(self.home, "corpora", name), exist_ok=True)

    def behave(self, rc=0, docs=7):
        for key, value in (("rc", rc), ("docs", docs)):
            with open(os.path.join(self.fake, key), "w") as handle:
                handle.write(str(value))

    def legacy_state(self, name, docs=5):
        """A corpus as the pre-#930 script left it: legacy ledger + legacy state dir."""
        self.corpus(name)
        os.makedirs(os.path.join(self.home, "state"), exist_ok=True)
        with open(self.state_path(name), "w") as handle:
            json.dump({"corpus": name, "indexed_at": "2026-08-07T08:43:16Z",
                       "prefix": "xc-" + name, "url": self.url, "autoindex_exit": 3}, handle)
        os.makedirs(os.path.join(self.fake, "default-state"), exist_ok=True)
        open(os.path.join(self.fake, "default-state", "LEGACY"), "w").close()
        self.node.indices["xc-%s-docs" % name] = docs
        self.node.indices["xc-%s-code" % name] = docs

    def state_path(self, name):
        return os.path.join(self.home, "state", name + ".json")

    def state(self, name):
        with open(self.state_path(name)) as handle:
            return json.load(handle)

    def argv(self):
        """One list of words per autoindex invocation (no test path has spaces)."""
        path = os.path.join(self.fake, "argv.log")
        if not os.path.exists(path):
            return []
        with open(path) as handle:
            return [line.split() for line in handle if line.strip()]

    def run(self, *args):
        env = dict(os.environ, XERJ_CODE_HOME=self.home, XERJ_URL=self.url,
                   XERJ_BIN=self.xerj, FAKE_DIR=self.fake, XC_COUNT_PAUSE="0")
        env.pop("XERJ_API_KEY", None)
        done = subprocess.run(["bash", SCRIPT, *args], env=env, capture_output=True,
                              text=True, timeout=120)
        return done.returncode, done.stdout, done.stderr

    def close(self):
        self.server.shutdown()


def flag(words, name):
    return words[words.index(name) + 1] if name in words else ""


def main():
    passed = failed = 0

    def check(name, cond, detail=""):
        nonlocal passed, failed
        if cond:
            passed += 1
            print("  ok   — %s" % name)
        else:
            failed += 1
            print("  FAIL — %s  %s" % (name, detail))

    # 1 ── the defect: --fresh over a corpus indexed before generation-v1 ─────
    print("--fresh over a legacy corpus (the #930 repro)")
    rig = Rig()
    rig.legacy_state("fix")
    rig.behave(rc=3, docs=7)
    code, out, err = rig.run("fix", "--fresh")
    check("exits 0", code == 0, "code=%s stderr=%s" % (code, err))
    argv = rig.argv()
    check("autoindex ran exactly once", len(argv) == 1, str(argv))
    words = argv[0] if argv else []
    check("`--fresh` is NOT forwarded to autoindex", "--fresh" not in words, str(words))
    check("a new --state-dir is passed", "/autoindex-state/fix/b" in flag(words, "--state-dir"), str(words))
    built = flag(words, "--prefix")
    check("the build prefix is xc-<corpus>-b<stamp>", re.fullmatch(r"xc-fix-b\d{14}", built) is not None, built)
    state = rig.state("fix")
    check("state `prefix` stays xc-<corpus>", state["prefix"] == "xc-fix", str(state))
    check("state `index_prefix` names the verified build", state.get("index_prefix") == built, str(state))
    check("state records the autoindex exit", state["autoindex_exit"] == 3, str(state))
    check("old indices are retired", "xc-fix-docs" not in rig.node.indices
          and "xc-fix-code" not in rig.node.indices, str(rig.node.indices))
    check("the replacement is live", rig.node.indices.get(built + "-docs") == 7, str(rig.node.indices))
    check("every xc-<corpus>* reader still resolves the corpus",
          len(rig.node.matching("xc-fix*")) == 2, str(rig.node.indices))
    deletes = [i for i, (method, _) in enumerate(rig.node.log) if method == "DELETE"]
    verified = [i for i, (method, path) in enumerate(rig.node.log)
                if method == "GET" and path == "/%s-*/_count" % built]
    check("nothing is deleted before the replacement's count was read",
          bool(deletes) and bool(verified) and min(deletes) > min(verified), str(rig.node.log))
    check("deletes are by exact name, never a wildcard",
          all("*" not in path for method, path in rig.node.log if method == "DELETE"), str(rig.node.log))
    check("the old build's catalog scope is cleaned by exact value",
          any(clause.get("term", {}).get("corpus_scope") == "xc-fix"
              for body in rig.node.catalog_deletes
              for clause in body["query"]["bool"]["should"]), str(rig.node.catalog_deletes))

    # 2 ── an update after a build reconciles THAT build in place ─────────────
    print("plain re-run after a build")
    code, out, err = rig.run("fix")
    words = rig.argv()[-1]
    check("exits 0", code == 0, err)
    check("reuses the build's prefix", flag(words, "--prefix") == built, str(words))
    check("reuses the build's state dir", flag(words, "--state-dir") == state["state_dir"], str(words))
    check("index_prefix is unchanged", rig.state("fix").get("index_prefix") == built)

    # 3 ── a second --fresh: build beside, verify, retire the previous BUILD ──
    print("--fresh over a previous build")
    rig.behave(rc=0, docs=11)
    code, out, err = rig.run("fix", "--fresh")
    second = rig.state("fix").get("index_prefix")
    check("exits 0", code == 0, err)
    check("a different build id, even within the same second", bool(second) and second != built,
          "%s vs %s" % (second, built))
    check("the previous build is retired", not rig.node.matching(built + "-*"), str(rig.node.indices))
    check("the new build survived its own retire step",
          rig.node.indices.get("%s-docs" % second) == 11, str(rig.node.indices))
    check("the previous build's state dir is removed", not os.path.exists(state["state_dir"]))
    check("the new build's state dir is kept", os.path.isdir(rig.state("fix")["state_dir"]))
    rig.close()

    # 4 ── a build that fails must not cost the working index ────────────────
    for label, rc, docs in (("autoindex aborts", 1, 0), ("autoindex exits 0 with zero records", 0, 0)):
        print("--fresh when %s" % label)
        rig = Rig()
        rig.legacy_state("fix", docs=5)
        with open(rig.state_path("fix"), "rb") as handle:
            ledger_before = handle.read()
        rig.behave(rc=rc, docs=docs)
        code, out, err = rig.run("fix", "--fresh")
        check("exits non-zero", code != 0, "code=%s" % code)
        check("the old index is untouched", rig.node.indices == {"xc-fix-docs": 5, "xc-fix-code": 5},
              str(rig.node.indices))
        with open(rig.state_path("fix"), "rb") as handle:
            check("the state file is byte-identical", handle.read() == ledger_before)
        check("says the existing index was not touched", "NOT touched" in err, err)
        check("the failed build's state dir is removed",
              not os.listdir(os.path.join(rig.home, "autoindex-state", "fix")))
        rig.close()

    # 5 ── a partial build is removed, the old index is not ──────────────────
    print("--fresh when autoindex aborts AFTER writing records, with a working index present")
    rig = Rig()
    rig.legacy_state("fix", docs=5)
    rig.behave(rc=1, docs=9)
    code, out, err = rig.run("fix", "--fresh")
    check("exits with autoindex's code", code == 1, "code=%s" % code)
    check("only the partial build is deleted; the working index stays",
          rig.node.indices == {"xc-fix-docs": 5, "xc-fix-code": 5}, str(rig.node.indices))
    check("state still points at the working index", "index_prefix" not in rig.state("fix"))
    rig.close()

    # 6 ── sibling corpora share a name prefix and must never be touched ─────
    print("--fresh next to a sibling corpus (battle / battle-terse)")
    rig = Rig()
    rig.legacy_state("battle", docs=5)
    rig.corpus("battle-terse")
    rig.node.indices["xc-battle-terse-docs"] = 42
    rig.behave(rc=0, docs=7)
    code, out, err = rig.run("battle", "--fresh")
    check("exits 0", code == 0, err)
    check("the sibling's index survives", rig.node.indices.get("xc-battle-terse-docs") == 42, str(rig.node.indices))
    check("the sibling is never named in a DELETE",
          not any("terse" in path for method, path in rig.node.log if method == "DELETE"), str(rig.node.log))
    check("the corpus' own old indices are retired", "xc-battle-docs" not in rig.node.indices, str(rig.node.indices))
    rig.close()

    # 7 ── first index of a corpus, and argument hygiene ─────────────────────
    print("first index, and unknown flags")
    rig = Rig()
    rig.corpus("new")
    rig.behave(rc=0, docs=3)
    code, out, err = rig.run("new")
    check("first index exits 0", code == 0, err)
    check("first index is recorded as a build", bool(rig.state("new").get("index_prefix")), str(rig.state("new")))
    check("nothing is deleted on a first index",
          not any(method == "DELETE" for method, _ in rig.node.log), str(rig.node.log))
    before = len(rig.argv())
    code, out, err = rig.run("new", "--frsh")
    check("a mistyped flag is rejected (exit 2), not silently ignored", code == 2, "code=%s" % code)
    check("…and autoindex is not run", len(rig.argv()) == before)
    rig.close()

    # 8 ── an interrupted FIRST build: nothing to fall back to, so it is kept ─
    # The complement of 5. autoindex exits 1 part-way through a large corpus
    # when the node keeps rejecting writes (#944); with no working index to
    # protect, throwing the partial build away would leave no corpus at all.
    # It is kept, recorded for what it is, and a plain re-run RESUMES it: same
    # prefix, same state directory, so autoindex picks its journal back up.
    print("an interrupted first build is kept, labelled, and resumed by a plain re-run")
    rig = Rig()
    rig.corpus("cut")
    rig.behave(rc=1, docs=7)
    code, out, err = rig.run("cut", "--fresh")
    check("the script does not fail the salvaged build", code == 0, "code=%s stderr=%s" % (code, err))
    check("it says so loudly", "WARNING" in err and "exited 1" in err, err)
    state = rig.state("cut")
    built = state.get("index_prefix", "")
    check("the ledger records the real exit code", state.get("autoindex_exit") == 1, str(state))
    check("the ledger records it as salvaged", state.get("salvaged") is True, str(state))
    check("the ledger still names the build", re.fullmatch(r"xc-cut-b\d{14}", built) is not None, str(state))
    check("the partial build's indices are kept", rig.node.indices.get(built + "-docs") == 7, str(rig.node.indices))
    check("nothing was deleted", not any(method == "DELETE" for method, _ in rig.node.log), str(rig.node.log))
    kept_state = state.get("state_dir", "")
    check("its state directory is kept for the resume",
          os.path.isfile(os.path.join(kept_state, "journal.ndjson")), kept_state)
    rig.behave(rc=0, docs=9)
    code, out, err = rig.run("cut")
    words = rig.argv()[-1]
    check("a plain re-run exits 0", code == 0, err)
    check("it re-runs autoindex under the SAME prefix", flag(words, "--prefix") == built, str(words))
    check("…and the SAME state directory, so the journal resumes", flag(words, "--state-dir") == kept_state, str(words))
    state = rig.state("cut")
    check("a finished resume clears the salvaged mark",
          state.get("salvaged") is False and state.get("autoindex_exit") == 0, str(state))
    check("the build id did not change", state.get("index_prefix") == built, str(state))
    rig.close()

    # 9 ── "the node did not say" is not "zero": the OLD index ───────────────
    # One 503 on one `_count` used to read a working index as "nothing to fall
    # back to": the FAILED build was kept over it and the working index retired,
    # exit 0. Only the OLD index's count is mute here — the new build's count
    # answers, which is exactly the combination that did the damage.
    print("--fresh when the node cannot count the WORKING index and the build fails")
    rig = Rig()
    rig.legacy_state("blind", docs=500)
    before_state = rig.state("blind")
    rig.behave(rc=1, docs=7)
    rig.node.count_mute.add("xc-blind-*")
    code, out, err = rig.run("blind", "--fresh")
    check("the failed build is NOT accepted (non-zero exit)", code != 0, "code=%s" % code)
    check("the working index survives", rig.node.indices.get("xc-blind-docs") == 500
          and rig.node.indices.get("xc-blind-code") == 500, str(rig.node.indices))
    check("the old index is never named in a DELETE",
          not any(path in ("/xc-blind-docs", "/xc-blind-code")
                  for method, path in rig.node.log if method == "DELETE"), str(rig.node.log))
    check("the state file still points at the working index", rig.state("blind") == before_state, str(rig.state("blind")))
    check("it says the count was unknown and what it presumed", "did not answer a record count" in err
          and "WORKING index" in err, err)
    rig.close()

    # 10 ── …and the NEW build: a finished build must not be deleted unverified
    # One 503 used to read a complete build as "empty": its indices AND its
    # resume state were deleted.
    print("--fresh when the build finishes but the node cannot count it (working index present)")
    rig = Rig()
    rig.legacy_state("mute", docs=500)
    before_state = rig.state("mute")
    rig.behave(rc=0, docs=7)
    # The old count must succeed (1 request) and every later one fail: let the
    # first through, then start the outage from inside the fake binary's run.
    with open(os.path.join(rig.fake, "outage"), "w") as handle:
        handle.write("6")
    code, out, err = rig.run("mute", "--fresh")
    built = [n for n in rig.node.indices if re.match(r"xc-mute-b\d{14}-", n)]
    check("exits non-zero: the build could not be verified", code != 0, "code=%s" % code)
    check("NOTHING is deleted", not any(method == "DELETE" for method, _ in rig.node.log), str(rig.node.log))
    check("the unverified build's indices are kept", len(built) == 2, str(rig.node.indices))
    check("the working index is kept", rig.node.indices.get("xc-mute-docs") == 500, str(rig.node.indices))
    check("readers are NOT switched to an unverified build", rig.state("mute") == before_state, str(rig.state("mute")))
    state_root = os.path.join(rig.home, "autoindex-state", "mute")
    check("the build's resume state is kept",
          os.path.isdir(state_root) and any(os.path.isfile(os.path.join(state_root, d, "journal.ndjson"))
                                            for d in os.listdir(state_root)), state_root)
    check("it says so", "NOTHING was deleted" in err and "NOT touched" in err, err)
    rig.close()

    print("a FIRST build the node cannot count is kept and recorded as unverified")
    rig = Rig()
    rig.corpus("first")
    rig.behave(rc=0, docs=7)
    rig.node.count_outage = 6
    code, out, err = rig.run("first", "--fresh")
    check("exits non-zero", code != 0, "code=%s" % code)
    check("nothing is deleted", not any(method == "DELETE" for method, _ in rig.node.log), str(rig.node.log))
    state = rig.state("first")
    check("the ledger records it as unverified (salvaged), with the real exit code",
          state.get("salvaged") is True and state.get("autoindex_exit") == 0, str(state))
    check("its indices are kept", rig.node.indices.get(state.get("index_prefix", "?") + "-docs") == 7, str(rig.node.indices))
    rig.node.count_outage = 0
    code, out, err = rig.run("first")
    state = rig.state("first")
    check("a plain re-run, once the node answers, confirms it and clears the mark",
          code == 0 and state.get("salvaged") is False, "code=%s state=%s" % (code, state))
    rig.close()

    # 11 ── patience: a node that answers on a later attempt is simply believed
    print("--fresh when the count fails a few times and then answers")
    rig = Rig()
    rig.legacy_state("slow", docs=500)
    rig.behave(rc=0, docs=7)
    rig.node.count_outage = 3
    code, out, err = rig.run("slow", "--fresh")
    check("exits 0", code == 0, "code=%s stderr=%s" % (code, err))
    check("the replacement was verified and readers switched",
          re.fullmatch(r"xc-slow-b\d{14}", rig.state("slow").get("index_prefix", "")) is not None, str(rig.state("slow")))
    check("the old index was retired only then", "xc-slow-docs" not in rig.node.indices, str(rig.node.indices))
    rig.close()

    # 12 ── legacy path: without a count from BEFORE the run, nothing is salvaged
    print("legacy update when the node could not count before the run")
    rig = Rig()
    rig.legacy_state("old", docs=500)
    os.remove(os.path.join(rig.fake, "default-state", "LEGACY"))
    before_state = rig.state("old")
    rig.behave(rc=1, docs=9)
    rig.node.count_outage = 6
    code, out, err = rig.run("old")
    check("a failed legacy run is not salvaged on an unknown baseline", code != 0, "code=%s" % code)
    check("the ledger is not re-dated", rig.state("old") == before_state, str(rig.state("old")))
    rig.close()

    print("\n%d passed, %d failed" % (passed, failed))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
