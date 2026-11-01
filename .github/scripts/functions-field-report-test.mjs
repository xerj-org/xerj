// Plain-Node tests for the field-report HTTP fallback. No network, deployment,
// or XERJ process: only R2 is replaced with an in-memory stand-in.
//   node .github/scripts/functions-field-report-test.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';

const source = readFileSync(new URL('../../functions/api/field-report.js', import.meta.url), 'utf8');
const { onRequest } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const LIMIT = 16 * 1024;
const encoder = new TextEncoder();
const valid = { agent: 'test-agent', used_for: 'local validation', verdict: 'works' };

function fakeR2() {
  const store = new Map();
  const reads = [];
  const writes = [];
  return {
    store, reads, writes,
    async get(key) {
      reads.push(key);
      return store.has(key) ? { text: async () => store.get(key) } : null;
    },
    async put(key, value) {
      writes.push(key);
      store.set(key, value);
    },
  };
}

function request(body, headers = {}) {
  return new Request('https://xerj.example.invalid/api/field-report', {
    method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body,
  });
}

function paddedBody(bytes, { multibyte = false, bom = false } = {}) {
  const raw = JSON.stringify({ ...valid, notes: (multibyte ? '界' : 'x').repeat(1000) });
  const prefix = bom ? new Uint8Array([0xef, 0xbb, 0xbf]) : new Uint8Array();
  const json = encoder.encode(raw);
  assert.ok(bytes >= prefix.byteLength + json.byteLength);
  const body = new Uint8Array(bytes).fill(0x20);
  body.set(prefix);
  body.set(json, prefix.byteLength);
  return body;
}

function assertNoStorage(bucket) {
  assert.deepEqual(bucket.reads, []);
  assert.deepEqual(bucket.writes, []);
  assert.equal(bucket.store.size, 0);
}

async function assertTooLarge(body, headers = {}) {
  const FIELD_REPORTS = fakeR2();
  const response = await onRequest({ request: request(body, headers), env: { FIELD_REPORTS } });
  assert.equal(response.status, 413);
  assert.deepEqual(await response.json(), { ok: false, error: 'body too large' });
  assert.equal(response.headers.get('access-control-allow-origin'), '*');
  assertNoStorage(FIELD_REPORTS);
}

test('multibyte JSON over 16 KiB is rejected without Content-Length or R2 access', async () => {
  const body = JSON.stringify({ agent: 'test', used_for: '界'.repeat(1000), verdict: '界'.repeat(1000), notes: '界'.repeat(4000) });
  assert.ok(body.length < LIMIT);
  assert.equal(encoder.encode(body).byteLength, 18054);
  assert.equal(request(body).headers.get('content-length'), null);
  await assertTooLarge(body);
});

test('exactly 16384 UTF-8 bytes are accepted and keep all three storage writes', async () => {
  const FIELD_REPORTS = fakeR2();
  const body = paddedBody(LIMIT, { multibyte: true });
  const response = await onRequest({ request: request(body), env: { FIELD_REPORTS } });
  assert.equal(body.byteLength, 16384);
  assert.equal(response.status, 200);
  const result = await response.json();
  assert.equal(result.ok, true);
  assert.equal(result.stored, true);
  assert.match(result.note, /github\.com\/xerj-org\/xerj/);
  assert.equal(response.headers.get('content-type'), 'application/json');
  assert.equal(response.headers.get('cache-control'), 'no-store');
  assert.equal(response.headers.get('access-control-allow-origin'), '*');
  assert.equal(FIELD_REPORTS.reads.length, 2);
  assert.equal(FIELD_REPORTS.writes.length, 3);
  assert.equal(FIELD_REPORTS.store.size, 3);
  assert.ok(FIELD_REPORTS.writes.some((key) => key.startsWith('rl/')));
  assert.ok(FIELD_REPORTS.writes.some((key) => key.startsWith('meta/count-')));
  const reportKey = FIELD_REPORTS.writes.find((key) => key.startsWith('reports/'));
  const report = JSON.parse(FIELD_REPORTS.store.get(reportKey));
  assert.equal(report.agent, valid.agent);
  assert.equal(report.notes, '界'.repeat(1000));
});

