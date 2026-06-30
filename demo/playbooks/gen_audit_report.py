#!/usr/bin/env python3
# Generate the ES-API coverage report from the audit JSON (+ embed benchmark).
# Usage: python3 gen_audit_report.py
import json, os
from collections import Counter

ROOT = os.path.dirname(os.path.abspath(__file__))
data = json.load(open('/tmp/xerj/audit_full.json'))   # list of {group, summary, results[]}

flat = []
for g in data:
    for r in g['results']:
        r2 = dict(r); r2['group'] = g['group']; flat.append(r2)

c = Counter(r['verdict'] for r in flat)
tot = len(flat)
EMOJI = {'REAL': '✅', 'PARTIAL': '🟡', 'STUB': '⚪', 'BROKEN': '❌'}

# machine-readable
json.dump(flat, open(os.path.join(ROOT, 'es_api_audit.json'), 'w'), indent=1)

out = []
out.append('# Xerj ES-compat API — coverage audit\n')
out.append('Every route in `build_es_compat_router` was **tested live against a running '
           'binary AND classified against its handler source** (16 parallel auditors). '
           'Verdicts: ✅ REAL = genuine behaviour reflecting engine state · 🟡 PARTIAL = works '
           'with simplified/missing ES semantics · ⚪ STUB = 2xx compatibility shim with no real '
           'subsystem · ❌ BROKEN.\n')
out.append(f'## Summary — {tot} endpoints, **{c.get("BROKEN",0)} broken**\n')
out.append('| verdict | count | share |\n|---|--:|--:|')
for v in ['REAL', 'PARTIAL', 'STUB', 'BROKEN']:
    out.append(f'| {EMOJI[v]} {v} | {c.get(v,0)} | {100*c.get(v,0)/tot:.0f}% |')
out.append('')
out.append('**Read it as:** the entire core data plane — documents, bulk, the full query DSL, '
           'aggregations, vectors/kNN, mappings, settings, templates, aliases, scroll/PIT, '
           'reindex, update/delete-by-query, snapshot/restore, ingest pipelines, analyzers — is '
           'REAL. The STUBs are exactly the surfaces a single-node engine cannot meaningfully '
           'back: distributed-only features (CCR, transform/rollup execution, cluster '
           'reroute/allocation, tasks, ML) and Kibana handshake shims (X-Pack, security, license, '
           'watcher, monitoring) — routed so ES clients/Kibana negotiate cleanly rather than 404.\n')

# per-group table
out.append('## Coverage by group\n')
out.append('| group | endpoints | ✅ | 🟡 | ⚪ | ❌ |\n|---|--:|--:|--:|--:|--:|')
for g in data:
    cc = Counter(r['verdict'] for r in g['results'])
    out.append(f"| {g['group']} | {len(g['results'])} | {cc.get('REAL',0)} | {cc.get('PARTIAL',0)} | {cc.get('STUB',0)} | {cc.get('BROKEN',0)} |")
out.append('')

# benchmark (embed if present)
bp = os.path.join(ROOT, 'BENCHMARK.md')
if os.path.exists(bp):
    bm = open(bp).read()
    # drop the H1, keep the rest under our H2s
    bm = '\n'.join(l for l in bm.splitlines() if not l.startswith('# '))
    out.append('## Performance (hot paths)\n')
    out.append(bm.strip() + '\n')

# full matrix per group
out.append('## Full endpoint matrix\n')
for g in data:
    out.append(f"### {g['group']}\n")
    out.append(f"_{g['summary']}_\n")
    out.append('| | method | path | handler | http | evidence |\n|---|---|---|---|--:|---|')
    for r in sorted(g['results'], key=lambda r: r['path']):
        ev = r['evidence'].replace('|', '\\|').replace('\n', ' ')
        out.append(f"| {EMOJI.get(r['verdict'],'?')} | {r['method']} | `{r['path']}` | `{r['handler']}` | {r['http_status']} | {ev} |")
    out.append('')

open(os.path.join(ROOT, 'ES_API_AUDIT.md'), 'w').write('\n'.join(out))
print(f"wrote ES_API_AUDIT.md ({tot} endpoints) + es_api_audit.json")
print("verdicts:", dict(c))
