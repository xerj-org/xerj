// Structural guarantees, checked against the SOURCE:
//
//  1. The guest shell's import graph contains no operator module. A guest
//     must never call an admin / console endpoint — the simplest proof is
//     that the code which knows those endpoints is not loaded at all.
//  2. Nothing in that graph (nor the operator-side reader/corpus glue)
//     contains an HTML-parsing sink. Record data can only become text nodes.
//  3. The console page has no inline script (it is served with
//     `script-src 'self'`), and boot.js decides guest-vs-operator BEFORE any
//     console API call.
//  4. No view shows fabricated documents: `mockSearch` is gone.
//
// Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync, readdirSync, statSync } from 'node:fs';
import { dirname, resolve, relative, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const SRC = join(ROOT, 'src');
const rel = (p) => relative(ROOT, p).split('\\').join('/');

/** Strip comments so prose ABOUT a sink ("never innerHTML") is not a hit. */
function code(file) {
  return readFileSync(file, 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:'"`\\])\/\/.*$/gm, '$1');
}

function importsOf(file) {
  const src = code(file);
  const out = [];
  const re = /(?:import|export)\s+(?:[^'"()]*?\sfrom\s+)?['"]([^'"]+)['"]|import\(\s*['"]([^'"]+)['"]\s*\)/g;
  let m;
  while ((m = re.exec(src))) {
    const spec = m[1] || m[2];
    assert.ok(spec.startsWith('.'), `${rel(file)} imports a bare/absolute specifier: ${spec}`);
    const target = resolve(dirname(file), spec);
    assert.ok(existsSync(target), `${rel(file)} imports a file that does not exist: ${spec}`);
    out.push(target);
  }
  return out;
}

function graph(entry) {
  const seen = new Set();
  const stack = [entry];
  while (stack.length) {
    const f = stack.pop();
    if (seen.has(f)) continue;
    seen.add(f);
    for (const dep of importsOf(f)) stack.push(dep);
  }
  return [...seen].map(rel).sort();
}

const GUEST_GRAPH = graph(join(SRC, 'guest-app.js'));

test('the guest shell imports only what a viewer needs', () => {
  assert.deepEqual(GUEST_GRAPH, [
    'src/data/catalog.js',
    'src/data/guest.js',
    'src/data/reader-api.js',
    'src/data/schema-roles.js',
    'src/data/search-body.js',
    'src/data/transport-guest.js',
    'src/guest-app.js',
    'src/ux/corpus-render.js',
    'src/ux/reader-render.js',
    'src/ux/reader-view.js',
    'src/ux/safe-dom.js',
  ], 'a new import into the guest shell is a security review, not a drive-by — update this list deliberately');
});

test('no module the guest loads knows a console / admin / cluster endpoint', () => {
  const FORBIDDEN = [/_xerj-console\/api/, /\/_cat\b/, /\/_cluster\b/, /\/_security\b/, /\/_nodes\b/, /\/v1\/metrics/, /\/_bulk\b/, /\/_doc\b/, /\/_update\b/, /_delete_by_query/, /\/_memory\b/, /\/_share\b/, /localStorage/, /document\.cookie/, /credentials:\s*['"](?:include|same-origin)['"]/];
  for (const f of GUEST_GRAPH) {
    const src = code(join(ROOT, f));
    for (const re of FORBIDDEN) assert.ok(!re.test(src), `${f} mentions ${re} — a guest must have no code path to it`);
  }
  // …and the only place a guest request is made is guestRequest.
  const fetchers = GUEST_GRAPH.filter((f) => /\bfetch\s*\(|\bdoFetch\s*\(|XMLHttpRequest|sendBeacon|WebSocket|EventSource/.test(code(join(ROOT, f))));
  assert.deepEqual(fetchers, ['src/data/guest.js'], 'every guest request goes through data/guest.js');
  const guest = code(join(SRC, 'data/guest.js'));
  // guest.js GUARDS XMLHttpRequest and sendBeacon (installGuestGuard wraps
  // them) but never USES them, and knows no WebSocket / EventSource at all.
  assert.ok(!/new\s+(XMLHttpRequest|WebSocket|EventSource)\s*\(|navigator\.sendBeacon\s*\(|\.send\s*\(/.test(guest), 'guest.js makes no request outside fetch');
  assert.ok(!/WebSocket|EventSource/.test(guest));
  const guard = guest.slice(guest.indexOf('export function installGuestGuard'));
  assert.ok(/XMLHttpRequest/.test(guard) && /sendBeacon/.test(guard), 'the guard covers XHR and beacons');
  assert.ok(!/XMLHttpRequest|sendBeacon/.test(guest.slice(0, guest.indexOf('export function installGuestGuard')).replace(/\/\*[\s\S]*?\*\/|\/\/.*$/gm, '')), 'and nothing before it mentions them');
  assert.ok(/credentials:\s*'omit'/.test(guest), 'guest requests never carry a session cookie');
});

const SINKS = [
  /\.innerHTML\b/, /\.outerHTML\b/, /insertAdjacentHTML/, /document\.write/, /DOMParser/, /createContextualFragment/, /\bsrcdoc\b/,
  /\beval\s*\(/, /new\s+Function\s*\(/, /setTimeout\s*\(\s*['"`]/, /setInterval\s*\(\s*['"`]/, /\.setAttribute\(\s*['"`]on/i,
  /javascript:/i, /\bRange\b.*createContextual/, /\$\(\s*['"`]</, /dangerouslySetInnerHTML/, /\.html\s*\(/,
];

test('no HTML-parsing sink anywhere record data flows', () => {
  // The whole guest graph, plus the operator-side glue for the same views.
  const files = new Set([...GUEST_GRAPH, 'src/dashboards/corpus.js', 'src/dashboards/reader.js', 'src/data/transport-console.js', 'src/data/console-index-api.js', 'src/boot.js', 'src/theme-boot.js']);
  for (const f of files) {
    const src = code(join(ROOT, f));
    for (const re of SINKS) assert.ok(!re.test(src), `${f} contains a markup/script sink: ${re}`);
  }
  // safe-dom builds DOM with exactly these calls.
  const sd = code(join(SRC, 'ux/safe-dom.js'));
  for (const call of ['createElement', 'createTextNode', 'setAttribute', 'replaceChildren']) assert.ok(sd.includes(call), call);
});

test('the operator shell hands record data to safe-dom, never to its own HTML strings', () => {
  const app = code(join(SRC, 'app.js'));
  assert.ok(/mountSafePanels\(/.test(app));
  assert.ok(!/renderRecord|renderResultCard|renderResultList|renderGraphPanel|renderCorpusCard/.test(app), 'app.js must not call the pure renderers itself (their output is a node tree, not a string)');
  for (const f of ['src/dashboards/corpus.js', 'src/dashboards/reader.js']) {
    const src = code(join(ROOT, f));
    assert.ok(!/\$\{/.test(src.replace(/`[^`]*DATASET\$\{[^`]*`/g, '')) || !/<[a-z]/.test(src.match(/`[^`]*\$\{[^`]*`/g)?.join('') || ''),
      `${f} interpolates into markup — its panel must stay an empty mount point`);
    assert.ok(/data-safe-mount=/.test(src));
  }
});

test('the console page has no inline script and boots through boot.js', () => {
  const html = readFileSync(join(ROOT, 'index.html'), 'utf8');
  const tags = html.match(/<script\b[^>]*>/g) || [];
  assert.equal(tags.length, 2);
  for (const t of tags) assert.ok(/\ssrc="src\/[a-z-]+\.js"/.test(t), `inline or unexpected script: ${t}`);
  assert.ok(!/\son[a-z]+\s*=/i.test(html), 'no inline event handlers');
  const bodies = [...html.matchAll(/<script\b[^>]*>([\s\S]*?)<\/script>/g)].map((m) => m[1].trim());
  assert.ok(bodies.every((b) => b === ''), 'script elements must be empty (script-src \'self\')');
});

test('boot.js picks the guest shell before ANY console API call', () => {
  const boot = code(join(SRC, 'boot.js'));
  // Static imports are hoisted: boot.js may statically import only guest.js.
  const statics = [...boot.matchAll(/^import\s[^'"]*['"]([^'"]+)['"]/gm)].map((m) => m[1]);
  assert.deepEqual(statics, ['./data/guest.js']);
  const iGuest = boot.indexOf('hasShareRecord()', boot.indexOf('async function bootOperator'));
  assert.ok(iGuest > 0);
  const tail = boot.slice(boot.lastIndexOf('if (hasShareRecord())'));
  assert.ok(/guest-app\.js/.test(tail) && /else\s*\{\s*await bootOperator\(\);?\s*\}/.test(tail), 'guest → guest-app.js, otherwise → operator');
  // the operator path (and only it) knows /me, sync, the dashboard store, app.js
  const op = boot.slice(boot.indexOf('async function bootOperator'), boot.lastIndexOf('if (hasShareRecord())'));
  for (const s of ['/_xerj-console/api/v1/me', 'xerj-console-sync.js', 'dashboard-store.js', './app.js']) {
    assert.ok(op.includes(s), s);
    assert.ok(!tail.includes(s), `${s} must not be reachable from the guest branch`);
  }
});

test('no fabricated search results: mockSearch is gone, document views never fall back to mock', async () => {
  const walk = (dir) => readdirSync(dir).flatMap((n) => { const p = join(dir, n); return statSync(p).isDirectory() ? walk(p) : (p.endsWith('.js') ? [p] : []); });
  for (const f of walk(SRC)) {
    assert.ok(!/\bmockSearch\b/.test(code(f)), `${rel(f)} still references mockSearch`);
  }
  const q = code(join(SRC, 'data/query.js'));
  assert.ok(/NEVER_MOCK\s*=\s*new Set\(\[\s*'corpus',\s*'reader',\s*'search-discover'\s*\]\)/.test(q));
  const sd = code(join(SRC, 'dashboards/search-discover.js'));
  for (const gone of ['buildPlan', 'QueryPlanTree', '52.4M', '18.9M', 'chat-events', 'logs-ssh-auth', "'knn'"]) {
    assert.ok(!sd.includes(gone), `search-discover.js still contains ${gone}`);
  }
  assert.ok(!existsSync(join(SRC, 'dashboards/case-review.js')) && !existsSync(join(SRC, 'data/email-probe.js')));
});
