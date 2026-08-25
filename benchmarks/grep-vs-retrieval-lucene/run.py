#!/usr/bin/env python3
"""Context-ceiling benchmark: native grep vs XERJ retrieval vs bare, across corpus scales.

  python3 run.py --arms native,xerj,bare --scales S1,S2,S3,S4 --reps 3

Cells are (arm x question x scale x rep). Each cell is one `claude -p` process
with its own --session-id so its transcript can be located and attributed.
"""
import argparse, json, os, re, subprocess, sys, time, uuid, shutil, hashlib

BENCH = os.path.dirname(os.path.abspath(__file__))
SCALES = os.path.join(BENCH, "scales")
RUNS = os.path.join(BENCH, "runs")
CLAUDE = "/home/claude/.local/bin/claude"
EMPTY = os.path.join(BENCH, "empty_cwd")

ANSWER_SCHEMA = json.dumps({
    "type": "object",
    "properties": {
        "answer": {"type": "string"},
        "evidence_path": {"type": "string"},
        "evidence_line": {"type": "integer"},
        "confidence": {"type": "string", "enum": ["high", "medium", "low", "unknown"]},
    },
    "required": ["answer", "evidence_path", "confidence"],
})

# Identical for every arm. The only difference between arms is what is on disk /
# what is prepended. No arm is told which files matter.
SYSTEM = (
    "You are answering a precise question about the Apache Lucene source code. "
    "Answer ONLY from the code you can actually see. "
    "If you cannot find the answer in the material available to you, set "
    "confidence to \"unknown\" and say so in `answer` rather than recalling it "
    "from general knowledge. A confident wrong answer is worse than \"unknown\"."
)

def transcript_dir(cwd):
    return os.path.join("/home/claude/.claude/projects",
                        "-" + cwd.strip("/").replace("/", "-").replace("_", "-"))

def find_transcript(sid):
    base = "/home/claude/.claude/projects"
    for d in os.listdir(base):
        p = os.path.join(base, d, sid + ".jsonl")
        if os.path.exists(p):
            return p
    return None

def build_prompt(arm, q, scale):
    if arm == "xerj":
        paths = json.load(open(os.path.join(SCALES, "manifest.json")))["scales"][scale]["files"]
        pf = os.path.join(RUNS, "_paths_%s.json" % scale)
        if not os.path.exists(pf):
            json.dump(paths, open(pf, "w"))
        snip = subprocess.run([sys.executable, os.path.join(BENCH, "retrieve.py"),
                               q["question"], pf], capture_output=True, text=True).stdout
        return ("Reference material retrieved from a XERJ index of the Lucene source.\n"
                "This is all the source you have; there is no repository on disk.\n\n"
                "```java\n" + snip + "\n```\n\nQuestion: " + q["question"])
    if arm == "bare":
        return ("You have no repository and no tools. Question: " + q["question"])
    return ("The Apache Lucene source tree is in the current directory. "
            "Question: " + q["question"])

def run_cell(arm, q, scale, rep):
    sid = str(uuid.UUID(hashlib.md5(("%s|%s|%s|%d" % (arm, q["id"], scale, rep))
                                    .encode()).hexdigest()))
    if arm == "native":
        cwd = os.path.join(SCALES, scale)
        tools = ["--tools", "Bash,Read,Grep,Glob"]
    elif arm == "xerj":
        os.makedirs(EMPTY, exist_ok=True)
        cwd = EMPTY
        tools = ["--tools", "Bash,Read,Grep,Glob"]     # same agent as native, empty disk
    else:
        os.makedirs(EMPTY, exist_ok=True)
        cwd = EMPTY
        tools = ["--disallowedTools", "Bash,Read,Grep,Glob,WebSearch,WebFetch,Task"]

    cmd = [CLAUDE, "-p", build_prompt(arm, q, scale),
           "--output-format", "json",
           "--safe-mode",                       # no CLAUDE.md, skills, hooks, MCP
           "--permission-mode", "bypassPermissions",
           "--system-prompt", SYSTEM,
           "--json-schema", ANSWER_SCHEMA,
           "--session-id", sid,
           "--max-budget-usd", "1.50"] + tools

    t0 = time.time()
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=1800)
    wall = time.time() - t0
    try:
        res = json.loads(p.stdout)
    except Exception:
        res = {"is_error": True, "raw": p.stdout[-4000:], "stderr": p.stderr[-2000:]}
    tr = find_transcript(sid)
    attr = None
    if tr:
        sys.path.insert(0, BENCH)
        import attribute
        attr = attribute.analyze(tr)
    return {"arm": arm, "qid": q["id"], "qclass": q["class"], "scale": scale, "rep": rep,
            "session_id": sid, "transcript": tr, "wall_s": round(wall, 1),
            "result": res.get("result"), "is_error": res.get("is_error"),
            "num_turns": res.get("num_turns"),
            "total_cost_usd": res.get("total_cost_usd"),
            "usage": res.get("usage"), "modelUsage": res.get("modelUsage"),
            "attribution": attr}

def grade(cell, q):
    """Regex grading over the model's own `answer` field. No LLM judge."""
    raw = cell.get("result") or ""
    try:
        obj = json.loads(raw)
        txt = " ".join(str(obj.get(k, "")) for k in ("answer", "evidence_path"))
        conf = obj.get("confidence")
    except Exception:
        txt, conf = raw, None
    correct = bool(re.search(q["correct"], txt, re.I | re.S))
    guessed = bool(q.get("guess") and re.search(q["guess"], txt, re.I | re.S))
    abstained = (conf == "unknown")
    return {"correct": correct and not guessed, "confabulated": guessed and not correct,
            "abstained": abstained, "confidence": conf,
            # the metric that matters for a security audience:
            "confidently_wrong": (not correct) and conf in ("high", "medium")}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--arms", default="native,xerj,bare")
    ap.add_argument("--scales", default="S1,S2,S3,S4")
    ap.add_argument("--questions", default="")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--out", default=os.path.join(RUNS, "results.jsonl"))
    a = ap.parse_args()
    os.makedirs(RUNS, exist_ok=True)
    qs = json.load(open(os.path.join(BENCH, "questions.json")))["questions"]
    if a.questions:
        keep = set(a.questions.split(","))
        qs = [q for q in qs if q["id"] in keep]
    fh = open(a.out, "a")
    for rep in range(a.reps):
        for scale in a.scales.split(","):
            for arm in a.arms.split(","):
                # bare has no corpus: run it once, at S1 only, and reuse
                if arm == "bare" and scale != a.scales.split(",")[0]:
                    continue
                for q in qs:
                    cell = run_cell(arm, q, scale, rep)
                    cell["grade"] = grade(cell, q)
                    fh.write(json.dumps(cell) + "\n"); fh.flush()
                    g = cell["grade"]
                    at = cell.get("attribution") or {}
                    print("%-6s %-3s %-3s r%d  correct=%-5s conf=%-7s search_share=%-6s cost=%.4f"
                          % (arm, q["id"], scale, rep, g["correct"], g["confidence"],
                             at.get("share_of_context"), cell.get("total_cost_usd") or 0))
    fh.close()

if __name__ == "__main__":
    main()
