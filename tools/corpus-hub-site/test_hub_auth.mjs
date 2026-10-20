// node --test tools/corpus-hub-site/test_hub_auth.mjs  (Node 18+; no dependencies)
// Sign-in for hub.xerj.org: session signing, OAuth state/PKCE, disabled mode.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  config, sign, verify, makeSessionCookie, currentUser, safeReturn, sessionUser, requireUser,
  SESSION_COOKIE, OAUTH_COOKIE, sha256b64url,
} from '../../functions/_lib/session.js';
import { login, callback } from '../../functions/_lib/oauth.js';
import { onRequestGet as me } from '../../functions/api/me.js';
import { onRequest as logout } from '../../functions/auth/logout.js';

const SECRET = 'x'.repeat(40);
const ENV = {
  HUB_SESSION_SECRET: SECRET,
  GITHUB_OAUTH_CLIENT_ID: 'gh-id', GITHUB_OAUTH_CLIENT_SECRET: 'gh-secret',
  HF_OAUTH_CLIENT_ID: 'hf-id', HF_OAUTH_CLIENT_SECRET: 'hf-secret',
};
const USER = { provider: 'github', id: '42', login: 'ann', name: 'Ann', avatar: 'https://avatars.githubusercontent.com/u/42' };
const req = (path, cookie) => new Request(`https://hub.xerj.org${path}`, { headers: cookie ? { cookie } : {} });
const cookieValue = (setCookie, name) => (setCookie.match(new RegExp(`${name}=([^;]*)`)) || [])[1];
const setCookies = (res) => res.headers.getSetCookie ? res.headers.getSetCookie() : [res.headers.get('set-cookie')];

