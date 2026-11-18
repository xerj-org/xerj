#!/usr/bin/env python3
"""
SWE-bench Verified — generate model patches for the `native` and `xerj` arms.

This is the GENERATION half of a defensible SWE-bench run. It does NOT judge
correctness itself: it writes standard SWE-bench prediction files, and you score
them with the OFFICIAL `swebench.harness.run_evaluation` (Docker) — so the number
is the real leaderboard number, not a home-grown verdict. See README.md.

Two arms, identical agent, one difference:
  native : the repo is checked out; the agent must FIND the code to change by grepping.
  xerj   : same, but XERJ has indexed the repo and PRE-RETRIEVED the relevant
           passages, injected into the prompt. (This is the reference-coding arm.)
(There is deliberately no `bare` arm: patching a real repo from pure memory is
not a meaningful task.)

Output: one predictions JSONL per (arm, model), lines of
  {"instance_id", "model_name_or_path", "model_patch"}

Requires: `datasets`, the `claude` CLI (authenticated), a running XERJ
(`XERJ_URL`, default http://localhost:9200) with its binary on `XERJ_BIN`, git.
Model is selected per run via ANTHROPIC_MODEL (verified from claude's JSON
`modelUsage`).
"""
import argparse, json, os, subprocess, tempfile, shutil, sys, time, pathlib

XERJ_URL = os.environ.get("XERJ_URL", "http://localhost:9200")
XERJ_BIN = os.environ.get("XERJ_BIN", "xerj")


def sh(cmd, cwd=None, timeout=None, env=None):
    return subprocess.run(cmd, cwd=cwd, timeout=timeout, capture_output=True,
                          text=True, env={**os.environ, **(env or {})})


def clone_at(repo, base_commit, dest):
    sh(["git", "clone", "--quiet", f"https://github.com/{repo}", dest]).check_returncode()
    sh(["git", "checkout", "--quiet", base_commit], cwd=dest).check_returncode()


def retrieve_context(repo_dir, instance_id, problem, k=6, chars=8000):
    """xerj arm: index the repo, retrieve passages relevant to the issue."""
    prefix = "swe-" + instance_id.replace("/", "-")
    idx = sh([XERJ_BIN, "autoindex", repo_dir, "--url", XERJ_URL,
              "--prefix", prefix, "--no-graph"], timeout=1800)
    # exit 3 == completed-with-junk == success
    if idx.returncode not in (0, 3):
        return None, f"index-failed:{idx.returncode}:{idx.stderr[:160]}"
    # first ~3 lines of the issue make the best lexical query on the default embedder
    query = " ".join(problem.split("\n")[:3])[:300]
    # `xerj code` (issue #977): same retrieval the xc.py wrapper used to do.
    # exit 1 == no match == no context injected; only 0 carries passages.
    r = sh([XERJ_BIN, "code", prefix, query, "-k", str(k),
            "--full", str(chars)], env={"XERJ_URL": XERJ_URL})
    if r.returncode != 0:
        return None, f"retrieve-failed:{r.returncode}:{r.stderr[:160]}"
    return r.stdout, None


