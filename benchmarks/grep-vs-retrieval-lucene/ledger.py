#!/usr/bin/env python3
"""
ledger.py -- turn one `claude -p --output-format stream-json --verbose` transcript
into an auditable token ledger.

WHY THIS FILE EXISTS
--------------------
`claude -p --output-format json` gives you TOTALS only. It has NO per-tool token
field. There is no "grep cost you N tokens" anywhere in the payload. So the claim
"agents burn X% of tokens on grep/read" cannot be read off the JSON -- it has to be
RECONSTRUCTED. This does that reconstruction, and refuses to emit numbers it cannot
reconcile against the CLI's own billing totals.

METHOD
------
Per-API-call usage is recovered from the stream's `assistant` events, deduped by
message.id (each id emits multiple events during streaming; we keep the max of each
counter, which is the final value for that message).

Billed input for call i = input_tokens + cache_creation_input_tokens + cache_read_input_tokens.

Tool-result attribution is CHARACTER-BASED against a constant measured on this exact
corpus by calibrate.sh: two prompts differing by 20,000 chars of real Lucene source
differed by 7,181 billed tokens => 2.785 chars/token. (The usual chars/4 rule of thumb
understates Java source by ~30%; using it would have understated grep cost.)

    tool_tokens(i) = len(tool_result_i) / CHARS_PER_TOKEN

An alternative estimator -- the cache_creation delta between consecutive calls -- was
tried and REJECTED as the primary: with extended thinking on, that delta is dominated
by re-sent thinking-block signature blobs, not by tool output (it read 1613 tokens for
819 chars of grep output, ~5x too high). It is retained only as a loose UPPER BOUND.

HONESTY BOUNDARY -- READ THIS BEFORE QUOTING ANY PERCENTAGE
-----------------------------------------------------------
The arm-vs-arm TOTALS (tokens, dollars) are EXACT: they come straight from the CLI's
billing counters and reconcile to the token. The per-tool SHARE is an ESTIMATE built
on the calibration above. So the headline claim must be the exact one
("N x fewer tokens for the same answer"), and any "% spent on grep" must be quoted as
an estimate with its method stated. Do not lead a public post with the estimate.

TWO DIFFERENT SHARES ARE REPORTED, BECAUSE THEY ANSWER DIFFERENT QUESTIONS
-------------------------------------------------------------------------
unique_share : tool tokens / total unique context tokens.
               "Of everything the agent read, how much was grep/read output?"
billed_share : sum over calls of (tool tokens resident in that call's context)
               / total billed input tokens.
               "Of what you actually PAID for, how much was grep/read output?"
               Higher than unique_share, because a tool result pulled in at turn 2 is
               re-billed (at cache-read rates) on every later call.
dollar_share : same as billed_share but weighted by the real price of each token
               class, since cache reads are ~10% the price of fresh input.

The user's "40%" is a billed_share/dollar_share style claim. Report which one you mean.
"""

import json
import sys
import os

# Measured on this corpus by calibrate.sh (2026-08-18): 20000 chars -> 7181 tokens.
CHARS_PER_TOKEN = 2.785

# Price model DERIVED (not assumed) by solving the CLI's own reported costUSD against
# its own token counters across two real runs; it recovered exactly $5.00/Mtok input and
# $25.00/Mtok output, confirming the multipliers below. The runs use the 1-HOUR cache
# (usage.cache_creation.ephemeral_1h_input_tokens), whose write multiplier is 2.0x, NOT
# the 1.25x of the 5-minute cache. Assuming 1.25x yields a negative output price, which
# is how the error was caught.
USD_PER_INPUT_TOK = 5.00 / 1e6
USD_PER_OUTPUT_TOK = 25.00 / 1e6
CACHE_WRITE_MULT = 2.0
CACHE_READ_MULT = 0.10


def load(path):
    """Parse a stream-json transcript into (per_call_usage, tool_results, result_event)."""
    calls = {}   # message.id -> usage dict (max of each counter seen)
    order = []   # message ids in first-seen order
    tools = []   # {'after_call': idx, 'name': str, 'chars': int}
    pending_tool_names = {}  # tool_use_id -> tool name
    result = None

    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue

            t = ev.get("type")

            if t == "assistant":
                msg = ev.get("message", {})
                mid = msg.get("id")
                if not mid:
                    continue
                if mid not in calls:
                    calls[mid] = {"input_tokens": 0, "output_tokens": 0,
                                  "cache_creation_input_tokens": 0,
                                  "cache_read_input_tokens": 0,
                                  "model": msg.get("model")}
                    order.append(mid)
                u = msg.get("usage") or {}
                for k in ("input_tokens", "output_tokens",
                          "cache_creation_input_tokens", "cache_read_input_tokens"):
                    if k in u and u[k] is not None:
                        calls[mid][k] = max(calls[mid][k], u[k])
                for c in msg.get("content", []):
                    if c.get("type") == "tool_use":
                        pending_tool_names[c.get("id")] = c.get("name")

            elif t == "user":
                content = ev.get("message", {}).get("content")
                if isinstance(content, list):
                    for c in content:
                        if c.get("type") == "tool_result":
                            payload = c.get("content")
                            text = payload if isinstance(payload, str) else json.dumps(payload)
                            tools.append({
                                "after_call": len(order) - 1,
                                "name": pending_tool_names.get(c.get("tool_use_id"), "?"),
                                "chars": len(text),
                            })

            elif t == "result":
                result = ev

    seq = [dict(calls[m], id=m) for m in order]
    return seq, tools, result


