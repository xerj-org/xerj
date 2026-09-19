// ============================================================
// XERJ Console — guest (shared-link) mode
//
// The share guest page (engine: `POST /_share/{id}/claim`, xerj-api/src/
// share.rs) writes ONE sessionStorage record before handing the browser to
// the console:
//
//   sessionStorage['xerj.share'] = JSON.stringify({
//     api_key:    '<base64 id:secret — a read-only key minted for this claim>',
//     index:      'ax-inbox'  |  'ax-inbox,ax-contracts',   // comma-joined
//     brain:      'inbox' | null,       // the brain whose graph they may walk
//     expires_at: '2026-09-25T18:00:00Z',                   // RFC 3339
//     label:      'Q3 board pack' | null,
//   })
//
// When that record is present, well-formed and unexpired, the console is a
// read-only VIEWER of exactly those indices with exactly that key:
//
//   • it boots `guest-app.js`, not the operator shell — no dashboards, no
//     Data / Users / Settings / Alerts, no edit mode, no sync, no mock data;
//   • the ONLY requests it can make are the four in GUEST_OPS below, each
//     built here from a validated index/brain name — there is no function in
//     guest mode that takes a path;
//   • `installGuestGuard` additionally refuses every other fetch before it
//     reaches the network, so a module that "just calls /_xerj-console/api/…"
//     cannot do so by accident. The engine denies those calls anyway (the key
//     is `read` on the shared indices and nothing else); the guard is there
//     so the UI never even asks, and so a bug shows up as a test failure
//     instead of as a 403 in someone's audit log;
//   • the key is attached per request by `guestRequest` — never globally, so
//     it cannot ride along on a request nobody meant to authorize;
//   • it FAILS CLOSED: a record with no key, no index, a malformed index or
//     brain name, or a missing / unparseable / past `expires_at` is not a
//     share, and the console falls back to the normal login flow.
//
// Everything here is pure or takes its globals as parameters, so it is
// testable under node (test/guest.test.mjs) and in a browser (test/browser/).
// ============================================================

export const SHARE_KEY = 'xerj.share';

// Index / brain names as the engine accepts them, minus everything a guest
// must never name: no patterns (`*`), no `_all`, no hidden/system (`.…`),
// no path or query metacharacters. A share naming anything else is rejected
// whole rather than "cleaned".
const NAME_RE = /^[a-z0-9][a-z0-9_.-]{0,254}$/;
const KEY_RE = /^[A-Za-z0-9+/=_-]{16,4096}$/;
export const MAX_GUEST_INDICES = 32; // share.rs MAX_INDICES

export function validName(s) {
  return typeof s === 'string' && NAME_RE.test(s) && s !== '_all' && !s.includes('..');
}

/** epoch-ms for an RFC 3339 / ISO string or an epoch-ms number; null when
 *  absent or unparseable. */
export function expiryMs(v) {
  if (v == null || v === '') return null;
  if (typeof v === 'number') return Number.isFinite(v) && v > 0 ? v : null;
  if (typeof v !== 'string') return null;
  const t = v.trim();
  if (/^\d{10,16}$/.test(t)) return Number(t);
  const p = Date.parse(t);
  return Number.isFinite(p) ? p : null;
}

/**
 * Parse a stored share record. Null when absent, malformed, incomplete or
 * expired at `now` — see "FAILS CLOSED" above.
 */
export function parseShare(raw, now = Date.now()) {
  if (!raw) return null;
  let obj = raw;
  if (typeof raw === 'string') {
    try { obj = JSON.parse(raw); } catch { return null; }
  }
  if (!obj || typeof obj !== 'object' || Array.isArray(obj)) return null;

  const api_key = typeof obj.api_key === 'string' ? obj.api_key.trim() : '';
  if (!KEY_RE.test(api_key)) return null;

  let names = [];
  if (typeof obj.index === 'string') names = obj.index.split(',');
  else if (Array.isArray(obj.indices)) names = obj.indices;
  names = names.map((n) => (typeof n === 'string' ? n.trim() : n));
  if (!names.length || names.length > MAX_GUEST_INDICES) return null;
  if (!names.every(validName)) return null;
  const indices = [...new Set(names)];

  let brain = null;
  if (obj.brain != null && obj.brain !== '') {
    if (!validName(obj.brain)) return null;
    brain = obj.brain;
  }

  const expires_at = expiryMs(obj.expires_at);
  if (expires_at == null || expires_at <= now) return null;

  const label = typeof obj.label === 'string' && obj.label.trim()
    ? obj.label.trim().slice(0, 120)
    : indices.join(', ');
  return Object.freeze({ api_key, indices: Object.freeze(indices), index: indices.join(','), brain, expires_at, label });
}

