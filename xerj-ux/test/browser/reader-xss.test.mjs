// Hostile documents, real Chrome, the real SPA under the shipped CSP.
// Run: node --test xerj-ux/test/browser/     (needs Chrome; Node >= 22)
import test from 'node:test';
import assert from 'node:assert/strict';
import { setup, skipReason, openGuest, openOperator, census, assertCensusInert, assertNotPwned } from './harness.mjs';
import { PAYLOADS, HOSTILE_ID, hostileAttachment, hostileAttachment2, HL_PRE, HL_POST } from '../fixtures/hostile.mjs';

const skip = skipReason();
let ctx;
test.before(async () => { if (!skip) ctx = await setup(); });
test.after(async () => { if (ctx) await ctx.teardown(); });

const readerHash = (id) => `#/reader?index=ax-inbox&id=${encodeURIComponent(id)}`;

test('CONTROL: the fixtures are live — the same subject through innerHTML gets the page pwned', { skip }, async () => {
  const page = await ctx.browser.newPage();
  await page.goto(`${ctx.engine.origin}/__blank`);
  await page.eval(`sessionStorage.setItem('xerj.share', 'CANARY-KEY'), true`);
  await page.goto(`${ctx.engine.origin}/__canary`);
  await page.waitFor('window.__xerjPwned > 0', { label: 'the canary payload to execute' });
  const exfil = await (async () => { for (let i = 0; i < 40; i++) { const r = page.requests.find((x) => x.url.startsWith('https://evil.example/')); if (r) return r.url; await new Promise((res) => setTimeout(res, 50)); } return null; })();
  assert.ok(exfil && exfil.includes('CANARY-KEY'), 'and the harness SEES the key leaving for evil.example — so "no such request" below means something');
  await page.close();
});

test('guest reader: a hostile email with hostile attachments renders inert', { skip }, async () => {
  const page = await openGuest(ctx, { hash: readerHash(HOSTILE_ID) });
  await page.waitFor(`document.querySelector('[data-shape="email"]') && document.querySelectorAll('[data-rd-block="attachments"] .rd-att').length === 2 && document.querySelector('.rd-egroups')`, { label: 'email + attachments + graph' });
  // Search like a person: type, press Enter. The fragments that come back
  // were shaped by the attacker (they forge our delimiters, close a </mark>…).
  await page.eval(`(() => { const i = document.querySelector('.rd-q'); i.value = 'invoice'; i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return true; })()`);
  await page.waitFor(`document.querySelectorAll('.rd-card mark').length >= 1`, { label: 'highlighted results' });
  const c = await census(page, '[data-guest-main]');
  assertCensusInert(c, 'guest reader');
  // The person SEES what the document says…
  for (const needle of [PAYLOADS.script, PAYLOADS.imgOnerror, PAYLOADS.svgOnload, '<html><body onload=', 'https://evil.example/pay', hostileAttachment._source.attachment_name, hostileAttachment2._source.attachment_name]) {
    assert.ok(c.text.includes(needle), `displayed as text: ${needle.slice(0, 40)}`);
  }
  // …highlights are <mark> elements, and the delimiters never show.
  assert.ok(c.marks >= 1, 'highlight fragments produce <mark>');
  assert.ok(!c.text.includes(HL_PRE) && !c.text.includes(HL_POST));
  // …and the graph panel's hostile node titles / edge types are text too.
  const g = await census(page, '.rd-graph');
  assertCensusInert(g, 'graph panel');
  assert.ok(g.anchors === 3 && g.text.includes(PAYLOADS.script));
  await assertNotPwned(page, ctx.engine.origin, 'guest reader');

  // The key is still where the share page put it, and nowhere else.
  const where = await page.eval(`({ ss: Object.keys(sessionStorage), ls: Object.keys(localStorage).filter((k) => /share|key/i.test(k)), cookie: document.cookie, html: document.documentElement.outerHTML.includes(${JSON.stringify('Z3Vlc3Qta2V5')}) })`);
  assert.deepEqual(where.ss, ['xerj.share']);
  assert.deepEqual(where.ls, []);
  assert.equal(where.cookie, '');
  assert.equal(where.html, false, 'the key never appears in the DOM');
  await page.close();
});

