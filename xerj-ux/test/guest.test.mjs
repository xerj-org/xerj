// data/guest.js — the share-link guest contract, from the UI side.
//   sessionStorage['xerj.share'] = {api_key, index, brain, expires_at, label}
// Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  SHARE_KEY, parseShare, readShare, clearShare, isExpired, guestBanner, fmtRemaining, guestAuthHeader,
  guestUrl, guestIndexExpr, guestAllows, guestRequest, GuestError, GUEST_OPS, installGuestGuard, watchExpiry, validName,
} from '../src/data/guest.js';
import { makeGuestTransport } from '../src/data/transport-guest.js';
import { makeReaderApi, FATAL_KINDS } from '../src/data/reader-api.js';
import { ENGINE_403_BODY, LEAK_MARKERS } from './fixtures/hostile.mjs';

const NOW = Date.parse('2026-09-18T12:00:00Z');
const KEY = 'aWQtMTIzNDU2Nzg5MDpzZWNyZXQtc2VjcmV0LXNlY3JldA==';
const record = (over = {}) => ({ api_key: KEY, index: 'ax-inbox', brain: 'inbox', expires_at: '2026-09-25T18:00:00Z', label: 'Q3 board pack', ...over });
const share = (over) => parseShare(record(over), NOW);

function storage(initial = {}) {
  const m = new Map(Object.entries(initial));
  return { getItem: (k) => (m.has(k) ? m.get(k) : null), setItem: (k, v) => m.set(k, String(v)), removeItem: (k) => m.delete(k), _m: m };
}

test('the contract record parses', () => {
  const s = share();
  assert.deepEqual(s.indices, ['ax-inbox']);
  assert.equal(s.brain, 'inbox');
  assert.equal(s.label, 'Q3 board pack');
  assert.equal(s.expires_at, Date.parse('2026-09-25T18:00:00Z'));
  assert.equal(guestAuthHeader(s), `ApiKey ${KEY}`);
  assert.ok(Object.isFrozen(s) && Object.isFrozen(s.indices));
  assert.deepEqual(share({ index: 'ax-inbox,ax-contracts, ax-inbox' }).indices, ['ax-inbox', 'ax-contracts']);
  assert.equal(share({ brain: null }).brain, null);
  assert.equal(share({ label: null }).label, 'ax-inbox', 'no label → the index names');
  assert.equal(parseShare(JSON.stringify(record()), NOW).index, 'ax-inbox', 'the stored form is a JSON string');
  assert.equal(share({ expires_at: Date.parse('2026-09-25T18:00:00Z') }).expires_at, Date.parse('2026-09-25T18:00:00Z'));
});

test('FAILS CLOSED: anything less than a complete, unexpired, well-formed record is not a share', () => {
  const bad = {
    'no key': { api_key: '' },
    'short key': { api_key: 'abc' },
    'key with a space (header injection)': { api_key: `${KEY} extra` },
    'key with CRLF': { api_key: `${KEY}${String.fromCharCode(13, 10)}x-evil: 1` },
    'no index': { index: '' },
    'wildcard index': { index: '*' },
    'pattern index': { index: 'ax-*' },
    '_all': { index: '_all' },
    'system index': { index: '.xerj_users' },
    'brain namespace': { index: '.xerj-memory-alice' },
    'path traversal': { index: '../_cat/indices' },
    'slash': { index: 'a/b' },
    'query string': { index: 'a?scroll=1m' },
    'uppercase': { index: 'AX-Inbox' },
    'one bad name poisons the list': { index: 'ax-inbox,*' },
    'too many indices': { index: Array.from({ length: 33 }, (_, i) => `i${i}`).join(',') },
    'bad brain': { brain: '../x' },
    'brain pattern': { brain: '*' },
    'no expiry': { expires_at: null },
    'missing expiry': { expires_at: undefined },
    'unparseable expiry': { expires_at: 'next tuesday' },
    'expired': { expires_at: '2026-09-18T11:59:59Z' },
    'expires exactly now': { expires_at: '2026-09-18T12:00:00Z' },
    'negative expiry': { expires_at: -5 },
  };
  for (const [why, over] of Object.entries(bad)) assert.equal(share(over), null, why);
  for (const raw of [null, undefined, '', 'not json', '[]', '"str"', '42', 'null', '{"api_key":1}']) assert.equal(parseShare(raw, NOW), null, String(raw));
  assert.ok(!validName('_all') && !validName('a..b') && !validName('') && !validName(7) && validName('ax-inbox_2.v1'));
});

