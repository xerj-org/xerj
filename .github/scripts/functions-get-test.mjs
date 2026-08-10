// Behaviour + privacy test for functions/get.js (the /get, /get.ps1 install
// counter). Runs on plain Node — no wrangler, no network, no Cloudflare
// account — by stubbing the two bindings the function touches: env.ASSETS
// (static asset reads) and env.INSTALLS (the R2 bucket).
//
// What this DOES prove: the handler's logic — fail-open on every counter
// failure, correct headers, correct classification, and that no IP or stable
// identifier is ever written.
// What this does NOT prove: that Cloudflare routes /get to this function, or
// that the R2 binding resolves in production. Those need a real deploy (or
// `wrangler pages dev`) and cannot be checked here.
//
//   node .github/scripts/functions-get-test.mjs
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const SRC = join(REPO, 'functions', 'get.js');
const SCRIPT_BODY = readFileSync(join(REPO, 'landing', 'get'), 'utf8');
const PS1_BODY = readFileSync(join(REPO, 'landing', 'get.ps1'), 'utf8');

// functions/get.js is an ES module, but package.json has no "type":"module",
// so Node would parse a bare .js import as CommonJS. Copying the real bytes to
// a .mjs and importing that keeps the test honest (same source) without
// changing the repo's module mode.
const src = readFileSync(SRC, 'utf8');
const tmp = join(mkdtempSync(join(tmpdir(), 'xerj-fn-')), 'get.mjs');
writeFileSync(tmp, src);
const fn = await import(tmp);

let failures = 0;
const ok = (cond, name, extra = '') => {
  if (cond) { console.log(`  PASS  ${name}`); }
  else { console.log(`  FAIL  ${name}${extra ? ' — ' + extra : ''}`); failures++; }
};

/** In-memory stand-in for an R2 bucket. `mode` forces the failure paths. */
function fakeR2(mode = 'ok') {
  const store = new Map();
  return {
    store,
    async put(key, value) {
      if (mode === 'throw') throw new Error('R2 unavailable');
      store.set(key, value);
    },
    async list({ prefix, limit, cursor }) {
      const keys = [...store.keys()].filter((k) => k.startsWith(prefix)).sort();
      return { objects: keys.map((k) => ({ key: k })), truncated: false, cursor: undefined };
    },
    async get(key) {
      const v = store.get(key);
      return v === undefined ? null : { text: async () => v };
    },
  };
}

function fakeAssets(mode = 'ok') {
  return {
    async fetch(url) {
      if (mode === 'throw') throw new Error('assets down');
      const p = new URL(url).pathname;
      if (mode === '500') return new Response('boom', { status: 500 });
      if (p === '/get') return new Response(SCRIPT_BODY, { status: 200 });
      if (p === '/get.ps1') return new Response(PS1_BODY, { status: 200 });
      return new Response('not found', { status: 404 });
    },
  };
}

/** Build a request that carries request.cf, the way the edge delivers it. */
function req(path, ua, country = 'DE') {
  const r = new Request(`https://xerj.org${path}`, { headers: ua ? { 'user-agent': ua } : {} });
  Object.defineProperty(r, 'cf', { value: { country, city: 'Berlin', asn: 3320, latitude: '52.5', longitude: '13.4' } });
  return r;
}

/** Invoke the handler and settle everything waitUntil() deferred. */
async function call(request, env, method = 'GET') {
  const pending = [];
  const context = { request, env, waitUntil: (p) => pending.push(p) };
  const res = method === 'HEAD' ? await fn.onRequestHead(context) : await fn.onRequestGet(context);
  await Promise.allSettled(pending);
  return res;
}

const rowsOf = (r2) => [...r2.store.values()].map((v) => JSON.parse(v));

console.log('── functions/get.js ──');

