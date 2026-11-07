"""Print the hub page's markdown results table from out/summary.json and deps.json.
Usage: python3 md_table.py"""
import json, os
HERE = os.path.dirname(os.path.abspath(__file__))
S = {(r["system"], r["config"]): r for r in json.load(open(os.path.join(HERE, "out/summary.json")))}
ROWS = [
    ("xerj", "bm25", "BM25, default recall", "default keyword"),
    ("xerj", "semantic", "semantic, MiniLM", "embed"),
    ("xerj", "hybrid", "hybrid, MiniLM", "embed"),
    ("xerj", "hybrid_recency", "hybrid + recency 0.3", "embed"),
    ("mem0", "raw", "infer=False, MiniLM", "embed"),
    ("mem0", "infer", "infer=True, Qwen2.5 3B", "default embed llm"),
    ("mempalace", "default", "default, MiniLM", "default embed"),
    ("reme", "bm25_default", "default, BM25", "default keyword"),
    ("reme", "hybrid_minilm", "hybrid, MiniLM", "embed"),
    ("agentmemory", "keyless_bm25", "default, no key", "default keyword"),
    ("mcp-memory-service", "default", "default, MiniLM", "default embed"),
    ("memvid", "lex", "default, lexical", "default keyword"),
]
_R = [r for r in ROWS if (r[0], r[1]) in S]
NAMES = {"xerj": "XERJ", "mem0": "mem0", "mempalace": "MemPalace", "reme": "ReMe",
         "agentmemory": "agentmemory", "mcp-memory-service": "mcp-memory-service", "memvid": "memvid"}
print("| System | Setup | Top 1 % | Top 5 % | Reworded % | Newest first % | Write ms | Query ms | LLM calls per write |")
print("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
for sysn, cfg, label, _ in _R:
    r = S[(sysn, cfg)]
    print(f"| {NAMES[sysn]} | {label} | {r['hit_at_1_pct']} | {r['hit_at_5_pct']} | {r['paraphrase_hit5_pct']} | "
          f"{r['update_newest_wins_pct']} | {r['write_ms_p50']} | {r['query_ms_p50']} | {r['llm_calls_per_write']:g} |")
