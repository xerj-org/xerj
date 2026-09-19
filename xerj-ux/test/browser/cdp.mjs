// ============================================================
// A minimal Chrome DevTools Protocol driver — no npm dependency.
//
// xerj-ux is a buildless SPA with no package.json, and the security tests
// have to run in CI without `npm install`. This drives a real headless
// Chrome over its DevTools WebSocket using only what Node ships (the global
// `WebSocket`, Node >= 22).
//
// Chrome is found via $CHROME_BIN, then the usual install paths, then a
// Playwright browser cache. `findChrome()` returns null when there is none;
// the tests then SKIP — unless XERJ_REQUIRE_BROWSER=1 (CI), where a missing
// browser is a failure rather than a silently green security suite.
// ============================================================

import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { tmpdir, homedir } from 'node:os';
import { join } from 'node:path';

export function findChrome() {
  const candidates = [
    process.env.CHROME_BIN,
    '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable', '/usr/bin/chromium', '/usr/bin/chromium-browser',
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  ].filter(Boolean);
  const cache = join(homedir(), '.cache', 'ms-playwright');
  if (existsSync(cache)) {
    for (const d of readdirSync(cache).filter((n) => /^chromium-\d+$/.test(n)).sort().reverse()) {
      candidates.push(join(cache, d, 'chrome-linux', 'chrome'), join(cache, d, 'chrome-linux64', 'chrome'));
    }
  }
  return candidates.find((p) => existsSync(p)) || null;
}

export const browserRequired = () => process.env.XERJ_REQUIRE_BROWSER === '1';