// 1. Happy path: the script is served byte-for-byte and the request is counted.
{
  const INSTALLS = fakeR2();
  const res = await call(req('/get', 'curl/8.5.0'), { ASSETS: fakeAssets(), INSTALLS });
  const body = await res.text();
  ok(res.status === 200, 'GET /get -> 200', `got ${res.status}`);
  ok(body === SCRIPT_BODY, 'body is landing/get byte-for-byte');
  ok(res.headers.get('content-type') === 'text/plain; charset=utf-8', 'content-type is text/plain');
  ok(res.headers.get('cache-control') === 'no-store', 'cache-control is no-store (a cache hit is an uncounted install)');
  ok(res.headers.get('x-content-type-options') === 'nosniff', 'nosniff set');
  const rows = rowsOf(INSTALLS);
  ok(rows.length === 1, 'exactly one record written', `got ${rows.length}`);
  ok(rows[0].kind === 'installer', 'curl classified as installer', JSON.stringify(rows[0]));
  ok(rows[0].country === 'DE', 'country recorded');
  ok(rows[0].script === 'get', 'script recorded');
  ok([...INSTALLS.store.keys()][0].startsWith('installs/' + new Date().toISOString().slice(0, 10) + '/'), 'key is date-prefixed');
}

// 2. PowerShell path.
{
  const INSTALLS = fakeR2();
  const ua = 'Mozilla/5.0 (Windows NT 10.0; Microsoft Windows 10.0.19045; en-US) WindowsPowerShell/5.1.19041.4291';
  const res = await call(req('/get.ps1', ua, 'US'), { ASSETS: fakeAssets(), INSTALLS });
  const body = await res.text();
  ok(res.status === 200 && body === PS1_BODY, 'GET /get.ps1 serves landing/get.ps1');
  const r = rowsOf(INSTALLS)[0];
  ok(r.script === 'get.ps1', 'script=get.ps1');
  ok(r.kind === 'installer', 'PowerShell classified as installer', JSON.stringify(r));
  ok(r.os === 'windows', 'os=windows from UA', JSON.stringify(r));
}

// 3. Classification of the traffic we must NOT count as installs.
{
  const cases = [
    ['Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)', 'bot'],
    ['Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 Chrome/126 Safari/537.36', 'browser'],
    ['python-requests/2.31.0', 'bot'],
    ['curl/7.68.0', 'installer'],
    ['Wget/1.21.2', 'installer'],
    ['', 'unknown'],
  ];
  for (const [ua, want] of cases) {
    const INSTALLS = fakeR2();
    await call(req('/get', ua), { ASSETS: fakeAssets(), INSTALLS });
    const got = rowsOf(INSTALLS)[0].kind;
    ok(got === want, `classify(${ua.slice(0, 28) || '<empty>'}) = ${want}`, `got ${got}`);
  }
}

// 4. FAIL-OPEN: no binding, and a bucket that throws. The install must survive.
{
  const res = await call(req('/get', 'curl/8.5.0'), { ASSETS: fakeAssets() });
  ok(res.status === 200 && (await res.text()) === SCRIPT_BODY, 'no INSTALLS binding -> script still served');

  const INSTALLS = fakeR2('throw');
  const res2 = await call(req('/get', 'curl/8.5.0'), { ASSETS: fakeAssets(), INSTALLS });
  ok(res2.status === 200 && (await res2.text()) === SCRIPT_BODY, 'R2 put throws -> script still served');
  ok(INSTALLS.store.size === 0, 'nothing stored when the bucket throws');

  const res3 = await call(req('/get', 'curl/8.5.0'), { ASSETS: fakeAssets(), INSTALLS: {} });
  ok(res3.status === 200, 'malformed binding -> script still served', `got ${res3.status}`);
}

// 5. Asset failure is the one case that cannot degrade silently.
{
  for (const mode of ['throw', '500']) {
    const res = await call(req('/get', 'curl/8.5.0'), { ASSETS: fakeAssets(mode), INSTALLS: fakeR2() });
    const body = await res.text();
    ok(res.status === 503, `assets ${mode} -> 503`, `got ${res.status}`);
    ok(body.includes('releases/latest') && body.trim().endsWith('exit 1'), `assets ${mode} -> shell-safe fallback that exits non-zero`);
  }
}

