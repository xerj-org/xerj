#!/usr/bin/env python3
"""Read-only post-deploy verification of XERJ's agent-discovery resources.

Run [origin] [--skills] [--json]. Exit 0: pass; 1: delivery/contract failure;
2: usage error. Live checks belong after deployment, never in a PR merge gate.
Only fixed XERJ resource paths are requested; advertised URLs are never fetched.
Requests are direct HTTP(S), without cookies, credentials, proxies or redirects.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import re
import sys
import urllib.parse
from email.message import Message

from verify_live import USER_AGENT

MAX_BYTES = 1024 * 1024
TARGETS = {
    "describedby": ("/llms.txt", "text/plain"),
    "service-desc": ("/openapi.json", "application/json"),
    "service-doc": ("/docs/agents/endpoints", "text/html"),
}
SKILL_INDEX = "/.well-known/agent-skills/index.json"
SKILL_FILE = "/.well-known/agent-skills/xerj/SKILL.md"
SKILL_SCHEMA = "https://schemas.agentskills.io/discovery/0.2.0/schema.json"


class FinalResponse(http.client.HTTPResponse):
    def _read_status(self):
        # HTTPResponse in supported Python versions skips 100, but can expose
        # 103 as the final response. Consume informational headers separately;
        # only the final response's Link fields can satisfy discovery checks.
        # RFC 9110 section 15.2; bounded against endless informational replies.
        for _ in range(9):
            version, status, reason = super()._read_status()
            if not 100 <= status < 200 or status == 101:
                return version, status, reason
            http.client.parse_headers(self.fp)
        raise http.client.HTTPException("too many informational responses")


def origin(value: str) -> str:
    p = urllib.parse.urlsplit(value)
    if (p.scheme not in {"http", "https"} or not p.hostname
            or p.username is not None or p.password is not None
            or p.path not in {"", "/"} or p.query or p.fragment
            or any(c.isspace() for c in value)):
        raise ValueError("expected an http(s) origin without credentials, path, query or fragment")
    _ = p.port  # validate malformed/out-of-range ports before any request
    return urllib.parse.urlunsplit((p.scheme, p.netloc, "", "", ""))


def fetch(base: str, path: str, method: str, timeout: float):
    p = urllib.parse.urlsplit(base)
    cls = http.client.HTTPSConnection if p.scheme == "https" else http.client.HTTPConnection
    conn = cls(p.hostname, p.port, timeout=timeout)
    conn.response_class = FinalResponse
    try:
        conn.request(method, path, headers={"User-Agent": USER_AGENT,
                                           "Accept-Encoding": "identity"})
        response = conn.getresponse()
        body = response.read(MAX_BYTES + 1)
        if len(body) > MAX_BYTES:
            raise ValueError("response exceeds the 1 MiB verification limit")
        return response.status, response.headers, body, None
    except (OSError, ValueError, http.client.HTTPException) as exc:
        return 0, Message(), b"", f"{type(exc).__name__}: {exc}"
    finally:
        conn.close()


def split_links(value: str) -> list[str]:
    """Split commas outside URI brackets and quoted strings (RFC 8288)."""
    parts, start, quoted, escaped, bracket = [], 0, False, False, False
    for i, char in enumerate(value):
        if escaped:
            escaped = False
        elif quoted and char == "\\":
            escaped = True
        elif char == '"' and not bracket:
            quoted = not quoted
        elif not quoted:
            if char == "<":
                bracket = True
            elif char == ">":
                bracket = False
            elif char == "," and not bracket:
                parts.append(value[start:i])
                start = i + 1
    if quoted or bracket:
        raise ValueError("unclosed Link URI or quoted string")
    return parts + [value[start:]]


def relations(headers: Message, base: str) -> set[tuple[str, str]]:
    found = set()
    for value in headers.get_all("Link", []):
        for link in split_links(value):
            match = re.fullmatch(r"\s*<([^<>]*)>\s*(;.*)?\s*", link)
            if not match:
                raise ValueError("malformed Link field")
            params = Message()
            params["Content-Type"] = "link" + (match[2] or "")
            # get_param takes the first rel; a later duplicate must not turn
            # an unrelated link into a valid discovery relation (RFC 8288).
            rel = params.get_param("rel", "")
            if not isinstance(rel, str) or params.get_param("anchor") is not None:
                continue  # an explicit anchor changes the link's context
            target = urllib.parse.urljoin(base + "/", match[1])
            for name in rel.split():
                found.add((name.lower(), target))
    return found


def verify(base: str, *, skills: bool = False, timeout: float = 15) -> list[dict]:
    results = []

    def request(path, method="GET", media=None):
        status, headers, body, error = fetch(base, path, method, timeout)
        failures = [error] if error else []
        if not error and status != 200:
            failures.append(f"HTTP {status}; expected 200 (redirects are not followed)")
        if status == 200 and media and (len(headers.get_all("Content-Type", [])) != 1
                                        or headers.get_content_type() != media):
            failures.append(f"Content-Type {headers.get('Content-Type')!r}; expected {media}")
        row = {"method": method, "url": base + path, "status": status,
               "failures": failures}
        results.append(row)
        return row, headers, body

    for method in ("GET", "HEAD"):
        row, headers, _ = request("/", method, "text/html")
        if row["status"] == 200:
            try:
                found = relations(headers, base)
                for rel, (path, _) in TARGETS.items():
                    if (rel, base + path) not in found:
                        row["failures"].append(f"missing {rel} link to {path} on final response")
            except ValueError as exc:
                row["failures"].append(str(exc))

    for rel, (path, media) in TARGETS.items():
        row, _, body = request(path, media=media)
        if rel == "service-desc" and not row["failures"]:
            try:
                spec = json.loads(body)
                if not isinstance(spec, dict) or not isinstance(spec.get("openapi"), str) \
                        or not isinstance(spec.get("paths"), dict):
                    raise ValueError("missing OpenAPI version or paths object")
            except (ValueError, UnicodeError) as exc:
                row["failures"].append(f"invalid OpenAPI JSON: {exc}")

    if skills:
        row, headers, body = request(SKILL_INDEX, media="application/json")
        for key, expected in (("Access-Control-Allow-Origin", "*"),):
            if row["status"] == 200 and headers.get(key) != expected:
                row["failures"].append(f"{key} must be {expected}")
        digest = None
        if not row["failures"]:
            try:
                index = json.loads(body)
                if not isinstance(index, dict) or index.get("$schema") != SKILL_SCHEMA \
                        or not isinstance(index.get("skills"), list):
                    raise ValueError("expected the discovery v0.2.0 schema and skills array")
                entries = [e for e in index["skills"] if isinstance(e, dict) and e.get("name") == "xerj"]
                if len(entries) != 1:
                    raise ValueError("expected exactly one xerj entry")
                entry = entries[0]
                if (entry.get("type") != "skill-md"
                        or not isinstance(entry.get("url"), str)
                        or urllib.parse.urljoin(base + SKILL_INDEX, entry["url"]) != base + SKILL_FILE
                        or not isinstance(entry.get("description"), str)
                        or not 1 <= len(entry["description"].strip()) <= 1024
                        or not isinstance(entry.get("digest"), str)
                        or not re.fullmatch(r"sha256:[0-9a-f]{64}", entry["digest"])):
                    raise ValueError("invalid xerj skill metadata, URL or digest")
                digest = entry["digest"]
            except (ValueError, UnicodeError) as exc:
                row["failures"].append(f"invalid skill index: {exc}")
        if digest is not None:
            row, headers, body = request(SKILL_FILE, media="text/markdown")
            if row["status"] == 200:
                if headers.get("Access-Control-Allow-Origin") != "*":
                    row["failures"].append("Access-Control-Allow-Origin must be *")
                if "sha256:" + hashlib.sha256(body).hexdigest() != digest:
                    row["failures"].append("skill bytes do not match the index SHA-256")

    for row in results:
        row["ok"] = not row["failures"]
    return results


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="https://xerj.org")
    parser.add_argument("--skills", action="store_true", help="also require the public xerj skill index and digest")
    parser.add_argument("--timeout", type=float, default=15, help="per socket operation, seconds (0 < n <= 60)")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    try:
        base = origin(args.base)
        if not 0 < args.timeout <= 60:
            raise ValueError("timeout must be greater than 0 and at most 60 seconds")
    except ValueError as exc:
        parser.error(str(exc))
    results = verify(base, skills=args.skills, timeout=args.timeout)
    failed = sum(not row["ok"] for row in results)
    report = {"base": base, "skills": args.skills, "checked": len(results),
              "failed": failed, "results": results}
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        for row in results:
            print(f"{'PASS' if row['ok'] else 'FAIL'} {row['method']} {row['url']} "
                  f"HTTP {row['status']} " + json.dumps(row["failures"], ensure_ascii=True))
        print(f"verify_agent_discovery: {len(results)} requests, {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
