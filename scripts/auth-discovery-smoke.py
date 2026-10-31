#!/usr/bin/env python3
"""Check MCP auth diagnostics with private keys/cwd/home and no node requests."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def check(binary, case):
    with tempfile.TemporaryDirectory(prefix="xerj-auth-smoke-") as temporary:
        root = Path(temporary)
        home, project, other = (root / name for name in ("home", "project", "other"))
        for directory in (home, project, other):
            directory.mkdir()
        secret = "fixture-only-discovered-secret"
        explicit = "fixture-only-explicit-secret"
        key = (project if case == "local" else other) / "data" / "admin.key"
        key.parent.mkdir()
        key.write_text(secret + "\n")
        if case != "local":
            pointer = home / ".xerj" / "by-port" / "19991"
            pointer.parent.mkdir(parents=True)
            pointer.write_text(str(key) + "\n")
        # Child-only environment: no operator credentials or configuration.
        env = {name: os.environ[name] for name in (
            "PATH", "LANG", "LC_ALL", "SYSTEMROOT", "WINDIR", "TEMP", "TMP",
            "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH",
        ) if name in os.environ}
        env.update(HOME=str(home), USERPROFILE=str(home), XERJ_DISABLE_FEEDBACK="true")
        url = "https://example.invalid:19991" if case == "remote" else "http://127.0.0.1:19991"
        argv = [str(binary), "mcp", "--url", url]
        if case == "explicit-flag":
            argv += ["--auth", "ApiKey " + explicit]
        if case == "explicit-env":
            env["XERJ_AUTH"] = "ApiKey " + explicit
        request = "\n".join(json.dumps(message) for message in [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "auth-smoke", "version": "1"},
            }},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ]) + "\n"
        result = subprocess.run(argv, input=request, capture_output=True, text=True,
                                cwd=project, env=env, timeout=20)
        assert result.returncode == 0, "MCP process failed"
        assert secret not in result.stdout + result.stderr, "discovered key was exposed"
        assert explicit not in result.stdout + result.stderr, "explicit key was exposed"
        messages = [json.loads(line) for line in result.stdout.splitlines()]
        replies = [m for m in messages if m.get("id") == 2]
        assert len(replies) == 1 and any(
            tool["name"] == "xerj_map" for tool in replies[0]["result"]["tools"]
        ), "stdout must remain the working JSON-RPC channel"
        stderr = result.stderr
        if case == "foreign":
            assert str(key) in stderr, "the selected key path is missing"
            assert "published by the node on port 19991" in stderr, "pointer provenance is missing"
            assert "outside the working directory" in stderr, "foreign-directory warning is missing"
            assert "--auth or XERJ_AUTH" in stderr, "MCP's actual auth controls are missing"
            assert "XERJ_API_KEY" not in stderr, "MCP must not recommend an unread environment variable"
        elif case == "local":
            assert "found under the working directory" in stderr, "local provenance is missing"
            assert "outside the working directory" not in stderr, "local key was flagged as foreign"
        else:
            assert "using the admin key" not in stderr, "unexpected key autodiscovery"
            assert "outside the working directory" not in stderr, "unexpected foreign-key warning"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    binary = parser.parse_args().binary.resolve(strict=True)
    failed = 0
    for case in ("foreign", "local", "explicit-flag", "explicit-env", "remote"):
        try:
            check(binary, case)
        except (AssertionError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
            print(f"FAIL {case}: {error}")
            failed += 1
        else:
            print(f"PASS {case}")
    return int(failed != 0)


if __name__ == "__main__":
    raise SystemExit(main())
