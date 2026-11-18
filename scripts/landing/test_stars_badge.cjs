// The nav stars badge refresh in landing/theme.js: cache, fetch, silent failure.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');
const source = fs.readFileSync(path.join(__dirname, '../../landing/theme.js'), 'utf8');

function run({ cached, response, now = 1_000_000_000 }) {
  const ready = [];
  const store = new Map(cached ? [['xerj.ghstars', JSON.stringify(cached)]] : []);
  const link = { attrs: {}, setAttribute(k, v) { this.attrs[k] = v; } };
  const span = { textContent: '\u2605 14.2k', closest: () => link };
  const calls = [];
  const document = {
    documentElement: { getAttribute: () => null, setAttribute() {} },
    addEventListener: (_, fn) => ready.push(fn),
    querySelector: sel => (sel === '[data-gh-stars]' ? span : null),
    querySelectorAll: sel => (sel === '[data-gh-stars]' ? [span] : []),
  };
  const fetch = url => {
    calls.push(url);
    if (response === 'throw') return Promise.reject(Error('offline'));
    return Promise.resolve({ ok: response !== 'error', json: () => Promise.resolve(response) });
  };
  const localStorage = { getItem: k => store.get(k) ?? null, setItem: (k, v) => store.set(k, v) };
  const window = { fetch };
  vm.runInNewContext(source, { document, localStorage, fetch, window, Date: { now: () => now }, JSON });
  ready.forEach(fn => fn());
  return new Promise(r => setTimeout(r, 0)).then(() => ({ span, link, calls, store }));
}

test('a fresh cache is shown and skips the API', async () => {
  const r = await run({ cached: { n: 15049, t: 1_000_000_000 - 1000 }, response: 'error' });
  assert.equal(r.span.textContent, '\u2605 15k');
  assert.equal(r.calls.length, 0);
});

test('a stale cache refetches and stores the live count', async () => {
  const r = await run({ cached: { n: 1, t: 0 }, response: { stargazers_count: 14251 } });
  assert.equal(r.calls.length, 1);
  assert.equal(r.span.textContent, '\u2605 14.3k');
  assert.equal(r.link.attrs['aria-label'], 'XERJ on GitHub, 14,251 stars');
  assert.equal(JSON.parse(r.store.get('xerj.ghstars')).n, 14251);
});

test('API failure keeps the shipped number', async () => {
  for (const response of ['error', 'throw', { message: 'API rate limit exceeded' }]) {
    const r = await run({ response });
    assert.equal(r.span.textContent, '\u2605 14.2k');
    assert.equal(r.store.size, 0);
  }
});

test('small counts are shown in full', async () => {
  const r = await run({ response: { stargazers_count: 950 } });
  assert.equal(r.span.textContent, '\u2605 950');
});