test('readShare / clearShare tolerate a blocked storage', () => {
  const st = storage({ [SHARE_KEY]: JSON.stringify(record()) });
  assert.equal(readShare(st, NOW).index, 'ax-inbox');
  clearShare(st);
  assert.equal(readShare(st, NOW), null);
  const blocked = { getItem() { throw new Error('denied'); }, removeItem() { throw new Error('denied'); } };
  assert.equal(readShare(blocked, NOW), null);
  assert.doesNotThrow(() => clearShare(blocked));
  assert.equal(readShare(null, NOW), null);
});

test('the banner says guest, read-only, and when it ends', () => {
  const b = guestBanner(share(), NOW);
  assert.match(b, /^Guest · read-only · Q3 board pack · expires 2026-09-25 18:00 UTC \(in 7d 6h\)$/);
  assert.equal(fmtRemaining(0), 'now');
  assert.equal(fmtRemaining(90_000), 'in 1m');
  assert.equal(fmtRemaining(3_700_000), 'in 1h 1m');
  assert.equal(fmtRemaining(5_000), 'in 5s');
  assert.ok(isExpired(share(), share().expires_at) && !isExpired(share(), NOW) && isExpired(null, NOW));
});

test('a guest can build exactly four kinds of URL, all from the share itself', () => {
  const s = share({ index: 'ax-inbox,ax-contracts' });
  assert.deepEqual(Object.keys(GUEST_OPS).sort(), ['count', 'ego', 'mapping', 'search']);
  assert.equal(guestUrl(s, 'search', { indices: ['ax-inbox'] }), '/ax-inbox/_search');
  assert.equal(guestUrl(s, 'search'), '/ax-inbox,ax-contracts/_search');
  assert.equal(guestUrl(s, 'count', { indices: 'ax-contracts' }), '/ax-contracts/_count');
  assert.equal(guestUrl(s, 'mapping', { indices: ['ax-inbox'] }), '/ax-inbox/_mapping');
  assert.equal(guestUrl(s, 'ego', { query: { node: 'a b&c', hops: 1 } }), '/_graph/inbox/ego?node=a+b%26c&hops=1');
  // not part of the share → no URL at all
  for (const idx of ['other', '*', '_all', '.xerj_users', 'ax-inbox,other', '../x', '']) {
    assert.equal(guestUrl(s, 'search', { indices: [idx] }), null, idx);
    assert.equal(guestIndexExpr(s, [idx]), null, idx);
  }
  assert.equal(guestUrl(s, 'search', { indices: [] }), null);
  for (const op of ['delete', '_doc', 'bulk', '__proto__', 'constructor', 'toString', '']) assert.equal(guestUrl(s, op, {}), null, op);
  assert.equal(guestUrl(share({ brain: null }), 'ego', { query: { node: 'x' } }), null, 'no brain in the share → no graph URL');
  assert.equal(guestUrl(null, 'search'), null);
});

