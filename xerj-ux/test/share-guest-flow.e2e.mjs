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
//      in NO URL the browser requests — it travels in the body of the claim
//      POST (`/_share/claim`), so no request line an access log records holds it;
//   6. opening the same link again in the same tab keeps the session (and does
//      not spend a second claim — on a one-open share that would lock the
//      guest out), and a link pasted over another one, which changes only the
//      fragment, is noticed;
//   7. a query no document contains says so before listing similarity ranks;
//   8. on a quick-tunnel hostname the page says Cloudflare can read the
//      traffic, and says nothing of the kind anywhere else;
//   9. sign-out empties sessionStorage and the page.
// ============================================================
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

const URL_BASE = (process.env.XERJ_URL || 'http://127.0.0.1:9510').replace(/\/$/, '');
const DATA_DIR = process.env.XERJ_DATA_DIR;
if (!DATA_DIR) { console.error('XERJ_DATA_DIR is required (the throwaway node\'s data dir)'); process.exit(2); }
const ADMIN = readFileSync(join(DATA_DIR, 'admin.key'), 'utf8').trim();
const INDEX = 'guestroom';          // dynamic mapping: keyword (BM25) search
const SEM_INDEX = 'guestroom-sem';  // a semantic_text field: the hybrid query
// A name shaped like a quick tunnel's. Chrome is told to resolve it to the
// node (see Browser); `.test` hosts never resolve, this one must look real.
const TUNNEL_HOST = 'xerj-e2e-not-a-real-tunnel.trycloudflare.com';

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

