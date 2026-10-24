#!/usr/bin/env python3
"""Exercise delivery failures over loopback HTTP; never contacts production."""

import contextlib
import hashlib
import http.server
import io
import json
import threading
import unittest
from email.message import Message
from unittest import mock

import verify_agent_discovery as verifier

LINKS = ', '.join(f'<{path}>; rel="{rel}"; type="{media}"'
                  for rel, (path, media) in verifier.TARGETS.items())
SKILL = b'---\r\nname: xerj\r\ndescription: "Fixture"\r\n---\r\n# Skill\r\n'


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        self.server.requests.append((self.command, self.path))
        status, headers, body = self.server.routes.get(self.path, (404, {}, b"missing"))
        if self.path == "/" and self.server.early:
            self.wfile.write(b'HTTP/1.1 103 Early Hints\r\nLink: ' +
                             self.server.early.encode() + b'\r\n\r\n')
        self.send_response(status)
        for key, value in headers.items():
            for item in value if isinstance(value, list) else [value]:
                self.send_header(key, item)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)


class DeliveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.server.daemon_threads = True
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = "http://127.0.0.1:" + str(cls.server.server_port)

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def setUp(self):
        self.server.early = None
        self.server.requests = []
        self.index = {"$schema": verifier.SKILL_SCHEMA, "skills": [{
            "name": "xerj", "type": "skill-md", "description": "Fixture",
            "url": verifier.SKILL_FILE,
            "digest": "sha256:" + hashlib.sha256(SKILL).hexdigest(),
        }]}
        self.server.routes = {
            "/": (200, {"Content-Type": "text/html", "Link": LINKS}, b"<html>home</html>"),
            "/llms.txt": (200, {"Content-Type": "text/plain; charset=utf-8"}, b"# XERJ"),
            "/openapi.json": (200, {"Content-Type": "application/json"},
                              b'{"openapi":"3.1.0","paths":{}}'),
            "/docs/agents/endpoints": (200, {"Content-Type": "text/html"}, b"<h1>Endpoints</h1>"),
            verifier.SKILL_FILE: (200, {"Content-Type": "text/markdown; charset=utf-8",
                                       "Access-Control-Allow-Origin": "*"}, SKILL),
        }
        self.publish_index()

    def publish_index(self):
        self.server.routes[verifier.SKILL_INDEX] = (
            200, {"Content-Type": "application/json", "Access-Control-Allow-Origin": "*"},
            json.dumps(self.index).encode())

    def failed(self, skills=False):
        return [r for r in verifier.verify(self.base, skills=skills, timeout=2) if not r["ok"]]

    def test_success_and_read_only_request_scope(self):
        self.assertEqual(self.failed(skills=True), [])
        self.assertEqual(self.server.requests, [("GET", "/"), ("HEAD", "/"),
                         ("GET", "/llms.txt"), ("GET", "/openapi.json"),
                         ("GET", "/docs/agents/endpoints"),
                         ("GET", verifier.SKILL_INDEX), ("GET", verifier.SKILL_FILE)])

    def test_missing_links_fail_get_and_head(self):
        self.server.routes["/"][1].pop("Link")
        failures = self.failed()
        self.assertEqual([r["method"] for r in failures], ["GET", "HEAD"])
        self.assertIn("missing service-desc", ' '.join(failures[0]["failures"]))

    def test_early_hints_cannot_satisfy_final_response(self):
        self.server.early = LINKS
        self.server.routes["/"][1]["Link"] = '<https://fonts.example>; rel="preconnect"'
        self.assertEqual(len(self.failed()), 2)

    def test_early_hints_followed_by_valid_final_headers(self):
        self.server.early = '<https://fonts.example>; rel="preconnect"'
        self.server.routes["/"][1]["Link"] = LINKS.split(', ')
        self.assertEqual(self.failed(), [])

    def test_bad_target_delivery_is_a_failure(self):
        for status, media, body in [(404, "text/plain", b"missing"),
                                    (200, "text/html", b"<html>soft fallback</html>")]:
            with self.subTest(status=status, media=media):
                self.server.routes["/llms.txt"] = (status, {"Content-Type": media}, body)
                failures = self.failed()
                self.assertEqual(len(failures), 1)
                self.assertEqual(failures[0]["url"], self.base + "/llms.txt")

    def test_invalid_openapi_is_not_a_successful_json_response(self):
        for body in (b"not JSON", b"[]", b'{}'):
            with self.subTest(body=body):
                self.server.routes["/openapi.json"] = (200, {"Content-Type": "application/json"}, body)
                self.assertEqual(len(self.failed()), 1)

    def test_missing_content_type_is_not_inferred_as_plain_text(self):
        self.server.routes["/llms.txt"][1].pop("Content-Type")
        self.assertEqual(len(self.failed()), 1)

    def test_redirects_are_reported_without_following_them(self):
        self.server.routes["/llms.txt"] = (302, {"Location": "http://127.0.0.1:1/private"}, b"")
        self.assertEqual(len(self.failed()), 1)
        self.assertEqual(len(self.server.requests), 5)
        self.assertNotIn(("GET", "/private"), self.server.requests)

    def test_skill_check_is_explicitly_opt_in(self):
        self.server.routes.pop(verifier.SKILL_INDEX)
        self.assertEqual(self.failed(), [])
        self.assertEqual(len(self.failed(skills=True)), 1)

    def test_changed_skill_bytes_fail_the_advertised_digest(self):
        status, headers, body = self.server.routes[verifier.SKILL_FILE]
        self.server.routes[verifier.SKILL_FILE] = status, headers, body.replace(b"\r\n", b"\n")
        failures = self.failed(skills=True)
        self.assertEqual(len(failures), 1)
        self.assertIn("skill bytes do not match", failures[0]["failures"][0])

    def test_invalid_index_never_drives_arbitrary_requests(self):
        for key, value in [("url", "http://127.0.0.1:1/private"),
                           ("url", "/get"), ("digest", "sha256:bad"),
                           ("description", ""), ("type", "archive")]:
            with self.subTest(key=key, value=value):
                self.setUp()
                self.index["skills"][0][key] = value
                self.publish_index()
                self.assertEqual(len(self.failed(skills=True)), 1)
                self.assertEqual(len(self.server.requests), 6)

    def test_duplicate_or_malformed_index(self):
        for index in ([], {"skills": []}, {"$schema": verifier.SKILL_SCHEMA, "skills": []},
                      {"$schema": verifier.SKILL_SCHEMA, "skills": self.index["skills"] * 2}):
            with self.subTest(index=index):
                self.index = index
                self.publish_index()
                self.assertEqual(len(self.failed(skills=True)), 1)

    def test_cors_and_media_types_are_checked(self):
        self.server.routes[verifier.SKILL_FILE][1].pop("Access-Control-Allow-Origin")
        self.assertEqual(len(self.failed(skills=True)), 1)
        self.publish_index()
        self.server.routes[verifier.SKILL_INDEX][1]["Content-Type"] = "text/html"
        self.assertEqual(len(self.failed(skills=True)), 1)

    def test_bounded_response_read(self):
        self.server.routes["/llms.txt"] = (200, {}, b"x" * 40)
        with mock.patch.object(verifier, "MAX_BYTES", 20):
            status, _, _, error = verifier.fetch(self.base, "/llms.txt", "GET", 2)
        self.assertEqual(status, 0)
        self.assertIn("verification limit", error)

    def test_json_cli_and_exit_codes(self):
        for broken, expected in [(False, 0), (True, 1)]:
            if broken:
                self.server.routes["/"][1].pop("Link")
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                code = verifier.main([self.base, "--json"])
            report = json.loads(output.getvalue())
            self.assertEqual(code, expected)
            self.assertEqual(report["failed"], 2 if broken else 0)
            self.assertEqual(report["checked"], 5)