test('guestAllows: the fetch guard\'s allow-list', () => {
  const s = share({ index: 'ax-inbox,ax-contracts' });
  const yes = [['POST', '/ax-inbox/_search'], ['POST', '/ax-inbox,ax-contracts/_search'], ['POST', '/ax-contracts/_count'], ['GET', '/ax-inbox/_mapping'], ['GET', '/_graph/inbox/ego'], ['get', '/_graph/inbox/ego']];
  for (const [m, p] of yes) assert.ok(guestAllows(s, m, p), `${m} ${p}`);
  const no = [
    ['GET', '/_xerj-console/api/v1/me'], ['GET', '/_xerj-console/api/v1/prefs'], ['POST', '/_xerj-console/api/v1/data-sources/connections/built-in/indices/ax-inbox/search'],
    ['GET', '/_xerj-console/api/v1/dashboards'], ['GET', '/_cat/indices'], ['GET', '/_cat/indices/.xerj-memory-*'], ['GET', '/_mapping'], ['GET', '/_cluster/health'],
    ['GET', '/v1/metrics'], ['POST', '/_security/api_key'], ['GET', '/_security/_authenticate'], ['POST', '/_search'], ['POST', '/*/_search'], ['POST', '/_all/_search'],
    ['POST', '/other/_search'], ['POST', '/ax-inbox,other/_search'], ['POST', '/.xerj-memory-inbox-edges/_search'], ['GET', '/.xerj-memory-inbox-edges/_doc/__xerj-brain-meta'],
    ['DELETE', '/ax-inbox'], ['PUT', '/ax-inbox/_doc/1'], ['POST', '/ax-inbox/_doc'], ['POST', '/ax-inbox/_bulk'], ['POST', '/ax-inbox/_update/1'], ['POST', '/ax-inbox/_delete_by_query'],
    ['GET', '/ax-inbox/_search'], ['DELETE', '/ax-inbox/_search'], ['POST', '/ax-inbox/_mapping'], ['PUT', '/ax-inbox/_mapping'],
    ['GET', '/_graph/other/ego'], ['POST', '/_graph/inbox/ego'], ['POST', '/_graph/inbox/link'], ['GET', '/_graph/inbox/overview'], ['DELETE', '/_graph/inbox'],
    ['POST', '/ax-inbox/_search/../../_cat/indices'], ['POST', '/ax-inbox/_search/x'], ['POST', '/ax-inbox%2F..%2F_all/_search'], ['POST', '/%/_search'],
    ['GET', '/_share/abc/claim'], ['POST', '/_memory/x/_recall'],
  ];
  for (const [m, p] of no) assert.ok(!guestAllows(s, m, p), `${m} ${p} must be refused`);
  assert.ok(!guestAllows(share({ brain: null }), 'GET', '/_graph/null/ego'));
  assert.ok(!guestAllows(null, 'POST', '/ax-inbox/_search') && !guestAllows(s, 'POST', null));
});

function fakeFetch(respond) {
  const calls = [];
  const f = async (url, init) => { calls.push({ url, init }); return respond(url, init); };
  f.calls = calls;
  return f;
}
const json = (status, body) => ({ status, ok: status >= 200 && status < 300, json: async () => body, text: async () => JSON.stringify(body) });

test('guestRequest attaches the key, omits cookies, and only on a share URL', async () => {
  const s = share();
  const f = fakeFetch(() => json(200, { hits: { total: { value: 0 }, hits: [] } }));
  await guestRequest(s, 'search', { indices: ['ax-inbox'], body: { query: { match_all: {} } } }, { fetch: f, now: () => NOW });
  assert.equal(f.calls.length, 1);
  const { url, init } = f.calls[0];
  assert.equal(url, '/ax-inbox/_search');
  assert.equal(init.method, 'POST');
  assert.equal(init.credentials, 'omit', 'a guest never rides a console session cookie');
  assert.equal(init.referrerPolicy, 'no-referrer');
  assert.equal(init.cache, 'no-store');
  assert.equal(init.headers.authorization, `ApiKey ${KEY}`);
  assert.deepEqual(JSON.parse(init.body), { query: { match_all: {} } });

  // Not in the share → refused locally: no request, so no key leaves the page.
  for (const args of [['search', { indices: ['other'] }], ['search', { indices: ['*'] }], ['nope', {}]]) {
    await assert.rejects(() => guestRequest(s, ...args, { fetch: f, now: () => NOW }), (e) => e instanceof GuestError && e.kind === 'blocked');
  }
  await assert.rejects(() => guestRequest(share({ brain: null }), 'ego', { query: { node: 'x' } }, { fetch: f, now: () => NOW }), (e) => e.kind === 'blocked');
  assert.equal(f.calls.length, 1, 'refused calls never reach fetch');

  // Expired → refused locally too.
  await assert.rejects(() => guestRequest(s, 'search', { indices: ['ax-inbox'] }, { fetch: f, now: () => s.expires_at + 1 }), (e) => e.kind === 'expired');
  assert.equal(f.calls.length, 1);
});

