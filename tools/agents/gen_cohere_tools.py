#!/usr/bin/env python3
"""Generate landing/docs/agents/schemas/cohere-tools.json from openai-tools.json.

Cohere's Chat API v2 takes tools in the same envelope OpenAI uses:
{"type": "function", "function": {"name", "description", "parameters"}}.
Its parameter schemas accept a narrower JSON-Schema subset, so this script
rewrites the keywords that subset may reject instead of hoping they pass:

  - "additionalProperties": dropped (free-form objects stay {"type": "object"}).
  - "default": dropped, its value moved into the description.
  - "minimum" / "maximum" / "minItems" / "maxItems": dropped, moved into the
    description so the model still sees the bound.
  - "type": [a, b, ...] (a union): the first type is kept and the description
    says which other forms the XERJ API also accepts.

Which of these Cohere accepts as-is has not been checked against a live key;
see docs/examples/cohere/README.md ("Not verified yet").

Usage:
  python3 tools/agents/gen_cohere_tools.py           # write the file
  python3 tools/agents/gen_cohere_tools.py --check   # exit 1 if it is stale
"""
import copy
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SRC = ROOT / "landing/docs/agents/schemas/openai-tools.json"
DST = ROOT / "landing/docs/agents/schemas/cohere-tools.json"

BOUNDS = {"minimum": "Minimum {}.", "maximum": "Maximum {}.", "minItems": "At least {} items.", "maxItems": "At most {} items."}


def note(desc, extra):
    desc = (desc or "").rstrip()
    sep = " " if desc.endswith(".") or not desc else ". "
    return f"{desc}{sep}{extra}".strip()


def convert(schema):
    if isinstance(schema, list):
        return [convert(s) for s in schema]
    if not isinstance(schema, dict):
        return schema
    out = {}
    notes = []
    for key, value in schema.items():
        if key == "additionalProperties":
            continue
        if key == "default":
            notes.append(f"Default: {json.dumps(value)}.")
            continue
        if key in BOUNDS:
            notes.append(BOUNDS[key].format(value))
            continue
        if key == "type" and isinstance(value, list):
            out["type"] = value[0]
            others = ", ".join(value[1:])
            notes.append(f"Given here as {value[0]}; the XERJ API also accepts {others}.")
            continue
        if key == "properties" and isinstance(value, dict):
            out[key] = {name: convert(sub) for name, sub in value.items()}
            continue
        if key == "items":
            out[key] = convert(value)
            continue
        out[key] = copy.deepcopy(value)
    if notes:
        out["description"] = note(out.get("description", ""), " ".join(notes))
    return out


def build():
    tools = json.loads(SRC.read_text())
    result = []
    for tool in tools:
        fn = tool["function"]
        result.append({
            "type": "function",
            "function": {
                "name": fn["name"],
                "description": fn["description"],
                "parameters": convert(fn["parameters"]),
            },
        })
    return json.dumps(result, indent=2, ensure_ascii=False) + "\n"


def main():
    text = build()
    if "--check" in sys.argv:
        current = DST.read_text() if DST.exists() else ""
        if current != text:
            print(f"FAIL: {DST.relative_to(ROOT)} is stale; run python3 tools/agents/gen_cohere_tools.py")
            return 1
        print(f"ok: {DST.relative_to(ROOT)} matches {SRC.relative_to(ROOT)}")
        return 0
    DST.write_text(text)
    print(f"wrote {DST.relative_to(ROOT)} ({len(json.loads(text))} tools)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
