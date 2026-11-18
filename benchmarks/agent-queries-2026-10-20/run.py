#!/usr/bin/env python3
"""Capture the 2026-10-20 agent-query answer pages against a local xerj.

Writes raw command output under this directory. The GitHub issue and PR
bodies stay in the work directory (they are public, but they are inputs,
not the measured output). Re-run from the repo root:

    python3 benchmarks/agent-queries-2026-10-20/run.py /path/to/xerj
"""

from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = Path(__file__).resolve().parent
PORT = 9217
URL = f"http://127.0.0.1:{PORT}"
WORK = Path("/tmp/xerj-aq-2026-10-20")


def run(cmd: list[str], dest: Path, cwd: Path | None = None, timeout: int = 600,
        env: dict | None = None) -> subprocess.CompletedProcess:
    """Run cmd, write combined output to dest, return the completed process."""
    print("RUN", " ".join(cmd), flush=True)
    proc = subprocess.run(
        cmd,
        cwd=cwd or REPO,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )
    header = "$ " + " ".join(cmd) + f"\n# exit {proc.returncode}\n"
    dest.write_text(header + proc.stdout, encoding="utf-8", errors="replace")
    print(f"  -> {dest.name} exit={proc.returncode} bytes={dest.stat().st_size}", flush=True)
    return proc


def http_json(method: str, path: str, body: dict | None = None, timeout: int = 60):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        URL + path,
        data=data,
        method=method,
        headers={"content-type": "application/json"} if data else {},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read()
            return resp.status, raw
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read()


def save_http(name: str, method: str, path: str, body: dict | None = None) -> dict | None:
    status, raw = http_json(method, path, body)
    dest = OUT / name
    text = raw.decode("utf-8", errors="replace")
    dest.write_text(f"HTTP {status} {method} {path}\n{text}\n", encoding="utf-8")
    print(f"  HTTP {status} {path} -> {name} ({len(raw)} B)", flush=True)
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


def wait_health(proc: subprocess.Popen, seconds: int = 60) -> None:
    deadline = time.time() + seconds
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SystemExit(f"node exited {proc.returncode} before health")
        try:
            with urllib.request.urlopen(URL + "/_cluster/health", timeout=2) as resp:
                if resp.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError, socket.timeout):
            time.sleep(0.4)
    raise SystemExit("node did not answer /_cluster/health")


def to_jsonl(src: Path, dest: Path) -> int:
    rows = json.loads(src.read_text(encoding="utf-8"))
    with dest.open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row, ensure_ascii=False) + "\n")
    return len(rows)


def write_session_fixture(folder: Path) -> None:
    """Claude Code session shape: one JSON object per line.

    No live ~/.claude directory was on the capture machine. These three files
    follow the on-disk session shape (type, message, sessionId, timestamp,
    uuid) so the run is a format check, not a reading of anyone's chats.
    """
    folder.mkdir(parents=True, exist_ok=True)
    sid = "11111111-1111-4111-8111-111111111111"

    def line(obj: dict) -> str:
        return json.dumps(obj, ensure_ascii=False)

    short = [
        {
            "type": "user",
            "sessionId": sid,
            "uuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
            "timestamp": "2026-10-01T15:04:05.000Z",
            "message": {"role": "user", "content": "why did we decide to compress segments with zstd"},
        },
        {
            "type": "assistant",
            "sessionId": sid,
            "uuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
            "timestamp": "2026-10-01T15:04:11.000Z",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": "We picked zstd because the bench showed a smaller segment than gzip at the same read latency.",
                    }
                ],
            },
        },
    ]
    (folder / "session-short.jsonl").write_text(
        "\n".join(line(x) for x in short) + "\n", encoding="utf-8"
    )

    tool_text = "TOOL_OUTPUT_MARKER " + ("segment-byte " * 800)
    # Keep the line well under the 16 MB extractor cap and well over 4096.
    long_line = {
        "type": "assistant",
        "sessionId": sid,
        "uuid": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa3",
        "timestamp": "2026-10-01T15:05:00.000Z",
        "message": {
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "Bash",
                    "input": {"command": "wc -c segments.bin"},
                },
                {"type": "tool_result", "content": tool_text},
            ],
        },
    }
    encoded = line(long_line)
    (folder / "session-tool.jsonl").write_text(encoded + "\n", encoding="utf-8")
    (OUT / "07-long-line-bytes.txt").write_text(
        f"session-tool.jsonl line bytes {len(encoded.encode())}\n", encoding="utf-8"
    )

    # A second session, so search has more than one file.
    other = {
        "type": "user",
        "sessionId": "22222222-2222-4222-8222-222222222222",
        "uuid": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb1",
        "timestamp": "2026-09-02T09:00:00.000Z",
        "message": {"role": "user", "content": "leave the default embedder on lexical feature hashing"},
    }
    (folder / "session-other.jsonl").write_text(line(other) + "\n", encoding="utf-8")


