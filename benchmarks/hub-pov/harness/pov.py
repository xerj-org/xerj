#!/usr/bin/env python3
"""Corpus Hub v2 proof-of-value harness (benchmarks/hub-pov/PLAN.md).

One harness for all nine arms (PLAN §3: "All arms share one agent harness and
differ only in the tools on PATH and the prompt's tool instructions"). Every
`claude -p` runs inside ../sandbox.sh — there is no code path that launches an
agent outside it.

Subcommands
  prepare   fetch + verify the real signed packs, build the catalogue override,
            write the shims                                  (once per run dir)
  cell      run one cell: arm x task(s) x repeat              (one sandboxed agent)
  matrix    run every cell of a matrix file, serially (PLAN §9: one at a time)
  score     grade + metrics + contamination for every cell of a run
  report    aggregate a scored run into a table (+ the §7 gate, refused < 3 repeats)
  verify-c1 check the vendored C1 suites against branch bench/corpus-tasks

Nothing here publishes anything. Results stay in the run dir (default
/home/claude/hubdev-wt/pov-run, override with POV_RUN_DIR).
"""
from __future__ import annotations

import argparse
import base64
import datetime as dt
import json
import os
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
POV = HERE.parent                      # benchmarks/hub-pov
REPO = POV.parent.parent               # repo root
SANDBOX = POV / "sandbox.sh"
TASKS = POV / "tasks"
RUN_DIR = Path(os.environ.get("POV_RUN_DIR", "/home/claude/hubdev-wt/pov-run"))

# The catalogue override is served INSIDE each jail (its network namespace cannot
# reach the host loopback) on this port, from a read-only bind of <run>/hub/site.
CAT_PORT = 18080
CAT_URL = f"http://127.0.0.1:{CAT_PORT}"
# Where the real packs live. pack_url in the override catalogue points HERE, so the
# v2 arms download the real released bytes over the real network.
RELEASE_BASE = "https://github.com/xerj-org/xerj/releases/download"

# PLAN §3 leaves these unfixed ("locked at freeze", §10). PROVISIONAL values used
# by the smoke run only — see SMOKE.md "Ambiguities".
A3_ENDPOINT = "https://sourcegraph.com/.api/search/stream"
A4_ENDPOINT = "https://html.duckduckgo.com/html/"

# PLAN §3: "same --max-turns" for every arm but A0 (1). The number is not in the
# PLAN — PROVISIONAL, flagged in SMOKE.md. Session cells (C4) get the per-question
# budget times the number of questions.
MAX_TURNS = 15
CELL_TIMEOUT_S = 900
JUDGE_MODEL = "claude-opus-4-8"       # PLAN §6, verbatim

BASE_TOOLS = "Bash,Read,Grep,Glob"
ARMS = {
    # tools: the --tools list (the built-in tools the arm may call at all)
    # shims: which PATH shims are exposed; xerj: which xerj shim (None = not on PATH)
    "A0": {"role": "memory only (floor)", "tools": "", "xerj": None, "git_noclone": False},
    "A1": {"role": "normal agent + git (gate baseline)", "tools": BASE_TOOLS, "xerj": None, "git_noclone": False},
    "A2": {"role": "normal agent + web only (gate baseline)", "tools": BASE_TOOLS + ",WebFetch,WebSearch", "xerj": None, "git_noclone": True},
    "A3": {"role": "hosted regex code search (reference)", "tools": BASE_TOOLS, "xerj": None, "git_noclone": True},
    "A4": {"role": "web-search API (reference)", "tools": BASE_TOOLS, "xerj": None, "git_noclone": True},
    "A5": {"role": "XERJ today, v1 corpus add path", "tools": BASE_TOOLS, "xerj": "v1", "git_noclone": False},
    "A6": {"role": "XERJ v2 first use (cold)", "tools": BASE_TOOLS, "xerj": "v2", "git_noclone": False},
    "A7": {"role": "XERJ v2 steady state (pack cached)", "tools": BASE_TOOLS, "xerj": "v2", "git_noclone": False},
}
XERJ_ARMS = {"A5", "A6", "A7"}
V2_ARMS = {"A6", "A7"}


# --------------------------------------------------------------------------- utils
def now() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def die(msg: str, code: int = 2):
    print(f"pov.py: {msg}", file=sys.stderr)
    sys.exit(code)


def load_json(p: Path):
    return json.loads(Path(p).read_text())


def write_json(p: Path, obj) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(obj, indent=1, sort_keys=False) + "\n")


def hub_pubkey_hex() -> str:
    src = (REPO / "engine/crates/xerj-common/src/hub.rs").read_text()
    m = re.search(r'HUB_PUBKEY_HEX: &str = "([0-9a-f]{64})"', src)
    if not m:
        die("cannot read HUB_PUBKEY_HEX from engine/crates/xerj-common/src/hub.rs")
    return m.group(1)


def ed25519(*args: str) -> str:
    return subprocess.run(["bash", str(REPO / "tools/hub-packs/ed25519.sh"), *args],
                          check=True, capture_output=True, text=True).stdout.strip()


PKCS8_PREFIX = "302e020100300506032b657004220420"


def ed25519_sign(seed_hex: str, data_file: Path, sig_file: Path, scratch: Path) -> None:
    """Detached signature in the hub format (128 hex chars), with openssl only."""
    der = scratch / "seed.der"
    der.write_bytes(bytes.fromhex(PKCS8_PREFIX + seed_hex))
    raw = subprocess.run(["openssl", "pkeyutl", "-sign", "-inkey", str(der), "-keyform", "DER",
                          "-rawin", "-in", str(data_file)], check=True, capture_output=True).stdout
    der.unlink()
    sig_file.write_text(raw.hex() + "\n")


# --------------------------------------------------------------------------- tasks
def registry_manifest(prep: dict, slug: str) -> dict:
    return load_json(Path(prep["registry"]) / f"{slug}.json")


def load_task(ref: str, allow_smoke: bool) -> dict:
    """ref = '<class>:<suite>:<id>' e.g. C1:snappy:T4, C2:lz4.smoke:S1."""
    try:
        cls, suite, tid = ref.split(":")
    except ValueError:
        die(f"task ref must be <class>:<suite>:<id>, got {ref!r}")
    f = TASKS / cls / f"{suite}.json"
    if not f.exists():
        die(f"no task file {f}")
    doc = load_json(f)
    if doc.get("smoke_only") and not allow_smoke:
        die(f"{f} is a smoke-only fixture (not pre-registered, PLAN §5); refusing without --allow-smoke-fixtures")
    task = next((t for t in doc["tasks"] if t["id"] == tid), None)
    if task is None:
        die(f"no task {tid} in {f}")
    return {"ref": ref, "class": cls, "suite": suite, "corpus": doc["corpus"],
            "subject": doc.get("subject", doc["corpus"]), "suite_pin": doc.get("pin"),
            "smoke_only": bool(doc.get("smoke_only")), "task": task}