test('a refusal carries a kind and a status — never the engine\'s words', async () => {
  const s = share();
  const cases = [[401, 'unauthorized'], [403, 'forbidden'], [404, 'not-found'], [500, 'http'], [429, 'http']];
  for (const [status, kind] of cases) {
    const f = fakeFetch(() => json(status, ENGINE_403_BODY));
    let err;
    try { await guestRequest(s, 'search', { indices: ['ax-inbox'] }, { fetch: f, now: () => NOW }); } catch (e) { err = e; }
    assert.ok(err instanceof GuestError, `HTTP ${status}`);
    assert.equal(err.kind, kind);
    assert.equal(err.status, status);
    const everything = JSON.stringify({ ...err, message: err.message, stack: String(err.stack) });
    for (const marker of LEAK_MARKERS) assert.ok(!everything.includes(marker), `HTTP ${status}: error leaks ${marker}`);
    assert.ok(!everything.includes(KEY), 'the error never carries the key');
  }
  const down = fakeFetch(() => { throw new TypeError('Failed to fetch'); });
  await assert.rejects(() => guestRequest(s, 'search', { indices: ['ax-inbox'] }, { fetch: down, now: () => NOW }), (e) => e.kind === 'network');
  assert.deepEqual([...FATAL_KINDS].sort(), ['expired', 'unauthorized']);
});

test('reader-api over the guest transport: a 403 is an empty, labelled result — nothing invented, nothing leaked', async () => {
  const s = share();
  const f = fakeFetch((url) => (url.includes('_mapping') ? json(200, { 'ax-inbox': { mappings: { properties: { body: { type: 'semantic_text' } } } } }) : json(403, ENGINE_403_BODY)));
  const api = makeReaderApi(makeGuestTransport(s, { fetch: f, now: () => NOW }));
  const r = await api.search('ax-inbox', { q: 'salary' });
  assert.deepEqual(r.hits, []);
  assert.equal(r.total, 0);
  assert.equal(r.error, 'not permitted');
  assert.equal(r.kind, 'forbidden');
  const rec = await api.fetchRecord('ax-inbox', 'x');
  assert.equal(rec.hit, null);
  assert.equal(rec.error, 'not permitted');
  const sum = await api.indexSummary('ax-inbox');
  assert.equal(sum.records, 0);
  assert.equal(sum.error, 'not permitted');
  for (const out of [r, rec, sum]) for (const m of LEAK_MARKERS) assert.ok(!JSON.stringify(out).includes(m), m);

  // another index: refused before any request
  const before = f.calls.length;
  const other = await api.search('hr-salaries-2026', { q: 'x' });
  assert.equal(other.error, 'not part of this share');
  assert.deepEqual(other.hits, []);
  assert.equal(f.calls.filter((c) => c.url.includes('hr-salaries')).length, 0);
  assert.ok(f.calls.length === before, 'no request was made for an index outside the share');

  // the graph: 403 → "denied" (a panel state), NOT the end of the session
  const ego = await api.fetchEgo('inbox', 'm1');
  assert.equal(ego.status, 'denied');
  assert.ok(!FATAL_KINDS.has(ego.kind));
  // another brain: never requested
  const n = f.calls.length;
  assert.equal((await api.fetchEgo('alice', 'm1')).status, 'no-brain');
  assert.equal(f.calls.length, n);
  // a guest's brain is the share's, whatever the URL hint says
  assert.equal(await api.resolveBrain('ax-inbox', 'alice'), 'inbox');
});