test('guest reader: clicking a hostile attachment opens IT — by route, not by its name', { skip }, async () => {
  const page = await openGuest(ctx, { hash: readerHash(HOSTILE_ID) });
  await page.waitFor(`document.querySelectorAll('[data-rd-block="attachments"] .rd-att').length === 2`);
  // att-2 is NAMED "javascript:…". Click it like a person would.
  await page.eval(`(document.querySelectorAll('[data-rd-block="attachments"] .rd-att')[1].click(), true)`);
  await page.waitFor(`document.querySelector('[data-shape="attachment"]') && location.hash.includes('id=att-2') && /FROM EMAIL/.test(document.querySelector('[data-guest-main]').textContent)`, { label: 'the attachment record + its parent email' });
  const c = await census(page, '[data-guest-main]');
  assertCensusInert(c, 'attachment record');
  assert.ok(c.text.includes(hostileAttachment2._source.attachment_name));
  assert.ok(c.text.includes('FROM EMAIL'), 'the parent email is linked back');
  await assertNotPwned(page, ctx.engine.origin, 'attachment click');
  // every other shape, by route
  for (const [id, shape] of [['att-1', 'attachment'], ['pdf-1', 'pdf'], ['sym-1', 'code-symbol'], ['gen-1', 'generic'], ['file-1', 'file']]) {
    await page.setHash(readerHash(id));
    await page.waitFor(`document.querySelector('[data-shape="${shape}"]') && location.hash.includes(${JSON.stringify('id=' + id)})`, { label: shape });
    assertCensusInert(await census(page, '[data-guest-main]'), shape);
  }
  // the file record lists the records that came out of the file, as links
  await page.waitFor(`/RECORDS IN THIS FILE · 1/.test(document.querySelector('[data-guest-main]').textContent)`, { label: 'file record siblings' });
  assertCensusInert(await census(page, '[data-guest-main]'), 'file record');
  await assertNotPwned(page, ctx.engine.origin, 'all shapes');
  await page.close();
});

test('guest corpus home renders hostile format names inert', { skip }, async () => {
  const page = await openGuest(ctx, { hash: '#/corpus' });
  await page.waitFor(`document.querySelector('[data-corpus-index="ax-inbox"] .cp-card__nums')`, { label: 'the shared dataset card' });
  const c = await census(page, '[data-guest-main]');
  assertCensusInert(c, 'guest corpus');
  assert.ok(c.text.includes(`eml${PAYLOADS.imgOnerror}`), 'a hostile format bucket is text');
  assert.ok(c.text.includes('7 records'));
  await assertNotPwned(page, ctx.engine.origin, 'guest corpus');
  await page.close();
});

test('a hostile hash route cannot inject or escape', { skip }, async () => {
  const evil = `#/reader?index=ax-inbox&id=${encodeURIComponent(PAYLOADS.imgOnerror)}&brain=${encodeURIComponent('../../_cat')}`;
  const page = await openGuest(ctx, { hash: evil });
  await page.waitFor(`/No such record|Not found/.test(document.querySelector('[data-guest-main]')?.textContent || '')`, { label: 'not-found state' });
  assertCensusInert(await census(page, '[data-guest-main]'), 'hostile route');
  // an index that is not in the share is ignored, not requested
  ctx.engine.state.log.length = 0;
  await page.setHash('#/reader?index=hr-salaries&id=1');
  await page.waitFor(`/No such record|Not found/.test(document.querySelector('[data-guest-main]')?.textContent || '')`);
  assert.deepEqual(ctx.engine.apiLog().filter((r) => r.path.includes('hr-salaries')), []);
  await assertNotPwned(page, ctx.engine.origin, 'hostile route');
  await page.close();
});

test('operator reader + corpus home: the same documents, through the session proxy, inert', { skip }, async () => {
  const page = await openOperator(ctx, readerHash(HOSTILE_ID));
  await page.waitFor(`document.querySelector('[data-safe-mount="reader"] [data-shape="email"]') && document.querySelectorAll('[data-rd-block="attachments"] .rd-att').length === 2`, { label: 'operator reader' });
  await page.eval(`(() => { const i = document.querySelector('.rd-q'); i.value = 'invoice'; i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return true; })()`);
  await page.waitFor(`document.querySelectorAll('.rd-card mark').length >= 1`, { label: 'highlights through the session proxy' });
  const c = await census(page, '[data-safe-mount="reader"]');
  assertCensusInert(c, 'operator reader');
  assert.ok(c.text.includes(PAYLOADS.script) && c.marks >= 1);
  await assertNotPwned(page, ctx.engine.origin, 'operator reader');

  await page.setHash('#/corpus');
  await page.waitFor(`document.querySelector('[data-safe-mount="corpus"] [data-corpus-index="ax-inbox"]')`, { label: 'catalog card' });
  const k = await census(page, '[data-safe-mount="corpus"]');
  assertCensusInert(k, 'operator corpus');
  assert.ok(k.text.includes(`pdf${PAYLOADS.script}`), 'hostile catalog format');
  assert.ok(k.text.includes(`email_from${PAYLOADS.imgOnerror}`), 'hostile field name');
  // a sample-query button carries its query as DATA and lands in the reader
  await page.eval(`(document.querySelector('[data-corpus-query]').click(), true)`);
  await page.waitFor(`location.hash.startsWith('#/reader?index=ax-inbox') && document.querySelector('.rd-q')?.value.includes('invoice')`, { label: 'sample query → reader' });
  assertCensusInert(await census(page, '[data-safe-mount="reader"]'), 'reader after sample query');
  await assertNotPwned(page, ctx.engine.origin, 'operator corpus');
  await page.close();
});

