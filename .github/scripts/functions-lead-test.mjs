// Plain-Node request validation tests for /lead. No network or Cloudflare
// account: storage is an in-memory R2 stand-in.
//   node .github/scripts/functions-lead-test.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

// Import the real source as an ES module without changing package.json's
// module mode or keeping a second copy of the handler in the test.
const source = readFileSync(new URL('../../functions/lead.js', import.meta.url), 'utf8');
const { onRequestPost } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);

function fakeR2(fail = false) {
  const store = new Map();
  let attempts = 0;
  return {
    store,
    get attempts() { return attempts; },
    async put(key, value) {
      attempts++;
      if (fail) throw new Error('R2 unavailable');
      store.set(key, value);
    },
  };
}

async function post(raw, env) {
  return onRequestPost({
    request: new Request('https://xerj.org/lead', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: raw,
    }),
    env,
  });
}

const invalid = [
  ['JSON null', 'null'],
  ['malformed JSON', '{'],
  ['missing email', '{}'],
  ['null email', '{"email":null}'],
  ['numeric email', '{"email":42}'],
  ['boolean email', '{"email":true}'],
  ['object email', '{"email":{}}'],
  ['empty email array', '{"email":[]}'],
];
for (const [name, raw] of invalid) {
  test(`${name} returns 400 without writing`, async () => {
    const LEADS = fakeR2();
    const response = await post(raw, { LEADS });
    assert.equal(response.status, 400);
    assert.deepEqual(await response.json(), { ok: false, error: 'invalid email' });
    assert.equal(LEADS.attempts, 0);
    assert.equal(LEADS.store.size, 0);
  });
}

test('valid email is normalized and stored once', async () => {
  const LEADS = fakeR2();
  const response = await post('{"email":"  Person@Example.COM  "}', { LEADS });
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), { ok: true, stored: true });
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.equal(LEADS.attempts, 1);
  assert.equal(LEADS.store.size, 1);
  const [[key, value]] = LEADS.store;
  assert.ok(key.startsWith('leads/'));
  const record = JSON.parse(value);
  assert.equal(record.email, 'person@example.com');
  assert.equal(record.source, 'landing');
});

test('valid email without a bucket keeps the existing unstored response', async () => {
  const response = await post('{"email":"person@example.com"}', {});
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), { ok: true, stored: false });
});

test('a real storage failure still returns 500', async () => {
  const LEADS = fakeR2(true);
  const response = await post('{"email":"person@example.com"}', { LEADS });
  assert.equal(response.status, 500);
  assert.deepEqual(await response.json(), { ok: false, error: 'Error: R2 unavailable' });
  assert.equal(LEADS.attempts, 1);
  assert.equal(LEADS.store.size, 0);
});