def run_agent(repo_dir, problem, ref_block, timeout, max_turns):
    base = sh(["git", "rev-parse", "HEAD"], cwd=repo_dir)
    base.check_returncode()
    ref = ""
    if ref_block:
        ref = ("\n\nXERJ retrieved these passages from THIS repository — the code you "
               "most likely need to change is here (cite file:line):\n\n" + ref_block)
    prompt = (
        "You are fixing a bug in the checked-out repository at the current working "
        "directory. Here is the issue:\n\n" + problem +
        "\n\nEdit the source files to fix it. Do NOT edit or add tests — a hidden "
        "suite will judge your patch. Do not run the test suite. Make the smallest "
        "correct change. When done, stop." + ref)
    t0 = time.time()
    p = subprocess.run(
        ["claude", "-p", prompt, "--output-format", "json",
         "--permission-mode", "bypassPermissions", "--max-turns", str(max_turns)],
        cwd=repo_dir, capture_output=True, text=True, timeout=timeout)
    el = time.time() - t0
    usage, model = {}, None
    try:
        d = json.loads(p.stdout)
        u = d.get("usage", {})
        usage = {"in": u.get("input_tokens", 0), "out": u.get("output_tokens", 0),
                 "cost": d.get("total_cost_usd", 0.0)}
        model = list(d.get("modelUsage", {}).keys())
    except Exception:
        pass
    # This is a disposable task clone. Include new files without ignoring
    # staged edits or commits the agent made relative to the pinned base.
    sh(["git", "add", "--intent-to-add", "--all"], cwd=repo_dir).check_returncode()
    diff = sh(["git", "diff", "--binary", base.stdout.strip()], cwd=repo_dir)
    diff.check_returncode()
    return diff.stdout, usage, model, el


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arm", choices=["native", "xerj"], required=True)
    ap.add_argument("--dataset", default="princeton-nlp/SWE-bench_Verified")
    ap.add_argument("--repos", nargs="*", help="restrict to these repos (e.g. psf/requests)")
    ap.add_argument("--limit", type=int, default=0, help="max instances (0=all)")
    ap.add_argument("--instances", nargs="*", help="explicit instance_ids")
    ap.add_argument("--max-turns", type=int, default=60)
    ap.add_argument("--timeout", type=int, default=1200)
    ap.add_argument("--out", required=True)
    ap.add_argument("--tokens-out", help="optional per-instance token/cost log (JSONL)")
    args = ap.parse_args()

    from datasets import load_dataset
    ds = load_dataset(args.dataset, split="test")
    rows = list(ds)
    if args.repos:
        rows = [r for r in rows if r["repo"] in set(args.repos)]
    if args.instances:
        rows = [r for r in rows if r["instance_id"] in set(args.instances)]
    if args.limit:
        rows = rows[:args.limit]
    model_name = os.environ.get("ANTHROPIC_MODEL", "default") + f"+xerj-{args.arm}"
    print(f"generating {len(rows)} predictions | arm={args.arm} | model_env="
          f"{os.environ.get('ANTHROPIC_MODEL','default')}", file=sys.stderr)

    outf = open(args.out, "w")
    tlog = open(args.tokens_out, "w") if args.tokens_out else None
    for i, r in enumerate(rows, 1):
        iid = r["instance_id"]
        work = tempfile.mkdtemp(prefix="swe-")
        repo_dir = os.path.join(work, "repo")
        try:
            clone_at(r["repo"], r["base_commit"], repo_dir)
            ref, err = (None, None)
            if args.arm == "xerj":
                ref, err = retrieve_context(repo_dir, iid, r["problem_statement"])
                if err:
                    print(f"  [{i}/{len(rows)}] {iid} retrieve error: {err}", file=sys.stderr)
            diff, usage, model, el = run_agent(
                repo_dir, r["problem_statement"], ref, args.timeout, args.max_turns)
            outf.write(json.dumps({"instance_id": iid,
                                   "model_name_or_path": model_name,
                                   "model_patch": diff}) + "\n")
            outf.flush()
            if tlog:
                tlog.write(json.dumps({"instance_id": iid, "arm": args.arm,
                                       "model": model, "usage": usage,
                                       "elapsed_s": round(el, 1),
                                       "patch_bytes": len(diff)}) + "\n")
                tlog.flush()
            print(f"  [{i}/{len(rows)}] {iid} {model} out={usage.get('out')} "
                  f"patch={len(diff)}B {el:.0f}s", file=sys.stderr)
        except subprocess.TimeoutExpired:
            print(f"  [{i}/{len(rows)}] {iid} TIMEOUT", file=sys.stderr)
            outf.write(json.dumps({"instance_id": iid,
                                   "model_name_or_path": model_name,
                                   "model_patch": ""}) + "\n")
        except Exception as e:
            print(f"  [{i}/{len(rows)}] {iid} ERROR {e}", file=sys.stderr)
        finally:
            shutil.rmtree(work, ignore_errors=True)
    outf.close()
    if tlog:
        tlog.close()
    print(f"wrote {args.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