test('a 401 is fatal (the key was revoked): the view must log the guest out', async () => {
  const s = share();
  const f = fakeFetch(() => json(401, ENGINE_403_BODY));
  const api = makeReaderApi(makeGuestTransport(s, { fetch: f, now: () => NOW }));
  const r = await api.search('ax-inbox', { q: '' });
  assert.equal(r.kind, 'unauthorized');
  assert.ok(FATAL_KINDS.has(r.kind));
  assert.deepEqual(r.hits, []);
  const rec = await api.fetchRecord('ax-inbox', 'x');
  assert.ok(FATAL_KINDS.has(rec.kind));
});

test('the fetch guard refuses everything but the share\'s own operations', async () => {
  const s = share();
  const reached = [];
  const win = {
    location: { href: 'http://localhost:9550/_xerj-console/#/reader', origin: 'http://localhost:9550' },
    Response: globalThis.Response,
    fetch: async (input, init) => { reached.push(`${(init && init.method) || 'GET'} ${typeof input === 'string' ? input : input.url}`); return new Response('{}', { status: 200 }); },
  };
  installGuestGuard(s, win);
  const guarded = win.fetch;
  installGuestGuard(s, win);
  assert.equal(win.fetch, guarded, 'idempotent');

  const blocked = [
    ['/_xerj-console/api/v1/me'], ['/_xerj-console/api/v1/prefs'], ['/_xerj-console/api/v1/dashboards'],
    ['/_xerj-console/api/v1/data-sources/connections/built-in/indices'], ['/_cat/indices/.xerj-memory-*'], ['/_mapping'], ['/v1/metrics'],
    ['/other/_search', { method: 'POST' }], ['/ax-inbox/_doc/1', { method: 'PUT' }], ['/ax-inbox', { method: 'DELETE' }],
    ['https://evil.example/k?x'], ['//evil.example/k'], ['http://localhost:9551/ax-inbox/_search', { method: 'POST' }],
    [{ url: 'https://evil.example/req', method: 'POST' }], ['http://[::1'],
  ];
  for (const [input, init] of blocked) {
    const r = await win.fetch(input, init);
    assert.equal(r.status, 403, JSON.stringify(input));
    assert.deepEqual(await r.json(), { error: { type: 'guest_blocked' } });
  }
  assert.deepEqual(reached, [], 'nothing refused ever reached the network');
  assert.equal(win.__xerjGuestBlocked.length, blocked.length);

  await win.fetch('/ax-inbox/_search', { method: 'POST' });
  await win.fetch('/ax-inbox/_mapping');
  await win.fetch('/_graph/inbox/ego?node=x');
  assert.deepEqual(reached, ['POST /ax-inbox/_search', 'GET /ax-inbox/_mapping', 'GET /_graph/inbox/ego?node=x']);
});