// 6. Unknown paths and HEAD.
{
  const INSTALLS = fakeR2();
  const res = await call(req('/nope', 'curl/8.5.0'), { ASSETS: fakeAssets(), INSTALLS });
  ok(res.status === 404, 'unrelated path -> 404', `got ${res.status}`);
  ok(INSTALLS.store.size === 0, '404 writes no record');

  const head = await call(req('/get', 'curl/8.5.0'), { ASSETS: fakeAssets(), INSTALLS }, 'HEAD');
  ok(head.status === 200, 'HEAD /get -> 200', `got ${head.status}`);
  ok((await head.text()) === '', 'HEAD has no body');
  ok(INSTALLS.store.size === 0, 'HEAD is not counted as an install attempt');
}

// 7. PRIVACY — the assertions this whole file exists for.
{
  const INSTALLS = fakeR2();
  const r = new Request('https://xerj.org/get', {
    headers: {
      'user-agent': 'curl/8.5.0',
      'cf-connecting-ip': '203.0.113.7',
      'x-forwarded-for': '203.0.113.7, 198.51.100.2',
      'x-real-ip': '203.0.113.7',
      referer: 'https://example.com/secret-internal-wiki',
      cookie: 'session=abcdef123456',
      'accept-language': 'de-DE,de;q=0.9',
    },
  });
  Object.defineProperty(r, 'cf', {
    value: { country: 'DE', city: 'Berlin', postalCode: '10115', asn: 3320, asOrganization: 'Deutsche Telekom', latitude: '52.52', longitude: '13.40' },
  });
  await call(r, { ASSETS: fakeAssets(), INSTALLS });
  const stored = [...INSTALLS.store.values()].join('\n');
  const row = JSON.parse([...INSTALLS.store.values()][0]);

  ok(!stored.includes('203.0.113.7'), 'no IP address stored');
  ok(!stored.includes('198.51.100.2'), 'no forwarded-for IP stored');
  ok(!/\b\d{1,3}(\.\d{1,3}){3}\b/.test(stored), 'stored record contains no IPv4 literal at all');
  ok(!stored.includes('example.com'), 'no Referer stored');
  ok(!stored.includes('abcdef123456'), 'no cookie stored');
  ok(!stored.includes('de-DE'), 'no Accept-Language stored');
  ok(!stored.includes('Berlin') && !stored.includes('10115'), 'no city/postcode from request.cf');
  ok(!stored.includes('52.52') && !stored.includes('13.40'), 'no lat/long from request.cf');
  ok(!stored.includes('3320') && !stored.includes('Telekom'), 'no ASN/network operator from request.cf');
  const allowed = new Set(['ts', 'script', 'kind', 'country', 'os', 'arch', 'ua', 'ver']);
  const unexpected = Object.keys(row).filter((k) => !allowed.has(k));
  ok(unexpected.length === 0, 'record has only the documented fields', 'unexpected: ' + unexpected.join(','));
  // The R2 key holds a random UUID; it is a write key, never returned to the
  // client, so it cannot correlate two requests. Assert it stays out of the body.
  ok(!('id' in row) && !('uuid' in row), 'no identifier inside the record body');

  // Source-level guard: these header names must never appear in the function.
  for (const forbidden of ['cf-connecting-ip', 'x-forwarded-for', 'x-real-ip', 'set-cookie']) {
    const codeOnly = src.split('\n').filter((l) => !l.trim().startsWith('//')).join('\n');
    ok(!codeOnly.toLowerCase().includes(forbidden), `source never reads ${forbidden}`);
  }
}

