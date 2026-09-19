// Shared setup + the in-page inertness census for the browser tests.
import assert from 'node:assert/strict';
import { launch, findChrome, browserRequired } from './cdp.mjs';
import { startFakeEngine, GUEST_KEY } from './fake-engine.mjs';

export { GUEST_KEY };

export function skipReason() {
  if (findChrome() && typeof WebSocket === 'function') return false;
  const why = findChrome() ? 'Node >= 22 is required (global WebSocket)' : 'no Chrome/Chromium found (set CHROME_BIN)';
  if (browserRequired()) throw new Error(`XERJ_REQUIRE_BROWSER=1 but ${why}`);
  return why;
}

export async function setup() {
  const engine = await startFakeEngine();
  const browser = await launch();
  return { engine, browser, async teardown() { await browser.close(); await engine.close(); } };
}

export function shareRecord(over = {}) {
  return { api_key: GUEST_KEY, index: 'ax-inbox', brain: 'inbox', expires_at: new Date(Date.now() + 3_600_000).toISOString(), label: 'Hostile inbox', ...over };
}

/** Open a fresh tab as a share-link guest: seed sessionStorage on a blank
 *  same-origin page (what the share guest page does), then go to the console. */
export async function openGuest(ctx, { record = shareRecord(), hash = '' } = {}) {
  const page = await ctx.browser.newPage();
  await page.goto(`${ctx.engine.origin}/__blank`);
  await page.waitFor('document.readyState === "complete"');
  await page.eval(`sessionStorage.setItem('xerj.share', ${JSON.stringify(typeof record === 'string' ? record : JSON.stringify(record))}), true`);
  await page.goto(`${ctx.engine.origin}/_xerj-console/${hash}`);
  return page;
}

export async function openOperator(ctx, hash = '') {
  ctx.engine.state.operator = true;
  const page = await ctx.browser.newPage();
  await page.goto(`${ctx.engine.origin}/_xerj-console/${hash}`);
  return page;
}

/**
 * The census, run INSIDE the page. `scope` is a CSS selector; everything
 * under it is document-derived and must be inert.
 */
export async function census(page, scope) {
  return page.eval(`(() => {
    const root = document.querySelector(${JSON.stringify(scope)});
    if (!root) return { missing: true };
    const all = [root, ...root.querySelectorAll('*')];
    const forbidden = 'script,img,iframe,object,embed,style,link,meta,base,form,video,audio,source,math,template,details,frame,textarea,image,animate,foreignObject';
    const onAttrs = [], badHrefs = [], styleAttrs = [], srcAttrs = [];
    for (const el of all) {
      for (const a of el.attributes) {
        if (/^on/i.test(a.name)) onAttrs.push(el.tagName + '[' + a.name + ']');
        if (a.name === 'style') styleAttrs.push(el.tagName);
        if (['src', 'srcset', 'srcdoc', 'action', 'formaction', 'xlink:href', 'background', 'poster'].includes(a.name.toLowerCase())) srcAttrs.push(el.tagName + '[' + a.name + ']');
      }
      if (el.tagName === 'A' && el.hasAttribute('href') && !el.getAttribute('href').startsWith('#/')) badHrefs.push(el.getAttribute('href').slice(0, 80));
    }
    return {
      elements: all.length,
      forbidden: [...root.querySelectorAll(forbidden)].map((e) => e.tagName),
      svg: root.querySelectorAll('svg').length,
      onAttrs, badHrefs, styleAttrs, srcAttrs,
      anchors: root.querySelectorAll('a').length,
      marks: root.querySelectorAll('mark').length,
      text: root.textContent,
    };
  })()`);
}

export function assertCensusInert(c, label, { allowSvg = false, allowStyleAttr = false } = {}) {
  assert.ok(!c.missing, `${label}: scope not found`);
  assert.ok(c.elements > 3, `${label}: nothing rendered`);
  assert.deepEqual(c.forbidden, [], `${label}: forbidden elements in the DOM`);
  if (!allowSvg) assert.equal(c.svg, 0, `${label}: <svg> in document-derived DOM`);
  assert.deepEqual(c.onAttrs, [], `${label}: event-handler attributes`);
  assert.deepEqual(c.srcAttrs, [], `${label}: resource-loading attributes`);
  assert.deepEqual(c.badHrefs, [], `${label}: a clickable href that is not an in-app route`);
  if (!allowStyleAttr) assert.deepEqual(c.styleAttrs, [], `${label}: style attributes`);
}

const ALLOWED_THIRD_PARTY = /^https:\/\/fonts\.(googleapis|gstatic)\.com\//; // index.html's stylesheet link, as always

/** Nothing ran, nothing opened, nothing left the origin, the policy held. */
export async function assertNotPwned(page, origin, label) {
  const flags = await page.eval(`({ pwned: window.__xerjPwned || 0, csp: window.__cspViolations || [] })`);
  assert.equal(flags.pwned, 0, `${label}: a payload EXECUTED`);
  assert.deepEqual(page.dialogs, [], `${label}: a dialog opened`);
  assert.deepEqual(flags.csp, [], `${label}: Content-Security-Policy violations (the SPA and the shipped policy disagree, or something tried to break out)`);
  const foreign = page.requests.map((r) => r.url).filter((u) => /^https?:/.test(u) && !u.startsWith(origin + '/') && !ALLOWED_THIRD_PARTY.test(u));
  assert.deepEqual(foreign, [], `${label}: a request left the origin`);
}