# --------------------------------------------------------------------------- prompts
COMMON = ("You are answering one technical question about the {subject} source tree. Be precise "
          "and brief. You MUST end your reply with a single line of the form 'ANSWER: <your answer>'.")
COMMON_SESSION = ("You are answering {n} technical questions about the {subject} source tree. Be "
                  "precise and brief. You MUST end your reply with one line per question, of the "
                  "form 'ANSWER <id>: <your answer>', in the order given.")
PIN_LINE = ("The reference version is commit {pin} ({pin_date}) of {url}; answer for that version.")


def arm_instructions(arm: str, slug: str, url: str, pin: str, prep: dict) -> str:
    org_repo = url.removeprefix("https://github.com/")
    if arm == "A0":   # PROTOCOL.md §4 bare prompt, verbatim
        return ("You are offline: no network access, and its source code is not available to you. "
                "Answer from your own knowledge; if you do not know, say 'unknown' in the ANSWER "
                "line — do not guess a fabricated constant.")
    if arm == "A1":
        return (f"You have a bash shell with network access. The project's public git repository is "
                f"{url}. You may clone it (e.g. `git clone --depth 1 {url}`; the reference commit "
                f"can be fetched with `git fetch --depth 1 origin {pin}`), search it with grep "
                f"(or ripgrep if installed) and read files.")
    if arm == "A2":
        return (f"You have a bash shell with network access and web tools (WebFetch, WebSearch, "
                f"curl). You may read web pages, including single files on GitHub (for example "
                f"https://raw.githubusercontent.com/{org_repo}/{pin}/<path> or the GitHub web UI). "
                f"You may NOT clone or fetch the git repository and may NOT download an "
                f"archive/tarball of it. The project's home is {url}.")
    if arm == "A3":
        return (f"You have a bash shell with network access and ONE tool for finding code: the "
                f"Sourcegraph public code-search API, via curl only: "
                f"curl -s -H 'Accept: text/event-stream' '{A3_ENDPOINT}?q=<url-encoded query>&display=20' "
                f"(query syntax: `repo:^github\\.com/{org_repo}$`, `file:<regex>`, "
                f"`patternType:regexp`). Do not clone the repository and do not fetch files from "
                f"any other site.")
    if arm == "A4":
        return (f"You have a bash shell with network access and ONE web-search API, via curl: "
                f"curl -s -A 'Mozilla/5.0' '{A4_ENDPOINT}?q=<url-encoded query>' (DuckDuckGo HTML "
                f"results). You may then fetch result pages with curl. Do not clone the repository "
                f"and do not download an archive/tarball of it.")
    if arm == "A5":
        manifest = Path(prep["registry"]) / f"{slug}.json"
        return ("XERJ is installed (`xerj` is on PATH). Use its reference-code flow: (1) start a node "
                "in the BACKGROUND, never in the foreground: `nohup xerj --insecure --port 9200 "
                "--data-dir $HOME/xerj-data > $HOME/xerj-node.log 2>&1 &` and wait until "
                "`curl -s localhost:9200/_cluster/health` answers; (2) `xerj corpus add --from "
                f"{manifest}`; (3) `xerj corpus index {slug}`; (4) query it: `xerj code {slug} \"<your "
                "search query>\"`. Do not clone or download the source yourself.")
    if arm in V2_ARMS:
        return ("XERJ is installed (`xerj` is on PATH). Ask it in plain English from bash: "
                "`xerj \"<your question>\"` — it picks the right reference corpus and answers from a "
                "signed, pre-indexed hub pack (`xerj ask --help` lists options). Use it as much as "
                "helps; cite what you found. Do not clone or download the source yourself.")
    die(f"unknown arm {arm}")


def build_prompt(arm: str, tasks: list[dict], prep: dict) -> str:
    slug = tasks[0]["corpus"]
    if any(t["corpus"] != slug for t in tasks):
        die("a session cell must stay within one corpus (PLAN §4 C4: 'ten questions of one corpus')")
    man = registry_manifest(prep, slug)
    src = man["repos"][0]
    pin_date = prep["packs"][slug]["pin_date"]
    subject = tasks[0]["subject"]
    if len(tasks) == 1:
        head = COMMON.format(subject=subject)
        qs = "The question: " + tasks[0]["task"]["q"]
    else:
        head = COMMON_SESSION.format(subject=subject, n=len(tasks))
        qs = "The questions:\n" + "\n".join(f"[{t['task']['id']}] {t['task']['q']}" for t in tasks)
    pin_line = PIN_LINE.format(pin=src["sha"], pin_date=pin_date, url=src["url"])
    return " ".join([head, pin_line, arm_instructions(arm, slug, src["url"], src["sha"], prep)]) + "\n" + qs


