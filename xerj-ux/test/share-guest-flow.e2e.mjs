// ============================================================
// Share-link guest page — the real flow, in a real browser.
//
//   XERJ_URL=http://127.0.0.1:9510 XERJ_DATA_DIR=/path/to/data \
//     node xerj-ux/test/share-guest-flow.e2e.mjs
//
// Needs a THROWAWAY node with authentication on (it creates an index and
// shares) and a Chrome/Chromium binary (CHROME_BIN, or one of the usual
// places). `.github/scripts/share-links-smoke.sh` boots the node and runs this.
//
// No npm dependencies on purpose: Chrome is driven over the DevTools
// protocol's *pipe* transport (`--remote-debugging-pipe`: NUL-delimited JSON
// on fd 3/4), so there is no WebSocket client, no Playwright, no install step,
// and the file is not named *.test.mjs because `node --test` in the ux-tests
// job has no node to talk to.
//
// What it proves:
//   1. wrong passcode → an error, and nothing in sessionStorage;
//   2. right passcode → the reading room, the share id gone from the address
//      bar, and sessionStorage['xerj.share'] holding EXACTLY
//      {api_key, index, brain, expires_at, label} (the console guest-mode
//      contract);
//   3. search returns highlighted snippets; a document opens;
//   4. a document full of markup is shown as the text it is — no element is
//      created from it, no handler runs, nothing is fetched — in the result
//      list, in the snippet and in the document view;
//   5. the page talks to its own origin and nowhere else, and the share id is
//      never part of a URL the browser requests other than the claim POST;
//   6. sign-out empties sessionStorage and the page.
// ============================================================
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

const URL_BASE = (process.env.XERJ_URL || 'http://127.0.0.1:9510').replace(/\/$/, '');
const DATA_DIR = process.env.XERJ_DATA_DIR;
if (!DATA_DIR) { console.error('XERJ_DATA_DIR is required (the throwaway node\'s data dir)'); process.exit(2); }
const ADMIN = readFileSync(join(DATA_DIR, 'admin.key'), 'utf8').trim();
const INDEX = 'guestroom';          // dynamic mapping: keyword (BM25) search
const SEM_INDEX = 'guestroom-sem';  // a semantic_text field: the hybrid query

let passed = 0, failed = 0;
function ok(name, cond, detail) {
  if (cond) { passed += 1; console.log('  ok  ', name); }
  else { failed += 1; console.log('  FAIL', name, '->', String(detail === undefined ? '' : JSON.stringify(detail)).slice(0, 500)); }
}

// ── the node, as its owner ─────────────────────────────────────────────────
async function api(method, path, body, ndjson) {
  const resp = await fetch(URL_BASE + path, {
    method,
    headers: { authorization: `ApiKey ${ADMIN}`, 'content-type': ndjson ? 'application/x-ndjson' : 'application/json' },
    body: body === undefined ? undefined : (ndjson ? body : JSON.stringify(body)),
  });
  let json = null;
  try { json = await resp.json(); } catch { /* not json */ }
  return { status: resp.status, json };
}

// Text a hostile sender would put in an email. Every one of these runs, loads
// or navigates if it is ever parsed as HTML.
const XSS_SUBJECT = '<img src=x onerror="window.__xss=\'subject\'">Quarterly invoice';
const XSS_BODY = [
  'Please review the invoice attached.',
  '<script>window.__xss="script"</script>',
  '<img src="/_xerj-console/share/icon.svg?beacon=1" onerror="window.__xss=\'img\'" onload="window.__xss=\'imgload\'">',
  '<svg onload="window.__xss=\'svg\'"></svg>',
  '<iframe src="javascript:parent.__xss=\'iframe\'"></iframe>',
  '<a href="javascript:window.__xss=\'link\'" id="xss-link">click for the invoice</a>',
  '<style>#view-room{display:none}</style>',
  '</p></div></article><h1 id="xss-breakout">broke out</h1>',
  'invoice total due on receipt.',
].join('\n');