/** Read the share record from a Storage-like object (sessionStorage). */
export function readShare(storage, now = Date.now()) {
  if (!storage || typeof storage.getItem !== 'function') return null;
  let raw = null;
  try { raw = storage.getItem(SHARE_KEY); } catch { return null; }
  return parseShare(raw, now);
}

/**
 * "A share ended in this tab" — the REASON only ('expired' | 'unauthorized' |
 * 'left' | 'invalid'), never the key, the index or the label. boot.js boots
 * the guest shell when it is present, so a reload shows the ended screen again
 * instead of bouncing a guest to the operator's passkey login.
 */
export const ENDED_KEY = 'xerj.share.ended';
const ENDED_REASONS = new Set(['expired', 'unauthorized', 'left', 'invalid']);
export function markEnded(storage, reason) {
  try { if (storage && typeof storage.setItem === 'function') storage.setItem(ENDED_KEY, ENDED_REASONS.has(reason) ? reason : 'invalid'); } catch { /* storage blocked */ }
}
export function readEnded(storage) {
  try { const r = storage && storage.getItem(ENDED_KEY); return ENDED_REASONS.has(r) ? r : (r != null ? 'invalid' : null); } catch { return null; }
}
export function clearEnded(storage) {
  try { if (storage && typeof storage.removeItem === 'function') storage.removeItem(ENDED_KEY); } catch { /* storage blocked */ }
}

/** Forget the share. Called on expiry, on a 401, and on "leave". */
export function clearShare(storage) {
  try { if (storage && typeof storage.removeItem === 'function') storage.removeItem(SHARE_KEY); } catch { /* storage blocked */ }
}

export function isExpired(share, now = Date.now()) {
  return !share || !(share.expires_at > now);
}

/** "in 1h 20m" / "in 3d 2h" / "now" — relative remaining time. */
export function fmtRemaining(ms) {
  if (ms == null) return '';
  if (ms <= 0) return 'now';
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400), hh = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d > 0) return `in ${d}d ${hh}h`;
  if (hh > 0) return `in ${hh}h ${m}m`;
  if (m > 0) return `in ${m}m`;
  return `in ${s}s`;
}

/** Banner text: `Guest · read-only · <label> · expires <UTC> (<relative>)`. */
export function guestBanner(share, now = Date.now()) {
  if (!share) return '';
  const iso = new Date(share.expires_at).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
  return `Guest · read-only · ${share.label} · expires ${iso} (${fmtRemaining(share.expires_at - now)})`;
}

/** The Authorization header value the engine expects for an API key. */
export function guestAuthHeader(share) {
  return `ApiKey ${share.api_key}`;
}

// ----- the only requests a guest can make ------------------------------

/** op → { method, path(share, arg) }. `arg` is validated by `guestUrl`. */
export const GUEST_OPS = Object.freeze({
  search:  { method: 'POST', suffix: '/_search' },
  count:   { method: 'POST', suffix: '/_count' },
  mapping: { method: 'GET',  suffix: '/_mapping' },
  ego:     { method: 'GET' },
});

/** Subset of the share's indices → the `/{a,b}` path segment, or null if any
 *  requested name is not part of the share. No argument = all of them. */
export function guestIndexExpr(share, indices) {
  const want = indices == null ? share.indices : (Array.isArray(indices) ? indices : [indices]);
  if (!want.length) return null;
  for (const n of want) if (!share.indices.includes(n)) return null;
  return want.map(encodeURIComponent).join(',');
}

/** Build the same-origin path for an op, or null when the share does not
 *  allow it. `query` (ego only) is a plain object of string params. */