def build(path, main_model_hint="opus"):
    seq, tools, result = load(path)
    if result is None:
        raise SystemExit(f"{path}: no result event -- run did not complete")

    # --- reconcile our reconstructed per-call ledger against the CLI's own totals ---
    # NOTE: output_tokens in streamed assistant events are PARTIAL (streaming deltas)
    # and do not sum to the true total, so output is taken from the authoritative
    # result event. Input/cache counters DO reconcile exactly and are asserted.
    tot = result.get("usage", {})
    recon = {
        "input_tokens": sum(c["input_tokens"] for c in seq),
        "cache_creation_input_tokens": sum(c["cache_creation_input_tokens"] for c in seq),
        "cache_read_input_tokens": sum(c["cache_read_input_tokens"] for c in seq),
    }
    mismatches = {k: (recon[k], tot.get(k)) for k in recon if recon[k] != tot.get(k)}
    recon["output_tokens"] = tot.get("output_tokens", 0)  # authoritative

    # --- isolate the main model; the CLI also bills a haiku sidecar we must not blame
    #     on either arm's retrieval strategy ---
    mu = result.get("modelUsage", {})
    main = {k: v for k, v in mu.items() if main_model_hint in k}
    side = {k: v for k, v in mu.items() if main_model_hint not in k}
    main_cost = sum(v.get("costUSD", 0) for v in main.values())
    side_cost = sum(v.get("costUSD", 0) for v in side.values())

    # --- tool token attribution: calibrated char-based (primary) ---
    tool_tokens_by_call = {}
    for tr in tools:
        i = tr["after_call"]
        tool_tokens_by_call[i] = tool_tokens_by_call.get(i, 0) + tr["chars"] / CHARS_PER_TOKEN
    tool_tokens_by_call = {i: int(round(v)) for i, v in tool_tokens_by_call.items()}

    tool_tokens_total = sum(tool_tokens_by_call.values())
    tool_chars_total = sum(t["chars"] for t in tools)

    # loose upper bound from cache_creation deltas (contaminated by thinking blobs)
    ub = 0
    for i in tool_tokens_by_call:
        if i + 1 < len(seq):
            ub += max(0, seq[i + 1]["cache_creation_input_tokens"])

    # --- shares ---
    unique_ctx = recon["input_tokens"] + recon["cache_creation_input_tokens"]
    billed_in = unique_ctx + recon["cache_read_input_tokens"]

    # billed exposure: tokens introduced at call i are re-billed on every later call
    billed_exposure = 0
    for i, n in tool_tokens_by_call.items():
        later_calls = len(seq) - (i + 1)
        billed_exposure += n * max(0, later_calls)

    # dollar weighting: cache read ~0.1x, cache write ~1.25x of base input
    PRICE = {"fresh": 1.0, "write": CACHE_WRITE_MULT, "read": CACHE_READ_MULT}
    dollar_units_total = (recon["input_tokens"] * PRICE["fresh"]
                          + recon["cache_creation_input_tokens"] * PRICE["write"]
                          + recon["cache_read_input_tokens"] * PRICE["read"])
    dollar_units_tool = 0.0
    for i, n in tool_tokens_by_call.items():
        dollar_units_tool += n * PRICE["write"]
        dollar_units_tool += n * max(0, len(seq) - (i + 1)) * PRICE["read"]

    def pct(a, b):
        return round(100.0 * a / b, 1) if b else None

    return {
        "file": os.path.basename(path),
        "ok": result.get("is_error") is False and result.get("subtype") == "success",
        "answer": result.get("result", ""),
        "num_turns": result.get("num_turns"),
        "api_calls": len(seq),
        "tool_calls": len(tools),
        "tool_breakdown": _by_name(tools),
        "duration_ms": result.get("duration_ms"),
        "totals": recon,
        "cli_totals": {k: tot.get(k) for k in recon},
        "reconciled": not mismatches,
        "mismatches": mismatches,
        "billed_input_tokens": billed_in,
        "unique_context_tokens": unique_ctx,
        "output_tokens": recon["output_tokens"],
        "total_tokens_billed": billed_in + recon["output_tokens"],
        "cost_usd_total": result.get("total_cost_usd"),
        # Cache-neutral cost: every input token priced as fresh input. This removes
        # cache-warmth luck, which otherwise dominates arm-vs-arm dollar comparisons
        # (an arm that happens to run against a cold cache pays 2.0x on writes while a
        # warm arm pays 0.1x on reads -- observed as a 2.4x cost inversion on q01 even
        # though the cheaper-looking arm used MORE tokens). Use this for fair $ claims.
        "cost_usd_cache_neutral": round(billed_in * USD_PER_INPUT_TOK
                                        + recon["output_tokens"] * USD_PER_OUTPUT_TOK, 6),
        "cache_write_tokens": recon["cache_creation_input_tokens"],
        "cache_read_tokens": recon["cache_read_input_tokens"],
        "cost_usd_main_model": round(main_cost, 6),
        "cost_usd_sidecar": round(side_cost, 6),
        "sidecar_models": list(side.keys()),
        "tool_tokens_est": tool_tokens_total,
        "tool_tokens_upper_bound": ub,
        "tool_chars": tool_chars_total,
        "chars_per_token_used": CHARS_PER_TOKEN,
        "share_of_unique_ctx_pct": pct(tool_tokens_total, unique_ctx),
        "share_of_billed_pct": pct(tool_tokens_total + billed_exposure, billed_in),
        "share_of_dollars_pct": pct(dollar_units_tool, dollar_units_total),
    }


def _by_name(tools):
    out = {}
    for t in tools:
        d = out.setdefault(t["name"], {"calls": 0, "chars": 0})
        d["calls"] += 1
        d["chars"] += t["chars"]
    return out


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("usage: ledger.py <transcript.jsonl> [...]")
    for p in sys.argv[1:]:
        print(json.dumps(build(p)))