async function seed() {
  await api('DELETE', `/${INDEX}`);
  await api('DELETE', `/${SEM_INDEX}`);
  const made = await api('PUT', `/${SEM_INDEX}`, { mappings: { properties: { subject: { type: 'text' }, body: { type: 'semantic_text' } } } });
  if (made.status !== 200) throw new Error(`semantic_text index: ${JSON.stringify(made)}`);
  const lines = [
    { index: { _index: INDEX, _id: 'lease' } },
    { subject: 'Lease dispute', body: 'The landlord refused to return the deposit after the inspection.', email_from: 'dana@example.test' },
    { index: { _index: SEM_INDEX, _id: 'receipt' } },
    { subject: 'Receipt', body: `${'Rent was paid on the first of every month without fail. '.repeat(8)}The deposit of 1200 was paid in January.` },
    { index: { _index: INDEX, _id: 'xss' } },
    { subject: XSS_SUBJECT, body: XSS_BODY, email_from: '"><script>window.__xss="from"</script>', body_html: '<html><body onload="window.__xss=\'html\'"><b>bold</b></body></html>' },
  ].map((l) => JSON.stringify(l)).join('\n') + '\n';
  const bulk = await api('POST', '/_bulk', lines, true);
  if (bulk.status !== 200 || bulk.json.errors) throw new Error(`seed failed: ${JSON.stringify(bulk)}`);
  await api('POST', `/${INDEX},${SEM_INDEX}/_refresh`);
  const share = await api('POST', '/_share', { index: [INDEX, SEM_INDEX], expires_in: '1h', max_claims: 1, label: 'Files for Dana <b>(bold?)</b>' });
  if (share.status !== 200) throw new Error(`share create failed: ${JSON.stringify(share)}`);
  return share.json;
}

// ── Chrome over the DevTools pipe ──────────────────────────────────────────
function findChrome() {
  const candidates = [process.env.CHROME_BIN, '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium', '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'];
  const pw = join(homedir(), '.cache', 'ms-playwright');
  for (const c of candidates) if (c && existsSync(c)) return c;
  if (existsSync(pw)) {
    for (const dir of ['chromium_headless_shell-1243/chrome-linux/headless_shell', 'chromium-1243/chrome-linux/chrome']) {
      if (existsSync(join(pw, dir))) return join(pw, dir);
    }
  }
  return null;
}

class Browser {
  constructor(bin) {
    this.profile = mkdtempSync(join(tmpdir(), 'xerj-share-e2e-'));
    this.proc = spawn(bin, [
      '--headless=new', '--remote-debugging-pipe', '--no-first-run', '--no-default-browser-check',
      '--disable-gpu', '--disable-extensions', '--disable-background-networking', '--disable-sync',
      '--disable-dev-shm-usage', '--no-sandbox', `--user-data-dir=${this.profile}`, 'about:blank',
    ], { stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'] });
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = [];
    let buffer = Buffer.alloc(0);
    this.proc.stdio[4].on('data', (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      let end;
      while ((end = buffer.indexOf(0)) >= 0) {
        const msg = JSON.parse(buffer.subarray(0, end).toString('utf8'));
        buffer = buffer.subarray(end + 1);
        if (msg.id && this.pending.has(msg.id)) {
          const { resolve, reject } = this.pending.get(msg.id);
          this.pending.delete(msg.id);
          if (msg.error) reject(new Error(`${msg.error.message}`)); else resolve(msg.result);
        } else if (msg.method) {
          for (const l of this.listeners) l(msg);
        }
      }
    });
  }

  send(method, params = {}, sessionId) {
    const id = this.nextId++;
    const msg = { id, method, params };
    if (sessionId) msg.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.stdio[3].write(JSON.stringify(msg) + '\0');
    });
  }

  async close() {
    try { await this.send('Browser.close'); } catch { /* already gone */ }
    this.proc.kill();
    try { rmSync(this.profile, { recursive: true, force: true }); } catch { /* best effort */ }
  }
}

