// node --test tools/corpus-hub-site/test_hub_404.mjs  (Node 18+; no dependencies)
// Issue #1895: unknown /api/* paths answer a JSON 404, real static files pass through.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { onRequest } from '../../functions/api/[[path]].js';

const ctx = (path, asset) => ({ request: new Request('https://hub.xerj.org' + path), next: async () => asset });

test('a missing /api path that the asset server answers with the HTML page becomes a JSON 404', async () => {
  const html = new Response('<!doctype html><html>', { status: 200, headers: { 'content-type': 'text/html; charset=utf-8' } });
  const res = await onRequest(ctx('/api/v1/nope.json', html));
  assert.equal(res.status, 404);
  assert.match(res.headers.get('content-type'), /^application\/json/);
  const body = await res.json();
  assert.deepEqual(body, { ok: false, error: 'not_found', path: '/api/v1/nope.json' });
});

test('a plain 404 from the asset server (404.html present) also becomes a JSON 404', async () => {
  const nf = new Response('<!doctype html>not found', { status: 404, headers: { 'content-type': 'text/html' } });
  const res = await onRequest(ctx('/api/zz', nf));
  assert.equal(res.status, 404);
  assert.match(res.headers.get('content-type'), /^application\/json/);
});

test('a real static file under /api (the signed catalogue) passes through untouched', async () => {
  const cat = new Response('{"format_version":1,"corpora":[]}', { status: 200, headers: { 'content-type': 'application/json' } });
  const res = await onRequest(ctx('/api/v1/catalogue.json', cat));
  assert.equal(res, cat);
  const sig = new Response('x', { status: 200, headers: { 'content-type': 'application/pgp-signature' } });
  assert.equal(await onRequest(ctx('/api/v1/catalogue.json.sig', sig)), sig);
});
