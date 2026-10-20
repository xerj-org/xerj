// Signed session cookies for hub.xerj.org sign-in (GitHub, Hugging Face).
//
// A helper module, not a route: Pages only routes files that export onRequest*.
//
// ─────────────────────────────── WHAT IS STORED ───────────────────────────
// The session cookie `hub_session` carries ONLY: provider ("github" | "hf"), the
// provider's stable user id, the login, the display name, the avatar URL, and the
// issue/expiry times. No email, no access token, no IP. It is not encrypted (there
// is nothing secret in it); it is signed, so it cannot be forged or edited:
//
//   hub_session = base64url(JSON payload) "." base64url(HMAC-SHA256(secret, payload))
//
// HttpOnly (page scripts cannot read it; they ask /api/me), Secure, SameSite=Lax
// (sent on top-level navigations back from the provider, never on cross-site
// subrequests), Path=/, 30 days.
//
// The short-lived `hub_oauth` cookie (10 min, Path=/auth/) holds the OAuth state,
// the PKCE verifier and where to return, signed the same way with a different
// purpose tag so neither cookie can be replayed as the other.
//
// ─────────────────────────────── DISABLED MODE ────────────────────────────
// Without HUB_SESSION_SECRET (at least 32 characters) nothing here signs or
// verifies anything: config() reports enabled:false and every caller treats the
// visitor as anonymous. A provider is offered only when both its client id and
// secret are set. Same pattern as /api/hub-stars without its R2 binding.

export const SESSION_COOKIE = 'hub_session';
export const OAUTH_COOKIE = 'hub_oauth';
export const SESSION_TTL_S = 30 * 24 * 3600;
export const OAUTH_TTL_S = 600;
const MIN_SECRET = 32;

const enc = new TextEncoder();
const dec = new TextDecoder();