# --------------------------------------------------------------------------- prepare
def cmd_prepare(a):
    run = RUN_DIR
    xerj = Path(a.xerj).resolve()
    if not xerj.is_file():
        die(f"no xerj binary at {xerj}")
    registry = Path(a.registry).resolve()
    backlog = registry / "backlog/backlog-100.json"
    hub = run / "hub"
    (hub / "packs").mkdir(parents=True, exist_ok=True)
    (hub / "site/api/v1").mkdir(parents=True, exist_ok=True)
    (hub / "site/sigs").mkdir(parents=True, exist_ok=True)
    scratch = run / "scratch"; scratch.mkdir(exist_ok=True)
    pub_real = hub_pubkey_hex()
    tags = [t.strip() for t in a.tags.split(",") if t.strip()]
    rows, packs = [], {}
    for tag in tags:
        m = re.fullmatch(r"pack-(.+)-([0-9a-f]{8})", tag)
        if not m:
            die(f"not a pack release tag: {tag}")
        slug, pin8 = m.groups()
        fname = f"{slug}-{pin8}.hubpack.tar.zst"
        dest = hub / "packs" / tag
        dest.mkdir(exist_ok=True)
        if not (dest / fname).exists():
            subprocess.run(["gh", "release", "download", tag, "-R", "xerj-org/xerj", "-D", str(dest),
                            "--clobber"], check=True)
        # 1. The released bytes are the genuine, hub-key-signed pack.
        ed25519("verify", pub_real, str(dest / fname), str(dest / f"{fname}.sig"))
        pj = json.loads(subprocess.run(f"zstd -dc {shlex.quote(str(dest / fname))} | tar -xOf - pack.json",
                                       shell=True, check=True, capture_output=True, text=True).stdout)
        packs[slug] = {"tag": tag, "file": fname, "pin8": pin8, "pin_sha": pj["pin_sha"],
                       "pin_date": pj["pin_date"], "bytes": (dest / fname).stat().st_size,
                       "licence_class": pj.get("licence_class"),
                       "real_sig_verified_against": pub_real}
        rows.append(f"{slug}\t{pin8}\t{tag}\n")
    # 2. A throwaway signing key for the override. The live catalogue is not
    #    deployed and the hub seed is a CI secret, so the override catalogue cannot
    #    be signed with the real key; the client is pointed at the throwaway public
    #    key (XERJ_HUB_PUBKEY, the override consumer_e2e.sh documents for local
    #    rehearsals) and the pack signatures are re-made with it. The pack BYTES are
    #    still the released ones (sha256 in the catalogue; downloaded from GitHub).
    seed_f = run / "hub/throwaway.seed"
    if not seed_f.exists():
        seed_f.write_text(os.urandom(32).hex() + "\n")
        seed_f.chmod(0o600)
    pub_tmp = ed25519("derive", str(seed_f))
    flat = hub / "flat"; flat.mkdir(exist_ok=True)
    for slug, p in packs.items():
        src = hub / "packs" / p["tag"] / p["file"]
        link = flat / p["file"]
        if not link.exists():
            os.link(src, link)
        sig = hub / "site/sigs" / f"{p['file']}.sig"
        ed25519_sign(seed_f.read_text().strip(), src, sig, scratch)
        ed25519("verify", pub_tmp, str(src), str(sig))
        shutil.copy(sig, flat / f"{p['file']}.sig")   # build-catalogue reads it beside the pack
    (hub / "tags.tsv").write_text("".join(rows))
    cat = hub / "site/api/v1/catalogue.json"
    subprocess.run([str(xerj), "hub", "build-catalogue", "--registry", str(registry), "--backlog",
                    str(backlog), "--packs", str(flat), "--tags", str(hub / "tags.tsv"),
                    "--release-base", RELEASE_BASE, "--out", str(cat), "--sign-key", str(seed_f)],
                   check=True, env=dict(os.environ, XERJ_HUB_PUBKEY=pub_tmp))
    doc = load_json(cat)
    for e in doc["corpora"]:
        p = packs[e["slug"]]
        if not e["pack_url"].startswith(RELEASE_BASE):
            die(f"unexpected pack_url {e['pack_url']}")
        e["sig_url"] = f"{CAT_URL}/sigs/{p['file']}.sig"
    cat.write_text(json.dumps(doc, separators=(",", ":")))
    ed25519_sign(seed_f.read_text().strip(), cat, Path(str(cat) + ".sig"), scratch)
    ed25519("verify", pub_tmp, str(cat), str(cat) + ".sig")
    # 3. Shims.
    shims = run / "shims"
    for kind in ("v1", "v2"):
        d = shims / f"xerj-{kind}"; d.mkdir(parents=True, exist_ok=True)
        exports = "" if kind == "v1" else (f"export XERJ_HUB_URL='{CAT_URL}'\n"
                                           f"export XERJ_HUB_PUBKEY='{pub_tmp}'")
        body = (HERE / "shim_xerj.sh").read_text().replace("@HUB_EXPORTS@", exports).replace("@REAL@", str(xerj))
        (d / "xerj").write_text(body); (d / "xerj").chmod(0o755)
    d = shims / "git-noclone"; d.mkdir(parents=True, exist_ok=True)
    shutil.copy(HERE / "shim_git_noclone.sh", d / "git"); (d / "git").chmod(0o755)
    claude = Path(shutil.which("claude") or die("claude not on PATH")).resolve()
    prep = {"prepared_at": now(), "xerj": str(xerj), "xerj_version": subprocess.run(
                [str(xerj), "--version"], capture_output=True, text=True).stdout.strip(),
            "claude": str(claude), "claude_version": subprocess.run(
                [str(claude), "--version"], capture_output=True, text=True).stdout.strip(),
            "registry": str(registry), "registry_commit": a.registry_commit,
            "catalogue_override": {"site": str(hub / "site"), "url_in_jail": CAT_URL,
                                   "throwaway_pubkey": pub_tmp, "real_pubkey": pub_real,
                                   "pack_url_base": RELEASE_BASE},
            "packs": packs, "shims": str(shims)}
    write_json(run / "prepared.json", prep)
    print(json.dumps(prep, indent=1))


def load_prep() -> dict:
    p = RUN_DIR / "prepared.json"
    if not p.exists():
        die(f"{p} missing — run `pov.py prepare` first")
    return load_json(p)


# --------------------------------------------------------------------------- cell
def cell_dir(run_id: str, arm: str, refs: list[str], repeat: int) -> Path:
    key = "+".join(r.replace(":", "_") for r in refs) if len(refs) <= 2 else \
        f"{refs[0].split(':')[0]}_{refs[0].split(':')[1]}_session{len(refs)}"
    return RUN_DIR / "results" / run_id / arm / key / f"r{repeat}"