export function guestUrl(share, op, { indices, query } = {}) {
  if (!share || !Object.prototype.hasOwnProperty.call(GUEST_OPS, op)) return null;
  if (op === 'ego') {
    if (!share.brain) return null;
    const qs = new URLSearchParams();
    for (const [k, v] of Object.entries(query || {})) if (v != null) qs.set(k, String(v));
    return `/_graph/${encodeURIComponent(share.brain)}/ego?${qs.toString()}`;
  }
  const expr = guestIndexExpr(share, indices);
  if (expr == null) return null;
  return `/${expr}${GUEST_OPS[op].suffix}`;
}

/**
 * Is `method path` one of the requests this share may make? Used by the
 * fetch guard. `path` is a same-origin pathname (no query string).
 */
export function guestAllows(share, method, path) {
  if (!share || typeof path !== 'string') return false;
  const m = String(method || 'GET').toUpperCase();
  if (share.brain && m === 'GET' && path === `/_graph/${encodeURIComponent(share.brain)}/ego`) return true;
  const mm = path.match(/^\/([^/]+)\/(_search|_count|_mapping)$/);
  if (!mm) return false;
  const opMethod = mm[2] === '_mapping' ? 'GET' : 'POST';
  if (m !== opMethod) return false;
  let names;
  try { names = mm[1].split(',').map(decodeURIComponent); } catch { return false; }
  return names.length > 0 && names.every((n) => share.indices.includes(n));
}

/** Thrown by `guestRequest`. `kind` is what the UI acts on; the engine's
 *  error body is deliberately NOT carried — a 403 names resources and the
 *  grant that would fix it, which is not the guest's to read. */
export class GuestError extends Error {
  constructor(kind, status) {
    super(kind);
    this.name = 'GuestError';
    this.kind = kind;       // 'expired' | 'unauthorized' | 'forbidden' | 'not-found' | 'blocked' | 'http' | 'network'
    this.status = status || 0;
  }
}

/**
 * Make one guest request. The URL comes from `guestUrl` (never from a
 * caller-supplied path), the key is attached here and only here.
 *
 *   env: { fetch, now }   — injected for tests; defaults to globals.
 */
export async function guestRequest(share, op, { indices, query, body, signal } = {}, env = {}) {
  const now = (env.now || Date.now)();
  if (isExpired(share, now)) throw new GuestError('expired', 0);
  const url = guestUrl(share, op, { indices, query });
  if (!url) throw new GuestError('blocked', 0);
  const doFetch = env.fetch || globalThis.fetch;
  const init = {
    method: GUEST_OPS[op].method,
    credentials: 'omit',           // a guest never rides a console session cookie
    cache: 'no-store',
    referrerPolicy: 'no-referrer',
    headers: { accept: 'application/json', authorization: guestAuthHeader(share) },
    signal,
  };
  if (init.method === 'POST') {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body || {});
  }
  let r;
  try { r = await doFetch(url, init); } catch (e) {
    if (e && e.name === 'AbortError') throw e;
    throw new GuestError('network', 0);
  }
  if (r.status === 401) throw new GuestError('unauthorized', 401);
  if (r.status === 403) throw new GuestError('forbidden', 403);
  if (r.status === 404) throw new GuestError('not-found', 404);
  if (!r.ok) throw new GuestError('http', r.status);
  try { return await r.json(); } catch { throw new GuestError('http', r.status); }
}

// ----- the fetch guard --------------------------------------------------

/**
 * Wrap the page's request APIs so that, while a guest session is active,
 * NOTHING but the share's own four operations leaves the page:
 *
 *   fetch                 a refused call resolves to a synthetic 403 (calling
 *                         code sees an ordinary denied response)
 *   XMLHttpRequest.open   a refused call throws a SecurityError
 *   navigator.sendBeacon  a refused call returns false
 *
 * Each refusal is counted on `win.__xerjGuestBlocked` for the tests.
 * Cross-origin requests are refused too — the guest shell has no reason to
 * make one, and a key-bearing page should not be talking to third parties.
 * (What this does not cover — WebSocket, EventSource, a `<form>` — is stopped
 * by the Content-Security-Policy's `connect-src 'self'` / `form-action` for
 * other origins and by the engine, which has no session and no key, on this
 * one. The guard is a UI-side tripwire, not the boundary; the key is.)
 *
 * The guard does NOT add the Authorization header. Idempotent.
 */