test('session: sign → verify round trip, cookie attributes', async () => {
  const sc = await makeSessionCookie(SECRET, USER);
  assert.match(sc, /HttpOnly/); assert.match(sc, /Secure/); assert.match(sc, /SameSite=Lax/); assert.match(sc, /Path=\//);
  const u = await currentUser(req('/', `${SESSION_COOKIE}=${cookieValue(sc, SESSION_COOKIE)}`), ENV);
  assert.deepEqual(u, USER);
});

test('session: tampered, wrong key, wrong purpose, expired → null', async () => {
  const v = await sign(SECRET, 'session', { ...USER, exp: Math.floor(Date.now() / 1000) + 60 });
  assert.ok(await verify(SECRET, 'session', v));
  const [body, mac] = v.split('.');
  const forged = Buffer.from(JSON.stringify({ ...USER, login: 'mallory', pur: 'session', exp: 9e9 })).toString('base64url');
  assert.equal(await verify(SECRET, 'session', `${forged}.${mac}`), null);
  assert.equal(await verify(SECRET, 'session', `${body}.${mac.slice(0, -2)}AA`), null);
  assert.equal(await verify('y'.repeat(40), 'session', v), null);
  assert.equal(await verify(SECRET, 'oauth', v), null);              // purpose-bound
  assert.equal(await verify(SECRET, 'session', v, Date.now() + 61_000), null);  // expired
  assert.equal(await verify(SECRET, 'session', 'garbage'), null);
  assert.equal(await verify(SECRET, 'session', undefined), null);
});

test('session holds only provider, id, login, name, avatar', async () => {
  const u = sessionUser({ ...USER, email: 'a@b.c', token: 'gho_x', avatar: 'javascript:alert(1)' });
  assert.deepEqual(Object.keys(u).sort(), ['avatar', 'id', 'login', 'name', 'provider']);
  assert.equal(u.avatar, '');
  const sc = await makeSessionCookie(SECRET, { ...USER, email: 'a@b.c', access_token: 'gho_x' });
  const payload = Buffer.from(cookieValue(sc, SESSION_COOKIE).split('.')[0], 'base64url').toString();
  assert.doesNotMatch(payload, /a@b\.c|gho_x/);
});

test('safeReturn keeps same-site paths only', () => {
  assert.equal(safeReturn('/contributors/ann.html'), '/contributors/ann.html');
  for (const bad of ['https://evil.example', '//evil.example', '/\\evil.example', 'evil', '', null, '/a\nb'])
    assert.equal(safeReturn(bad), '/');
});

test('disabled mode: no secret or no provider → enabled:false everywhere', async () => {
  for (const env of [{}, { HUB_SESSION_SECRET: SECRET }, { ...ENV, HUB_SESSION_SECRET: 'short' }]) {
    assert.equal(config(env).enabled, false);
    const r = await me({ request: req('/api/me'), env });
    assert.deepEqual(await r.json(), { ok: true, enabled: false, providers: { github: false, hf: false }, user: null });
    const l = await login({ request: req('/auth/github/login'), env }, 'github');
    assert.equal(l.status, 302); assert.equal(l.headers.get('location'), '/?signin=disabled');
    const [, deny] = await requireUser(req('/api/v1/me/stars'), env);
    assert.equal(deny.status, 401); assert.equal((await deny.json()).enabled, false);
  }
  // one provider configured: only that one is offered
  const env = { HUB_SESSION_SECRET: SECRET, HF_OAUTH_CLIENT_ID: 'i', HF_OAUTH_CLIENT_SECRET: 's' };
  const b = await (await me({ request: req('/api/me'), env })).json();
  assert.equal(b.enabled, true); assert.deepEqual(b.providers, { github: false, hf: true });
  assert.equal((await login({ request: req('/auth/github/login'), env }, 'github')).headers.get('location'), '/?signin=disabled');
});

test('gated API: 401 without a session, user with one', async () => {
  let [u, deny] = await requireUser(req('/api/v1/me/stars'), ENV);
  assert.equal(u, null); assert.equal(deny.status, 401);
  assert.equal((await deny.json()).enabled, true);
  const sc = await makeSessionCookie(SECRET, USER);
  [u, deny] = await requireUser(req('/api/v1/me/stars', `${SESSION_COOKIE}=${cookieValue(sc, SESSION_COOKIE)}`), ENV);
  assert.equal(deny, null); assert.equal(u.login, 'ann');
});

test('/api/me reports the signed-in user without the provider id', async () => {
  const sc = await makeSessionCookie(SECRET, USER);
  const b = await (await me({ request: req('/api/me', `${SESSION_COOKIE}=${cookieValue(sc, SESSION_COOKIE)}`), env: ENV })).json();
  assert.deepEqual(b.user, { provider: 'github', login: 'ann', name: 'Ann', avatar: USER.avatar });
});

// A stand-in for the provider: records the token request, returns a profile.
function fakeProvider(provider) {
  const calls = [];
  const f = async (url, init = {}) => {
    calls.push({ url: String(url), init });
    if (/access_token|oauth\/token/.test(url)) return new Response(JSON.stringify({ access_token: 'tok123', token_type: 'bearer' }), { status: 200 });
    if (/api\.github\.com\/user/.test(url)) return new Response(JSON.stringify({ id: 42, login: 'ann', name: 'Ann', avatar_url: USER.avatar, email: 'ann@example.com' }));
    if (/oauth\/userinfo/.test(url)) return new Response(JSON.stringify({ sub: 'hf-7', preferred_username: 'annhf', name: 'Ann HF', picture: 'https://cdn-avatars.huggingface.co/a.png', email: 'ann@example.com' }));
    return new Response('nope', { status: 404 });
  };
  return { f, calls };
}

async function startLogin(provider, ret = '/corpus/aider.html') {
  const r = await login({ request: req(`/auth/${provider}/login?return=${encodeURIComponent(ret)}`), env: ENV }, provider);
  assert.equal(r.status, 302);
  const loc = new URL(r.headers.get('location'));
  const sc = setCookies(r).find((c) => c.startsWith(OAUTH_COOKIE));
  assert.match(sc, /Path=\/auth\//); assert.match(sc, /HttpOnly/); assert.match(sc, /Max-Age=600/);
  return { loc, oauth: cookieValue(sc, OAUTH_COOKIE) };
}

for (const [provider, host, expectLogin] of [['github', 'github.com', 'ann'], ['hf', 'huggingface.co', 'annhf']]) {
  test(`${provider}: login sends state + S256 PKCE; callback sets a session and drops the token`, async () => {
    const { loc, oauth } = await startLogin(provider);
    assert.equal(loc.host, host);
    const state = loc.searchParams.get('state');
    assert.ok(state && state.length >= 32);
    assert.equal(loc.searchParams.get('code_challenge_method'), 'S256');
    assert.equal(loc.searchParams.get('redirect_uri'), `https://hub.xerj.org/auth/${provider}/callback`);
    if (provider === 'hf') assert.equal(loc.searchParams.get('scope'), 'openid profile');
    const { f, calls } = fakeProvider(provider);
    const r = await callback({ request: req(`/auth/${provider}/callback?code=abc&state=${state}`, `${OAUTH_COOKIE}=${oauth}`), env: ENV }, provider, f);
    assert.equal(r.status, 302); assert.equal(r.headers.get('location'), '/corpus/aider.html');
    const cookies = setCookies(r);
    const session = cookies.find((c) => c.startsWith(`${SESSION_COOKIE}=`));
    assert.ok(cookies.some((c) => c.startsWith(`${OAUTH_COOKIE}=;`)), 'state cookie cleared');
    // PKCE: the verifier sent to the token endpoint hashes to the challenge
    const body = new URLSearchParams(String(calls[0].init.body));
    assert.equal(await sha256b64url(body.get('code_verifier')), loc.searchParams.get('code_challenge'));
    assert.equal(body.get('code'), 'abc');
    const u = await currentUser(req('/', `${SESSION_COOKIE}=${cookieValue(session, SESSION_COOKIE)}`), ENV);
    assert.equal(u.provider, provider); assert.equal(u.login, expectLogin);
    const payload = Buffer.from(cookieValue(session, SESSION_COOKIE).split('.')[0], 'base64url').toString();
    assert.doesNotMatch(payload, /tok123|ann@example\.com/);
  });
}

test('callback: state mismatch, missing/forged cookie, other provider → no session, no token request', async () => {
  const { loc, oauth } = await startLogin('github');
  const state = loc.searchParams.get('state');
  const cases = [
    [`?code=abc&state=${state}x`, `${OAUTH_COOKIE}=${oauth}`, 'github'],           // wrong state
    [`?code=abc&state=${state}`, '', 'github'],                                     // no cookie (CSRF)
    [`?code=abc&state=${state}`, `${OAUTH_COOKIE}=${oauth.slice(0, -3)}abc`, 'github'], // forged cookie
    [`?code=abc&state=${state}`, `${OAUTH_COOKIE}=${oauth}`, 'hf'],                // cookie from another provider
    [`?code=abc`, `${OAUTH_COOKIE}=${oauth}`, 'github'],                            // no state
  ];
  for (const [q, cookie, p] of cases) {
    const { f, calls } = fakeProvider(p);
    const r = await callback({ request: req(`/auth/${p}/callback${q}`, cookie), env: ENV }, p, f);
    assert.equal(r.status, 302);
    assert.match(r.headers.get('location'), /signin=failed/);
    assert.ok(!setCookies(r).some((c) => c && c.startsWith(`${SESSION_COOKIE}=`)), `no session for ${q}`);
    assert.equal(calls.length, 0, 'no token exchange');
  }
  // a session cookie is not accepted as an oauth state cookie
  const sc = cookieValue(await makeSessionCookie(SECRET, USER), SESSION_COOKIE);
  const { f, calls } = fakeProvider('github');
  const r = await callback({ request: req(`/auth/github/callback?code=a&state=${state}`, `${OAUTH_COOKIE}=${sc}`), env: ENV }, 'github', f);
  assert.match(r.headers.get('location'), /signin=failed/); assert.equal(calls.length, 0);
});

test('callback: provider error or failed exchange → back to the page, no session', async () => {
  const { loc, oauth } = await startLogin('hf');
  const state = loc.searchParams.get('state');
  let r = await callback({ request: req(`/auth/hf/callback?error=access_denied&state=${state}`, `${OAUTH_COOKIE}=${oauth}`), env: ENV }, 'hf', fakeProvider('hf').f);
  assert.equal(r.headers.get('location'), '/corpus/aider.html?signin=cancelled');
  r = await callback({ request: req(`/auth/hf/callback?code=a&state=${state}`, `${OAUTH_COOKIE}=${oauth}`), env: ENV }, 'hf',
    async () => new Response(JSON.stringify({ error: 'invalid_grant' }), { status: 400 }));
  assert.equal(r.headers.get('location'), '/corpus/aider.html?signin=failed');
  assert.ok(!setCookies(r).some((c) => c && c.startsWith(`${SESSION_COOKIE}=`)));
});

test('login: return path is sanitised before it is stored', async () => {
  const { loc, oauth } = await startLogin('github', 'https://evil.example/');
  const { f } = fakeProvider('github');
  const r = await callback({ request: req(`/auth/github/callback?code=a&state=${loc.searchParams.get('state')}`, `${OAUTH_COOKIE}=${oauth}`), env: ENV }, 'github', f);
  assert.equal(r.headers.get('location'), '/');
});

test('logout clears the session cookie', async () => {
  const r = logout({ request: req('/auth/logout?return=/contributors/ann.html') });
  assert.equal(r.status, 302); assert.equal(r.headers.get('location'), '/contributors/ann.html');
  assert.match(r.headers.get('set-cookie'), new RegExp(`^${SESSION_COOKIE}=; Path=/; Max-Age=0`));
});
