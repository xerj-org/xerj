#!/usr/bin/env python3
"""Measure what XERJ's MCP tool catalog costs an agent, in bytes and tokens.

Every MCP client puts the server's `tools/list` answer (each tool's name,
description and input schema) into the model's context before the first
call. That cost is paid on every session, so #1785 asks for it to be measured
and published.

Input: `landing/docs/agents/schemas/mcp-tools.json`, the live `tools/list`
response written by `scripts/mcp-schema-check.sh --write` and pinned to the
server by `engine/crates/xerj-mcp/tests/published_schema_drift.rs`. Each tool
is serialised as compact JSON, the form it has on the wire.

Two catalogs are measured:
  with_hub_key  every tool (the published file)
  no_hub_key    the same minus `xerj_hub_*`, what a build with no hub signing
                key serves (`tool_specs_with(false)`)

Tokens are counted with tiktoken's `o200k_base` and `cl100k_base`. Claude's
tokenizer is not public, and each client wraps the tool list in its own
prompt format, so treat the counts as an estimate of the order of magnitude,
not an exact bill. Bytes are exact.

    pip install tiktoken
    python3 scripts/mcp/catalog_tokens.py            # print the table
    python3 scripts/mcp/catalog_tokens.py --write    # also write the results

`--write` updates `benchmarks/mcp-catalog-size/results/catalog.json`.
`engine/crates/xerj-mcp/tests/catalog_size.rs` fails when the bytes recorded
there drift more than 10% from what the server serves, so rerun this after a
large change to tool descriptions or schemas.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCHEMA = ROOT / "landing/docs/agents/schemas/mcp-tools.json"
RESULTS = ROOT / "benchmarks/mcp-catalog-size/results/catalog.json"
ENCODINGS = ("o200k_base", "cl100k_base")
HUB_PREFIX = "xerj_hub_"


def compact(value) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def measure(tools: list, encoders: dict) -> dict:
    rows = []
    for tool in tools:
        text = compact(tool)
        row = {"name": tool["name"], "bytes": len(text.encode("utf-8"))}
        for name, enc in encoders.items():
            row[name] = len(enc.encode(text))
        rows.append(row)
    whole = compact(tools)
    total = {"tools": len(tools), "bytes": len(whole.encode("utf-8"))}
    for name, enc in encoders.items():
        total[name] = len(enc.encode(whole))
    return {"total": total, "per_tool": rows}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--write", action="store_true", help=f"write {RESULTS.relative_to(ROOT)}")
    args = ap.parse_args()

    try:
        import tiktoken
    except ImportError:
        print("FAIL: tiktoken is required (pip install tiktoken)", file=sys.stderr)
        return 2
    encoders = {name: tiktoken.get_encoding(name) for name in ENCODINGS}

    tools = json.loads(SCHEMA.read_text(encoding="utf-8"))["tools"]
    if not tools:
        print(f"FAIL: {SCHEMA} lists no tools", file=sys.stderr)
        return 1
    no_hub = [t for t in tools if not t["name"].startswith(HUB_PREFIX)]
    if len(no_hub) == len(tools):
        print(f"FAIL: no {HUB_PREFIX}* tool in {SCHEMA}; the no-key catalog would equal the full one", file=sys.stderr)
        return 1

    out = {
        "source": str(SCHEMA.relative_to(ROOT)),
        "serialisation": "compact JSON per tool object, UTF-8",
        "tokenizers": {name: f"tiktoken {tiktoken.__version__} {name}" for name in ENCODINGS},
        "catalogs": {
            "with_hub_key": measure(tools, encoders),
            "no_hub_key": measure(no_hub, encoders),
        },
    }

    head = f"{'catalog':<14} {'tools':>5} {'bytes':>8} " + " ".join(f"{e:>12}" for e in ENCODINGS)
    print(head)
    for label, cat in out["catalogs"].items():
        t = cat["total"]
        print(f"{label:<14} {t['tools']:>5} {t['bytes']:>8} " + " ".join(f"{t[e]:>12}" for e in ENCODINGS))
    print()
    print(f"{'tool':<24} {'bytes':>8} " + " ".join(f"{e:>12}" for e in ENCODINGS))
    for row in sorted(out["catalogs"]["with_hub_key"]["per_tool"], key=lambda r: -r["bytes"]):
        print(f"{row['name']:<24} {row['bytes']:>8} " + " ".join(f"{row[e]:>12}" for e in ENCODINGS))

    if args.write:
        RESULTS.parent.mkdir(parents=True, exist_ok=True)
        RESULTS.write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"\nwrote {RESULTS.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