export async function launch({ chrome = findChrome(), timeoutMs = 30_000, extraArgs = [], debugPort = Number(process.env.XERJ_TEST_CDP_PORT) || 0 } = {}) {
  if (!chrome) throw new Error('no Chrome/Chromium found (set CHROME_BIN)');
  if (typeof WebSocket !== 'function') throw new Error('global WebSocket missing — Node >= 22 required');
  const profile = mkdtempSync(join(tmpdir(), 'xerj-ux-chrome-'));
  const proc = spawn(chrome, [
    '--headless=new', '--no-sandbox', '--disable-gpu', '--disable-dev-shm-usage', '--no-first-run', '--no-default-browser-check',
    '--disable-extensions', '--disable-background-networking', '--disable-sync', '--disable-component-update', '--mute-audio',
    // An OS-assigned port by default. XERJ_TEST_CDP_PORT (or `debugPort`) pins
    // it, for a shared machine where each run is given its own port block
    // (then run the browser files one at a time: `--test-concurrency=1`).
    `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extraArgs, 'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });

  const wsUrl = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(() => reject(new Error(`Chrome did not announce DevTools within ${timeoutMs}ms:\n${buf.slice(-2000)}`)), timeoutMs);
    proc.stderr.on('data', (d) => {
      buf += d;
      const m = buf.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) { clearTimeout(timer); resolve(m[1]); }
    });
    proc.on('exit', (code) => { clearTimeout(timer); reject(new Error(`Chrome exited (${code}) before DevTools came up:\n${buf.slice(-2000)}`)); });
  });

  const ws = new WebSocket(wsUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('DevTools socket failed')); });
  let nextId = 1;
  const pending = new Map();
  const listeners = new Set();
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject, method } = pending.get(msg.id);
      pending.delete(msg.id);
      if (msg.error) reject(new Error(`${method}: ${msg.error.message}`)); else resolve(msg.result);
      return;
    }
    for (const fn of listeners) fn(msg);
  };
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve, reject, method });
    ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });

  async function newPage() {
    const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
    const page = new Page(send, sessionId, targetId);
    listeners.add((msg) => { if (msg.sessionId === sessionId) page._event(msg); });
    await page.init();
    return page;
  }

  async function close() {
    try { await send('Browser.close'); } catch { /* already gone */ }
    try { ws.close(); } catch { /* */ }
    await new Promise((r) => { if (proc.exitCode != null) r(); else { proc.once('exit', r); setTimeout(() => { try { proc.kill('SIGKILL'); } catch { /* */ } r(); }, 3000); } });
    try { rmSync(profile, { recursive: true, force: true }); } catch { /* */ }
  }

  return { newPage, close, chrome };
}

class Page {
  constructor(send, sessionId, targetId) {
    this._send = send; this.sessionId = sessionId; this.targetId = targetId;
    /** Every request the page tried to make (including blocked ones). */
    this.requests = [];
    /** JS dialogs that tried to open (alert/confirm/prompt) — each is dismissed. */
    this.dialogs = [];
    /** Uncaught exceptions + console.error text. */
    this.errors = [];
    this.consoleMessages = [];
  }

  send(method, params) { return this._send(method, params, this.sessionId); }

  async init() {
    await this.send('Page.enable');
    await this.send('Runtime.enable');
    await this.send('Network.enable');
    await this.send('Log.enable');
    // Runs before any page script in every new document. CDP-injected
    // scripts are not subject to the page's CSP.
    await this.send('Page.addScriptToEvaluateOnNewDocument', {
      source: `window.__cspViolations = [];
        document.addEventListener('securitypolicyviolation', (e) => window.__cspViolations.push(e.violatedDirective + ' <- ' + (e.blockedURI || 'inline') + ' @' + (e.sourceFile || '') + ':' + e.lineNumber));`,
    });
  }

  _event(msg) {
    const p = msg.params || {};
    switch (msg.method) {
      case 'Network.requestWillBeSent':
        this.requests.push({ url: p.request.url, method: p.request.method, headers: p.request.headers, type: p.type });
        break;
      case 'Page.javascriptDialogOpening':
        this.dialogs.push({ type: p.type, message: p.message });
        this.send('Page.handleJavaScriptDialog', { accept: false }).catch(() => {});
        break;
      case 'Runtime.exceptionThrown':
        this.errors.push(String(p.exceptionDetails?.exception?.description || p.exceptionDetails?.text || 'exception'));
        break;
      case 'Runtime.consoleAPICalled':
        this.consoleMessages.push(`${p.type}: ${(p.args || []).map((a) => a.value ?? a.description ?? '').join(' ')}`);
        break;
      case 'Log.entryAdded':
        this.consoleMessages.push(`${p.entry.level}: ${p.entry.text}`);
        break;
      default:
    }
  }

  async goto(url) {
    const { errorText } = await this.send('Page.navigate', { url });
    if (errorText) throw new Error(`navigate ${url}: ${errorText}`);
  }

  /** Evaluate an expression (may be async) and return its JSON value. */
  async eval(expression) {
    const r = await this.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error(`eval failed: ${r.exceptionDetails.exception?.description || r.exceptionDetails.text}\n${expression.slice(0, 200)}`);
    return r.result.value;
  }

  /** Poll `expression` until truthy. Returns its value. */
  async waitFor(expression, { timeoutMs = 15_000, label } = {}) {
    const t0 = Date.now();
    let last;
    for (;;) {
      try { last = await this.eval(expression); } catch (e) { last = undefined; if (!/Cannot find context|Execution context was destroyed|Inspected target navigated/.test(String(e))) throw e; }
      if (last) return last;
      if (Date.now() - t0 > timeoutMs) {
        let text = '';
        try { text = await this.eval('document.body ? document.body.innerText.slice(0, 1500) : ""'); } catch { /* */ }
        throw new Error(`timed out waiting for: ${label || expression}\n--- page text ---\n${text}\n--- console ---\n${this.consoleMessages.slice(-12).join('\n')}`);
      }
      await new Promise((r) => setTimeout(r, 60));
    }
  }

  async setHash(hash) { await this.eval(`(location.hash = ${JSON.stringify(hash)}, true)`); }
  async close() { try { await this._send('Target.closeTarget', { targetId: this.targetId }); } catch { /* */ } }
}

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
