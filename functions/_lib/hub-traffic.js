// Hub traffic: GitHub-Insights-style counts per corpus page on hub.xerj.org.
// Not a route (no onRequest export).
//
// ─────────────────────────────── WHAT IS STORED ─────────────────────────
// Views: one R2 object per counted GET of /corpus/<slug>(.html) (functions/corpus/[[path]].js)
//   pv/<YYYY-MM-DD>/<corpus>/<ms>-<rand>   empty body; customMetadata
//        u = daily visitor hash: first 16 hex of sha256(IPv4 or IPv6 /64 + day + salt).
//            The salt is the Pages secret HUB_TRAFFIC_SALT (else HUB_STARS_SALT, the
//            hub-stars approach); the day in the hash means two days never link.
//        a = user-agent family (uaFamily), f = referring site host (external only)
//   tagg/<day>.json   the day's views per corpus: count, distinct u, families, sites
//                     (counts only, no hashes), written on the first read after the day
// Pulls: GitHub release download counts for pack releases, snapshotted at most once
//   a day into gh/<day>.json ({tag: [slug, downloads]}); a day's pulls are the growth
//   since the day before. GitHub reports no unique count, so pulls have none.
// Searches: the hosted-hub query events of functions/_lib/hub-usage.js.
// Search-engine crawlers (uaFamily 'crawler') are not counted as views.
// No raw IP, no full user agent, no full referrer URL, no cookie, no user id.
// Lifecycle on xerj-hub-usage: pv/, tagg/, gh/ expire after 40 days.
import { sourceKey } from '../api/hub-stars.js';
import { uaFamily, optedOut, overRate, day } from './hub-usage.js';

export const VIEW_CAP = 4000;          // ×2 Class-A writes/day = 240k/mo
export const TRAFFIC_DAYS = 14;
export const REFERRER_K = 3;           // a referring site is shown once it sent >= 3 views
const SELF = /(^|\.)(hub\.xerj\.org|xerj-hub\.pages\.dev|localhost|127\.0\.0\.1)$/;

export async function dailyHash(ip, d, salt) {
  const key = sourceKey(ip);
  if (!key) return '';
  const dig = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`${key}|${d}|${salt}`));
  return [...new Uint8Array(dig).slice(0, 8)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** The referring site, host only, for an external referrer; '' otherwise. */
export function refHost(referer) {
  try {
    const u = new URL(String(referer || ''));
    if (!/^https?:$/.test(u.protocol)) return '';
    const h = u.hostname.toLowerCase().replace(/^www\./, '');
    return !h || SELF.test(h) || /^\d+(\.\d+){3}$/.test(h) || h.includes(':') ? '' : h.slice(0, 80);
  } catch (_) { return ''; }
}

export async function recordView(env, request, corpus) {
  try {
    const bucket = env && env.HUB_USAGE;
    if (!bucket) return 'disabled';
    if (optedOut(request)) return 'opted-out';
    if (request.headers.get('sec-purpose') || request.headers.get('purpose') === 'prefetch') return 'prefetch';
    const family = uaFamily(request.headers.get('user-agent'));
    if (family === 'crawler') return 'crawler';          // search-engine crawlers are not readers
    if (await overRate(request)) return 'rate-limited';
    const today = day();
    const cntKey = `meta/pv-count-${today}`;
    let n = 0;
    try { const c = await bucket.get(cntKey); if (c) n = parseInt(await c.text(), 10) || 0; } catch (_) { n = 0; }
    if (n >= VIEW_CAP) return 'capped';
    const salt = env.HUB_TRAFFIC_SALT || env.HUB_STARS_SALT || 'xerj-hub-traffic-v1';
    const meta = {
      u: await dailyHash(request.headers.get('CF-Connecting-IP'), today, salt),
      a: family,
      f: refHost(request.headers.get('referer')),
    };
    await bucket.put(`pv/${today}/${corpus}/${Date.now()}-${Math.random().toString(36).slice(2, 8)}`, '', { customMetadata: meta });
    await bucket.put(cntKey, String(n + 1));
    return 'recorded';
  } catch (e) {
    console.log('[hub-traffic] record failed:', String(e));
    return 'error';
  }
}

/** Views of one day → {corpus: {v, vu, a:{}, f:{}}}; the visitor hashes are reduced to a count. */
export function rollupViews(events) {
  const out = {}, seen = {};
  for (const e of events) {
    const c = (out[e.corpus] ||= { v: 0, vu: 0, a: {}, f: {} });
    c.v++;
    const s = (seen[e.corpus] ||= new Set());
    if (e.u && !s.has(e.u)) { s.add(e.u); c.vu++; }
    c.a[e.a || 'unknown'] = (c.a[e.a || 'unknown'] || 0) + 1;
    if (e.f) c.f[e.f] = (c.f[e.f] || 0) + 1;
  }
  return out;
}

/** Pack slug from a release asset name `<slug>-<8hex>.hubpack.tar.zst`. */
export function packSlug(asset) {
  const m = /^(.+)-[0-9a-f]{7,40}\.hubpack\.tar\.zst$/.exec(String(asset || ''));
  return m ? m[1].toLowerCase() : '';
}

/** {tag: [slug, downloads]} from GitHub's releases list (pack tarballs only). */
export function snapshotFromReleases(releases) {
  const snap = {};
  for (const r of releases || []) {
    if (!String(r.tag_name || '').startsWith('pack-')) continue;
    for (const a of r.assets || []) {
      const slug = packSlug(a.name);
      if (slug) snap[r.tag_name] = [slug, (snap[r.tag_name]?.[1] || 0) + (a.download_count || 0)];
    }
  }
  return snap;
}

/** Per-slug pulls on day d = growth of each pack release since the day before (a new release counts whole). */
export function pullsBetween(prev, cur) {
  const out = {};
  if (!prev || !cur) return null;
  for (const [tag, [slug, n]] of Object.entries(cur)) {
    const before = prev[tag] ? prev[tag][1] : 0;
    out[slug] = (out[slug] || 0) + Math.max(0, n - before);
  }
  return out;
}

export async function fetchPackSnapshot(fetchImpl = fetch, token = '') {
  const releases = [];
  for (let page = 1; page <= 15; page++) {
    const res = await fetchImpl(`https://api.github.com/repos/xerj-org/xerj/releases?per_page=100&page=${page}`, {
      headers: { 'user-agent': 'xerj-hub-traffic', accept: 'application/vnd.github+json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    });
    if (!res.ok) throw new Error(`github releases ${res.status}`);
    const batch = await res.json();
    releases.push(...batch);
    if (batch.length < 100) break;
  }
  return snapshotFromReleases(releases);
}
