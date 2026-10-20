// node --test tools/corpus-hub-site/test_hub_details.mjs  (Node 18+; no dependencies)
// Signed-in details: /api/v1/contributors/<login>, /api/v1/corpus/<slug>, /api/me's
// "this is me" link. Gated: 401 without a session, 401 when sign-in is disabled.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { makeSessionCookie } from '../../functions/_lib/session.js';
import { contributorFor, details } from '../../functions/_lib/details.js';
import { contributorResponse, onRequestGet as contributorGet } from '../../functions/api/v1/contributors/[login].js';
import { corpusResponse, onRequestGet as corpusGet } from '../../functions/api/v1/corpus/[slug].js';

const SECRET = 'k'.repeat(40);
const ENV = { HUB_SESSION_SECRET: SECRET, GITHUB_OAUTH_CLIENT_ID: 'i', GITHUB_OAUTH_CLIENT_SECRET: 's' };
const DATA = {
  v: 1, generated: '2026-10-19', hub_pubkey: 'ab'.repeat(32),
  contributors: { ann: { login: 'Ann', name: 'Ann A', also_pinned_in: {}, work: [
    { repo: 'ann/one', repo_url: 'https://github.com/ann/one', commit: 'a'.repeat(40), title: 'feat: x', areas: ['src/a'], corpora: [{ slug: 'live-one', page: true, status: 'live' }] }] } },
  corpora: { 'live-one': { slug: 'live-one', lane: 'A', status: 'live', builds: [{ run: '2', result: 'published', tag: 'pack-live-one-bbbbbbbb' }], pins: [] } },
};
const session = async (provider, login) => (await makeSessionCookie(SECRET, { provider, id: '1', login, name: login, avatar: '' })).split(';')[0];
const req = (path, cookie) => new Request(`https://hub.xerj.org${path}`, { headers: cookie ? { cookie } : {} });

test('the committed hubdata.js is the null stub: no details in git', () => {
  assert.equal(details(), null);
});

test('gated: 401 without a session, 401 when sign-in is disabled', async () => {
  for (const [get, params, path] of [[contributorGet, { login: 'Ann' }, '/api/v1/contributors/Ann'], [corpusGet, { slug: 'live-one' }, '/api/v1/corpus/live-one']]) {
    let r = await get({ request: req(path), env: ENV, params });
    assert.equal(r.status, 401); assert.equal((await r.json()).enabled, true);
    r = await get({ request: req(path, await session('github', 'ann')), env: {}, params });
    assert.equal(r.status, 401); assert.equal((await r.json()).enabled, false);
    r = await get({ request: req(path, 'hub_session=forged.value'), env: ENV, params });
    assert.equal(r.status, 401);
    // signed in, but this deploy carries no details (the stub): 503, not a fake answer
    r = await get({ request: req(path, await session('github', 'ann')), env: ENV, params });
    assert.equal(r.status, 503);
  }
});

test('contributor details: full commit list, rebuilt commit url, is_me for the matching GitHub login', async () => {
  const user = { provider: 'github', login: 'ANN', id: '1' };
  let b = await (await contributorResponse(user, 'ann', DATA)).json();
  assert.equal(b.ok, true); assert.equal(b.is_me, true);
  assert.equal(b.contributor.work[0].commit_url, `https://github.com/ann/one/commit/${'a'.repeat(40)}`);
  assert.equal(b.contributor.work[0].pin, 'a'.repeat(40));
  b = await (await contributorResponse({ provider: 'hf', login: 'ann', id: '1' }, 'Ann', DATA)).json();
  assert.equal(b.is_me, false, 'a Hugging Face username is not a GitHub login');
  b = await (await contributorResponse({ provider: 'github', login: 'bob', id: '2' }, 'Ann', DATA)).json();
  assert.equal(b.is_me, false);
  assert.equal((await contributorResponse(user, 'nobody', DATA)).status, 404);
  assert.equal((await contributorResponse(user, '../etc', DATA)).status, 400);
  assert.equal(contributorFor(user, DATA), 'ann');
  assert.equal(contributorFor({ provider: 'hf', login: 'ann' }, DATA), null);
});

test('corpus details: builds, signing key, catalogue entry passthrough', async () => {
  const cat = { slug: 'live-one', pack_url: 'https://x/p.tar.zst', pack_sha256: 'f'.repeat(64), sig_url: 'https://x/p.tar.zst.sig' };
  const b = await (await corpusResponse('live-one', DATA, cat)).json();
  assert.equal(b.corpus.builds[0].tag, 'pack-live-one-bbbbbbbb');
  assert.equal(b.signing.hub_pubkey, 'ab'.repeat(32));
  assert.deepEqual(b.catalogue, cat);
  assert.equal((await corpusResponse('nope', DATA)).status, 404);
  assert.equal((await corpusResponse('Bad Slug', DATA)).status, 400);
});
