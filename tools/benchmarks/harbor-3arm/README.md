# Harbor 3-arm benchmark harness — stock Claude Code vs XERJ vs tuned XERJ

Reproducible A/B/C on recognized agentic-coding benchmarks (Frontier-Bench
v0.1, DeepSWE v1.1, Senior SWE-Bench, Terminal-Bench 2.1, SWE-bench Verified):
the ONLY thing that varies between arms is the agent's search tooling — task,
environment, verifier and scoring are byte-identical, so numbers stay
comparable across arms and with public leaderboards.

| arm | agent |
|---|---|
| `native` | stock `claude-code` (Harbor built-in) |
| `xerj` | + XERJ server in the task container; website-default `autoindex` + the search hint autoindex itself prints |
| `xerj-tuned` | + source-informed config: `--no-semantic` indexing, definition-first query playbook (`defs` phrase boost, `ax_format:code` filter, `_passage`), cross-index IDF caveat with per-index scoping |

Both XERJ arms make the *agent* run `xerj autoindex` — a real user's agent pays
the indexing cost, so the benchmark does too. XERJ setup (binary upload, server
boot) happens in agent SETUP and is not billed to the agent.

## Prerequisites

```sh
pip install harbor                       # 0.21+
docker --version && docker buildx version && docker compose version
#  ^ plain docker.io is NOT enough — harbor dies on `unknown flag: --file`
#    without the buildx and compose-v2 plugins.
export XERJ_BIN=/path/to/xerj            # x86_64-unknown-linux-musl RELEASE
#  ^ musl, not a local glibc build — task containers run older distros and a
#    glibc binary fails inside them with `GLIBC_2.43 not found`.
claude setup-token                       # subscription auth for headless runs
export CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat-...
export ANTHROPIC_API_KEY=...             # ONLY for senior-swe's LLM judge
```

Parallel cells (measured): running many cells at once exhausts Docker's
default ~30-subnet address pool ("all predefined address pools have been fully
subnetted") — every trial creates 2 networks. Expand it once:

```json
// /etc/docker/daemon.json
{ "default-address-pools": [ {"base": "10.200.0.0/13", "size": 24} ] }
```

Auth note (measured): Harbor's automatic key resolution remaps whatever it
finds into `ANTHROPIC_API_KEY`, which an OAuth token cannot serve — the runner
therefore injects `CLAUDE_CODE_OAUTH_TOKEN` + `CLAUDE_FORCE_OAUTH=true` via
`--ae`. Verify any model slug by reading `modelUsage` back from
`claude --model <m> -p ... --output-format json` first: on subscription plans a
wrong slug can silently reroute (observed: `claude-fable-5` → opus-5).

## Run

```sh
./run_matrix.sh frontier-bench claude-opus-5 native      # one cell
./run_matrix.sh frontier-bench claude-opus-5 xerj
./run_matrix.sh frontier-bench claude-opus-5 xerj-tuned
./stage1.sh          # or: the aligned 15-task subset x 3 models x 3 arms
./collect_results.py # Wilson CIs + complete-matrix arm comparison
```

Every cell is one Harbor job; re-running a cell **resumes** it, so quota
pauses, token expiry and crashes cost only the trial in flight.

## Reading results honestly

`collect_results.py` reports per-cell resolve rates with Wilson 95% intervals
and a **complete matrix**: arm-vs-arm numbers count only tasks finished in
every arm of that benchmark+model. Do not quote cross-arm numbers from
unaligned cells. The collector reads Harbor v0.21's `rewards`, `cost_usd`,
`n_input_tokens` and `n_output_tokens` fields (including per-step usage),
while retaining the older field aliases. Wall time comes from trial timestamps
when no explicit duration is recorded. Per-cell cost/time includes recorded
failed attempts; complete-matrix cost/time remains scoped to matched tasks.
Unavailable metrics stay unknown and their coverage gaps are printed.
Anthropic's published Frontier-Bench figures use the
mini-swe-agent harness with mean reward over 5 attempts (`-a mini-swe-agent
-k 5`); runs with `claude-code` measure the agent product, not that protocol —
label accordingly. FrontierCode v1.1 is Cognition's private held-out set and
cannot be run by third parties.
