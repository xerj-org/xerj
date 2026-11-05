"""Builds the data-driven figures for the xerj.org comparison pages from out/summary.csv,
out/per_question.csv, dataset.json and deps.json. Nothing is typed in by hand.
Usage: python3 make_visuals.py <repo_root>   (writes content/visuals/memory-bench-*.html)
"""
import csv, html, json, os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(sys.argv[1], "content", "visuals")
S = {(r["system"], r["config"]): r for r in csv.DictReader(open(os.path.join(HERE, "out", "summary.csv")))}
PQ = {}
for r in csv.DictReader(open(os.path.join(HERE, "out", "per_question.csv"))):
    PQ[(r["system"], r["config"], r["id"])] = r
DS = json.load(open(os.path.join(HERE, "dataset.json")))
DEPS = json.load(open(os.path.join(HERE, "deps.json")))["systems"]
NAME = {"xerj": "XERJ", "mem0": "Mem0", "mempalace": "MemPalace", "reme": "ReMe", "agentmemory": "agentmemory",
        "mcp-memory-service": "mcp-memory-service", "memvid": "memvid"}
# (system, config, setup label, tags)
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
ROWS = [r for r in ROWS if (r[0], r[1]) in S]
TYPES = [("exact", "Same words"), ("paraphrase", "Reworded"), ("update", "Updated facts"), ("distractor", "Near misses")]
E = html.escape


def f1(x):
    return f"{float(x):.1f}"


def ms(x):
    v = float(x)
    return f"{v:.1f}" if v < 100 else f"{v:.0f}"


def table():
    head = ('<tr><th scope="col" data-sort="text">System</th><th scope="col">Setup</th>'
            '<th scope="col" class="is-num" data-sort="num">Top 1 %</th><th scope="col" class="is-num" data-sort="num">Top 5 %</th>'
            '<th scope="col" class="is-num" data-sort="num">Reworded %</th><th scope="col" class="is-num" data-sort="num">Newest first %</th>'
            '<th scope="col" class="is-num" data-sort="num">Write ms</th><th scope="col" class="is-num" data-sort="num">Query ms</th>'
            '<th scope="col" class="is-num" data-sort="num">LLM calls per write</th><th scope="col" class="is-num" data-sort="num">Packages</th></tr>')
    body = []
    for sysn, cfg, label, tags in ROWS:
        r = S[(sysn, cfg)]
        cls = ' class="is-us"' if sysn == "xerj" else ""
        calls = r["llm_calls_per_write"] or "0"
        pk = DEPS[sysn]["packages"]
        cells = [
            f'<th scope="row">{E(NAME[sysn])}</th>', f"<td>{E(label)}</td>",
            f'<td class="is-num" data-v="{r["hit_at_1_pct"]}">{f1(r["hit_at_1_pct"])}</td>',
            f'<td class="is-num" data-v="{r["hit_at_5_pct"]}">{f1(r["hit_at_5_pct"])}</td>',
            f'<td class="is-num" data-v="{r["paraphrase_hit5_pct"]}">{f1(r["paraphrase_hit5_pct"])}</td>',
            f'<td class="is-num" data-v="{r["update_newest_wins_pct"]}">{f1(r["update_newest_wins_pct"])}</td>',
            f'<td class="is-num" data-v="{r["write_ms_p50"]}">{ms(r["write_ms_p50"])}</td>',
            f'<td class="is-num" data-v="{r["query_ms_p50"]}">{ms(r["query_ms_p50"])}</td>',
            f'<td class="is-num" data-v="{float(calls)}">{E(str(calls))}</td>',
            f'<td class="is-num" data-v="{pk}">{pk}</td>',
        ]
        body.append(f'<tr{cls} data-tags="{tags}">' + "".join(cells) + "</tr>")
    n = len(DS["questions"])
    return f"""<figure class="vz" id="fig-mb-table" aria-labelledby="fig-mb-table-cap">
  <span class="vz-label">Try it: sort and filter the results</span>
  <p class="vz-title">{len(ROWS)} setups, {n} questions each</p>
  <div data-vz-sort>
    <div class="vz-js-only">
      <div class="vz-q">
        <button type="button" class="vz-btn" data-tag="all" aria-pressed="true">All setups</button>
        <button type="button" class="vz-btn" data-tag="default" aria-pressed="false">Out of the box</button>
        <button type="button" class="vz-btn" data-tag="keyword" aria-pressed="false">Keyword only</button>
        <button type="button" class="vz-btn" data-tag="embed" aria-pressed="false">With embeddings</button>
      </div>
    </div>
    <div class="vz-scroll">
      <table class="vz-table">
        <thead>{head}</thead>
        <tbody>
          {(chr(10) + '          ').join(body)}
        </tbody>
      </table>
    </div>
    <p class="vz-out vz-js-only" aria-live="polite"></p>
  </div>
  <figcaption id="fig-mb-table-cap">Runs in your browser and sends nothing. Click a column name to sort. Top 1 and Top 5: share of the {n} questions where a returned memory holds the right answer at rank 1 or in the first five. Reworded: the 25 questions asked in other words. Newest first: the 15 questions about a fact that changed, where the new value must rank above the old one. Times are medians on one shared 8 core box. Packages: resolved install size from deps.json. Raw data is in benchmarks/agent-memory in the XERJ repository.</figcaption>
</figure>
"""