async function shareStatus(handle) {
  const list = await api('GET', '/_share');
  return (list.json.shares || []).find((s) => s.handle === handle) || null;
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
      '--disable-dev-shm-usage', '--no-sandbox', `--user-data-dir=${this.profile}`,
      // Phase 8 opens the page under a quick-tunnel HOSTNAME without a tunnel:
      // that one name resolves to the node under test. Nothing leaves the box.
      `--host-resolver-rules=MAP ${TUNNEL_HOST} ${new URL(URL_BASE).hostname}`,
      'about:blank',
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
  let otherHandle = null;
  try {
    const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true });
    const cdp = (method, params) => browser.send(method, params, sessionId);
    browser.listeners.push((msg) => {
      if (msg.sessionId !== sessionId) return;
      if (msg.method === 'Network.requestWillBeSent') requests.push({ url: msg.params.request.url, method: msg.params.request.method, headers: msg.params.request.headers, postData: msg.params.request.postData || '' });
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
    // A predicate that throws is "not yet", not a failure: right after a
    // navigation the document may still be the previous page (or about:blank),
    // and `document.getElementById(…)` is null until the new one is parsed.
    // Seen once under a load average of 54, when the first wait ran before the
    // guest page had loaded.
    const waitFor = async (expression, what, ms = 15000) => {
      const deadline = Date.now() + ms;
      for (;;) {
        let hit = false;
        try { hit = await evaluate(`!!(${expression})`); } catch { hit = false; }
        if (hit) return true;
        if (Date.now() > deadline) { ok(`waited for: ${what}`, false, await evaluate('document.body.innerText.slice(0, 400)')); return false; }
        await new Promise((r) => setTimeout(r, 100));
      }
    };
    const visible = (id) => `(!!document.getElementById('${id}') && !document.getElementById('${id}').hidden)`;
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
    // Beside the session: the share's PUBLIC handle — sha256(id)[..12], what
    // `xerj share --list` prints — so the page can tell that a link opened
    // again in this tab is the one it already holds a key for. The page
    // computes it itself; it has to match the node's.
    const keptHandle = await evaluate(`sessionStorage.getItem('xerj.share.link')`);
    ok('beside it, only the share\'s public handle — and it is the node\'s own sha256(id)[..12]',
      keptHandle === share.handle && keptHandle === createHash('sha256').update(share.share_id).digest('hex').slice(0, 12)
      && !share.share_id.includes(keptHandle), { keptHandle, handle: share.handle });
    ok('nothing else is kept in the browser', await evaluate(`sessionStorage.length === 2 && localStorage.length === 0 && document.cookie === ''`));
    ok('the share id itself is stored nowhere', await evaluate(`!JSON.stringify(Object.entries(sessionStorage)).includes(${JSON.stringify(share.share_id)})`));
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
    // The id is kept out of the page URL (fragment) so that no server logs it.
    // The claim must not undo that: a request PATH is what an access log, a
    // reverse proxy and a tunnel's edge write down. The first cut posted to
    // /_share/<id>/claim, and the node's own access log recorded the id.
    const withId = requests.filter((r) => r.url.includes(share.share_id));
    ok('the share id is in NO url the browser requested', withId.length === 0, withId.map((r) => r.url));
    const claims = requests.filter((r) => r.method === 'POST' && r.url.startsWith(`${origin}/_share`));
    ok('it travels in the BODY of the claim POST, to /_share/claim, and nowhere else',
      claims.length === 2 && claims.every((r) => r.url === `${origin}/_share/claim` && JSON.parse(r.postData).id === share.share_id)
      && requests.filter((r) => r.postData.includes(share.share_id)).length === 2,
      claims.map((r) => [r.url, r.postData.replace(share.share_id, '<id>')]));
    const referers = [...requests.map((r) => r.headers), ...wireHeaders]
      .flatMap((h) => Object.entries(h).filter(([k, v]) => k.toLowerCase() === 'referer' && v).map(([, v]) => v));
    ok('no request carried a Referer', wireHeaders.length > 0 && referers.length === 0, { seen: wireHeaders.length, referers });
    ok('the page raised no script errors or CSP violations', consoleErrors.filter((e) => !/401|Failed to load resource/.test(e)).length === 0, consoleErrors);

    // ── 7. the same link again, in the same tab ─────────────────────────────
    // A guest reading in this tab clicks the link in their email a second
    // time. The first cut dropped the session whenever a fragment was present
    // and asked for the passcode again; on this one-open share the second
    // claim is refused ("used up"), so the guest was locked out for good of a
    // share that had an hour (in the docs' example: seven days) left to run.
    // Via about:blank, so that this is a full page load with the fragment in
    // the URL (from the page itself it would be a fragment-only change, which
    // is the next check). sessionStorage belongs to the tab and survives it.
    await cdp('Page.navigate', { url: 'about:blank' });
    await waitFor(`location.href === 'about:blank'`, 'about:blank');
    await cdp('Page.navigate', { url: link });
    await waitFor(`${visible('view-room')} || ${visible('view-claim')} || ${visible('view-ended')}`, 'the page after re-opening the link');
    ok('re-opening the SAME link in the same tab keeps the session: the room, not the passcode prompt',
      await evaluate(`${visible('view-room')} && sessionStorage.getItem('xerj.share') !== null`),
      await evaluate(`({ room: ${visible('view-room')}, claim: ${visible('view-claim')}, ended: ${visible('view-ended')}, stored: sessionStorage.getItem('xerj.share') !== null })`));
    ok('…and the id is taken out of the address bar again', await evaluate(`location.hash === ''`), await evaluate('location.href'));
    await search('deposit');
    ok('…and the kept key still searches', await evaluate(`document.querySelectorAll('#results li').length === 2`));
    // The same thing without a page load: only the fragment changes.
    await evaluate(`location.hash = ${JSON.stringify('#' + share.share_id)}`);
    await new Promise((r) => setTimeout(r, 300));
    ok('the same link arriving as a fragment-only change leaves the reader where they were',
      await evaluate(`${visible('view-room')} && location.hash === '' && document.querySelectorAll('#results li').length === 2`));
    const spent = await shareStatus(share.handle);
    ok('none of that spent a second claim', spent && spent.claims === 1, spent);

    // ── 8. a query no document contains ─────────────────────────────────────
    // The vector leg ranks something for any string. Say that nothing matched
    // before listing what was ranked instead.
    await search('zzzzqqqqxxxx');
    ok('a query no document contains says so first, instead of "N documents ranked"',
      await evaluate(`/^No document contains/.test(document.getElementById('room-status').textContent) && /vector similarity/.test(document.getElementById('room-status').textContent)`),
      await evaluate(`document.getElementById('room-status').textContent`));
    await search('deposit');
    ok('…and a query that does match is counted as before',
      await evaluate(`/ranked, best first/.test(document.getElementById('room-status').textContent)`),
      await evaluate(`document.getElementById('room-status').textContent`));

    // ── 9. a DIFFERENT link pasted over this one ────────────────────────────
    // Pasting a link into the address bar of a tab already on the page changes
    // only the fragment — no browser reloads for that. Without a `hashchange`
    // listener the old view stayed: typically "this link is no longer active",
    // whose own advice is to ask for a new link and open it.
    const other = await api('POST', '/_share', { index: INDEX, expires_in: '1h', max_claims: 1, label: 'second share' });
    const bogus = 'ab'.repeat(16);
    await evaluate(`location.hash = ${JSON.stringify('#' + bogus)}`);
    await waitFor(visible('view-claim'), 'the passcode prompt for a different link');
    ok('a different link asks for ITS passcode — and the session this tab holds is not thrown away yet',
      await evaluate(`sessionStorage.getItem('xerj.share') !== null && document.getElementById('view-room').hidden`));
    await submitPasscode('whatever-code');
    await waitFor(visible('view-ended'), 'the not-recognised view');
    ok('an unknown link ends in "not recognised"', await evaluate(`/not recognised/i.test(document.getElementById('ended-title').textContent)`));
    await evaluate(`location.hash = ${JSON.stringify('#' + other.json.share_id)}`);
    await waitFor(visible('view-claim'), 'the passcode prompt after pasting a new link over a dead one');
    ok('a new link pasted over a dead one (fragment-only navigation) shows the passcode prompt, without a reload',
      await evaluate(`${visible('view-claim')} && document.getElementById('view-ended').hidden`));
    await submitPasscode(other.json.passcode);
    await waitFor(`${visible('view-room')} && document.getElementById('room-label').textContent === 'second share'`, 'the second share\'s room');
    ok('claiming it replaces the session and the remembered handle',
      await evaluate(`JSON.parse(sessionStorage.getItem('xerj.share')).label === 'second share' && sessionStorage.getItem('xerj.share.link') === ${JSON.stringify(other.json.handle)}`));
    ok('no tunnel notice on an ordinary hostname', await evaluate(`document.getElementById('via-tunnel').hidden && document.getElementById('room-via-tunnel').hidden`));
    otherHandle = other.json.handle; // revoked in `finally`: its key is still needed below

    // ── 10. under a quick-tunnel hostname ───────────────────────────────────
    // Through `xerj share --tunnel` the browser talks to Cloudflare, which
    // ends TLS and can read the passcode, the key and every document. The page
    // used to tell the guest "Nothing is uploaded to a third party — your
    // browser talks to their machine", and it said so on the tunnel too.
    const port = new URL(URL_BASE).port;
    await cdp('Page.navigate', { url: `http://${TUNNEL_HOST}:${port}/_xerj-console/share#${bogus}` });
    const loaded = await waitFor(`location.hostname === ${JSON.stringify(TUNNEL_HOST)} && ${visible('view-claim')}`, 'the guest page under a quick-tunnel hostname');
    if (loaded) {
      ok('on a quick-tunnel hostname the page says, before the passcode is typed, that Cloudflare can read the traffic',
        await evaluate(`!document.getElementById('via-tunnel').hidden && /Cloudflare/.test(document.getElementById('via-tunnel').textContent) && /your passcode/.test(document.getElementById('via-tunnel').textContent)`),
        await evaluate(`document.getElementById('view-claim').innerText`));
    }
    ok('the page never claims that no third party is involved',
      await evaluate(`!/third party|talks to their machine/i.test(document.getElementById('app').textContent)`));
    await cdp('Page.navigate', { url: `${URL_BASE}/_xerj-console/share` });
    await waitFor(visible('view-room'), 'back in the room on the node\'s own address');

    // ── 11. sign out ────────────────────────────────────────────────────────
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
    if (otherHandle) await api('DELETE', `/_share/${otherHandle}`);
    await api('DELETE', `/${INDEX}`);
    await api('DELETE', `/${SEM_INDEX}`);
  }
  console.log(`\npassed=${passed} failed=${failed}`);
  process.exit(failed === 0 ? 0 : 1);
}

main().catch((e) => { console.error(e); process.exit(1); });