test('the guard also covers XMLHttpRequest and sendBeacon — same allow-list, same origin rule', () => {
  // PR #945 review: only `fetch` was wrapped; an XHR from page code reached
  // the server (and got 401 there). The guard is a tripwire, not the boundary,
  // but a tripwire with a hole is a false statement in the docs.
  const s = share();
  const opened = [];
  const beaconed = [];
  class FakeXHR { open(method, url) { opened.push(`${method} ${url}`); } }
  const win = {
    location: { href: 'http://localhost:9550/_xerj-console/#/reader', origin: 'http://localhost:9550' },
    Response: globalThis.Response, DOMException: globalThis.DOMException,
    fetch: async () => new Response('{}', { status: 200 }),
    XMLHttpRequest: FakeXHR,
    navigator: { sendBeacon: (url) => { beaconed.push(url); return true; } },
  };
  installGuestGuard(s, win);
  installGuestGuard(s, win);
  const x = new FakeXHR();
  for (const [m, u] of [['GET', '/_xerj-console/api/v1/me'], ['POST', '/other/_search'], ['GET', 'https://evil.example/k'], ['DELETE', '/ax-inbox'], ['GET', '/_cat/indices']]) {
    assert.throws(() => x.open(m, u), (e) => e.name === 'SecurityError', `${m} ${u}`);
  }
  assert.deepEqual(opened, [], 'nothing refused reached XMLHttpRequest.open');
  x.open('POST', '/ax-inbox/_search'); x.open('get', '/ax-inbox/_mapping'); x.open('GET', '/_graph/inbox/ego?node=x');
  assert.deepEqual(opened, ['POST /ax-inbox/_search', 'get /ax-inbox/_mapping', 'GET /_graph/inbox/ego?node=x'], 'the share\'s own operations pass through unchanged');
  assert.equal(win.navigator.sendBeacon('https://evil.example/b', 'key'), false);
  assert.equal(win.navigator.sendBeacon('/_xerj-console/api/v1/audit', 'key'), false);
  assert.equal(win.navigator.sendBeacon('/ax-inbox/_count'), true, 'a beacon to a share operation is the same POST the guard already allows');
  assert.deepEqual(beaconed, ['/ax-inbox/_count']);
  assert.deepEqual(win.__xerjGuestBlocked, [
    'XHR GET /_xerj-console/api/v1/me', 'XHR POST /other/_search', 'XHR GET https://evil.example', 'XHR DELETE /ax-inbox', 'XHR GET /_cat/indices',
    'BEACON POST https://evil.example', 'BEACON POST /_xerj-console/api/v1/audit',
  ]);
  assert.ok(win.XMLHttpRequest.prototype.open.__xerjGuestGuard && win.navigator.sendBeacon.__xerjGuestGuard);
});

test('expiry fires once, re-checks the clock, and survives long timers', () => {
  const s = share({ expires_at: NOW + 150_000 });
  let now = NOW;
  const timers = [];
  const env = { now: () => now, setTimeout: (fn, ms) => { timers.push({ fn, ms }); return timers.length; }, clearTimeout: () => {}, maxStepMs: 60_000 };
  let fired = 0;
  watchExpiry(s, () => fired++, env);
  assert.equal(timers.at(-1).ms, 60_000, 'never sleeps past maxStepMs');
  now += 60_000; timers.at(-1).fn();
  now += 60_000; timers.at(-1).fn();
  assert.equal(timers.at(-1).ms, 30_000);
  assert.equal(fired, 0);
  now += 30_000; timers.at(-1).fn();
  assert.equal(fired, 1);
  timers.at(-1).fn();
  assert.equal(fired, 1, 'fires once');

  // a throttled background tab: the timer comes back late — still expires
  let late = 0;
  now = NOW;
  watchExpiry(s, () => late++, env);
  now += 10 * 86_400_000; timers.at(-1).fn();
  assert.equal(late, 1);

  // already expired at start
  let immediate = 0;
  watchExpiry(share({ expires_at: NOW + 1 }), () => immediate++, { ...env, now: () => NOW + 5 });
  assert.equal(immediate, 1);

  // cancel
  let cancelled = 0;
  now = NOW;
  const cancel = watchExpiry(s, () => cancelled++, env);
  cancel();
  now += 10 * 86_400_000; timers.at(-1).fn();
  assert.equal(cancelled, 0);
});