// 8. UA is bounded, so one hostile request cannot balloon an object.
{
  const INSTALLS = fakeR2();
  await call(req('/get', 'curl/' + 'A'.repeat(5000)), { ASSETS: fakeAssets(), INSTALLS });
  ok(rowsOf(INSTALLS)[0].ua.length === 200, 'user-agent truncated to 200 chars', `got ${rowsOf(INSTALLS)[0].ua.length}`);
}

// 9. The export must never be reachable without BOTH a caller token and a
//    configured secret. Any miss serves the installer, and — because it took
//    the install path — is itself counted; hence a bucket of its own here.
{
  const INSTALLS = fakeR2();
  const env = { ASSETS: fakeAssets(), INSTALLS, INSTALLS_TOKEN: 's3cret' };
  const noTok = await call(req('/get', 'curl/8.5.0'), env);
  ok((await noTok.text()) === SCRIPT_BODY, 'no token -> installer script, not stats');
  const badTok = await call(new Request('https://xerj.org/get?token=wrong'), env);
  ok((await badTok.text()) === SCRIPT_BODY, 'wrong token -> installer script, not stats');
  const unset = await call(new Request('https://xerj.org/get?token=s3cret'), { ASSETS: fakeAssets(), INSTALLS: fakeR2() });
  ok((await unset.text()) === SCRIPT_BODY, 'token sent but no secret configured -> script, never stats');
  const empty = await call(new Request('https://xerj.org/get?token='), { ...env, INSTALLS_TOKEN: '' });
  ok((await empty.text()) === SCRIPT_BODY, 'empty token vs empty secret -> script, never stats');
}

// 10. Export aggregation, on a bucket nothing else has written to.
{
  const INSTALLS = fakeR2();
  const env = { ASSETS: fakeAssets(), INSTALLS, INSTALLS_TOKEN: 's3cret' };
  await call(req('/get', 'curl/8.5.0', 'DE'), env);
  await call(req('/get', 'curl/8.5.0', 'FR'), env);
  await call(req('/get.ps1', 'WindowsPowerShell/5.1', 'US'), env);
  await call(req('/get', 'Mozilla/5.0 (compatible; Googlebot/2.1)', 'US'), env);
  ok(INSTALLS.store.size === 4, 'four requests recorded', `got ${INSTALLS.store.size}`);

  const res = await call(new Request('https://xerj.org/get?token=s3cret'), env);
  const j = JSON.parse(await res.text());
  ok(j.ok === true, 'export returns json');
  ok(res.headers.get('content-type') === 'application/json', 'export is json, not text/plain');
  ok(j.total_requests === 4, 'export counts every recorded request', `got ${j.total_requests}`);
  ok(j.installer_shaped_requests === 3, 'export separates installer-shaped from bot', `got ${j.installer_shaped_requests}`);
  ok(j.by_country.DE === 1 && j.by_country.FR === 1 && j.by_country.US === 2, 'country histogram', JSON.stringify(j.by_country));
  ok(j.by_script.get === 3 && j.by_script['get.ps1'] === 1, 'script histogram', JSON.stringify(j.by_script));
  ok(typeof j.caveat === 'string' && j.caveat.includes('not an install'), 'export ships its own caveat');
  const raw = await call(new Request('https://xerj.org/get?token=s3cret&format=raw'), env);
  ok(JSON.parse(await raw.text()).rows.length === 4, 'raw export returns rows');
  const capped = await call(new Request('https://xerj.org/get?token=s3cret&days=99999'), env);
  ok(JSON.parse(await capped.text()).window_days === 365, 'days window is capped at 365');
  const floored = await call(new Request('https://xerj.org/get?token=s3cret&days=-5'), env);
  ok(JSON.parse(await floored.text()).window_days === 1, 'negative days floored to 1');
}

console.log('──');
if (failures) { console.log(`FUNCTIONS TEST FAILED (${failures} failing)`); process.exit(1); }
console.log('FUNCTIONS TEST PASSED');
