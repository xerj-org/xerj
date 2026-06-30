#!/usr/bin/env python3
"""Extract the live route inventory from the API router source.

Parses crates/xerj-api/src/router.rs and emits JSON of every registered route
(surface = es | native, method, path, handler). Kept in the repo so CI can
build the inventory fresh each run (no drift) for the liveness check.

Usage: python3 extract_routes.py [path/to/router.rs]  (prints JSON to stdout)
"""
import re, sys, json, os

ROUTER = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(__file__), '..', '..', 'engine', 'crates', 'xerj-api', 'src', 'router.rs')

src = open(ROUTER).read()

def routes_in(fn_name, surface):
    m = re.search(r'pub fn ' + fn_name + r'.*?\n\}', src, re.S)
    if not m:
        return []
    body = m.group(0)
    out = []
    for path, meths in re.findall(r'\.route\(\s*"([^"]+)"\s*,\s*((?:[a-z]+\([a-z_:]+::[a-z_0-9]+\)\.?)+)', body):
        for m_, h in re.findall(r'\b(get|post|put|delete|head|patch)\(es_compat::([a-z_0-9]+)\)', meths):
            out.append({'surface': surface, 'method': m_.upper(), 'path': path, 'handler': h})
        for m_, h in re.findall(r'\b(get|post|put|delete|head|patch)\(native::([a-z_0-9]+)\)', meths):
            out.append({'surface': 'native', 'method': m_.upper(), 'path': path, 'handler': h})
    return out

routes = routes_in('build_es_compat_router', 'es') + routes_in('build_native_router', 'native')
json.dump(routes, sys.stdout, indent=1)
sys.stderr.write(f"extracted {len(routes)} routes "
                 f"({sum(1 for r in routes if r['surface']=='es')} es, "
                 f"{sum(1 for r in routes if r['surface']=='native')} native)\n")
