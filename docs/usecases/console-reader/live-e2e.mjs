// Live end-to-end: the real `xerj` binary on :9550 (auth ON), a real folder of
// email + PDFs indexed by `xerj brain`, then real Chrome:
//   A. operator: passkey setup (virtual authenticator) → corpus home → reader
//   B. guest: a scoped read-only API key in sessionStorage['xerj.share']
// Usage: node live.mjs <origin> <admin-key> <setup-link|-> <shots-dir>
import { writeFileSync } from 'node:fs';
import { launch, sleep } from '../../../xerj-ux/test/browser/cdp.mjs';
import { census, assertCensusInert } from '../../../xerj-ux/test/browser/harness.mjs';

const [origin, adminKey, setupLink, shots] = process.argv.slice(2);
const out = {};
const api = async (method, path, body, key = adminKey) => {
  const r = await fetch(origin + path, { method, headers: { authorization: `ApiKey ${key}`, 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch { /* */ }
  return { status: r.status, json, text };
};
const shot = async (page, name) => {
  await page.send('Emulation.setDeviceMetricsOverride', { width: 1440, height: 1200, deviceScaleFactor: 1, mobile: false });
  await sleep(600);
  const { data } = await page.send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(`${shots}/${name}.png`, Buffer.from(data, 'base64'));
};
const pwned = async (page, label, base = origin) => {
  const f = await page.eval(`({ pwned: window.__xerjPwned || 0, csp: window.__cspViolations || [] })`);
  const foreign = page.requests.map((r) => r.url).filter((u) => /^https?:/.test(u) && !u.startsWith(base + '/') && !/^https:\/\/fonts\.(googleapis|gstatic)\.com\//.test(u));
  out[label] = { pwned: f.pwned, cspViolations: f.csp, dialogs: page.dialogs.length, foreignRequests: foreign };
  if (f.pwned || f.csp.length || page.dialogs.length || foreign.length) throw new Error(`${label}: NOT INERT ${JSON.stringify(out[label])}`);
};

// ---- what did `xerj brain` write? -------------------------------------
const cat = await api('GET', '/_cat/indices?format=json');
out.indices = (cat.json || []).map((i) => `${i.index}:${i['docs.count']}`).sort();
const catalog = await api('POST', '/autoindex-catalog/_search', { query: { term: { doc_kind: 'dataset' } }, size: 50 });
out.datasets = (catalog.json?.hits?.hits || []).map((h) => ({ index: h._source.index_name, records: h._source.record_count, formats: h._source.formats, semantic_field: h._source.semantic_field }));
const emailIndex = out.datasets.find((d) => (d.formats || []).includes('eml'))?.index;
if (!emailIndex) throw new Error('no email dataset in the catalog: ' + JSON.stringify(out.datasets));
const hostile = await api('POST', `/${emailIndex}/_search`, { query: { term: { email_message_id: 'hostile-5@evil.example' } }, size: 10 } /* the extractor stores the id without its <> */);
const clip = (v, n) => (typeof v === 'string' ? v.slice(0, n) + '…' : v);
out.hostileRecords = (hostile.json?.hits?.hits || []).map((h) => ({ id: h._id, subject: clip(h._source.email_subject, 48), attachment: clip(h._source.attachment_name, 32), page: h._source.page }));
const hostileEmail = hostile.json.hits.hits.find((h) => !h._source.attachment_name);
const hostileAtt = hostile.json.hits.hits.find((h) => h._source.attachment_name);
if (!hostileEmail || !hostileAtt) throw new Error('hostile email/attachment not indexed: ' + hostile.text.slice(0, 400));
// highlight fragments from the REAL engine, both spellings, on the ES-compat route
const hl = await api('POST', `/${emailIndex}/_search`, { query: { match: { email_subject: 'invoice' } }, size: 3, highlight: { pre_tags: [''], post_tags: [''], pre_tag: '', post_tag: '', fields: { email_subject: {} } } });
out.engineHighlight = (hl.json?.hits?.hits || []).map((h) => h.highlight?.email_subject?.[0]).filter(Boolean).slice(0, 2);

const browser = await launch();
// The console's WebAuthn relying-party origin is fixed at http://localhost:9200
// (xerj-console-api/src/state.rs, RpConfig::default) — a node on any other
// port refuses passkey enrolment ("relying party origin does not match").
// So the OPERATOR half runs in a second Chrome that resolves localhost:9200 to
// this run's node. Nothing is sent to whatever really listens on :9200: the
// first request is a GET that only THIS branch's bundle answers, and the run
// stops if it does not.
const port = new URL(origin).port;
const opOrigin = 'http://localhost:9200';
const opBrowser = await launch({ extraArgs: [`--host-resolver-rules=MAP localhost:9200 127.0.0.1:${port}`], debugPort: Number(process.env.XERJ_TEST_CDP_PORT2) || 0 });
try {
  // ================= A. operator =====================================
  if (setupLink && setupLink !== '-') {
    const page = await opBrowser.newPage();
    await page.goto(`${opOrigin}/_xerj-console/src/boot.js`);
    const isOurs = await page.eval(`document.body.innerText.includes('guest-app.js')`);
    if (!isOurs) throw new Error('host mapping is not in effect — refusing to touch the node on :9200');
    // …and prove it is THIS run's node, not merely one built from this branch:
    // an index with a random name is created on the node this script booted
    // (direct origin, its own admin key) and must be visible through the mapped
    // origin. It is removed again before any page that lists indices opens.
    const nonce = `e2e-own-${Math.random().toString(36).slice(2, 12)}`;
    const made = await api('PUT', `/${nonce}`, { settings: { number_of_shards: 1 } });
    if (made.status >= 300) throw new Error(`could not create the ownership probe index: ${made.text.slice(0, 200)}`);
    const seenThroughMap = await page.eval(`fetch(${JSON.stringify(`/${nonce}/_count`)}, { headers: { authorization: ${JSON.stringify(`ApiKey ${adminKey}`)} } }).then((r) => r.status).catch(() => 0)`);
    await api('DELETE', `/${nonce}`);
    if (seenThroughMap !== 200) throw new Error(`the node behind ${opOrigin} is not the node this run booted (probe index answered ${seenThroughMap}) — refusing to enrol a passkey on it`);
    out.operatorNodeIsOurs = true;
    await page.send('WebAuthn.enable');
    await page.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true } });
    await page.goto(opOrigin + new URL(setupLink).pathname + new URL(setupLink).hash);
    await page.waitFor(`document.getElementById('btn-enrol') && !document.getElementById('btn-enrol').disabled && document.readyState === 'complete'`, { label: 'setup page' });
    await sleep(800);
    await page.eval(`(document.getElementById('email').value = 'op@acme.example', document.getElementById('display').value = 'Op', document.getElementById('btn-enrol').click(), true)`);
    await page.waitFor(`location.pathname === '/_xerj-console/' && !!document.querySelector('[data-nav-status]')`, { timeoutMs: 30000, label: 'console after passkey enrolment' });
    out.operator = { landedOn: await page.eval('location.hash') };

    await page.setHash('#/corpus');
    await page.waitFor(`document.querySelector('[data-safe-mount="corpus"] [data-corpus-index]')`, { label: 'corpus cards', timeoutMs: 20000 });
    const corpus = await census(page, '[data-safe-mount="corpus"]');
    assertCensusInert(corpus, 'live operator corpus');
    out.operator.corpusCards = await page.eval(`[...document.querySelectorAll('[data-corpus-index]')].map((c) => c.getAttribute('data-corpus-index') + ' | ' + (c.querySelector('.cp-card__nums')?.textContent || '').replace(/\\s+/g, ' '))`);
    out.operator.corpusPill = await page.eval(`document.querySelector('[data-nav-status]').textContent`);
    await shot(page, 'live-operator-corpus');

    await page.setHash(`#/reader?index=${encodeURIComponent(emailIndex)}&id=${encodeURIComponent(hostileEmail._id)}`);
    await page.waitFor(`document.querySelector('[data-shape="email"]') && document.querySelectorAll('[data-rd-block="attachments"] .rd-att').length >= 1 && !/Walking brain/.test(document.querySelector('.rd-graph').textContent)`, { label: 'hostile email + attachment + graph state', timeoutMs: 20000 });
    await page.eval(`(() => { const i = document.querySelector('.rd-q'); i.value = 'invoice'; i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return true; })()`);
    await page.waitFor(`document.querySelectorAll('.rd-card').length >= 1 && !/SEARCHING/.test(document.querySelector('[data-rd-slot="list-head"]').textContent)`, { label: 'search results' });
    const rd = await census(page, '[data-safe-mount="reader"]');
    assertCensusInert(rd, 'live operator reader');
    out.operator.reader = {
      title: await page.eval(`document.querySelector('.rd-title').textContent.slice(0, 80)`),
      attachments: await page.eval(`[...document.querySelectorAll('[data-rd-block="attachments"] .rd-att__name')].map((a) => a.textContent.slice(0, 60))`),
      marks: rd.marks,
      listHead: await page.eval(`document.querySelector('[data-rd-slot="list-head"]').textContent`),
      graph: await page.eval(`document.querySelector('.rd-graph').textContent.replace(/\\s+/g, ' ').slice(0, 260)`),
      pill: await page.eval(`document.querySelector('[data-nav-status]').textContent`),
    };
    await shot(page, 'live-operator-reader');

    await page.setHash('#/discover');
    await page.waitFor(`document.querySelector('.hits-list .hit') || document.querySelector('[data-hits-state="error"]')`, { label: 'discover', timeoutMs: 20000 });
    out.operator.discover = await page.eval(`({ eyebrow: [...document.querySelectorAll('.panel .key')].map((k) => k.textContent).filter((t) => /RESULTS|DATE_HISTOGRAM|REQUEST/.test(t)), rows: document.querySelectorAll('.hits-list .hit').length, error: document.querySelector('[data-hits-state="error"]')?.textContent || null, facets: [...document.querySelectorAll('.facet > .key')].map((k) => k.textContent), indexButtons: [...document.querySelectorAll('[data-search-index]')].map((b) => b.textContent) })`);
    await shot(page, 'live-operator-discover');
    await pwned(page, 'operatorInert', opOrigin);
    await page.close();
  }

  // ================= B. guest =========================================
  const brain = process.env.BRAIN || 'casefile';
  const mint = await api('POST', '/_security/api_key', { name: 'e2e-guest', expiration: '1h', role_descriptors: { guest: { indices: [{ names: [emailIndex, `.xerj-memory-${brain}-edges`], privileges: ['read'] }] } } });
  if (mint.status !== 200) throw new Error('mint failed: ' + mint.text.slice(0, 300));
  const guestKey = mint.json.encoded;
  // what the key can and cannot do, asked of the REAL engine
  const others = (cat.json || []).map((i) => i.index).filter((n) => n !== emailIndex && !n.startsWith('.'));
  out.guestKey = {
    ownSearch: (await api('POST', `/${emailIndex}/_search`, { size: 0 }, guestKey)).status,
    ownMapping: (await api('GET', `/${emailIndex}/_mapping`, null, guestKey)).status,
    ego: (await api('GET', `/_graph/${brain}/ego?node=${encodeURIComponent(hostileEmail._id)}&hops=1&direction=both&limit=5&include_nodes=true&include_evidence=false`, null, guestKey)).status,
    otherIndexSearch: others[0] ? (await api('POST', `/${others[0]}/_search`, { size: 0 }, guestKey)).status : 'n/a',
    catalogSearch: (await api('POST', '/autoindex-catalog/_search', { size: 0 }, guestKey)).status,
    write: (await api('PUT', `/${emailIndex}/_doc/evil`, { x: 1 }, guestKey)).status,
    consoleMe: (await fetch(origin + '/_xerj-console/api/v1/me', { headers: { authorization: `ApiKey ${guestKey}` } })).status,
  };

  const page = await browser.newPage();
  const seen = [];
  await page.goto(`${origin}/_xerj-console/src/theme-boot.js`); // any same-origin document, to seed sessionStorage
  const record = { api_key: guestKey, index: emailIndex, brain, expires_at: new Date(Date.now() + 3600_000).toISOString(), label: 'Casefile (live e2e)' };
  await page.eval(`sessionStorage.setItem('xerj.share', ${JSON.stringify(JSON.stringify(record))}), true`);
  page.requests.length = 0;
  await page.goto(`${origin}/_xerj-console/#/reader?index=${encodeURIComponent(emailIndex)}&id=${encodeURIComponent(hostileEmail._id)}&brain=${brain}`);
  await page.waitFor(`document.querySelector('[data-shape="email"]') && document.querySelectorAll('[data-rd-block="attachments"] .rd-att').length >= 1 && !/Walking brain/.test(document.querySelector('.rd-graph').textContent)`, { label: 'guest: hostile email', timeoutMs: 20000 });
  await page.eval(`(() => { const i = document.querySelector('.rd-q'); i.value = 'invoice'; i.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); return true; })()`);
  await page.waitFor(`document.querySelectorAll('.rd-card mark').length >= 1`, { label: 'guest: highlighted results from the real engine' });
  const g = await census(page, '[data-guest-main]');
  assertCensusInert(g, 'live guest reader');
  out.guest = {
    banner: await page.eval(`document.querySelector('[data-guest-banner-text]').textContent`),
    nav: await page.eval(`[...document.querySelectorAll('nav a')].map((a) => a.textContent)`),
    title: await page.eval(`document.querySelector('.rd-title').textContent.slice(0, 80)`),
    attachments: await page.eval(`[...document.querySelectorAll('[data-rd-block="attachments"] .rd-att__name')].map((a) => a.textContent.slice(0, 60))`),
    marks: g.marks,
    graph: await page.eval(`document.querySelector('.rd-graph').textContent.replace(/\\s+/g, ' ').slice(0, 300)`),
  };
  await shot(page, 'live-guest-reader');
  // open the hostile-named attachment by clicking it
  await page.eval(`(document.querySelector('[data-rd-block="attachments"] .rd-att').click(), true)`);
  await page.waitFor(`document.querySelector('[data-shape="attachment"]') && /FROM EMAIL/.test(document.querySelector('[data-guest-main]').textContent)`, { label: 'guest: attachment page' });
  out.guest.attachmentPage = await page.eval(`({ eyebrow: document.querySelector('.rd-eyebrow').textContent, title: document.querySelector('.rd-title').textContent.slice(0, 70), bodyChars: document.querySelector('.rd-body').textContent.length })`);
  assertCensusInert(await census(page, '[data-guest-main]'), 'live guest attachment');
  await shot(page, 'live-guest-attachment');
  await page.setHash('#/corpus');
  await page.waitFor(`document.querySelector('[data-corpus-index] .cp-card__nums')`, { label: 'guest corpus' });
  out.guest.corpus = await page.eval(`document.querySelector('[data-corpus-index]').textContent.replace(/\\s+/g, ' ').slice(0, 200)`);
  await shot(page, 'live-guest-corpus');
  await pwned(page, 'guestInert');
  const reqs = page.requests.filter((r) => r.url.startsWith(origin + '/') && !r.url.startsWith(origin + '/_xerj-console/')).map((r) => `${r.method} ${new URL(r.url).pathname}`);
  out.guest.dataRequests = [...new Set(reqs)];
  out.guest.consoleApiRequests = page.requests.filter((r) => r.url.includes('/_xerj-console/api/')).length;
  await page.close();
  await api('DELETE', '/_security/api_key', { ids: [mint.json.id] });
} finally {
  await browser.close();
  await opBrowser.close();
}
console.log(JSON.stringify(out, null, 2));
