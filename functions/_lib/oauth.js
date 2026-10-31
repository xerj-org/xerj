// OAuth 2.0 sign-in with GitHub and Hugging Face for hub.xerj.org.
//
//   GET /auth/<github|hf>/login[?return=/path]   → 302 to the provider
//   GET /auth/<github|hf>/callback?code&state    → 302 back to the return path,
//                                                  with the hub_session cookie set
//
// CSRF / code injection: login mints a random `state` and a PKCE verifier (S256),
// both kept in the signed, HttpOnly hub_oauth cookie (Path=/auth/, 10 minutes).
// The callback accepts a code only when the cookie verifies, names the same
// provider, and its state equals the query's state; the cookie is cleared either
// way, so a state is single-use. PKCE: Hugging Face (OpenID Connect) and GitHub
// both accept code_challenge/code_verifier.
//
// Scopes: GitHub none (public profile only); Hugging Face "openid profile".
// Tokens: the provider access token is used once, in the callback, to read the
// public profile, and then dropped. It is never stored or put in a cookie.
// Email is never requested or stored.
//
// Disabled mode: without HUB_SESSION_SECRET or the provider's client id/secret,
// login redirects home with ?signin=disabled and nothing else happens.

import {
  OAUTH_COOKIE, OAUTH_TTL_S, clearCookie, config, cookie, makeSessionCookie, parseCookies,
  randomToken, safeReturn, sameString, sha256b64url, sign, verify,
} from './session.js';

const UA = 'xerj-hub-signin (+https://hub.xerj.org)';

export const PROVIDERS = {
  github: {
    authorize: 'https://github.com/login/oauth/authorize',
    token: 'https://github.com/login/oauth/access_token',
    scope: '',
    clientAuth: 'post',     // GitHub reads client_id/client_secret from the form body
    creds: (env) => [env.GITHUB_OAUTH_CLIENT_ID, env.GITHUB_OAUTH_CLIENT_SECRET],
    async profile(token, fetchImpl) {
      const r = await fetchImpl('https://api.github.com/user', {
        headers: { authorization: `Bearer ${token}`, accept: 'application/vnd.github+json', 'user-agent': UA },
      });
      if (!r.ok) throw new Error(`github user ${r.status}`);
      const u = await r.json();
      return { provider: 'github', id: String(u.id || ''), login: u.login || '', name: u.name || u.login || '', avatar: u.avatar_url || '' };
    },
  },
  hf: {
    authorize: 'https://huggingface.co/oauth/authorize',
    token: 'https://huggingface.co/oauth/token',
    scope: 'openid profile',
    clientAuth: 'basic',    // client_secret_basic (HTTP Basic), the OIDC default
    creds: (env) => [env.HF_OAUTH_CLIENT_ID, env.HF_OAUTH_CLIENT_SECRET],
    async profile(token, fetchImpl) {
      const r = await fetchImpl('https://huggingface.co/oauth/userinfo', {
        headers: { authorization: `Bearer ${token}`, accept: 'application/json', 'user-agent': UA },
      });
      if (!r.ok) throw new Error(`hf userinfo ${r.status}`);
      const u = await r.json();
      return { provider: 'hf', id: String(u.sub || ''), login: u.preferred_username || '', name: u.name || u.preferred_username || '', avatar: u.picture || '' };
    },
  },
};

const redirect = (location, cookies = []) => {
  const h = new Headers({ location, 'cache-control': 'no-store', 'x-robots-tag': 'noindex, nofollow' });
  for (const c of cookies) h.append('set-cookie', c);
  return new Response(null, { status: 302, headers: h });
};

export function callbackUrl(request, env, provider) {
  const origin = env.HUB_PUBLIC_ORIGIN || new URL(request.url).origin;
  return `${origin}/auth/${provider}/callback`;
}

// Never a 5xx: any unexpected error (bad config, crypto, a malformed URL) sends
// the visitor home with ?signin=failed. Search Console logged 5xx on these URLs
// in October 2026; a crawler that follows a sign-in link must get a redirect.
export async function login(ctx, provider) {
  try {
    return await startLogin(ctx, provider);
  } catch {
    return redirect('/?signin=failed');
  }
}

async function startLogin({ request, env }, provider) {
  const p = PROVIDERS[provider];
  const c = config(env);
  if (!p || !c.sessions || !c.providers[provider]) return redirect('/?signin=disabled');
  const url = new URL(request.url);
  const state = randomToken(24);
  const verifier = randomToken(48);
  const now = Math.floor(Date.now() / 1000);
  const oauth = await sign(c.secret, 'oauth', {
    p: provider, state, verifier, ret: safeReturn(url.searchParams.get('return') || '/'), exp: now + OAUTH_TTL_S,
  });
  const q = new URLSearchParams({
    client_id: p.creds(env)[0], redirect_uri: callbackUrl(request, env, provider), response_type: 'code', state,
    code_challenge: await sha256b64url(verifier), code_challenge_method: 'S256',
  });
  if (p.scope) q.set('scope', p.scope);
  if (provider === 'github') q.set('allow_signup', 'false');
  return redirect(`${p.authorize}?${q}`, [cookie(OAUTH_COOKIE, oauth, { maxAge: OAUTH_TTL_S, path: '/auth/' })]);
}

export async function callback({ request, env }, provider, fetchImpl = fetch) {
  const p = PROVIDERS[provider];
  const c = config(env);
  const clear = clearCookie(OAUTH_COOKIE, '/auth/');
  if (!p || !c.sessions || !c.providers[provider]) return redirect('/?signin=disabled', [clear]);
  const url = new URL(request.url);
  const saved = await verify(c.secret, 'oauth', parseCookies(request)[OAUTH_COOKIE]);
  const state = url.searchParams.get('state') || '';
  if (!saved || saved.p !== provider || !sameString(saved.state, state)) return redirect('/?signin=failed', [clear]);
  const ret = safeReturn(saved.ret);
  const back = (flag) => `${ret}${ret.includes('?') ? '&' : '?'}signin=${flag}`;
  if (url.searchParams.get('error')) return redirect(back('cancelled'), [clear]);
  const code = url.searchParams.get('code');
  if (!code) return redirect(back('failed'), [clear]);
  let user;
  try {
    const [id, secret] = p.creds(env);
    const form = new URLSearchParams({
      grant_type: 'authorization_code', code, redirect_uri: callbackUrl(request, env, provider),
      client_id: id, code_verifier: saved.verifier,
    });
    const headers = { 'content-type': 'application/x-www-form-urlencoded', accept: 'application/json', 'user-agent': UA };
    // One client-authentication method per request (RFC 6749 §2.3).
    if (p.clientAuth === 'basic') headers.authorization = `Basic ${btoa(`${encodeURIComponent(id)}:${encodeURIComponent(secret)}`)}`;
    else form.set('client_secret', secret);
    const tr = await fetchImpl(p.token, { method: 'POST', headers, body: form });
    const t = await tr.json().catch(() => ({}));
    if (!tr.ok || !t.access_token) throw new Error(`token ${tr.status} ${t.error || ''}`);
    user = await p.profile(t.access_token, fetchImpl);    // the token goes out of scope here
  } catch (e) {
    console.log(`[auth] ${provider} callback failed:`, String(e).slice(0, 200));
    return redirect(back('failed'), [clear]);
  }
  let session;
  try { session = await makeSessionCookie(c.secret, user); } catch (_) { return redirect(back('failed'), [clear]); }
  return redirect(ret, [clear, session]);
}