def write_epoch_fixture(folder: Path) -> None:
    """Fractional-epoch timestamps, the shape a Slack ts uses. Not a Slack export."""
    folder.mkdir(parents=True, exist_ok=True)
    rows = []
    for i in range(40):
        rows.append({
            "id": f"p{i}",
            "text": "mcp mention" if i % 2 == 0 else "unrelated note",
            "ts": f"{1700000000 + i * 86400}.{i:06d}",
        })
    with (folder / "epoch.jsonl").open("w", encoding="utf-8") as fh:
        for row in rows:
            fh.write(json.dumps(row) + "\n")


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: run.py /path/to/xerj")
    binary = Path(sys.argv[1]).resolve()
    if not binary.is_file():
        raise SystemExit(f"no binary at {binary}")
    OUT.mkdir(parents=True, exist_ok=True)
    if WORK.exists():
        shutil.rmtree(WORK)
    WORK.mkdir(parents=True)
    env = os.environ.copy()
    env["XERJ_MAX_PROCESS_MEMORY_MB"] = "4096"
    env["XERJ_DISABLE_FEEDBACK"] = "1"

    run([str(binary), "--version"], OUT / "00-version.txt", env=env, timeout=30)
    meta = subprocess.check_output(["uname", "-a"], text=True)
    mem = Path("/proc/meminfo").read_text(encoding="utf-8").splitlines()[0]
    (OUT / "00-environment.txt").write_text(
        f"date 2026-10-20\nbinary {binary}\n{meta}{mem}\nnproc {os.cpu_count()}\n"
        f"XERJ_MAX_PROCESS_MEMORY_MB=4096\n",
        encoding="utf-8",
    )

    issues_json = WORK / "issues.json"
    prs_json = WORK / "prs.json"
    subprocess.run(
        ["gh", "issue", "list", "--repo", "xerj-org/xerj", "--state", "all", "--limit", "1000",
         "--json", "number,title,body,state,url,createdAt"],
        check=True, stdout=issues_json.open("w", encoding="utf-8"), timeout=180,
    )
    subprocess.run(
        ["gh", "pr", "list", "--repo", "xerj-org/xerj", "--state", "all", "--limit", "1000",
         "--json", "number,title,body,state,url,createdAt"],
        check=True, stdout=prs_json.open("w", encoding="utf-8"), timeout=180,
    )
    issue_dir = WORK / "issues"
    issue_dir.mkdir()
    n_issues = to_jsonl(issues_json, issue_dir / "issues.jsonl")
    n_prs = to_jsonl(prs_json, issue_dir / "prs.jsonl")
    (OUT / "01-export-stats.txt").write_text(
        f"issues {n_issues} bytes {issues_json.stat().st_size}\n"
        f"prs {n_prs} bytes {prs_json.stat().st_size}\n"
        f"jsonl issues {(issue_dir / 'issues.jsonl').stat().st_size}\n"
        f"jsonl prs {(issue_dir / 'prs.jsonl').stat().st_size}\n",
        encoding="utf-8",
    )

    write_session_fixture(WORK / "sessions")
    write_epoch_fixture(WORK / "epoch")
    # Copy the session fixture into the benchmark folder. It is synthetic and small.
    sess_out = OUT / "fixtures" / "sessions"
    if sess_out.exists():
        shutil.rmtree(sess_out)
    shutil.copytree(WORK / "sessions", sess_out)
    shutil.copytree(WORK / "epoch", OUT / "fixtures" / "epoch")

    node_log = open(OUT / "00-node.log", "w", encoding="utf-8")
    node = subprocess.Popen(
        [str(binary), "--insecure", "--data-dir", str(WORK / "node"),
         "--bind", "127.0.0.1", "--port", str(PORT)],
        stdout=node_log, stderr=subprocess.STDOUT, env=env,
    )
    try:
        wait_health(node)
        ax = [str(binary), "autoindex", "--url", URL, "--yes", "--no-graph",
              "--progress", "plain", "--disable-feedback"]

        def autoindex(folder: Path, prefix: str, dest: str) -> None:
            state = WORK / f"state-{prefix}"
            run(ax + [str(folder), "--prefix", prefix, "--state-dir", str(state)],
                OUT / dest, timeout=900, env=env)

        autoindex(issue_dir, "gh", "01-autoindex-issues.txt")
        save_http("01-mapping.json", "GET", "/gh-*/_mapping")

        dup_body = {
            "size": 5,
            "_source": ["number", "title", "state", "url"],
            "query": {"multi_match": {
                "query": "xerj feedback --open-pr commits and pushes",
                "fields": ["title", "body"],
            }},
        }
        save_http("01-duplicate-search.json", "POST", "/gh-*/_search", dup_body)

        para_body = {
            "size": 5,
            "_source": ["number", "title", "state", "url"],
            "query": {"multi_match": {
                "query": "does the feedback command commit files I did not mean to publish",
                "fields": ["title", "body"],
            }},
        }
        save_http("01-paraphrase-search.json", "POST", "/gh-*/_search", para_body)

        mlt_body = {
            "size": 5,
            "_source": ["number", "title", "state"],
            "query": {"more_like_this": {
                "fields": ["title", "body"],
                "like": ["xerj feedback --open-pr commits and pushes into whatever repository it is run from"],
                "min_term_freq": 1,
                "min_doc_freq": 1,
            }},
        }
        save_http("01-more-like-this.json", "POST", "/gh-*/_search", mlt_body)

        run([str(binary), "search", "--url", URL, "--prefix", "gh", "-k", "3",
             "xerj feedback --open-pr commits and pushes"],
            OUT / "01-xerj-search.txt", timeout=60, env=env)

        gh_search = run(
            ["gh", "issue", "list", "--repo", "xerj-org/xerj", "--state", "all", "--limit", "20",
             "--search", "feedback open-pr",
             "--json", "number,title,body,state,url"],
            OUT / "01-gh-search.txt", timeout=120, env=env,
        )
        (OUT / "01-byte-compare.txt").write_text(
            f"xerj_search_file_bytes {(OUT / '01-xerj-search.txt').stat().st_size}\n"
            f"gh_search_file_bytes {(OUT / '01-gh-search.txt').stat().st_size}\n"
            f"gh_search_exit {gh_search.returncode}\n",
            encoding="utf-8",
        )

        autoindex(REPO / "docs", "docs", "02-autoindex-docs.txt")
        save_http("02-mapping.json", "GET", "/docs-*/_mapping")
        q = "what is in here"
        run([str(binary), "search", "--url", URL, "--prefix", "docs", "-k", "3", q],
            OUT / "02-xerj-search.txt", timeout=60, env=env)
        run(["rg", "-n", "--heading", "-g", "*.md", "what is in here", "docs"],
            OUT / "02-rg.txt", timeout=60, env=env)
        (OUT / "02-byte-compare.txt").write_text(
            f"query {q}\n"
            f"xerj_search_bytes {(OUT / '02-xerj-search.txt').stat().st_size}\n"
            f"rg_bytes {(OUT / '02-rg.txt').stat().st_size}\n",
            encoding="utf-8",
        )

        claims = [
            "the primary user is an AI agent",
            "shares no code and no architecture",
            "point-at-folder",
            "Content sniffing never trusts the extension",
            "XERJ is a fork of Lucene",
        ]
        (OUT / "03-claims.json").write_text(json.dumps(claims, indent=2) + "\n", encoding="utf-8")
        lines = []
        for claim in claims:
            t0 = time.perf_counter()
            status, raw = http_json("POST", "/docs-*/_search", {
                "size": 1,
                "_source": ["ax_path"],
                "query": {"match_phrase": {"text": claim}},
            })
            elapsed_ms = round((time.perf_counter() - t0) * 1000)
            payload = json.loads(raw.decode())
            hits = payload.get("hits", {}).get("hits", []) if isinstance(payload, dict) else []
            total = payload.get("hits", {}).get("total", {}) if isinstance(payload, dict) else {}
            took = payload.get("took") if isinstance(payload, dict) else None
            path = ""
            if hits:
                path = str(hits[0].get("_source", {}).get("ax_path", ""))
            lines.append(json.dumps({
                "claim": claim,
                "http": status,
                "took": took,
                "client_ms": elapsed_ms,
                "total": total,
                "ax_path": path,
                "found": bool(hits),
            }))
        (OUT / "03-verify-output.jsonl").write_text("\n".join(lines) + "\n", encoding="utf-8")

        # 1000 ids: every exported issue number, then unused integers.
        numbers = [row["number"] for row in json.loads(issues_json.read_text(encoding="utf-8"))]
        ids = list(numbers)
        extra = 10_000_000
        while len(ids) < 1000:
            ids.append(extra)
            extra += 1
        ids = ids[:1000]
        (OUT / "04-ids.txt").write_text("\n".join(str(i) for i in ids) + "\n", encoding="utf-8")
        save_http("04-batch-terms.json", "POST", "/gh-issues/_search", {
            "size": 0,
            "track_total_hits": True,
            "query": {"terms": {"number": ids}},
        })
        # Per-id found/not-found needs the hits. Ask for the numbers back.
        save_http("04-batch-terms-hits.json", "POST", "/gh-issues/_search", {
            "size": 1000,
            "_source": ["number"],
            "track_total_hits": True,
            "query": {"terms": {"number": ids}},
        })

        # _msearch: a wildcard index, then a concrete index. Record whatever came back.
        msearch_wild = (
            '{"index":"gh-*"}\n'
            + json.dumps({"size": 0, "query": {"terms": {"number": ids[:6]}}})
            + "\n"
        ).encode()
        req = urllib.request.Request(
            URL + "/_msearch",
            data=msearch_wild,
            method="POST",
            headers={"content-type": "application/x-ndjson"},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                wild_status, wild_body = resp.status, resp.read()
        except urllib.error.HTTPError as exc:
            wild_status, wild_body = exc.code, exc.read()
        (OUT / "04-msearch-wildcard.txt").write_text(
            f"HTTP {wild_status}\n" + wild_body.decode("utf-8", errors="replace") + "\n",
            encoding="utf-8",
        )
        msearch_named = (
            '{"index":"gh-issues"}\n'
            + json.dumps({"size": 0, "query": {"terms": {"number": numbers[:6]}}})
            + "\n"
        ).encode()
        req = urllib.request.Request(
            URL + "/_msearch",
            data=msearch_named,
            method="POST",
            headers={"content-type": "application/x-ndjson"},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                named_status, named_body = resp.status, resp.read()
        except urllib.error.HTTPError as exc:
            named_status, named_body = exc.code, exc.read()
        (OUT / "04-msearch-named.txt").write_text(
            f"HTTP {named_status}\n" + named_body.decode("utf-8", errors="replace") + "\n",
            encoding="utf-8",
        )

        save_http("04-fuzzy.json", "POST", "/gh-issues/_search", {
            "size": 3,
            "_source": ["number", "title", "state"],
            "query": {"match": {"title": {"query": "feedbak open-pr", "fuzziness": "AUTO"}}},
        })

        save_http("06-mapping-createdAt.json", "GET", "/gh-issues/_mapping")
        save_http("06-histogram.json", "POST", "/gh-issues/_search", {
            "size": 0,
            "query": {"match_phrase": {"title": "autoindex"}},
            "aggs": {"per_month": {"date_histogram": {
                "field": "createdAt", "calendar_interval": "month",
            }}},
        })

        autoindex(WORK / "epoch", "epoch", "06-autoindex-epoch.txt")
        save_http("06-epoch-mapping.json", "GET", "/epoch-*/_mapping")

        init_dir = WORK / "initproj"
        (init_dir / ".codex").mkdir(parents=True)
        (init_dir / ".cursor").mkdir()
        (init_dir / ".codex" / "config.toml").write_text('model = "gpt-5"\n', encoding="utf-8")
        run([str(binary), "init", "--url", URL, "--disable-feedback"],
            OUT / "05-init-stdout.txt", cwd=init_dir, timeout=30, env=env)
        for name in (".mcp.json", ".cursor/mcp.json", ".codex/config.toml"):
            src = init_dir / name
            if src.is_file():
                text = src.read_text(encoding="utf-8")
                (OUT / ("05-" + name.replace("/", "-"))).write_text(text, encoding="utf-8")

        init_req = json.dumps({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "capture", "version": "0"}},
        })
        note = json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        payload = init_req + "\n" + note + "\n" + tools + "\n"
        proc = subprocess.run(
            [str(binary), "mcp", "--url", URL, "--disable-feedback"],
            input=payload, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            timeout=30, check=False, env=env,
        )
        (OUT / "05-tools-list.txt").write_text(
            f"# exit {proc.returncode}\n" + proc.stdout, encoding="utf-8"
        )

        autoindex(WORK / "sessions", "sessions", "07-autoindex-sessions.txt")
        run([str(binary), "search", "--url", URL, "--prefix", "sessions", "-k", "3",
             "why did we decide zstd"],
            OUT / "07-xerj-search.txt", timeout=60, env=env)
        save_http("07-search.json", "POST", "/sessions-*/_search", {
            "size": 5,
            "_source": ["ax_path", "type", "text"],
            "query": {"match": {"text": "zstd"}},
        })
        save_http("07-long-line-search.json", "POST", "/sessions-*/_search", {
            "size": 3,
            "_source": ["ax_path"],
            "query": {"match_phrase": {"text": "TOOL_OUTPUT_MARKER"}},
        })
        save_http("07-count.json", "POST", "/sessions-*/_count", {"query": {"match_all": {}}})
    finally:
        node.terminate()
        try:
            node.wait(timeout=15)
        except subprocess.TimeoutExpired:
            node.kill()
        node_log.close()
    print("DONE", flush=True)


if __name__ == "__main__":
    main()