test('16385 UTF-8 bytes are rejected even with an understated Content-Length', async () => {
  const body = paddedBody(LIMIT + 1, { multibyte: true });
  assert.equal(body.byteLength, 16385);
  await assertTooLarge(body, { 'content-length': '1' });
});

test('oversized malformed JSON is rejected by byte size before JSON parsing', async () => {
  const body = '界'.repeat(6000);
  assert.ok(body.length < LIMIT);
  assert.equal(encoder.encode(body).byteLength, 18000);
  await assertTooLarge(body);
});

test('ASCII bodies keep the same exact byte boundary', async () => {
  const FIELD_REPORTS = fakeR2();
  const response = await onRequest({ request: request(paddedBody(LIMIT)), env: { FIELD_REPORTS } });
  assert.equal(response.status, 200);
  assert.equal(FIELD_REPORTS.writes.length, 3);
  await assertTooLarge(paddedBody(LIMIT + 1));
});

test('the UTF-8 BOM counts toward the limit before text decoding removes it', async () => {
  const body = paddedBody(LIMIT + 1, { bom: true });
  assert.equal(body.byteLength, 16385);
  assert.equal((await request(body).text()).length, 16382);
  await assertTooLarge(body);
  const response = await onRequest({ request: request(paddedBody(LIMIT, { bom: true })), env: {} });
  assert.equal(response.status, 200);
});

test('a declared oversize body is rejected before storage regardless of its actual size', async () => {
  await assertTooLarge(JSON.stringify(valid), { 'content-length': String(LIMIT + 1) });
});

test('valid reports without a bucket still return the unstored canonical response', async () => {
  const response = await onRequest({ request: request(JSON.stringify(valid)), env: {} });
  assert.equal(response.status, 200);
  const result = await response.json();
  assert.equal(result.ok, true);
  assert.equal(result.stored, false);
  assert.match(result.note, /github\.com\/xerj-org\/xerj/);
});

test('method and OPTIONS responses preserve CORS and never touch storage', async () => {
  for (const [method, status] of [['GET', 405], ['PUT', 405], ['OPTIONS', 204]]) {
    const FIELD_REPORTS = fakeR2();
    const response = await onRequest({ request: new Request('https://xerj.example.invalid/api/field-report', { method }), env: { FIELD_REPORTS } });
    assert.equal(response.status, status);
    assert.equal(response.headers.get('access-control-allow-origin'), '*');
    assert.equal(response.headers.get('access-control-allow-methods'), 'POST, OPTIONS');
    assertNoStorage(FIELD_REPORTS);
  }
});

test('wrong Content-Type is rejected without storage', async () => {
  const FIELD_REPORTS = fakeR2();
  const response = await onRequest({ request: request(JSON.stringify(valid), { 'content-type': 'text/plain' }), env: { FIELD_REPORTS } });
  assert.equal(response.status, 415);
  assertNoStorage(FIELD_REPORTS);
});

test('malformed JSON, non-object bodies and invalid fields still return 400 before storage', async () => {
  for (const body of ['{', 'null', '[]', '{}', JSON.stringify({ ...valid, agent: '' }), JSON.stringify({ ...valid, notes: 1 })]) {
    const FIELD_REPORTS = fakeR2();
    const response = await onRequest({ request: request(body), env: { FIELD_REPORTS } });
    assert.equal(response.status, 400);
    assertNoStorage(FIELD_REPORTS);
  }
});

test('a body read failure still returns 400 without storage', async () => {
  const FIELD_REPORTS = fakeR2();
  const body = new ReadableStream({ start(controller) { controller.error(new Error('read failed')); } });
  const response = await onRequest({
    request: new Request('https://xerj.example.invalid/api/field-report', { method: 'POST', headers: { 'content-type': 'application/json' }, body, duplex: 'half' }),
    env: { FIELD_REPORTS },
  });
  assert.equal(response.status, 400);
  assert.deepEqual(await response.json(), { ok: false, error: 'could not read body' });
  assertNoStorage(FIELD_REPORTS);
});