def bars():
    def lst(key, title, gap=False):
        items = []
        for sysn, cfg, label, tags in sorted(ROWS, key=lambda r: -float(S[(r[0], r[1])][key])):
            v = float(S[(sysn, cfg)][key])
            fill = "vz-fill" if sysn == "xerj" else "vz-fill is-cmp"
            items.append(f'<li class="vz-bar"><span>{E(NAME[sysn])}, {E(label)}</span><span class="vz-track"><span class="{fill}" style="width:{v:.1f}%"></span></span><span class="vz-v">{v:.1f}</span></li>')
        return f'<p class="vz-panel-h"{' style="margin-top:24px"' if gap else ''}>{title}</p>\n  <ul class="vz-bars">\n    ' + "\n    ".join(items) + "\n  </ul>"
    return f"""<figure class="vz" id="fig-mb-bars" aria-labelledby="fig-mb-bars-cap">
  <span class="vz-label">Recorded run, {len(DS['questions'])} questions</span>
  <p class="vz-title">Did the right memory come back?</p>
  <p class="vz-legend"><span><i></i>XERJ</span><span><i class="is-cmp"></i>Other projects</span></p>
  {lst("hit_at_5_pct", "Right answer in the top 5, all questions, %")}
  {lst("update_newest_wins_pct", "New value ranked above the old one, 15 updated facts, %", True)}
  <figcaption id="fig-mb-bars-cap">Drawn from out/summary.csv in benchmarks/agent-memory. Bars start at 0 and the full width is 100%. Every setup got the same 85 memories in the same order and the same 75 questions, with k = 5.</figcaption>
</figure>
"""


def qx(fid, cols, title, start="paraphrase"):
    """cols: list of (system, config, short label)."""
    qs = DS["questions"]
    head = '<tr><th scope="col">Question</th><th scope="col">Answer</th>' + "".join(f'<th scope="col">{E(l)}</th>' for _, _, l in cols) + "</tr>"
    rows = []
    for q in qs:
        cells = [f'<th scope="row">{E(q["question"])}</th>', f"<td>{E(q['answer'])}</td>"]
        for sysn, cfg, _ in cols:
            p = PQ[(sysn, cfg, q["id"])]
            hit = p["hitk"] == "True"
            rank = p["answer_rank"]
            if not hit:
                cells.append('<td data-hit="0"><span class="vz-chip is-no">miss</span></td>')
            elif q["type"] in ("update", "distractor") and p["correct_first"] == "False":
                cells.append(f'<td data-hit="1"><span class="vz-chip is-mid">{rank}, old first</span></td>')
            else:
                cells.append(f'<td data-hit="1"><span class="vz-chip">{rank}</span></td>')
        rows.append(f'<tr data-type="{q["type"]}">' + "".join(cells) + "</tr>")
    btns = "\n        ".join(
        f'<button type="button" class="vz-btn" data-type="{t}" aria-pressed="{"true" if t == start else "false"}">{lab}</button>' for t, lab in TYPES)
    return f"""<figure class="vz" id="{fid}" aria-labelledby="{fid}-cap">
  <span class="vz-label">Try it: every question we asked</span>
  <p class="vz-title">{E(title)}</p>
  <div data-vz-qx>
    <div class="vz-js-only">
      <div class="vz-q">
        {btns}
        <button type="button" class="vz-btn" data-type="all" aria-pressed="false">All 75</button>
      </div>
      <p class="vz-out" aria-live="polite"></p>
    </div>
    <div class="vz-scroll vz-tall">
      <table class="vz-table">
        <thead>{head}</thead>
        <tbody>
          {(chr(10) + '          ').join(rows)}
        </tbody>
      </table>
    </div>
  </div>
  <figcaption id="{fid}-cap">Runs in your browser and sends nothing. Each cell is the rank of the first returned memory that holds the answer, or miss if none of the top 5 does. "old first" means the old value of a changed fact ranked above the new one. From out/per_question.csv in benchmarks/agent-memory.</figcaption>
</figure>
"""


def write(name, text):
    p = os.path.join(OUT, name + ".html")
    open(p, "w").write(text.rstrip() + "\n")
    print("wrote", p)


write("memory-bench-table", table())
write("memory-bench-bars", bars())
write("memory-qx-reme", qx("fig-qx-reme", [("xerj", "bm25", "XERJ BM25"), ("xerj", "hybrid", "XERJ hybrid"), ("reme", "bm25_default", "ReMe default"), ("reme", "hybrid_minilm", "ReMe hybrid")], "XERJ and ReMe, question by question"))
write("memory-qx-agentmemory", qx("fig-qx-am", [("xerj", "bm25", "XERJ BM25"), ("xerj", "hybrid", "XERJ hybrid"), ("agentmemory", "keyless_bm25", "agentmemory")], "XERJ and agentmemory, question by question"))
write("memory-qx-mms", qx("fig-qx-mms", [("xerj", "hybrid", "XERJ hybrid"), ("xerj", "hybrid_recency", "XERJ recency"), ("mcp-memory-service", "default", "mcp-memory-service")], "XERJ and mcp-memory-service, question by question", start="update"))
