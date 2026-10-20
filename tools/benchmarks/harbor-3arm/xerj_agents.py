"""Harbor agents for the XERJ 3-arm benchmark campaign.

Arm A: stock `claude-code` (no class here — use `-a claude-code`).
Arm B: ClaudeCodeXerj       — website-default XERJ (plain autoindex, stock hint)
Arm C: ClaudeCodeXerjTuned  — source-informed config + search playbook

Design rules (what keeps the comparison honest):
  * The task, environment, verifier and judge are UNTOUCHED — only the agent
    varies, so scores stay comparable with the stock arm and the leaderboard.
  * XERJ is installed and its server started during agent SETUP (not billed to
    the agent), but the agent runs `xerj autoindex` itself at the start of the
    run — a real user's agent pays the indexing cost, so ours does too.
  * The instruction is extended via the agent's own Jinja2 prompt-template
    mechanism; the task text inside {{ instruction }} is never edited.

Auth: subscription OAuth. Pass through harbor run:
  --ae CLAUDE_CODE_OAUTH_TOKEN=<token from `claude setup-token`>
  --ae CLAUDE_FORCE_OAUTH=true
"""
import os
import shlex
from pathlib import Path

from typing_extensions import override

from harbor.agents.installed.claude_code import ClaudeCode
from harbor.environments.base import BaseEnvironment

HERE = Path(__file__).parent

# Point XERJ_BIN at the OFFICIAL musl release binary (xerj.org/get or the
# GitHub release's x86_64-unknown-linux-musl asset). Task containers run older
# distros, and a locally-built glibc binary dies inside them with
# `GLIBC_2.43 not found` (measured 2026-08-19).
XERJ_BIN = Path(os.environ.get("XERJ_BIN", ""))
XERJ_PORT = int(os.environ.get("XERJ_PORT", "9210"))
TARGET = "/usr/local/bin/xerj"


class ClaudeCodeXerj(ClaudeCode):
    """Claude Code + XERJ reference-coding (autoindex --no-graph)."""

    TEMPLATE = "xerj_prompt_default.j2"

    def __init__(self, *args, **kwargs):
        kwargs.setdefault("prompt_template_path", HERE / self.TEMPLATE)
        super().__init__(*args, **kwargs)

    @staticmethod
    def name() -> str:
        return "claude-code-xerj"

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        await super().install(environment)
        # Path("") is Path("."), which exists: an unset XERJ_BIN must fail here.
        if not XERJ_BIN.is_file():
            raise RuntimeError(
                "Set XERJ_BIN to the x86_64-unknown-linux-musl release binary "
                "(https://github.com/xerj-org/xerj/releases) — musl, not a "
                "local glibc build, or it will not run inside task containers")
        await self._upload_agent_owned_file(environment, XERJ_BIN, TARGET)
        await self.exec_as_root(environment, command=f"chmod 755 {shlex.quote(TARGET)}")
        # Start the server as the agent user; poll health so a dead server
        # fails SETUP loudly instead of surfacing as a confusing agent run.
        await self.exec_as_agent(
            environment,
            command=(
                "mkdir -p /tmp/xerj-data && "
                "printf '[server]\\nbind_address = \"127.0.0.1\"\\n"
                f"es_compat_port = {XERJ_PORT}\\n"
                f"rest_port = {XERJ_PORT + 1}\\n"
                f"grpc_port = {XERJ_PORT + 2}\\n' > /tmp/xerj.toml && "
                f"nohup {TARGET} -c /tmp/xerj.toml --insecure -d /tmp/xerj-data "
                f"> /tmp/xerj-server.log 2>&1 & "
                "for i in $(seq 1 60); do "
                f"  curl -sf http://localhost:{XERJ_PORT}/_cluster/health >/dev/null && exit 0; "
                "  sleep 1; "
                "done; "
                "echo 'XERJ server failed to start' >&2; tail -20 /tmp/xerj-server.log >&2; exit 1"
            ),
        )


class ClaudeCodeXerjTuned(ClaudeCodeXerj):
    """Claude Code + XERJ tuned from source knowledge: --no-semantic indexing
    (lexical embedder makes semantic fields pure overhead), definition-first
    query playbook (defs match_phrase boost, ax_format:code filter, _passage),
    and the cross-index IDF caveat with per-index scoping (the measured
    9/11 -> 11/11 retrieval fix)."""

    TEMPLATE = "xerj_prompt_tuned.j2"

    @staticmethod
    def name() -> str:
        return "claude-code-xerj-tuned"