class ParserTests(unittest.TestCase):
    def test_quoted_commas_multiple_relations_and_duplicate_parameters(self):
        headers = Message()
        headers["Link"] = ('</a,b>; title="quoted, \\"title\\""; rel="service-doc describedby", '
                           '</x>; rel="alternate"; rel="service-desc"')
        found = verifier.relations(headers, "https://example.test")
        self.assertIn(("service-doc", "https://example.test/a,b"), found)
        self.assertIn(("describedby", "https://example.test/a,b"), found)
        self.assertNotIn(("service-desc", "https://example.test/x"), found)

    def test_anchored_or_quoted_text_does_not_forge_homepage_links(self):
        headers = Message()
        headers["Link"] = '</x>; rel="service-desc"; anchor="/elsewhere"'
        self.assertEqual(verifier.relations(headers, "https://example.test"), set())
        headers.replace_header("Link", '</x>; title="<fake>; rel=service-desc"')
        self.assertEqual(verifier.relations(headers, "https://example.test"), set())
        headers.replace_header("Link", '</x>; rel="unclosed')
        with self.assertRaises(ValueError):
            verifier.relations(headers, "https://example.test")

    def test_invalid_origins_stop_before_any_request(self):
        for value in ("file:///tmp/data", "https://u:p@example.test", "https://example.test/path",
                      "https://example.test?q=x", "https://example.test#fragment",
                      "https://example.test:99999", "https://exam ple.test"):
            with self.subTest(value=value), mock.patch.object(verifier, "fetch") as fetch, \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    verifier.main([value])
                self.assertEqual(error.exception.code, 2)
                fetch.assert_not_called()


if __name__ == "__main__":
    unittest.main()
