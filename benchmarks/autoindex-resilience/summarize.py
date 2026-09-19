#!/usr/bin/env python3
"""Summarize an `xerj autoindex --progress plain` stderr capture.

Every number in this folder's README — and in the docs pages that cite it — is
produced by this script from a raw capture committed beside it, so none of them
is hand-copied:

    python3 summarize.py > results.json   # the four complete captures in CAPTURES
    python3 summarize.py --check          # results.json is what the captures produce

A capture is summarized as: the ordered list of phases with how many
`xerj-progress` lines each emitted, the time spent in each, how the last line of
each phase read (pct / eta_quality / since_progress_s), and the terminal
`xerj-done` line parsed into fields. Standard library only.
"""
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
# Complete, untrimmed captures only: a trimmed one would under-count its phases.
CAPTURES = [
    "before-rc74.stderr.txt",
    "slice-rc74.stderr.txt",
    "slice-after.stderr.txt",
    "after-955.full-corpus-resume.stderr.txt",
]


def fields(line):
    out = {}
    for pair in line.split()[1:]:
        key, sep, value = pair.partition("=")
        if sep:
            out[key] = value
    return out


def number(value):
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def summarize(path):
    phases = []
    done = None
    refused = []
    error = None
    with open(path, encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            line = raw.rstrip("\n")
            if line.startswith("xerj-progress "):
                f = fields(line)
                if not phases or phases[-1]["phase"] != f.get("phase"):
                    phases.append({"phase": f.get("phase"), "lines": 0, "first": f, "last": f})
                phases[-1]["lines"] += 1
                phases[-1]["last"] = f
            elif line.startswith("xerj-done "):
                done = fields(line)
            elif "REFUSED by the server" in line:
                refused.append(line.strip())
            elif line.startswith("error:"):
                error = line
    out_phases = []
    for p in phases:
        last = p["last"]
        out_phases.append({
            "phase": p["phase"],
            "progress_lines": p["lines"],
            "items": last.get("items"),
            "bytes": last.get("bytes"),
            "basis": last.get("basis"),
            "last_pct": last.get("pct"),
            "last_eta_quality": last.get("eta_quality"),
            "last_since_progress_s": number(last.get("since_progress_s")),
            "phase_elapsed_s": number(last.get("phase_elapsed_s")),
        })
    # The symptom of #931, measured rather than asserted: progress lines that
    # claim a finished scan (pct=100.0) while calling the run stalled.
    stalled_scan = 0
    longest_scan_silence = 0.0
    with open(path, encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            if not raw.startswith("xerj-progress "):
                continue
            f = fields(raw)
            if f.get("phase") == "scan" and f.get("pct") == "100.0" and f.get("eta_quality") == "stalled":
                stalled_scan += 1
                longest_scan_silence = max(longest_scan_silence, number(f.get("since_progress_s")) or 0.0)
    return {
        "capture": os.path.basename(path),
        "phases_in_order": [p["phase"] for p in out_phases],
        "phases": out_phases,
        "scan_lines_at_100pct_reading_stalled": stalled_scan,
        "longest_since_progress_s_while_scan_read_100pct": longest_scan_silence,
        "refusals_announced": len(refused),
        "done": done,
        "error_line_head": (error[:240] if error else None),
    }


def build(paths):
    return {"captures": [summarize(p) for p in paths]}


def main(argv):
    if argv[1:] == ["--check"]:
        expected = build([os.path.join(HERE, name) for name in CAPTURES])
        with open(os.path.join(HERE, "results.json"), encoding="utf-8") as handle:
            committed = json.load(handle)
        if committed != expected:
            print("results.json is stale: re-run summarize.py", file=sys.stderr)
            return 1
        print("results.json matches the committed captures")
        return 0
    paths = argv[1:] or [os.path.join(HERE, name) for name in CAPTURES]
    json.dump(build(paths), sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
