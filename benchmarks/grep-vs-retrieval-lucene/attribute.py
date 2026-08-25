#!/usr/bin/env python3
"""Per-tool token attribution from the Claude Code transcript.

WHY NOT THE RESULT JSON: `claude -p --output-format json` returns ONLY a
session-level aggregate. Verified on this machine: a 2-turn run reported
num_turns=2 but usage.iterations had length 1, and iterations[0].cache_creation
(7907) != usage.cache_creation_input_tokens (13341). The iterations array is
INCOMPLETE and must not be used for attribution. There is NO per-tool field.

WHAT WE USE INSTEAD: the transcript JSONL, where every assistant row carries its
own usage and every user row carries the tool_result content. Between API call i
and call i+1 the only things added to the context are (a) call i's own output and
(b) the tool_results for call i's tool_uses. So:

    prompt_i        = input_tokens + cache_creation_input_tokens + cache_read_input_tokens
    ingested_i      = prompt_{i+1} - prompt_i - output_tokens_i

`ingested_i` is the provider's own billing meter measuring the tool output, so it
needs no tokenizer and no estimate. If call i issued several tools, ingested_i is
split across them by tool_result character share (the only approximation here; it
is reported, and cells where a single call issued >1 tool are flagged).
"""
import json, sys, collections

SEARCH_TOOLS = {"Grep", "Glob", "Read", "NotebookRead"}
SEARCH_BASH = ("grep", "rg", "sed", "awk", "cat", "head", "tail", "find", "ls", "wc")

def bash_is_search(inp):
    cmd = (inp or {}).get("command", "")
    first = cmd.strip().split()[0] if cmd.strip() else ""
    return any(first.endswith(b) for b in SEARCH_BASH) or any(
        (" %s " % b) in cmd or ("|%s " % b) in cmd or ("| %s " % b) in cmd for b in SEARCH_BASH)

def load(path):
    return [json.loads(l) for l in open(path) if l.strip()]

def analyze(path):
    rows = load(path)
    calls = []          # one per assistant API call
    pending = None
    for r in rows:
        m = r.get("message") or {}
        if r.get("type") == "assistant" and m.get("usage"):
            u = m["usage"]
            prompt = (u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0)
                      + u.get("cache_read_input_tokens", 0))
            tools = []
            for c in (m.get("content") or []):
                if isinstance(c, dict) and c.get("type") == "tool_use":
                    tools.append({"name": c.get("name"), "input": c.get("input"),
                                  "id": c.get("id")})
            # consecutive assistant rows can share one usage object (thinking + tool_use
            # arrive as separate rows from the SAME API call); merge on identical usage
            if calls and calls[-1]["usage_sig"] == json.dumps(u, sort_keys=True):
                calls[-1]["tools"].extend(tools)
                continue
            calls.append({"prompt": prompt, "out": u.get("output_tokens", 0),
                          "tools": tools, "usage_sig": json.dumps(u, sort_keys=True),
                          "results": {}})
        elif r.get("type") == "user":
            cont = m.get("content") or []
            if not isinstance(cont, list):
                cont = []
            for c in cont:
                if isinstance(c, dict) and c.get("type") == "tool_result" and calls:
                    calls[-1]["results"][c.get("tool_use_id")] = len(json.dumps(c.get("content")))

    per_tool = collections.Counter()
    multi_tool_calls = 0
    for i in range(len(calls) - 1):
        ingested = calls[i + 1]["prompt"] - calls[i]["prompt"] - calls[i]["out"]
        if ingested <= 0:
            continue
        tools = calls[i]["tools"]
        if not tools:
            per_tool["_nontool"] += ingested
            continue
        if len(tools) > 1:
            multi_tool_calls += 1
        chars = {t["id"]: max(1, calls[i]["results"].get(t["id"], 1)) for t in tools}
        tot = sum(chars.values())
        for t in tools:
            share = ingested * chars[t["id"]] / tot
            name = t["name"]
            if name == "Bash":
                name = "Bash(search)" if bash_is_search(t["input"]) else "Bash(other)"
            per_tool[name] += share

    search_tokens = sum(v for k, v in per_tool.items()
                        if k in SEARCH_TOOLS or k == "Bash(search)")
    final_prompt = calls[-1]["prompt"] if calls else 0
    billed_prompt = sum(c["prompt"] for c in calls)
    return {
        "n_api_calls": len(calls),
        "per_tool_tokens": {k: round(v) for k, v in per_tool.items()},
        "search_tool_tokens": round(search_tokens),
        "final_context_tokens": final_prompt,
        "billed_prompt_tokens": billed_prompt,
        "output_tokens": sum(c["out"] for c in calls),
        # (A) headline: fraction of the agent's final context that is search output
        "share_of_context": round(search_tokens / final_prompt, 4) if final_prompt else None,
        # (B) cost view: search tokens re-charged on every later call
        "share_of_billed": round(
            sum(min(search_tokens, c["prompt"]) for c in calls) / billed_prompt, 4)
            if billed_prompt else None,
        "multi_tool_calls_approximated": multi_tool_calls,
    }

if __name__ == "__main__":
    print(json.dumps(analyze(sys.argv[1]), indent=1))