async function main() {
  const chrome = findChrome();
  if (!chrome) {
    console.error('no Chrome/Chromium found (set CHROME_BIN)');
    process.exit(process.env.XERJ_SHARE_REQUIRE_BROWSER === '1' ? 1 : 77);
  }
  console.log(`node ${URL_BASE} · browser ${chrome}`);
  const share = await seed();
  const link = `${URL_BASE}${share.url_path}`;
  ok('the link carries the share id only in its fragment',
    link.endsWith(`/_xerj-console/share#${share.share_id}`) && !link.split('#')[0].includes(share.share_id), link);

  const browser = new Browser(chrome);
  const requests = [];
  const wireHeaders = [];
  const consoleErrors = [];
  const dialogs = [];
  try {
    const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true });
    const cdp = (method, params) => browser.send(method, params, sessionId);
    browser.listeners.push((msg) => {
      if (msg.sessionId !== sessionId) return;
      if (msg.method === 'Network.requestWillBeSent') requests.push({ url: msg.params.request.url, method: msg.params.request.method, headers: msg.params.request.headers });
      // The headers that actually went on the wire. `requestWillBeSent` lists a
      // `Referer` key for every request, empty when policy suppressed it.
      if (msg.method === 'Network.requestWillBeSentExtraInfo') wireHeaders.push(msg.params.headers);
      if (msg.method === 'Runtime.exceptionThrown') consoleErrors.push(msg.params.exceptionDetails.text);
      if (msg.method === 'Log.entryAdded' && msg.params.entry.level === 'error') consoleErrors.push(msg.params.entry.text);
      if (msg.method === 'Page.javascriptDialogOpening') dialogs.push(msg.params.message);
    });
    await cdp('Page.enable'); await cdp('Runtime.enable'); await cdp('Network.enable'); await cdp('Log.enable');

    const evaluate = async (expression) => {
      const r = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
      if (r.exceptionDetails) throw new Error(`page threw: ${r.exceptionDetails.text} ${r.exceptionDetails.exception?.description || ''}`);
      return r.result.value;
    };
    const waitFor = async (expression, what, ms = 15000) => {
      const deadline = Date.now() + ms;
      for (;;) {
        if (await evaluate(`!!(${expression})`)) return true;
        if (Date.now() > deadline) { ok(`waited for: ${what}`, false, await evaluate('document.body.innerText.slice(0, 400)')); return false; }
        await new Promise((r) => setTimeout(r, 100));
      }
    };
    const visible = (id) => `(!document.getElementById('${id}').hidden)`;
    const submitPasscode = (code) => evaluate(`(() => {
      const i = document.getElementById('passcode'); i.value = ${JSON.stringify(code)};
      document.getElementById('claim-form').requestSubmit(); })()`);
    // Submit a query and wait until the list shows THAT query's answer. The
    // previous rows stay on screen until the response lands, so a row count
    // alone cannot tell a fresh list from a stale one (two queries that both
    // return two rows looked identical to an earlier version of this test).
    const search = async (q) => {
      await evaluate(`(() => {
        const i = document.getElementById('q'); i.value = ${JSON.stringify(q)};
        document.getElementById('search-form').requestSubmit(); })()`);
      await waitFor(`document.getElementById('results').dataset.query === ${JSON.stringify(q)}`, `results for ${JSON.stringify(q)}`);
    };
    const injected = () => evaluate(`({
      xss: window.__xss === undefined ? null : String(window.__xss),
      foreign: document.querySelectorAll('#app script, #app img, #app svg, #app iframe, #app style, #app a[href^="javascript"], #xss-link, #xss-breakout, #app b').length,
      roomHidden: document.getElementById('view-room').hidden,
    })`);

    // ── 1. open the link; the page is the passcode prompt ───────────────────
    await cdp('Page.navigate', { url: link });
    await waitFor(visible('view-claim'), 'the passcode prompt');
    ok('nothing is stored before a passcode is given', await evaluate(`sessionStorage.length === 0 && localStorage.length === 0`));
    const pageReq = requests.find((r) => r.url.includes('/_xerj-console/share'));
    ok('the page request does not carry the share id', pageReq && !requests.some((r) => r.url.includes(share.share_id)), requests.map((r) => r.url));

    // ── 2. wrong passcode ───────────────────────────────────────────────────
    await submitPasscode('not-the-code');
    await waitFor(`!document.getElementById('claim-error').hidden`, 'the wrong-passcode message');
    ok('wrong passcode → a message, still on the prompt',
      await evaluate(`${visible('view-claim')} && /passcode is not right/i.test(document.getElementById('claim-error').textContent)`));
    ok('wrong passcode stores nothing', await evaluate(`sessionStorage.getItem('xerj.share') === null`));

    // ── 3. right passcode ───────────────────────────────────────────────────
    await submitPasscode(share.passcode);
    await waitFor(visible('view-room'), 'the reading room');
    const stored = JSON.parse(await evaluate(`sessionStorage.getItem('xerj.share')`));
    ok('sessionStorage[xerj.share] is EXACTLY {api_key, index, brain, expires_at, label}',
      JSON.stringify(Object.keys(stored)) === JSON.stringify(['api_key', 'index', 'brain', 'expires_at', 'label']), Object.keys(stored));
    ok('…with a usable key, this index, no brain, the owner\'s expiry and label',
      typeof stored.api_key === 'string' && stored.api_key.length > 20 && stored.index === `${INDEX},${SEM_INDEX}` && stored.brain === null
      && Number.isFinite(Date.parse(stored.expires_at)) && stored.label === 'Files for Dana <b>(bold?)</b>', stored);
    ok('nothing else is kept in the browser', await evaluate(`sessionStorage.length === 1 && localStorage.length === 0 && document.cookie === ''`));
    ok('the share id is gone from the address bar', await evaluate(`location.hash === '' && !location.href.includes(${JSON.stringify(share.share_id)})`));
    ok('the label is shown as text, not markup',
      await evaluate(`document.getElementById('room-label').textContent === 'Files for Dana <b>(bold?)</b>' && document.querySelectorAll('#room-label b').length === 0`));
    ok('the room says read-only, names the expiry, and says where the documents stay',
      await evaluate(`/read-only/i.test(document.getElementById('view-room').innerText) && /access ends/i.test(document.getElementById('room-expiry').textContent) && /stay on the owner/i.test(document.getElementById('room-notice').textContent)`));
    await waitFor(`document.getElementById('search-mode').textContent.length > 0`, 'the search-mode line');
    ok('the page says how it searches — hybrid where a semantic_text field exists, keyword elsewhere — and claims no AI',
      await evaluate(`/hybrid search where the index supports it, keyword search elsewhere/i.test(document.getElementById('search-mode').textContent) && /no text is generated/i.test(document.getElementById('search-mode').textContent) && !/\\b(AI|neural|semantic search|understands)\\b/i.test(document.getElementById('search-mode').textContent)`),
      await evaluate(`document.getElementById('search-mode').textContent`));

    // ── 4. search + highlighted snippet + document view ─────────────────────
    await search('deposit');
    await waitFor(`document.querySelectorAll('#results li').length > 0`, 'results for "deposit"');
    await waitFor(`document.querySelectorAll('#results li').length === 2`, 'one hit from each shared index');
    ok('search covers both shared indices, and every snippet shows the term highlighted',
      await evaluate(`[...document.querySelectorAll('#results li')].every((li) => [...li.querySelectorAll('mark')].some((m) => /deposit/i.test(m.textContent)))`),
      await evaluate(`document.getElementById('results').innerText`));
    ok('a hybrid hit with no server-side highlight gets a snippet cut around the term, not the opening lines',
      await evaluate(`[...document.querySelectorAll('#results .hit-snippet')].some((n) => n.textContent.startsWith('…') && /deposit of 1200/.test(n.textContent))`),
      await evaluate(`[...document.querySelectorAll('#results .hit-snippet')].map((n) => n.textContent)`));
    ok('the count line does not call vector-ranked results "matching"',
      await evaluate(`/ranked, best first/.test(document.getElementById('room-status').textContent)`),
      await evaluate(`document.getElementById('room-status').textContent`));
    await evaluate(`[...document.querySelectorAll('#results button.hit')].find((b) => b.textContent.includes('Lease dispute')).click()`);
    await waitFor(visible('doc-pane'), 'the document view');
    ok('the document view shows the subject and the body',
      await evaluate(`document.getElementById('doc-title').textContent === 'Lease dispute' && /landlord refused/.test(document.getElementById('doc-body').textContent)`));
    await evaluate(`document.getElementById('doc-back').click()`);

    // A query whose words are in NO document of the semantic_text index: the
    // vector leg still ranks its one document, and the page has to say so.
    await search('landlord');
    await waitFor(`document.querySelectorAll('#results li').length === 2`, 'results for "landlord"');
    ok('a hybrid hit that contains none of the words is labelled as similarity-ranked, and a real match is not',
      await evaluate(`(() => {
        const items = [...document.querySelectorAll('#results li')];
        const receipt = items.find((li) => li.textContent.includes('Receipt'));
        const lease = items.find((li) => li.textContent.includes('Lease dispute'));
        return !!receipt && !!lease && !!receipt.querySelector('.hit-note') && !lease.querySelector('.hit-note');
      })()`),
      await evaluate(`document.getElementById('results').innerText`));

    // ── 5. the hostile document ─────────────────────────────────────────────
    await search('invoice');
    await waitFor(`[...document.querySelectorAll('#results .hit-title')].some((t) => t.textContent.includes('Quarterly invoice'))`, 'the hostile document in the results');
    ok('its subject is listed as literal text', await evaluate(`[...document.querySelectorAll('#results .hit-title')].some((t) => t.textContent === ${JSON.stringify(XSS_SUBJECT)})`));
    let state = await injected();
    ok('result list + snippet: no element was created from document text, no handler ran', state.xss === null && state.foreign === 0 && !state.roomHidden, state);
    await evaluate(`[...document.querySelectorAll('#results button.hit')].find((b) => b.textContent.includes('Quarterly invoice')).click()`);
    await waitFor(visible('doc-pane'), 'the hostile document\'s view');
    ok('document view shows the markup as the text it is',
      await evaluate(`document.getElementById('doc-body').textContent.includes('<script>window.__xss="script"</script>') && document.getElementById('doc-body').textContent.includes('<a href="javascript:')`));
    ok('an HTML email body is shown as source text, never rendered',
      await evaluate(`document.getElementById('view-room').textContent.includes('<html><body onload=')`));
    await new Promise((r) => setTimeout(r, 500)); // give any handler that was going to run the time to run
    state = await injected();
    ok('document view: no element was created from document text, no handler ran', state.xss === null && state.foreign === 0 && !state.roomHidden, state);
    ok('no dialog was opened by document content', dialogs.length === 0, dialogs);
    ok('no request was made for a URL inside a document', !requests.some((r) => r.url.includes('beacon=1')), requests.map((r) => r.url));

    // ── 6. the page only ever talks to its own origin ───────────────────────
    const origin = new URL(URL_BASE).origin;
    const foreign = requests.filter((r) => !r.url.startsWith(origin + '/') && !r.url.startsWith('about:') && !r.url.startsWith('data:'));
    ok('every request went to the node itself — no third party', foreign.length === 0, foreign.map((r) => r.url));
    const withId = requests.filter((r) => r.url.includes(share.share_id));
    ok('the share id appears in exactly one kind of request: the claim POST',
      withId.length === 2 && withId.every((r) => r.method === 'POST' && r.url === `${origin}/_share/${share.share_id}/claim`), withId);
    const referers = [...requests.map((r) => r.headers), ...wireHeaders]
      .flatMap((h) => Object.entries(h).filter(([k, v]) => k.toLowerCase() === 'referer' && v).map(([, v]) => v));
    ok('no request carried a Referer', wireHeaders.length > 0 && referers.length === 0, { seen: wireHeaders.length, referers });
    ok('the page raised no script errors or CSP violations', consoleErrors.filter((e) => !/401|Failed to load resource/.test(e)).length === 0, consoleErrors);

    // ── 7. sign out ─────────────────────────────────────────────────────────
    await evaluate(`document.getElementById('signout').click()`);
    await waitFor(visible('view-ended'), 'the signed-out view');
    ok('sign-out empties sessionStorage', await evaluate(`sessionStorage.getItem('xerj.share') === null && sessionStorage.length === 0`));
    ok('sign-out empties the page', await evaluate(`document.getElementById('results').children.length === 0 && document.getElementById('doc-body').children.length === 0 && document.getElementById('view-room').hidden`));
    await cdp('Page.reload');
    await waitFor(`${visible('view-ended')} || ${visible('view-claim')}`, 'the page after a reload');
    ok('a reload after sign-out does not bring the room back', await evaluate(`document.getElementById('view-room').hidden && sessionStorage.length === 0`));
  } finally {
    await browser.close();
    await api('DELETE', `/_share/${share.handle}`);
    await api('DELETE', `/${INDEX}`);
    await api('DELETE', `/${SEM_INDEX}`);
  }
  console.log(`\npassed=${passed} failed=${failed}`);
  process.exit(failed === 0 ? 0 : 1);
}

main().catch((e) => { console.error(e); process.exit(1); });
