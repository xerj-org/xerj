"""usage: mcp_rank.py <xerj_binary> <url> <prefix> <query> [<query> ...]"""
import json, os, subprocess, sys

BIN, URL, PREFIX = sys.argv[1:4]
env = dict(os.environ, XERJ_URL=URL, XERJ_DISABLE_FEEDBACK="true")
p = subprocess.Popen([BIN, "mcp", "--url", URL], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.DEVNULL, text=True, env=env)


def call(i, method, params):
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n")
    p.stdin.flush()
    while True:
        line = p.stdout.readline()
        if not line:
            raise SystemExit("mcp closed")
        msg = json.loads(line)
        if msg.get("id") == i:
            return msg


call(1, "initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                       "clientInfo": {"name": "bench", "version": "0"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
p.stdin.flush()
for n, q in enumerate(sys.argv[4:], 2):
    r = call(n, "tools/call", {"name": "xerj_search", "arguments": {"query": q, "prefix": PREFIX, "size": 5}})
    text = r["result"]["content"][0]["text"]
    try:
        body = json.loads(text)
    except json.JSONDecodeError:
        print(f"== {q}: {text[:300]}")
        continue
    print(f"== mcp [{PREFIX}] {q}")
    for i, h in enumerate(body.get("hits", {}).get("hits", []), 1):
        s = h.get("_source", {})
        print(f"  {i}. {h.get('_score', 0):9.3f}  {s.get('ax_path') or s.get('path')}  kind={s.get('kind', '')}")
p.stdin.close()
p.wait(timeout=30)