def run_script(arm: str, prompt_file: Path, prep: dict, tasks: list[dict], model: str,
               max_turns: int) -> str:
    """The script executed INSIDE the jail (bash). Generated, then run via sandbox.sh."""
    slug = tasks[0]["corpus"]
    claude = prep["claude"]
    tools = ARMS[arm]["tools"]
    args = [claude, "-p", "--model", model, "--output-format", "stream-json", "--verbose",
            "--max-turns", str(max_turns), "--tools", tools, "--strict-mcp-config",
            "--mcp-config", '{"mcpServers":{}}', "--disable-slash-commands",
            "--dangerously-skip-permissions"]
    q = shlex.quote
    co = prep["catalogue_override"]
    lines = ["set -u", 'S="$HOME/.sandbox"; mkdir -p "$HOME/.pov"',
             # received bytes on every non-loopback interface of the jail's netns
             'netrx() { sed "s/:/ /" /proc/net/dev | awk \'NR>2 && $1!="lo" {s+=$2} END {print s+0}\'; }']
    if arm in V2_ARMS:
        lines += [f"python3 -m http.server {CAT_PORT} --bind 127.0.0.1 --directory {q(co['site'])} "
                  f'> "$S/catalogue-http.log" 2>&1 &',
                  f"for i in $(seq 1 50); do curl -fs -m1 {CAT_URL}/api/v1/catalogue.json >/dev/null && break; sleep 0.2; done"]
    if arm == "A7":
        # Steady state = the pack is already pulled AND the hub node is up (as on a
        # machine that has answered before). The pull runs with the REAL binary, not
        # the shim, so it is not counted as an agent call; it is timed separately.
        lines += ['t_s0=$(date +%s.%N)', 'srx0=$(netrx)',
                  f"XERJ_HUB_URL={q(CAT_URL)} XERJ_HUB_PUBKEY={q(co['throwaway_pubkey'])} "
                  f"{q(prep['xerj'])} hub pull {q(slug)} > \"$S/setup.log\" 2>&1; echo $? > \"$S/setup.rc\"",
                  'echo "$(date +%s.%N) $t_s0" > "$S/setup.time"', 'echo $(( $(netrx) - srx0 )) > "$S/setup.rx"']
    lines += ['ls -1 "$HOME/.xerj/hub/packs" > "$S/packs_before.txt" 2>/dev/null || true',
              'cat /proc/loadavg > "$S/load_before"', 'rx0=$(netrx)', 't0=$(date +%s.%N)',
              f'timeout {CELL_TIMEOUT_S} ' + " ".join(q(x) for x in args) +
              f' "$(cat {q(str(prompt_file))})" < /dev/null > "$S/transcript.jsonl" 2> "$S/claude.err"',
              'rc=$?', 't1=$(date +%s.%N)', 'rx1=$(netrx)', 'cat /proc/loadavg > "$S/load_after"',
              'ls -1 "$HOME/.xerj/hub/packs" > "$S/packs_after.txt" 2>/dev/null || true',
              'echo "{\\"rc\\": $rc, \\"t0\\": $t0, \\"t1\\": $t1, \\"rx_iface_bytes\\": $((rx1-rx0))}" > "$S/timing.json"',
              'exit 0']
    return "\n".join(lines) + "\n"


def cmd_cell(a, prep=None):
    prep = prep or load_prep()
    arm = a.arm
    if arm not in ARMS:
        die(f"unknown arm {arm}")
    refs = a.task
    tasks = [load_task(r, a.allow_smoke_fixtures) for r in refs]
    for t in tasks:
        if t["corpus"] not in prep["packs"]:
            die(f"corpus {t['corpus']} has no prepared v2 pack (prepared: {sorted(prep['packs'])})")
    cdir = cell_dir(a.run_id, arm, refs, a.repeat)
    if cdir.exists():
        if not a.force:
            print(f"skip (exists): {cdir}"); return
        shutil.rmtree(cdir)
    home = cdir / "home"
    (home / ".sandbox").mkdir(parents=True)
    max_turns = 1 if arm == "A0" else MAX_TURNS * len(tasks)
    prompt = build_prompt(arm, tasks, prep)
    # The prompt is staged OUTSIDE the jail HOME and exposed read-only, so the agent
    # cannot see other cells; it holds no gold answer.
    (cdir / "prompt.txt").write_text(prompt)
    script = run_script(arm, cdir / "prompt.txt", prep, tasks, a.model, max_turns)
    (cdir / "run.sh").write_text(script)
    ro = [prep["claude"], str(cdir / "prompt.txt"), str(cdir / "run.sh")]
    prefix = []
    if ARMS[arm]["xerj"]:
        prefix.append(str(Path(prep["shims"]) / f"xerj-{ARMS[arm]['xerj']}"))
        ro.append(prep["xerj"])
    if ARMS[arm]["git_noclone"]:
        prefix.append(str(Path(prep["shims"]) / "git-noclone"))
    if arm == "A5":
        ro.append(prep["registry"])
    if arm in V2_ARMS:
        ro.append(prep["catalogue_override"]["site"])
    cmd = ["bash", str(SANDBOX), "--home", str(home)]
    for p in ro:
        cmd += ["--ro", p]
    cmd += ["--", "bash", str(cdir / "run.sh")]
    env = dict(os.environ, XERJ_POV_PATH_PREFIX=":".join(prefix))
    meta = {"schema": "hub-pov/cell/1", "run_id": a.run_id, "arm": arm, "role": ARMS[arm]["role"],
            "tasks": refs, "repeat": a.repeat, "model_requested": a.model, "max_turns": max_turns,
            "tools": ARMS[arm]["tools"], "path_prefix": prefix, "ro_paths": ro,
            "started_at": now(), "host_load_before": Path("/proc/loadavg").read_text().split()[:3],
            "prepared": {"xerj_version": prep["xerj_version"], "claude_version": prep["claude_version"],
                         "catalogue_override": prep["catalogue_override"]}}
    print(f"[{now()}] cell {arm} {' '.join(refs)} r{a.repeat} -> {cdir}", flush=True)
    t0 = time.time()
    p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=CELL_TIMEOUT_S + 600)
    meta["sandbox_rc"] = p.returncode
    meta["sandbox_wall_s"] = round(time.time() - t0, 2)
    meta["host_load_after"] = Path("/proc/loadavg").read_text().split()[:3]
    meta["finished_at"] = now()
    (cdir / "sandbox.stderr").write_text(p.stderr)
    write_json(cdir / "cell.json", meta)
    if p.returncode != 0:
        print(f"  sandbox rc={p.returncode}: {p.stderr[-500:]}", file=sys.stderr)


def cmd_matrix(a):
    prep = load_prep()
    m = load_json(Path(a.matrix))
    for rep in range(1, m["repeats"] + 1):
        for cell in m["cells"]:
            for arm in m["arms"]:
                ns = argparse.Namespace(arm=arm, task=cell if isinstance(cell, list) else [cell],
                                        repeat=rep, run_id=a.run_id, model=m["model"],
                                        allow_smoke_fixtures=m.get("allow_smoke_fixtures", False),
                                        force=a.force)
                cmd_cell(ns, prep)


