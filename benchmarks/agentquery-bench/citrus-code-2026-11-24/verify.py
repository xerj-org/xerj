"""Analyze completed search trials without conflating missing usage and failed answers."""
from collections import Counter, defaultdict
import json
from pathlib import Path
import random
import re
from statistics import median

ROOT = Path(__file__).resolve().parent
ARMS = ("baseline", "free_choice", "guided")
KINDS = ("definition", "behavior", "literal")


def grade(answer, expected):
    def matches(parsed):
        if not isinstance(parsed, dict) or parsed.keys() != expected.keys():
            return False
        for key, value in expected.items():
            observed = parsed[key]
            if key == "locations":
                if (not isinstance(observed, list) or any(not isinstance(x, str) for x in observed)
                        or sorted(observed) != value):
                    return False
            elif isinstance(value, (int, float)) and not isinstance(value, bool):
                if type(observed) not in (int, float) or observed != value:
                    return False
            elif type(observed) is not type(value) or observed != value:
                return False
        return True

    def parse(text):
        try:
            return json.loads(text)
        except ValueError:
            return None

    body = answer.strip()
    fence = chr(96) * 3
    fenced = re.findall(re.escape(fence) + r"(?:json)?[ \t]*\r?\n(.*?)" + re.escape(fence),
                        body, flags=re.DOTALL | re.IGNORECASE)
    whole = re.fullmatch(re.escape(fence) + r"(?:json)?[ \t]*\r?\n(.*?)" + re.escape(fence),
                         body, flags=re.DOTALL | re.IGNORECASE)
    strict = matches(parse(whole[1] if whole else body))
    return strict, strict or (len(fenced) == 1 and matches(parse(fenced[0])))


def tokens(row):
    return row["input_tokens"] + row["output_tokens"]


def summarize(rows):
    known = [row for row in rows if row["complete_usage"]]
    known_correct = sum(row["content_correct"] for row in known)
    return {
        "runs": len(rows), "content_correct": sum(row["content_correct"] for row in rows),
        "structured_correct": sum(row["strict_correct"] for row in rows),
        "complete_usage": len(known), "correct_with_complete_usage": known_correct,
        "median_tokens": median(map(tokens, known)) if known else None,
        "tokens_per_correct": sum(map(tokens, known)) / known_correct if known_correct else None,
        "input_tokens": sum(row["input_tokens"] for row in known),
        "output_tokens": sum(row["output_tokens"] for row in known),
        "cache_read_tokens": sum(row["cache_read_tokens"] for row in known),
        "total_turns": sum(row["turns"] for row in rows),
        "median_turns": median(row["turns"] for row in rows),
        "median_wall_seconds": median(row["seconds"] for row in rows),
        "retry_events": sum(row["provider_retries"] for row in rows),
        "end_reasons": dict(Counter(row["end"] or row["failure"] or "unknown" for row in rows)),
        "runs_using_xerj": sum(any(tool.startswith("xerj_") for tool in row["tool_order"]) for row in rows),
    }


def contrast(rows, control, treatment):
    grouped = defaultdict(dict)
    for row in rows:
        grouped[(row["corpus"], row["case"], row["repeat"])][row["arm"]] = row
    by_question = defaultdict(list)
    for (corpus, case, _), arms in grouped.items():
        a, b = arms[control], arms[treatment]
        if all(row["content_correct"] and row["complete_usage"] for row in (a, b)):
            by_question[(corpus, case)].append((tokens(a), tokens(b)))
    pairs = [pair for group in by_question.values() for pair in group]
    left = sum(a for a, _ in pairs)
    right = sum(b for _, b in pairs)
    interval = None
    if len(by_question) > 1 and left:
        rng = random.Random(1787)
        clusters = [(sum(a for a, _ in group), sum(b for _, b in group))
                    for group in by_question.values()]
        estimates = []
        for _ in range(5000):
            sample = rng.choices(clusters, k=len(clusters))
            denominator = sum(a for a, _ in sample)
            if denominator:
                estimates.append(sum(b for _, b in sample) / denominator - 1)
        estimates.sort()
        interval = [estimates[int((len(estimates) - 1) * q)] for q in (0.025, 0.975)]
    return {
        "control": control, "treatment": treatment, "pairs": len(pairs), "questions": len(by_question),
        "control_tokens": left, "treatment_tokens": right, "delta": right / left - 1 if left else None,
        "median_pair_delta": median(b / a - 1 for a, b in pairs if a) if left else None,
        "lower": sum(b < a for a, b in pairs), "tied": sum(b == a for a, b in pairs),
        "higher": sum(b > a for a, b in pairs), "cluster_bootstrap_95": interval,
    }


def sections(rows):
    return {arm: summarize([row for row in rows if row["arm"] == arm]) for arm in ARMS}


def compare(rows):
    return {arm: contrast(rows, "baseline", arm) for arm in ARMS[1:]}


def analyze(run):
    manifest = json.loads((run / "manifest.json").read_text(encoding="utf-8"))
    rows = [json.loads(line) for line in (run / "runs.jsonl").read_text(encoding="utf-8").splitlines()]
    expected = {(job["corpus"], job["case"], job["repeat"], arm)
                for job in manifest["jobs"] for arm in ARMS}
    actual = {(row["corpus"], row["case"], row["repeat"], row["arm"]) for row in rows}
    if expected != actual or len(actual) != len(rows):
        raise ValueError("Results differ from the frozen run matrix.")

    cases = {(item["corpus"], item["id"]): item for item in manifest["cases"]}
    order = {(job["corpus"], job["case"], job["repeat"], arm): position
             for job in manifest["jobs"] for position, arm in enumerate(job["order"], 1)}
    for row in rows:
        case = cases[row["corpus"], row["case"]]
        identity = row["corpus"], row["case"], row["repeat"], row["arm"]
        if row["kind"] != case["kind"] or row["arm_order"] != order[identity]:
            raise ValueError(f"Run metadata differs from the manifest for {identity}")
        strict, content = grade(row["answer"], case["expected"])
        if (row["strict_correct"], row["content_correct"]) != (
            row["end"] == "stop" and strict, row["end"] == "stop" and content,
        ):
            raise ValueError(f"Recorded grade differs from the source-derived answer for {identity}")
        known = [event for event in row["usage"] if event is not None]
        complete = bool(row["turns"] and len(row["usage"]) == len(known) == row["turns"]
                        and row["provider_retries"] == 0)
        if complete != row["complete_usage"]:
            raise ValueError(f"Usage completeness differs from the recorded events for {identity}")
        for key in ("input_tokens", "output_tokens", "cache_read_tokens"):
            if row[key] != sum(event[key] or 0 for event in known):
                raise ValueError(f"Recorded {key} differs from the usage events for {identity}")

    data = {
        "arms": sections(rows), "paired": compare(rows),
        "guided_vs_free": contrast(rows, "free_choice", "guided"),
        "by_kind": {kind: {"arms": sections(subset), "paired": compare(subset)}
                    for kind in KINDS if (subset := [row for row in rows if row["kind"] == kind])},
        "by_corpus": {name: {"arms": sections(subset), "paired": compare(subset)}
                      for name in manifest["corpus_commits"]
                      if (subset := [row for row in rows if row["corpus"] == name])},
        "routing": {kind: {
            arm: dict(Counter(row["first_search_tool"] or "none"
                              for row in rows if row["kind"] == kind and row["arm"] == arm))
            for arm in ARMS
        } for kind in KINDS},
    }
    return data


if __name__ == "__main__":
    print(json.dumps(analyze(ROOT), indent=2))
