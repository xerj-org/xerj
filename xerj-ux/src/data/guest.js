// ============================================================
// XERJ Console — guest (shared-link) mode
//
// A share page (the engine's share flow) writes ONE sessionStorage record
// before handing the browser to the console:
//
//   sessionStorage['xerj.share'] = JSON.stringify({
//     api_key:    '<engine api key scoped to one index + one brain>',
//     index:      'ax-notes',          // the only index this viewer may read
//     brain:      'notes',             // the brain whose graph they may walk
//     expires_at: '2026-09-14T18:00:00Z' | 1789999999999,   // ISO or epoch-ms
//     label:      'Q3 board pack',     // what the sharer called it
//   })
//
// When that record is present and unexpired the console is a VIEWER:
//   • only Corpus (that index), Discover (locked to that index) and Reader
//     appear — no Dashboards / Alerts / Data / Users / Settings, no edit mode;
//   • every same-origin engine call carries `Authorization: ApiKey <key>`;
//   • nothing under /_xerj-console/api/v1/* is called except what a viewer
//     needs — the server denies the rest anyway, but a console that makes
//     failing calls reads as broken, so it does not make them.
//
// Everything except `installGuestFetch` is pure (testable under node).
// ============================================================

export const SHARE_KEY = 'xerj.share';

/** Sections a guest may see, in nav order. */
export const GUEST_SECTIONS = ['corpus', 'discover', 'reader'];

/** Parse a stored share record. Null when absent, malformed, incomplete
 *  (no key / no index) or expired at `now`. */
export function parseShare(raw, now = Date.now()) {
  if (!raw) return null;
  let obj = raw;
  if (typeof raw === 'string') {
    try { obj = JSON.parse(raw); } catch { return null; }
  }
  if (!obj || typeof obj !== 'object') return null;
  const api_key = typeof obj.api_key === 'string' ? obj.api_key.trim() : '';
  const index = typeof obj.index === 'string' ? obj.index.trim() : '';
  if (!api_key || !index) return null;
  const expiresMs = expiryMs(obj.expires_at);
  if (expiresMs != null && expiresMs <= now) return null;
  return {
    api_key,
    index,
    brain: typeof obj.brain === 'string' && obj.brain.trim() ? obj.brain.trim() : null,
    expires_at: expiresMs,
    label: typeof obj.label === 'string' && obj.label.trim() ? obj.label.trim() : index,
  };
}

/** epoch-ms for an ISO string or a number; null when absent/unparseable. */
export function expiryMs(v) {
  if (v == null || v === '') return null;
  if (typeof v === 'number') return Number.isFinite(v) ? v : null;
  const n = Number(v);
  if (Number.isFinite(n) && String(v).trim() !== '') return n;
  const t = Date.parse(String(v));
  return Number.isFinite(t) ? t : null;
}

/** Read the share record from a Storage-like object (sessionStorage). */
export function readShare(storage, now = Date.now()) {
  if (!storage || typeof storage.getItem !== 'function') return null;
  let raw = null;
  try { raw = storage.getItem(SHARE_KEY); } catch { return null; }
  return parseShare(raw, now);
}

/** Filter the primary nav to what a guest may open. Identity when not a guest. */
export function guestSections(sections, share) {
  if (!share) return sections;
  return (sections || []).filter((s) => GUEST_SECTIONS.includes(s.id));
}

/** May a guest open this section id? */
export function guestAllowsSection(id) {
  return GUEST_SECTIONS.includes(id);
}

/** The landing route for a guest: the reader on the shared index. */
export function guestHome(share) {
  return `#/reader?index=${encodeURIComponent(share.index)}` +
    (share.brain ? `&brain=${encodeURIComponent(share.brain)}` : '');
}

/** "in 1h 20m" / "in 3d 2h" / "now" — relative remaining time. */
export function fmtRemaining(ms) {
  if (ms == null) return '';
  if (ms <= 0) return 'now';
  const s = Math.floor(ms / 1000);
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d > 0) return `in ${d}d ${h}h`;
  if (h > 0) return `in ${h}h ${m}m`;
  if (m > 0) return `in ${m}m`;
  return `in ${s}s`;
}

/** Banner text: `shared: <label> · expires <UTC time> (<relative>)`. */
export function guestBanner(share, now = Date.now()) {
  if (!share) return '';
  const exp = share.expires_at;
  if (exp == null) return `shared: ${share.label} · no expiry recorded`;
  const iso = new Date(exp).toISOString().slice(0, 16).replace('T', ' ') + ' UTC';
  return `shared: ${share.label} · expires ${iso} (${fmtRemaining(exp - now)})`;
}

/** The Authorization header value the engine expects for an API key. */
export function guestAuthHeader(share) {
  return `ApiKey ${share.api_key}`;
}

/** True when `url` targets this origin (relative path or same absolute origin). */
export function isSameOrigin(url, origin) {
  const u = String(url || '');
  if (!u) return false;
  if (u.startsWith('/') && !u.startsWith('//')) return true;
  if (!origin) return false;
  return u === origin || u.startsWith(origin + '/') || u.startsWith(origin + '?') || u.startsWith(origin + '#');
}

/**
 * Wrap `win.fetch` so every same-origin request carries the share's API key
 * unless the caller already set an Authorization header. Idempotent: a
 * second install is a no-op.
 */
export function installGuestFetch(share, win = globalThis) {
  if (!share || !win || typeof win.fetch !== 'function' || win.fetch.__xerjGuest) return;
  const orig = win.fetch.bind(win);
  const origin = win.location && win.location.origin;
  const wrapped = (input, init) => {
    const url = typeof input === 'string' ? input
      : (input && typeof input.url === 'string') ? input.url
      : String(input || '');
    if (!isSameOrigin(url, origin)) return orig(input, init);
    const next = { ...(init || {}) };
    const headers = new win.Headers(next.headers || (input && input.headers) || undefined);
    if (!headers.has('authorization')) headers.set('authorization', guestAuthHeader(share));
    next.headers = headers;
    return orig(input, next);
  };
  wrapped.__xerjGuest = true;
  win.fetch = wrapped;
}