# --------------------------------------------------------------------------- parsing
def read_events(path: Path) -> list[dict]:
    out = []
    if not path.exists():
        return out
    for line in path.read_text(errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    return out


def transcript_metrics(events: list[dict]) -> dict:
    """PLAN §6: cost and output tokens are SUMMED over every `result` event."""
    results = [e for e in events if e.get("type") == "result"]
    init = next((e for e in events if e.get("type") == "system" and e.get("subtype") == "init"), {})
    tool_uses, tool_results = [], {}
    for e in events:
        msg = e.get("message") or {}
        content = msg.get("content") if isinstance(msg.get("content"), list) else []
        if e.get("type") == "assistant":
            for c in content:
                if c.get("type") == "tool_use":
                    tool_uses.append({"id": c.get("id"), "name": c.get("name"), "input": c.get("input")})
        elif e.get("type") == "user":
            for c in content:
                if c.get("type") == "tool_result":
                    body = c.get("content")
                    if isinstance(body, list):
                        body = "\n".join(x.get("text", "") for x in body if isinstance(x, dict))
                    tool_results[c.get("tool_use_id")] = {"text": str(body or ""), "is_error": bool(c.get("is_error"))}
    final = results[-1].get("result", "") if results else ""
    return {
        "n_result_events": len(results),
        "cost_usd": round(sum(float(r.get("total_cost_usd") or 0) for r in results), 6),
        "output_tokens": sum(int((r.get("usage") or {}).get("output_tokens") or 0) for r in results),
        "input_tokens_all": sum(sum(int((r.get("usage") or {}).get(k) or 0) for k in
                                    ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"))
                                for r in results),
        "turns": sum(int(r.get("num_turns") or 0) for r in results),
        "is_error": any(r.get("is_error") for r in results),
        "result_subtypes": [r.get("subtype") for r in results],
        "model": init.get("model"), "init_tools": init.get("tools"), "init_mcp": init.get("mcp_servers"),
        "init_plugins": [p.get("name") for p in init.get("plugins") or []],
        "tool_uses": tool_uses, "tool_results": tool_results, "final_text": final,
    }


BLOCK_RE = re.compile(r"captcha|security checkpoint|just a moment|attention required|access denied|"
                      r"cf-chl|too many requests|rate limit|\b(403|429)\b.{0,20}(forbidden|too many)|"
                      r"HTTP/[0-9.]+ (403|429)", re.I)


def is_fetch(tu: dict) -> bool:
    if tu["name"] in ("WebFetch", "WebSearch"):
        return True
    if tu["name"] == "Bash":
        return bool(re.search(r"\b(curl|wget)\b", (tu.get("input") or {}).get("command", "")))
    return False


def failed_fetches(tm: dict) -> tuple[int, int]:
    """(fetches, failed) — PLAN §6 'failed fetches'. HEURISTIC: a fetch whose result is
    empty, an error, or looks like a bot-protection / 403 / 429 page. Flagged in SMOKE.md."""
    n = failed = 0
    for tu in tm["tool_uses"]:
        if not is_fetch(tu):
            continue
        n += 1
        r = tm["tool_results"].get(tu["id"], {"text": "", "is_error": True})
        if r["is_error"] or not r["text"].strip() or BLOCK_RE.search(r["text"][:20000]):
            failed += 1
    return n, failed


def bash_cmds(tm: dict) -> list[str]:
    return [(tu.get("input") or {}).get("command", "") for tu in tm["tool_uses"] if tu["name"] == "Bash"]


def xerj_calls(home: Path) -> list[dict]:
    log = home / ".pov/xerj-calls.jsonl"
    calls = []
    if log.exists():
        for line in log.read_text().splitlines():
            try:
                c = json.loads(line)
            except json.JSONDecodeError:
                continue
            d = home / ".pov/calls"
            c["out"] = (d / f"{c['n']}.out").read_text(errors="replace") if (d / f"{c['n']}.out").exists() else ""
            c["err"] = (d / f"{c['n']}.err").read_text(errors="replace") if (d / f"{c['n']}.err").exists() else ""
            calls.append(c)
    return calls


SUBCOMMANDS = {"ask", "code", "corpus", "hub", "init", "autoindex", "feedback", "mcp", "serve"}


def classify_call(argv: list[str]) -> str:
    if not argv:
        return "none"
    a0 = argv[0]
    if a0 == "ask":
        return "v2-ask"
    if a0 == "hub":
        return "v2-hub-" + (argv[1] if len(argv) > 1 else "")
    if a0 == "code":
        return "code-def" if "--def" in argv else "code"
    if a0 == "corpus":
        return "v1-corpus-" + (argv[1] if len(argv) > 1 else "")
    if not a0.startswith("-") and " " in a0 and a0 not in SUBCOMMANDS:
        return "v2-ask"           # the bare `xerj "<question>"` form (spec §5)
    if a0.startswith("-"):
        return "node-or-flag"
    return "other:" + a0


def parse_answer_output(c: dict) -> dict:
    """Routed corpus + answered_from from one v2 call's output (text or --json)."""
    out = c["out"]
    r = {"rc": c["rc"], "corpus": None, "answered_from": None, "candidates": []}
    try:
        j = json.loads(out)
        if isinstance(j, dict):
            r["corpus"] = j.get("corpus"); r["answered_from"] = j.get("answered_from")
            return r
    except (json.JSONDecodeError, ValueError):
        pass
    m = re.search(r"^answered from:\s*(.+)$", out, re.M)
    if m:
        r["answered_from"] = m.group(1).strip().replace(" ", "_")
    m = re.match(r"\s*([a-z0-9][a-z0-9._-]*)\s+pin\s", out)
    if m:
        r["corpus"] = m.group(1)
    if c["rc"] == 4:
        r["candidates"] = re.findall(r"^\s*([a-z0-9][a-z0-9._-]*)\s+[—-]\s", out + "\n" + c["err"], re.M)
    return r


def routing(calls: list[dict], expected: str) -> dict:
    """PLAN §2 H2 / §6: did the router pick the right corpus on the FIRST v2 question?
    Confident-and-correct, or Ambiguous containing the expected slug, counts as correct;
    a first call that passes --corpus bypassed routing (recorded, counted not-correct)."""
    v2 = [c for c in calls if classify_call(c["argv"]) == "v2-ask"]
    if not v2:
        return {"status": "no_v2_call", "ok": False}
    first = v2[0]
    if "--corpus" in first["argv"]:
        return {"status": "bypassed (--corpus on first call)", "ok": False}
    p = parse_answer_output(first)
    if first["rc"] == 4:
        ok = expected in p["candidates"]
        return {"status": "ambiguous", "candidates": p["candidates"], "ok": ok}
    if p["corpus"]:
        return {"status": "confident", "corpus": p["corpus"], "ok": p["corpus"] == expected}
    text = first["out"] + "\n" + first["err"]
    if "no hub corpus matches" in text:
        # NoCorpus. The client prints "closest corpora: a, b; re-run with --corpus".
        # PLAN H2 counts only Confident / Ambiguous-with-expected as correct; whether a
        # NoCorpus whose closest list holds the expected slug should count is an open
        # question for the user (SMOKE.md) — recorded, counted NOT ok.
        m = re.search(r"closest corpora:\s*([^;\n]+)", text)
        closest = [x.strip() for x in m.group(1).split(",")] if m else []
        return {"status": "no_corpus", "closest": closest,
                "closest_contains_expected": expected in closest, "ok": False}
    return {"status": f"no_answer rc={first['rc']}", "ok": False}


# --------------------------------------------------------------------------- grading
def grade_c1(task: dict, answer_text: str) -> dict:
    tf = RUN_DIR / "scratch" / "c1_task.json"; rf = RUN_DIR / "scratch" / "c1_run.json"
    tf.parent.mkdir(parents=True, exist_ok=True)
    tf.write_text(json.dumps(task)); rf.write_text(json.dumps({"result": answer_text}))
    out = subprocess.run([sys.executable, str(HERE / "check.py"), str(tf), str(rf)],
                         capture_output=True, text=True, check=True).stdout
    g = json.loads(out)
    return {"grader": "check.py", "status": "pass" if g["pass"] else "fail", **g}


JUDGE_PROMPT = """You are grading one answer against a fixed rubric. Use ONLY the gold answer and rubric as the authority.

Question:
{q}

Gold answer:
{gold}

Rubric (each point must be judged met or not met):
{rubric}

Pass rule: {pass_rule} (\"all\" = every rubric point met).

Answer to grade:
<<<
{answer}
>>>

Reply with ONLY a JSON object: {{"rubric_met": [true/false per rubric point, in order], "pass": true/false, "note": "<one sentence>"}}"""


def grade_judge(task: dict, answer_text: str) -> dict:
    """PLAN §6: separate inference, claude-opus-4-8, temperature 0; no key or any
    error => `unscored` (never pass, never fail)."""
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        return {"grader": f"judge:{JUDGE_MODEL}", "status": "unscored", "reason": "ANTHROPIC_API_KEY unset"}
    body = {"model": JUDGE_MODEL, "max_tokens": 600, "temperature": 0,
            "messages": [{"role": "user", "content": JUDGE_PROMPT.format(
                q=task["q"], gold=task["gold"], rubric="\n".join(f"{i+1}. {r}" for i, r in enumerate(task["rubric"])),
                pass_rule=task.get("pass_rule", "all"), answer=answer_text)}]}
    req = urllib.request.Request("https://api.anthropic.com/v1/messages", data=json.dumps(body).encode(),
                                 headers={"x-api-key": key, "anthropic-version": "2023-06-01",
                                          "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            resp = json.loads(r.read())
        text = "".join(c.get("text", "") for c in resp.get("content", []))
        j = json.loads(re.search(r"\{.*\}", text, re.S).group(0))
        met = [bool(x) for x in j["rubric_met"]]
        if len(met) != len(task["rubric"]):
            raise ValueError("rubric_met length mismatch")
        rule_pass = all(met) if task.get("pass_rule", "all") == "all" else bool(j["pass"])
        return {"grader": f"judge:{JUDGE_MODEL}", "status": "pass" if rule_pass else "fail",
                "rubric_met": met, "judge_pass_field": j.get("pass"), "note": j.get("note"),
                "judge_usage": resp.get("usage")}
    except (urllib.error.URLError, ValueError, KeyError, AttributeError, json.JSONDecodeError) as e:
        return {"grader": f"judge:{JUDGE_MODEL}", "status": "unscored", "reason": f"judge error: {e}"}


def split_session_answers(text: str, ids: list[str]) -> dict[str, str]:
    out = {}
    for i in ids:
        m = re.findall(rf"ANSWER\s*\[?{re.escape(i)}\]?\s*:\s*(.+)", text)
        out[i] = ("ANSWER: " + m[-1]) if m else ""
    return out


# --------------------------------------------------------------------------- contamination
GITHUB_SRC_RE = re.compile(r"(raw\.githubusercontent\.com|codeload\.github\.com|/archive/|/tarball/|/zipball/|"
                           r"github\.com/[^\s'\"]+/(blob|tree|raw)/)", re.I)
CLONE_RE = re.compile(r"\bgit\s+(clone|fetch|pull)\b")
ARCHIVE_RE = re.compile(r"(codeload\.github\.com|/archive/|/tarball/|/zipball/|\.tar\.gz|\.zip\b)", re.I)
URL_HOST_RE = re.compile(r"https?://([A-Za-z0-9.-]+)")


def contamination(arm: str, tm: dict, calls: list[dict], home: Path, expected_slug: str) -> dict:
    """Did the arm have ONLY its intended tools, and did it use the thing under test?
    Returns violations (hard: the cell is invalid) and notes (soft: reported)."""
    v, notes = [], []
    declared = [t for t in ARMS[arm]["tools"].split(",") if t]
    used = sorted({tu["name"] for tu in tm["tool_uses"]})
    if tm["init_tools"] is not None and sorted(tm["init_tools"]) != sorted(declared):
        v.append(f"init tools {tm['init_tools']} != declared {declared}")
    if tm["init_mcp"]:
        v.append(f"MCP servers present: {tm['init_mcp']}")
    extra = [u for u in used if u not in declared]
    if extra:
        v.append(f"used undeclared tools {extra}")
    cmds = "\n".join(bash_cmds(tm))
    kinds = [classify_call(c["argv"]) for c in calls]
    if arm == "A0" and tm["tool_uses"]:
        v.append("memory arm used tools")
    if arm in ("A0", "A1", "A2", "A3", "A4"):
        if calls or re.search(r"(^|[\s;&|(])xerj\b", cmds):
            v.append("non-XERJ arm invoked xerj")
    if arm != "A0" and not tm["tool_uses"]:
        notes.append("answered with no tool call at all (model recall, not the arm's tools)")
    elif arm == "A1" and not CLONE_RE.search(cmds):
        notes.append("git arm never cloned/fetched (answered without the repo)")
    # A3/A4 are defined by their ONE endpoint: using the shell for other fetches
    # without ever calling it makes the cell an A2 cell (seen in the smoke: an A4
    # agent went straight to raw.githubusercontent.com).
    if arm == "A3" and tm["tool_uses"] and "sourcegraph.com" not in cmds:
        v.append("A3 never called the code-search endpoint")
    if arm == "A4" and tm["tool_uses"]:
        if "duckduckgo.com" not in cmds:
            v.append("A4 never called the web-search API (fetched pages directly)")
        else:
            other = sorted({h for h in URL_HOST_RE.findall(cmds)} - {"html.duckduckgo.com", "duckduckgo.com"})
            if other:
                notes.append(f"A4 fetched pages from {other} (allowed only as search results; check order)")
    if arm in ("A2", "A3", "A4"):
        refused = home / ".pov/git-refused.log"
        if CLONE_RE.search(cmds) or (refused.exists() and refused.read_text().strip()):
            v.append("git clone/fetch attempted in a no-git arm")
        if ARCHIVE_RE.search(cmds):
            v.append("repository archive download in a no-git arm")
    if arm == "A3":
        hosts = {h for h in URL_HOST_RE.findall(cmds)} - {"sourcegraph.com"}
        if hosts:
            v.append(f"A3 fetched hosts other than the code-search endpoint: {sorted(hosts)}")
        if any(tu["name"] in ("WebFetch", "WebSearch") for tu in tm["tool_uses"]):
            v.append("A3 used web tools")
    if arm in XERJ_ARMS:
        if CLONE_RE.search(cmds):
            v.append("XERJ arm ran git clone/fetch itself")
        if GITHUB_SRC_RE.search(cmds):
            v.append("XERJ arm fetched source from GitHub directly")
    if arm == "A5":
        if any(k.startswith("v2-") for k in kinds):
            v.append("v1 arm used the v2 surface (ask / hub)")
        if "v1-corpus-add" not in kinds:
            notes.append("v1 arm never ran `xerj corpus add`")
        if not any(k.startswith("code") for k in kinds):
            notes.append("v1 arm never ran `xerj code`")
    if arm in V2_ARMS:
        if any(k.startswith("v1-corpus-add") or k.startswith("v1-corpus-index") for k in kinds):
            v.append("v2 arm used the v1 corpus add/index path")
        answers = [parse_answer_output(c) for c in calls if classify_call(c["argv"]) in ("v2-ask", "code-def", "code")]
        froms = [x["answered_from"] for x in answers if x["answered_from"]]
        if not any(k == "v2-ask" for k in kinds):
            v.append("v2 arm made no `xerj \"<q>\"`/`xerj ask` call (a pass would be model recall, PLAN §3)")
        elif "local_pack" not in froms:
            v.append(f"no v2 answer came from the local pack (answered_from seen: {froms})")
        before = (home / ".sandbox/packs_before.txt")
        before_l = before.read_text().split() if before.exists() else []
        if arm == "A6" and before_l:
            v.append(f"A6 was not cold: packs cached before the agent ran: {before_l}")
        if arm == "A7" and not any(expected_slug in x for x in before_l):
            v.append(f"A7 was not warm: {expected_slug} pack not cached before the agent ran ({before_l})")
    return {"violations": v, "notes": notes, "tools_used": used, "xerj_call_kinds": kinds,
            "n_bash": len(bash_cmds(tm))}


# --------------------------------------------------------------------------- score
def dir_bytes(p: Path) -> int:
    if not p.exists():
        return 0
    return sum(f.stat().st_size for f in p.rglob("*") if f.is_file() and not f.is_symlink())


def net_counters(home: Path) -> dict:
    f = home / ".sandbox/net.json"
    if not f.exists():
        return {}
    out = {}
    for item in load_json(f).get("nftables", []):
        c = item.get("counter")
        if c:
            out[c["name"]] = c["bytes"]
    return out


def score_cell(cdir: Path) -> dict | None:
    meta_f = cdir / "cell.json"
    if not meta_f.exists():
        return None
    meta = load_json(meta_f)
    home = cdir / "home"
    sb = home / ".sandbox"
    tm = transcript_metrics(read_events(sb / "transcript.jsonl"))
    calls = xerj_calls(home)
    tasks = [load_task(r, True) for r in meta["tasks"]]
    slug = tasks[0]["corpus"]
    timing = load_json(sb / "timing.json") if (sb / "timing.json").exists() else {}
    wall = round(timing["t1"] - timing["t0"], 2) if timing else None
    net = net_counters(home)
    model_bytes = net.get("rx_model_api", 0)
    rx_iface = timing.get("rx_iface_bytes")
    # bytes downloaded (PLAN §6): everything the jail received while the agent ran,
    # minus model-API traffic (no setup traffic precedes the agent except A7's pull,
    # which is reported separately and excluded by the interface delta).
    bytes_dl = max(0, rx_iface - model_bytes) if rx_iface is not None else None
    setup = None
    if (sb / "setup.time").exists():
        t1s, t0s = map(float, (sb / "setup.time").read_text().split())
        setup = {"rc": int((sb / "setup.rc").read_text().strip() or 255), "seconds": round(t1s - t0s, 2),
                 "rx_bytes": int((sb / "setup.rx").read_text().strip() or 0) if (sb / "setup.rx").exists() else None,
                 "log_tail": (sb / "setup.log").read_text(errors="replace")[-400:]}
    pack_bytes_cached = dir_bytes(home / ".xerj/hub/packs")
    n_fetch, n_failed = failed_fetches(tm)
    # grading
    if len(tasks) == 1:
        per_task = {tasks[0]["task"]["id"]: tm["final_text"]}
    else:
        per_task = split_session_answers(tm["final_text"], [t["task"]["id"] for t in tasks])
    grades = []
    for t in tasks:
        ans = per_task.get(t["task"]["id"], "")
        g = grade_c1(t["task"], ans) if t["class"] in ("C1",) or "re" in t["task"] else grade_judge(t["task"], ans)
        g["ref"] = t["ref"]
        grades.append(g)
    cont = contamination(meta["arm"], tm, calls, home, slug)
    route = routing(calls, slug) if meta["arm"] in V2_ARMS else None
    # H2: a mis-route counts the v2 answer wrong.
    for g in grades:
        g["status_raw"] = g["status"]
        if route is not None and not route["ok"] and g["status"] == "pass":
            g["status"] = "fail"; g["misroute_override"] = True
    statuses = [g["status"] for g in grades]
    correct = None if "unscored" in statuses else sum(s == "pass" for s in statuses) / len(statuses)
    row = {
        "schema": "hub-pov/score/1", "run_id": meta["run_id"], "arm": meta["arm"], "tasks": meta["tasks"],
        "repeat": meta["repeat"], "model": tm["model"], "model_requested": meta["model_requested"],
        "valid": not cont["violations"] and tm["n_result_events"] >= 1 and timing.get("rc") == 0,
        "correct": correct, "grades": grades,
        "cost_usd": tm["cost_usd"], "output_tokens": tm["output_tokens"], "input_tokens_all": tm["input_tokens_all"],
        "turns": tm["turns"], "n_result_events": tm["n_result_events"], "claude_rc": timing.get("rc"),
        "result_subtypes": tm["result_subtypes"],
        "wall_s": wall, "load_before": (sb / "load_before").read_text().split()[:3] if (sb / "load_before").exists() else None,
        "load_after": (sb / "load_after").read_text().split()[:3] if (sb / "load_after").exists() else None,
        "bytes_downloaded": bytes_dl, "bytes_rx_iface": rx_iface, "bytes_model_api": model_bytes,
        "bytes_rx_jail_total": net.get("rx_all"), "pack_bytes_cached": pack_bytes_cached,
        "fetches": n_fetch, "failed_fetches": n_failed,
        "xerj_calls": len(calls), "routing": route, "setup": setup,
        "answered_from": [x for x in (parse_answer_output(c)["answered_from"] for c in calls
                                      if classify_call(c["argv"]) in ("v2-ask", "code", "code-def")) if x],
        "contamination": cont, "final_answer_tail": tm["final_text"][-300:],
    }
    write_json(cdir / "score.json", row)
    return row


def cells_of(run_id: str) -> list[Path]:
    root = RUN_DIR / "results" / run_id
    return sorted(p.parent for p in root.rglob("cell.json"))


def cmd_score(a):
    rows = [r for r in (score_cell(c) for c in cells_of(a.run_id)) if r]
    out = RUN_DIR / "results" / a.run_id / "scores.jsonl"
    out.write_text("".join(json.dumps(r) + "\n" for r in rows))
    print(f"scored {len(rows)} cells -> {out}")


# --------------------------------------------------------------------------- report
def fmt(x, nd=3):
    if x is None:
        return "–"
    if isinstance(x, float):
        return f"{x:.{nd}f}"
    return str(x)


def cmd_report(a):
    rows = [json.loads(l) for l in (RUN_DIR / "results" / a.run_id / "scores.jsonl").read_text().splitlines()]
    hdr = ("| arm | task(s) | rep | valid | correct | grade | cost $ | out tok | turns | wall s | load1 | "
           "bytes dl | fetches (failed) | xerj calls | routing | answered_from | violations / notes |")
    print(hdr)
    print("|" + "---|" * (hdr.count("|") - 1))
    for r in sorted(rows, key=lambda r: (r["tasks"], r["arm"], r["repeat"])):
        g = ",".join(x["status"] + ("*" if x.get("misroute_override") else "") for x in r["grades"])
        froms = ",".join(sorted(set(r.get("answered_from") or []))) or "–"
        cont = r["contamination"]
        vio = "; ".join(cont["violations"] + [f"note: {n}" for n in cont["notes"]]) or "clean"
        route = r["routing"]["status"] + ("/ok" if r["routing"]["ok"] else "/WRONG") if r["routing"] else "–"
        print(f"| {r['arm']} | {' '.join(r['tasks'])} | {r['repeat']} | {'yes' if r['valid'] else 'NO'} | "
              f"{fmt(r['correct'], 2)} | {g} | {fmt(r['cost_usd'], 4)} | {r['output_tokens']} | {r['turns']} | "
              f"{fmt(r['wall_s'], 1)} | {(r['load_before'] or ['–'])[0]} | {fmt(r['bytes_downloaded'])} | "
              f"{r['fetches']} ({r['failed_fetches']}) | {r['xerj_calls']} | {route} | {froms} | {vio} |")
    gate(rows)


def gate(rows):
    """PLAN §7, checked only on a complete matrix with >= 3 repeats per cell."""
    reps = {}
    for r in rows:
        reps.setdefault((r["arm"], tuple(r["tasks"])), []).append(r)
    if not reps or min(len(v) for v in reps.values()) < 3:
        print("\n§7 gate: NOT EVALUATED — fewer than 3 repeats per cell (PLAN §6/§7). "
              "These numbers are not results.")
        return
    print("\n§7 gate: evaluation over a complete >=3-repeat matrix is implemented in "
          "gate_eval(); run it on the frozen full matrix only.")
    for (arm, tasks), v in sorted(reps.items()):
        for metric in ("cost_usd", "output_tokens"):
            xs = [x[metric] for x in v]
            m = statistics.mean(xs)
            cv = statistics.pstdev(xs) / m if m else 0
            if cv > 0.5:
                print(f"  CV>{0.5}: {arm} {tasks} {metric} cv={cv:.2f} -> needs +2 repeats (PLAN §6)")


# --------------------------------------------------------------------------- verify-c1
def cmd_verify_c1(a):
    src = load_json(TASKS / "C1/SOURCE.json")
    ok = True
    for rel, blob in src["files"].items():
        local = TASKS / "C1" / Path(rel).name if rel.startswith("tasks/") else HERE / Path(rel).name
        h = subprocess.run(["git", "hash-object", str(local)], capture_output=True, text=True).stdout.strip()
        try:
            remote = subprocess.run(["git", "-C", str(REPO), "rev-parse",
                                     f"origin/{src['source_branch']}:benchmarks/corpus-tasks/{rel}"],
                                    capture_output=True, text=True, check=True).stdout.strip()
        except subprocess.CalledProcessError:
            remote = "(branch not fetched)"
        good = h == blob == remote
        ok &= good
        print(f"{'ok ' if good else 'BAD'} {rel}: local={h[:12]} recorded={blob[:12]} branch={remote[:12]}")
    sys.exit(0 if ok else 1)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("prepare"); p.add_argument("--xerj", required=True)
    p.add_argument("--registry", required=True, help="the corpus-hub registry dir (tools/xerj-code/hub)")
    p.add_argument("--registry-commit", default=None, help="the corpus-hub commit the registry came from")
    p.add_argument("--tags", default="pack-lz4-0774d055,pack-snappy-9c28114a,pack-zstd-01b7154f")
    p.set_defaults(fn=cmd_prepare)
    p = sub.add_parser("cell"); p.add_argument("--arm", required=True)
    p.add_argument("--task", required=True, action="append", help="<class>:<suite>:<id>; repeat for a session")
    p.add_argument("--repeat", type=int, default=1); p.add_argument("--run-id", required=True)
    p.add_argument("--model", required=True); p.add_argument("--allow-smoke-fixtures", action="store_true")
    p.add_argument("--force", action="store_true"); p.set_defaults(fn=cmd_cell)
    p = sub.add_parser("matrix"); p.add_argument("matrix"); p.add_argument("--run-id", required=True)
    p.add_argument("--force", action="store_true"); p.set_defaults(fn=cmd_matrix)
    p = sub.add_parser("score"); p.add_argument("--run-id", required=True); p.set_defaults(fn=cmd_score)
    p = sub.add_parser("report"); p.add_argument("--run-id", required=True); p.set_defaults(fn=cmd_report)
    p = sub.add_parser("verify-c1"); p.set_defaults(fn=cmd_verify_c1)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
