import json, sys
b = json.load(sys.stdin)
for i, h in enumerate(b.get("hits", {}).get("hits", []), 1):
    s = h.get("_source", {})
    path = s.get("ax_path") or s.get("path")
    print(f"  {i}. {h.get('_score', 0):9.3f}  {path}  kind={s.get('kind', '')} name={s.get('name', '')} line={s.get('line', s.get('start_line', ''))}")
