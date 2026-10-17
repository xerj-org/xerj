#!/usr/bin/env python3
"""Offline tests for pov.py's parsing, grading and contamination logic (no agent,
no network): `python3 -m unittest benchmarks/hub-pov/harness/test_pov.py`."""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
os.environ.setdefault("POV_RUN_DIR", tempfile.mkdtemp(prefix="pov-test-"))
import pov  # noqa: E402


def ev_tool(name, tid, **inp):
    return {"type": "assistant", "message": {"content": [{"type": "tool_use", "id": tid, "name": name, "input": inp}]}}


def ev_res(tid, text, err=False):
    return {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": tid, "content": text, "is_error": err}]}}


def result(cost, out, turns, text="ANSWER: 15"):
    return {"type": "result", "subtype": "success", "total_cost_usd": cost, "num_turns": turns,
            "usage": {"output_tokens": out, "input_tokens": 10}, "result": text}


INIT = {"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "tools": ["Bash", "Read", "Grep", "Glob"],
        "mcp_servers": [], "plugins": []}


class Metrics(unittest.TestCase):
    def test_sums_every_result_event(self):
        # PLAN §6 summing lesson: a background task emits several result events.
        tm = pov.transcript_metrics([INIT, result(0.01, 100, 3, "x"), result(0.02, 50, 2, "ANSWER: 8")])
        self.assertEqual(tm["n_result_events"], 2)
        self.assertAlmostEqual(tm["cost_usd"], 0.03)
        self.assertEqual(tm["output_tokens"], 150)
        self.assertEqual(tm["turns"], 5)
        self.assertEqual(tm["final_text"], "ANSWER: 8")

    def test_failed_fetch_heuristic(self):
        evs = [INIT, ev_tool("Bash", "1", command="curl -s https://grep.app/api"),
               ev_res("1", "<title>Vercel Security Checkpoint</title>"),
               ev_tool("Bash", "2", command="curl -s https://example.org"), ev_res("2", "hello"),
               ev_tool("WebFetch", "3", url="https://x"), ev_res("3", "", err=True),
               ev_tool("Bash", "4", command="ls"), ev_res("4", "")]
        self.assertEqual(pov.failed_fetches(pov.transcript_metrics(evs)), (3, 2))


class Calls(unittest.TestCase):
    def test_classify(self):
        c = pov.classify_call
        self.assertEqual(c(["what is the exact value of kInputMarginBytes in snappy"]), "v2-ask")
        self.assertEqual(c(["ask", "q"]), "v2-ask")
        self.assertEqual(c(["hub", "pull", "lz4"]), "v2-hub-pull")
        self.assertEqual(c(["corpus", "add", "--from", "x.json"]), "v1-corpus-add")
        self.assertEqual(c(["code", "lz4", "frame checksum"]), "code")
        self.assertEqual(c(["--insecure", "--port", "9200"]), "node-or-flag")

    def text_call(self, out, rc=0, argv=None):
        return {"argv": argv or ["how does snappy x"], "rc": rc, "out": out, "err": ""}

    def test_parse_text_answer(self):
        out = "snappy  pin 2026-10-02 (15 days old)  BSD-3-Clause  snappy.cc:12\n...\nanswered from: local pack\n"
        p = pov.parse_answer_output(self.text_call(out))
        self.assertEqual((p["corpus"], p["answered_from"]), ("snappy", "local_pack"))

    def test_parse_json_answer(self):
        p = pov.parse_answer_output(self.text_call(json.dumps({"corpus": "lz4", "answered_from": "local_pack"})))
        self.assertEqual((p["corpus"], p["answered_from"]), ("lz4", "local_pack"))

    def test_routing(self):
        ok = self.text_call("lz4  pin 2026-10-02 (15 days old)  x  a.c:1\nanswered from: local pack\n")
        self.assertTrue(pov.routing([ok], "lz4")["ok"])
        self.assertFalse(pov.routing([ok], "zstd")["ok"])
        amb = self.text_call("lz4 — an agent ...\nzstd — an agent ...\n", rc=4)
        self.assertTrue(pov.routing([amb], "zstd")["ok"])
        byp = self.text_call("lz4  pin x", argv=["ask", "q q", "--corpus", "lz4"])
        self.assertFalse(pov.routing([byp], "lz4")["ok"])
        self.assertEqual(pov.routing([], "lz4")["status"], "no_v2_call")


class Grading(unittest.TestCase):
    def test_c1_uses_registered_checker(self):
        task = {"id": "T4", "re": ["0xf|15"], "need": 1}
        self.assertEqual(pov.grade_c1(task, "blah\nANSWER: 15")["status"], "pass")
        self.assertEqual(pov.grade_c1(task, "ANSWER: unknown")["status"], "fail")

    def test_judge_without_key_is_unscored(self):
        old = os.environ.pop("ANTHROPIC_API_KEY", None)
        try:
            g = pov.grade_judge({"q": "q", "gold": "g", "rubric": ["a"]}, "ANSWER: x")
            self.assertEqual(g["status"], "unscored")
        finally:
            if old is not None:
                os.environ["ANTHROPIC_API_KEY"] = old

    def test_session_split(self):
        s = pov.split_session_answers("ANSWER T1: 4\nANSWER [T2]: 10\n", ["T1", "T2", "T3"])
        self.assertEqual(s, {"T1": "ANSWER: 4", "T2": "ANSWER: 10", "T3": ""})


class Contamination(unittest.TestCase):
    def tm(self, tools, *evs):
        init = dict(INIT, tools=tools)
        return pov.transcript_metrics([init, *evs, result(0.01, 1, 1)])

    def test_web_arm_clone_is_a_violation(self):
        tm = self.tm(["Bash", "Read", "Grep", "Glob", "WebFetch", "WebSearch"],
                     ev_tool("Bash", "1", command="git clone --depth 1 https://github.com/google/snappy"))
        c = pov.contamination("A2", tm, [], Path(tempfile.mkdtemp()), "snappy")
        self.assertTrue(any("clone" in v for v in c["violations"]))

    def test_git_arm_using_xerj_is_a_violation(self):
        tm = self.tm(["Bash", "Read", "Grep", "Glob"], ev_tool("Bash", "1", command="xerj 'how does x'"))
        c = pov.contamination("A1", tm, [], Path(tempfile.mkdtemp()), "snappy")
        self.assertTrue(any("xerj" in v for v in c["violations"]))

    def test_v2_arm_without_local_pack_answer_is_a_violation(self):
        tm = self.tm(["Bash", "Read", "Grep", "Glob"], ev_tool("Bash", "1", command="xerj 'how does x y'"))
        call = {"argv": ["how does x y"], "rc": 1, "out": "", "err": "no hub corpus matches"}
        c = pov.contamination("A6", tm, [call], Path(tempfile.mkdtemp()), "snappy")
        self.assertTrue(any("local pack" in v for v in c["violations"]))

    def test_undeclared_tool_is_a_violation(self):
        tm = self.tm(["Bash", "Read", "Grep", "Glob"], ev_tool("WebFetch", "1", url="https://x"))
        c = pov.contamination("A1", tm, [], Path(tempfile.mkdtemp()), "snappy")
        self.assertTrue(any("undeclared" in v for v in c["violations"]))

    def test_search_arm_that_skips_its_endpoint_is_a_violation(self):
        tm = self.tm(["Bash", "Read", "Grep", "Glob"],
                     ev_tool("Bash", "1", command="curl -s https://raw.githubusercontent.com/google/snappy/x/snappy.cc"))
        c = pov.contamination("A4", tm, [], Path(tempfile.mkdtemp()), "snappy")
        self.assertTrue(any("never called the web-search API" in v for v in c["violations"]))

    def test_memory_arm_clean(self):
        tm = self.tm([])
        c = pov.contamination("A0", tm, [], Path(tempfile.mkdtemp()), "snappy")
        self.assertEqual(c["violations"], [])


if __name__ == "__main__":
    unittest.main()