test('attachments collapse to each file\'s lowest page; highlight asks only for fields the index has', async () => {
  const s = share();
  const sent = [];
  const mid = '<m@x>';
  const pages = [3, 1, 2].map((p) => ({ _index: 'ax-inbox', _id: `big-${p}`, _source: { attachment_name: 'big.pdf', page: p, email_message_id: mid } }));
  const other = { _index: 'ax-inbox', _id: 'img', _source: { attachment_name: 'logo.png', email_message_id: mid } };
  const f = fakeFetch((url, init) => {
    if (url.includes('_mapping')) return json(200, { 'ax-inbox': { mappings: { properties: { body: { type: 'semantic_text' }, email_subject: { type: 'text' } } } } });
    sent.push(JSON.parse(init.body));
    return json(200, { hits: { total: { value: 4 }, hits: [...pages, other] } });
  });
  const api = makeReaderApi(makeGuestTransport(s, { fetch: f, now: () => NOW }));
  const rel = await api.fetchRelated({ _index: 'ax-inbox', _id: 'e', _source: { email_message_id: mid } });
  assert.deepEqual(rel.attachments.map((a) => a._id), ['big-1', 'img']);
  await api.search('ax-inbox', { q: 'invoice' });
  assert.deepEqual(Object.keys(sent.at(-1).highlight.fields), ['body', 'email_subject'], 'attachment_name is not in this mapping, so it is not requested');
  await api.search('ax-inbox', { q: '' });
  assert.equal(sent.at(-1).highlight, undefined, 'no query, no highlight');
  await api.search('ax-inbox', { q: 'x', type: 'semantic' });
  assert.equal(sent.at(-1).highlight, undefined);
});

test('minor (PR #945 review): a deep link survives the sign-in page — only ever as an in-console route', async () => {
  const { stashNext, takeNext, safeRoute, NEXT_KEY } = await import('../src/data/next-route.js');
  const st = storage();
  const link = '#/reader?index=ax-docs&id=3eeb8e72%22%3E&brain=case-file';
  assert.equal(stashNext(st, link), link);
  assert.equal(st.getItem(NEXT_KEY), link);
  assert.equal(takeNext(st), link);
  assert.equal(st.getItem(NEXT_KEY), null, 'read once');
  assert.equal(takeNext(st), '');
  // nothing but `#/…` route characters is kept — or returned, if something else wrote the key
  for (const bad of ['', '#', '#/', 'https://evil.example/', '#//evil.example', '//evil.example', 'javascript:alert(1)', '#/reader?x=javascript:alert(1)', '#/a b', '#/a<script>', `#/${'a'.repeat(3000)}`, null, 42]) {
    assert.equal(safeRoute(bad), '', `rejected: ${String(bad).slice(0, 40)}`);
    assert.equal(stashNext(st, bad), '');
    assert.equal(st.getItem(NEXT_KEY), null);
  }
  st.setItem(NEXT_KEY, 'https://evil.example/');
  assert.equal(takeNext(st), '', 'a planted value is dropped, and removed');
  assert.equal(st.getItem(NEXT_KEY), null);
  // a blocked storage is not an error
  const blocked = { getItem() { throw new Error('denied'); }, setItem() { throw new Error('denied'); }, removeItem() { throw new Error('denied'); } };
  assert.equal(stashNext(blocked, link), link);
  assert.equal(takeNext(blocked), '');
});

test('minor (PR #945 review): an ended share leaves a marker that holds the REASON and nothing else', async () => {
  const { markEnded, readEnded, clearEnded, ENDED_KEY } = await import('../src/data/guest.js');
  const st = storage();
  assert.equal(readEnded(st), null);
  markEnded(st, 'expired');
  assert.equal(st.getItem(ENDED_KEY), 'expired');
  assert.equal(readEnded(st), 'expired');
  markEnded(st, 'Z3Vlc3Qta2V5LWlkOnNlY3JldA=='); // anything that is not a known reason is not stored
  assert.equal(st.getItem(ENDED_KEY), 'invalid');
  st.setItem(ENDED_KEY, '<img src=x onerror=1>');
  assert.equal(readEnded(st), 'invalid', 'a planted value reads as "invalid", never as text');
  clearEnded(st);
  assert.equal(readEnded(st), null);
});