export function installGuestGuard(share, win = globalThis) {
  if (!share || !win || typeof win.fetch !== 'function') return;
  if (win.fetch.__xerjGuestGuard) return;
  const orig = win.fetch.bind(win);
  const base = win.location && win.location.href;
  const origin = win.location && win.location.origin;
  win.__xerjGuestBlocked = win.__xerjGuestBlocked || [];
  /** null when the request may go out; otherwise where it was headed. */
  const refusedAt = (method, raw) => {
    let u;
    try { u = new URL(raw, base); } catch { return '(unparseable url)'; }
    if (u.origin !== origin) return u.origin;
    if (!guestAllows(share, method, u.pathname)) return u.pathname;
    return null;
  };
  const refuse = (method, where) => {
    win.__xerjGuestBlocked.push(`${method} ${where}`);
    const Resp = win.Response || globalThis.Response;
    return Promise.resolve(new Resp('{"error":{"type":"guest_blocked"}}', {
      status: 403, headers: { 'content-type': 'application/json' },
    }));
  };
  const guarded = (input, init) => {
    const raw = typeof input === 'string' ? input : (input && typeof input.url === 'string' ? input.url : String(input || ''));
    const method = String((init && init.method) || (input && typeof input === 'object' && input.method) || 'GET').toUpperCase();
    const where = refusedAt(method, raw);
    return where == null ? orig(input, init) : refuse(method, where);
  };
  guarded.__xerjGuestGuard = true;
  win.fetch = guarded;

  const XHR = win.XMLHttpRequest;
  if (XHR && XHR.prototype && typeof XHR.prototype.open === 'function' && !XHR.prototype.open.__xerjGuestGuard) {
    const origOpen = XHR.prototype.open;
    const guardedOpen = function open(method, url, ...rest) {
      const m = String(method || 'GET').toUpperCase();
      const where = refusedAt(m, String(url));
      if (where != null) {
        win.__xerjGuestBlocked.push(`XHR ${m} ${where}`);
        const DomEx = win.DOMException || globalThis.DOMException;
        throw DomEx ? new DomEx('guest session: request refused', 'SecurityError') : new Error('guest session: request refused');
      }
      return origOpen.call(this, method, url, ...rest);
    };
    guardedOpen.__xerjGuestGuard = true;
    XHR.prototype.open = guardedOpen;
  }

  const nav = win.navigator;
  if (nav && typeof nav.sendBeacon === 'function' && !nav.sendBeacon.__xerjGuestGuard) {
    const origBeacon = nav.sendBeacon.bind(nav);
    const guardedBeacon = (url, data) => {
      const where = refusedAt('POST', String(url));
      if (where != null) { win.__xerjGuestBlocked.push(`BEACON POST ${where}`); return false; }
      return origBeacon(url, data);
    };
    guardedBeacon.__xerjGuestGuard = true;
    try { Object.defineProperty(nav, 'sendBeacon', { value: guardedBeacon, configurable: true, writable: true }); } catch { nav.sendBeacon = guardedBeacon; }
  }
}

// ----- expiry -----------------------------------------------------------

/**
 * Call `onExpire` once, when the share's `expires_at` passes. setTimeout
 * cannot hold more than ~24.8 days and is throttled in background tabs, so
 * this re-arms in steps of at most `maxStepMs` and re-checks the clock each
 * time. Returns a cancel function.
 *
 *   env: { now, setTimeout, clearTimeout }  — injected for tests.
 */
export function watchExpiry(share, onExpire, env = {}) {
  const now = env.now || Date.now;
  const set = env.setTimeout || globalThis.setTimeout;
  const clear = env.clearTimeout || globalThis.clearTimeout;
  const maxStepMs = env.maxStepMs || 60_000;
  let timer = null;
  let done = false;
  const tick = () => {
    if (done) return;
    const left = share.expires_at - now();
    if (left <= 0) { done = true; onExpire(); return; }
    timer = set(tick, Math.min(left, maxStepMs));
  };
  tick();
  return () => { done = true; if (timer != null) clear(timer); };
}