test('operator Discover: hostile hits, facet values and FIELD NAMES stay text', { skip }, async () => {
  const page = await openOperator(ctx, '#/discover');
  await page.waitFor(`document.querySelectorAll('.hits-list .hit').length >= 7 && document.querySelector('.facet')`, { label: 'Discover hits + facets' });
  // Discover is the shell's string-built HTML (esc() at every interpolation),
  // with legitimate <svg> bars and style="" attributes — so census every
  // panel that carries document data, allowing those two and nothing else.
  // (The citations panel links out to GitHub; its hrefs are code constants.)
  let text = '';
  for (const panel of ['searchbox', 'hits', 'facets', 'histogram', 'searchMetrics', 'dsl']) {
    const c = await census(page, `[data-panel="${panel}"]`);
    assertCensusInert(c, `Discover/${panel}`, { allowSvg: true, allowStyleAttr: true });
    text += c.text;
  }
  // …and across the WHOLE app no element carries an on* attribute.
  assert.deepEqual((await census(page, '#app')).onAttrs, []);
  assert.ok(text.includes(PAYLOADS.script), 'hit source is shown as text');
  assert.ok(text.includes(PAYLOADS.attrBreakout), 'a hostile facet value is text');
  assert.ok(text.includes(`field${PAYLOADS.imgOnerror}`.toUpperCase()) || text.includes(`field${PAYLOADS.imgOnerror}`), 'a hostile FIELD NAME is text');
  assert.ok(text.includes('ax-inbox'), 'the resolved index is named');
  // the executed request is what the REQUEST panel shows
  const shown = await page.eval(`document.querySelector('.qdsl')?.textContent || ''`);
  const sent = ctx.engine.state.log.filter((r) => r.path.endsWith('/ax-inbox/search')).length;
  assert.ok(sent >= 1 && /"track_total_hits": true/.test(shown) && /"by_email_from"/.test(shown));
  await assertNotPwned(page, ctx.engine.origin, 'Discover');

  // A failing search shows the failure and ZERO rows — never a sample.
  ctx.engine.state.fail.proxySearch = 500;
  await page.eval(`(() => { const i = document.querySelector('[data-search-input]'); i.value = 'earnout'; i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); i.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true })); return true; })()`);
  await page.waitFor(`document.querySelector('[data-hits-state="error"]')`, { label: 'the error state' });
  const after = await page.eval(`({ rows: document.querySelectorAll('.hits-list .hit').length, text: document.querySelector('[data-panel="hits"], .panel')?.closest('#app').textContent })`);
  assert.equal(after.rows, 0, 'no rows under a failed search');
  assert.ok(/Search failed/.test(after.text) && /No rows are shown in its place/.test(after.text));
  ctx.engine.state.fail = {};
  await page.close();
});

test('the whole operator console runs under the shipped policy with zero violations', { skip }, async () => {
  // The policy is new; the console is not. Every section the operator can
  // reach has to keep working under `script-src 'self'; connect-src 'self'`,
  // or the policy would have broken it silently (a blocked fetch is just a
  // failed fetch). The fake node answers most telemetry calls 404 — that is
  // fine; what must not happen is a VIOLATION.
  const page = await openOperator(ctx, '#/corpus');
  await page.waitFor(`document.querySelector('[data-nav-status]')`, { label: 'operator shell' });
  const routes = ['#/dashboards/ai-overview', '#/dashboards/rag-quality', '#/dashboards/vector-index', '#/dashboards/agent-memory', '#/dashboards/second-brain?brain=inbox',
    '#/dashboards/logs-overview', '#/dashboards/anomaly-detect', '#/dashboards/ingest-pipeline', '#/dashboards/system', '#/discover', '#/reader', '#/alerts', '#/data', '#/users', '#/settings', '#/corpus'];
  for (const r of routes) {
    await page.setHash(r);
    await page.waitFor(`document.getElementById('app').getAttribute('aria-busy') === 'false' && document.querySelector('.h-scene, h1')`, { label: r });
    await new Promise((res) => setTimeout(res, 150));
    const v = await page.eval('window.__cspViolations');
    assert.deepEqual(v, [], `${r}: Content-Security-Policy violation`);
    // Honest data-source status, per view: this node holds no telemetry, so a
    // telemetry dashboard is showing the in-memory sample — and must SAY so.
    // Only the views that really read the engine may say LIVE.
    const pill = await page.eval(`document.querySelector('[data-nav-status]')?.textContent || ''`);
    const telemetry = /ai-overview|rag-quality|vector-index|agent-memory|logs-overview|anomaly-detect|ingest-pipeline|system|alerts|users/.test(r);
    if (telemetry) {
      assert.match(pill, /MOCK FALLBACK|SAMPLE DATA/, `${r}: a sample-backed view must be labelled (${pill})`);
      assert.doesNotMatch(pill, /^LIVE/, `${r}: a sample-backed view must never claim LIVE`);
    }
    if (/corpus|discover|data$/.test(r)) assert.match(pill, /^LIVE/, `${r}: ${pill}`);
  }
  // theme switch + edit mode toggle exercise the shell's own handlers
  await page.eval(`(document.querySelector('[data-theme-set="day"], [data-theme="day"], [data-theme-toggle]')?.click(), true)`);
  await assertNotPwned(page, ctx.engine.origin, 'operator console tour');
  await page.close();
});
