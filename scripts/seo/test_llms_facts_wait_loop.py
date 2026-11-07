#!/usr/bin/env python3
"""The `/_facts` wait loop that llms.txt and the model quickstart hand to an
agent must stop on every state except `not_loaded` (#3021).

The old loop polled until `model.state == "loaded"`. A node started with
`--facts-model-dir` pointing at a missing or broken bundle answers
`GET /_facts` with HTTP 200 and `model.state: "unavailable"`, and a node
started without the flag answers `404 xerj_query_not_enabled`; neither ever
becomes `loaded`, so an agent following the docs waited forever.

Each loop is extracted verbatim from the file and run under bash with stub
`curl` and `sleep` binaries that replay a scripted sequence of `/_facts`
bodies (the bodies are the ones an rc.101 node returns). `jq` is the real one.

Run: python3 scripts/seo/test_llms_facts_wait_loop.py
"""

import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]

NOT_UP = ""
NOT_LOADED = json.dumps({"namespaces": [], "model": {"state": "not_loaded"}})
LOADED = json.dumps({"namespaces": [], "model": {"state": "loaded"}})
UNAVAILABLE = json.dumps(
    {
        "namespaces": [],
        "model": {
            "state": "unavailable",
            "error": "/models/nope/config.json: No such file or directory (os error 2)",
        },
    }
)
NOT_ENABLED = json.dumps(
    {
        "error": {
            "type": "xerj_query_not_enabled",
            "reason": "XERJ-Query is not enabled on this node; start the server "
            "with --facts-model-dir <DIR> (or XERJ_FACTS_MODEL_DIR) to serve /_facts",
        },
        "status": 404,
    }
)


def wait_loops():
    """Every `/_facts` wait loop an agent is told to run."""
    loops = {}
    llms = (ROOT / "landing" / "llms.txt").read_text(encoding="utf-8")
    found = re.findall(r"`(until [^`]*_facts[^`]*)`", llms)
    assert len(found) == 1, f"landing/llms.txt: expected one /_facts wait loop, got {found}"
    loops["landing/llms.txt"] = found[0]
    quick = (ROOT / "docs" / "MODEL_QUICKSTART.md").read_text(encoding="utf-8")
    found = [l for l in quick.splitlines() if l.startswith("until ") and "_facts" in l]
    assert len(found) == 1, f"docs/MODEL_QUICKSTART.md: expected one wait loop, got {found}"
    loops["docs/MODEL_QUICKSTART.md"] = found[0]
    return loops


class FactsWaitLoopTests(unittest.TestCase):
    def setUp(self):
        if shutil.which("jq") is None or shutil.which("bash") is None:
            self.skipTest("needs bash and jq")
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        stub_bin = self.dir / "bin"
        stub_bin.mkdir()
        # curl: print the next scripted body; repeat the last one forever.
        (stub_bin / "curl").write_text(
            "#!/usr/bin/env bash\n"
            'n=$(cat "$FAKE_DIR/n" 2>/dev/null || echo 0)\n'
            'total=$(ls "$FAKE_DIR"/body.* | wc -l)\n'
            'i=$(( n < total ? n : total - 1 ))\n'
            'echo $((n + 1)) > "$FAKE_DIR/n"\n'
            'cat "$FAKE_DIR/body.$i"\n'
        )
        (stub_bin / "sleep").write_text("#!/usr/bin/env bash\nexit 0\n")
        for f in stub_bin.iterdir():
            f.chmod(0o755)
        self.stub_bin = stub_bin

    def tearDown(self):
        self.tmp.cleanup()

    def run_loop(self, loop, bodies):
        fake = self.dir / "fake"
        if fake.exists():
            shutil.rmtree(fake)
        fake.mkdir()
        for i, body in enumerate(bodies):
            (fake / f"body.{i}").write_text(body)
        env = dict(os.environ)
        env["PATH"] = f"{self.stub_bin}:{env['PATH']}"
        env["FAKE_DIR"] = str(fake)
        try:
            out = subprocess.run(
                ["bash", "-c", loop],
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
            )
        except subprocess.TimeoutExpired:
            return None
        return out.stdout.strip().splitlines()[-1] if out.stdout.strip() else ""

    def test_loaded_after_preload_is_success(self):
        for name, loop in wait_loops().items():
            with self.subTest(name):
                last = self.run_loop(loop, [NOT_UP, NOT_LOADED, NOT_LOADED, LOADED])
                self.assertEqual(last, "loaded", f"{name}: {loop}")

    def test_missing_bundle_stops_the_loop_and_says_unavailable(self):
        for name, loop in wait_loops().items():
            with self.subTest(name):
                last = self.run_loop(loop, [NOT_UP, NOT_LOADED, UNAVAILABLE])
                self.assertIsNotNone(
                    last, f"{name}: the loop polls forever on model.state unavailable"
                )
                self.assertEqual(last, "unavailable", f"{name}: {loop}")

    def test_node_without_the_flag_stops_the_loop(self):
        for name, loop in wait_loops().items():
            with self.subTest(name):
                last = self.run_loop(loop, [NOT_UP, NOT_ENABLED])
                self.assertIsNotNone(
                    last, f"{name}: the loop polls forever on 404 xerj_query_not_enabled"
                )
                self.assertEqual(last, "xerj_query_not_enabled", f"{name}: {loop}")

    def test_llms_says_what_each_error_means(self):
        llms = (ROOT / "landing" / "llms.txt").read_text(encoding="utf-8")
        line = next(l for l in llms.splitlines() if "`until " in l and "_facts" in l)
        for needle in ("`unavailable`", "`xerj_query_not_enabled`", ".model.error", "bundle.json"):
            self.assertIn(needle, line)


if __name__ == "__main__":
    unittest.main()
