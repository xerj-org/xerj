#!/usr/bin/env node
// Deterministic ES/native API liveness gate for CI (no model needed).
//
// Probes every GET/HEAD route (read-only = safe to fire against a live server)
// extracted from router.rs, substituting a seeded index + dummy params. It is a
// regression gate: ANY 5xx fails the build (a route that panics / 500s). 4xx is
// tolerated (the route is wired; it just wants different input — write paths are
// covered by the smoke suite). Prints a status histogram + every 5xx.
//
// Usage: node liveness.mjs            (expects xerj on :9200 / :8080, `bench` seeded)
import { execFileSync } from 'node:child_process';
import path from 'node:path';

const ROOT = path.dirname(new URL(import.meta.url).pathname);
const ES = 'http://localhost:9200';
const NATIVE = 'http://localhost:8080';

// Build the route inventory fresh from source (no committed drift).
const routesJson = execFileSync('python3', [path.join(ROOT, 'extract_routes.py')], { encoding: 'utf8' });
const routes = JSON.parse(routesJson);

// Concrete substitutions for path params (all read-only targets).
const SUB = {
  ':index': 'bench', ':name': 'bench', ':id': '1', ':task_id': 'local:1',
  ':field': 'model', ':alias': 'bench', ':repo': 'bench', ':snapshot': 'bench',
  ':node_id': 'local', ':block': 'read', ':target': 'bench2', ':node': 'local',
};
function concretize(p) {
  return p.split('/').map((seg) => (seg.startsWith(':') ? (SUB[seg] || 'x') : seg)).join('/');
}

const probes = routes.filter((r) => r.method === 'GET' || r.method === 'HEAD');
const hist = {}; const fails = []; const errs = [];
for (const r of probes) {
  const base = r.surface === 'native' ? NATIVE : ES;
  const url = base + concretize(r.path);
  let code = 0;
  try {
    code = parseInt(execFileSync('curl', ['-s', '-o', '/dev/null', '-m', '15', '-w', '%{http_code}', '-X', r.method, url], { encoding: 'utf8' }).trim(), 10);
  } catch { code = -1; }
  const bucket = code >= 500 ? '5xx' : code >= 400 ? '4xx' : code >= 300 ? '3xx' : code >= 200 ? '2xx' : 'ERR';
  hist[bucket] = (hist[bucket] || 0) + 1;
  if (code >= 500) fails.push(`${r.method} ${concretize(r.path)} -> ${code} [${r.handler}]`);
  if (code < 0) errs.push(`${r.method} ${concretize(r.path)} -> connection error [${r.handler}]`);
}

console.log(`liveness: probed ${probes.length} read routes`);
console.log('status histogram:', JSON.stringify(hist));
if (errs.length) { console.log('\nCONNECTION ERRORS:'); errs.forEach((e) => console.log('  ' + e)); }
if (fails.length) {
  console.log(`\n❌ ${fails.length} route(s) returned 5xx:`);
  fails.forEach((f) => console.log('  ' + f));
  process.exit(1);
}
if (errs.length) process.exit(1);
console.log('\n✅ no 5xx across the read surface');