export function b64url(bytes) {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
export function unb64url(str) {
  const s = atob(String(str).replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(s, (c) => c.charCodeAt(0));
}

export function randomToken(n = 32) {
  return b64url(crypto.getRandomValues(new Uint8Array(n)));
}

export async function sha256b64url(input) {
  return b64url(new Uint8Array(await crypto.subtle.digest('SHA-256', enc.encode(input))));
}

const keyCache = new Map();
async function hmacKey(secret) {
  if (!keyCache.has(secret)) {
    keyCache.set(secret, crypto.subtle.importKey('raw', enc.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign', 'verify']));
  }
  return keyCache.get(secret);
}

/** Which parts of sign-in this deploy can offer. Never throws. */
export function config(env = {}) {
  const secret = typeof env.HUB_SESSION_SECRET === 'string' && env.HUB_SESSION_SECRET.length >= MIN_SECRET
    ? env.HUB_SESSION_SECRET : '';
  const providers = {
    github: !!(env.GITHUB_OAUTH_CLIENT_ID && env.GITHUB_OAUTH_CLIENT_SECRET),
    hf: !!(env.HF_OAUTH_CLIENT_ID && env.HF_OAUTH_CLIENT_SECRET),
  };
  const sessions = !!secret;
  return { sessions, secret, providers, enabled: sessions && (providers.github || providers.hf) };
}

/** Sign an object for a purpose ("session" | "oauth"); returns the cookie value. */
export async function sign(secret, purpose, obj) {
  const body = b64url(enc.encode(JSON.stringify({ ...obj, pur: purpose })));
  const mac = await crypto.subtle.sign('HMAC', await hmacKey(secret), enc.encode(body));
  return `${body}.${b64url(new Uint8Array(mac))}`;
}

/** Verify a cookie value; returns the object, or null if forged, other-purpose or expired. */
export async function verify(secret, purpose, value, now = Date.now()) {
  if (!secret || typeof value !== 'string' || value.length > 4096) return null;
  const dot = value.indexOf('.');
  if (dot < 1 || dot !== value.lastIndexOf('.')) return null;
  const body = value.slice(0, dot);
  let mac;
  try { mac = unb64url(value.slice(dot + 1)); } catch (_) { return null; }
  let ok = false;
  try { ok = await crypto.subtle.verify('HMAC', await hmacKey(secret), mac, enc.encode(body)); } catch (_) { return null; }
  if (!ok) return null;
  let obj;
  try { obj = JSON.parse(dec.decode(unb64url(body))); } catch (_) { return null; }
  if (!obj || typeof obj !== 'object' || obj.pur !== purpose) return null;
  if (typeof obj.exp !== 'number' || obj.exp * 1000 <= now) return null;
  return obj;
}

export function parseCookies(request) {
  const out = {};
  for (const part of (request.headers.get('cookie') || '').split(';')) {
    const i = part.indexOf('=');
    if (i > 0) out[part.slice(0, i).trim()] = part.slice(i + 1).trim();
  }
  return out;
}

export function cookie(name, value, { maxAge, path = '/' } = {}) {
  return `${name}=${value}; Path=${path}; Max-Age=${maxAge}; HttpOnly; Secure; SameSite=Lax`;
}
export const clearCookie = (name, path = '/') => cookie(name, '', { maxAge: 0, path });

const PROVIDERS = ['github', 'hf'];
const clip = (s, n) => (typeof s === 'string' ? s.slice(0, n) : '');

/** The only fields a session may hold (anything else from a provider is dropped). */
export function sessionUser(u) {
  if (!u || !PROVIDERS.includes(u.provider) || !u.id || !u.login) return null;
  const avatar = clip(u.avatar, 300);
  return {
    provider: u.provider, id: clip(String(u.id), 100), login: clip(String(u.login), 100),
    name: clip(u.name, 100) || clip(String(u.login), 100),
    avatar: /^https:\/\//.test(avatar) ? avatar : '',
  };
}

export async function makeSessionCookie(secret, user, now = Date.now()) {
  const u = sessionUser(user);
  if (!u) throw new Error('incomplete user');
  const iat = Math.floor(now / 1000);
  const value = await sign(secret, 'session', { ...u, iat, exp: iat + SESSION_TTL_S });
  return cookie(SESSION_COOKIE, value, { maxAge: SESSION_TTL_S });
}

/** The signed-in user for this request, or null (also null when sign-in is disabled). */
export async function currentUser(request, env, now = Date.now()) {
  const { sessions, secret } = config(env);
  if (!sessions) return null;
  const v = parseCookies(request)[SESSION_COOKIE];
  if (!v) return null;
  return sessionUser(await verify(secret, 'session', v, now));
}

/** A same-site path to return to after sign-in; anything else becomes "/". */
export function safeReturn(p) {
  if (typeof p !== 'string' || p.length > 300) return '/';
  if (!p.startsWith('/') || p.startsWith('//') || p.startsWith('/\\') || /[\u0000-\u001f\\]/.test(p)) return '/';
  return p;
}

/** Constant-time string compare (state values). */
export function sameString(a, b) {
  if (typeof a !== 'string' || typeof b !== 'string' || a.length !== b.length || !a.length) return false;
  let d = 0;
  for (let i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return d === 0;
}

export const JSON_HEADERS = { 'content-type': 'application/json', 'cache-control': 'no-store' };
export const json = (obj, status = 200, extra = {}) =>
  new Response(JSON.stringify(obj), { status, headers: { ...JSON_HEADERS, ...extra } });

/** Gate for /api/v1/*: returns [user, null] or [null, 401 Response] (401 also when disabled). */
export async function requireUser(request, env) {
  const c = config(env);
  if (!c.enabled) return [null, json({ ok: false, enabled: false, error: 'sign-in is not enabled on this hub yet' }, 401)];
  const user = await currentUser(request, env);
  if (!user) return [null, json({ ok: false, enabled: true, error: 'sign in to see this' }, 401)];
  return [user, null];
}
