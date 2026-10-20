// /api/hub-traffic — the Traffic panel of a corpus page (GitHub-Insights style).
//
//   GET /api/hub-traffic?corpus=<slug>
//   → { ok, enabled, corpus, window_days:14, days:[{date, views, unique_views, pulls, searches}],
//       totals:{views, unique_views, pulls, pulls_unique:null, searches},
//       agents:[{family,count}], referrers:[{host,count}], pulls_since, recording_since, k }
//   A day before recording_since (views, searches) or pulls_since (pulls) is null.
//   No HUB_USAGE binding → { ok:true, enabled:false }
//
// views / unique_views: GETs of the corpus page (functions/corpus/[[path]].js);
//   unique = distinct daily visitor hashes, so the 14-day unique figure is the sum
//   of daily uniques (the daily salt means two days cannot be linked).
// pulls: growth of the GitHub release download counts of the corpus's pack
//   releases (`xerj hub pull` downloads from there). null for days before the
//   first snapshot. Includes the downloads our own pack-verification CI makes.
// searches: hosted-hub code/def queries for the corpus (functions/_lib/hub-usage.js).
// agents: user-agent families of views + searches. referrers: external sites that
//   sent at least k=3 views (host only).
// What is stored and the opt-out: functions/_lib/hub-traffic.js, hub-usage.js.
import { day } from '../_lib/hub-usage.js';
import { loadDays } from './hub-usage.js';
import { rollupViews, pullsBetween, fetchPackSnapshot, TRAFFIC_DAYS, REFERRER_K } from '../_lib/hub-traffic.js';

const SLUG_RE = /^[a-z0-9][a-z0-9._-]{0,99}$/;
const json = (obj, status = 200, cache = 'no-store') => new Response(JSON.stringify(obj), {
  status, headers: { 'content-type': 'application/json', 'cache-control': cache },
});

const closedViews = new Map();
const snaps = new Map();
let todayViews = { day: '', at: 0, roll: null };
export function _resetTrafficCaches() { closedViews.clear(); snaps.clear(); todayViews = { day: '', at: 0, roll: null }; }

async function listViews(bucket, d) {
  const ev = [];
  let cursor;
  for (let page = 0; page < 30; page++) {
    const res = await bucket.list({ prefix: `pv/${d}/`, cursor, include: ['customMetadata'] });
    for (const o of res.objects) { const m = o.customMetadata || {}; ev.push({ corpus: o.key.split('/')[2], u: m.u, a: m.a, f: m.f }); }
    if (!res.truncated) break;
    cursor = res.cursor;
  }
  return rollupViews(ev);
}

async function viewsFor(bucket, d, today, now) {
  if (d === today) {
    if (todayViews.day !== today || now - todayViews.at > 120_000) todayViews = { day: today, at: now, roll: await listViews(bucket, today) };
    return todayViews.roll;
  }
  if (closedViews.has(d)) return closedViews.get(d);
  let roll = null;
  const o = await bucket.get(`tagg/${d}.json`);
  if (o) { try { roll = JSON.parse(await o.text()); } catch (_) { roll = null; } }
  if (!roll) { roll = await listViews(bucket, d); await bucket.put(`tagg/${d}.json`, JSON.stringify(roll)); }
  closedViews.set(d, roll);
  return roll;
}

async function snapFor(bucket, d) {
  if (snaps.has(d)) return snaps.get(d);
  const o = await bucket.get(`gh/${d}.json`);
  let s = null;
  if (o) { try { s = JSON.parse(await o.text()); } catch (_) { s = null; } }
  if (s) snaps.set(d, s);
  return s;
}

/** Take today's pack-download snapshot once (at most one try per hour). */
export async function ensureSnapshot(bucket, env, now = Date.now(), fetchImpl = fetch) {
  const today = day(now);
  if (await snapFor(bucket, today)) return 'have';
  const tryKey = `meta/gh-try-${today}-${new Date(now).getUTCHours()}`;
  if (await bucket.get(tryKey)) return 'tried';
  await bucket.put(tryKey, '1');
  const snap = await fetchPackSnapshot(fetchImpl, (env && env.HUB_GITHUB_TOKEN) || '');
  await bucket.put(`gh/${today}.json`, JSON.stringify(snap));
  snaps.set(today, snap);
  return 'taken';
}

export async function onRequest({ request, env, waitUntil }) {
  if (request.method !== 'GET') return json({ ok: false, error: 'method not allowed; use GET' }, 405);
  const corpus = (new URL(request.url).searchParams.get('corpus') || '').toLowerCase();
  if (!SLUG_RE.test(corpus)) return json({ ok: false, error: 'corpus is required' }, 400);
  const bucket = env && env.HUB_USAGE;
  if (!bucket) return json({ ok: true, enabled: false }, 200, 'public, max-age=300');

  const now = Date.now();
  const today = day(now);
  try {
    const snapP = ensureSnapshot(bucket, env, now).catch((e) => console.log('[hub-traffic] snapshot:', String(e)));
    if (waitUntil) waitUntil(snapP);
    const { days: usageDays } = await loadDays(bucket, now);
    const days = [];
    const agents = {}, refs = {};
    // Days before counting began anywhere on the hub are unknown (null), not 0.
    let recordingSince = null;
    let pullsSince = null;
    for (let i = TRAFFIC_DAYS - 1; i >= 0; i--) {
      const d = day(now - i * 86400000);
      const vroll = await viewsFor(bucket, d, today, now);
      if (!recordingSince && (Object.keys(vroll).length || Object.keys(usageDays[d] || {}).length)) recordingSince = d;
      const v = vroll[corpus] || { v: 0, vu: 0, a: {}, f: {} };
      const u = (usageDays[d] || {})[corpus] || { n: 0, a: {}, r: {} };
      const known = !!recordingSince;
      const prev = await snapFor(bucket, day(now - (i + 1) * 86400000));
      const cur = await snapFor(bucket, d);
      const delta = pullsBetween(prev, cur);
      if (delta && !pullsSince) pullsSince = d;
      days.push({ date: d, views: known ? v.v : null, unique_views: known ? v.vu : null,
                  pulls: delta ? (delta[corpus] || 0) : null,
                  searches: known ? (u.r.code || 0) + (u.r.def || 0) : null });
      for (const src of [v.a, u.a]) for (const [f, n] of Object.entries(src || {})) agents[f] = (agents[f] || 0) + n;
      for (const [h, n] of Object.entries(v.f || {})) refs[h] = (refs[h] || 0) + n;
    }
    const sum = (k) => days.reduce((n, x) => n + (x[k] || 0), 0);
    const sorted = (o, key) => Object.entries(o).sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)).map(([x, count]) => ({ [key]: x, count }));
    return json({
      ok: true, enabled: true, corpus, window_days: TRAFFIC_DAYS, k: REFERRER_K, days,
      totals: { views: sum('views'), unique_views: sum('unique_views'), pulls: pullsSince ? sum('pulls') : null,
                pulls_unique: null, searches: sum('searches') },
      pulls_since: pullsSince, recording_since: recordingSince,
      agents: sorted(agents, 'family').filter((x) => x.family !== 'unknown'),
      referrers: sorted(refs, 'host').filter((x) => x.count >= REFERRER_K).slice(0, 10),
    }, 200, 'public, max-age=120');
  } catch (e) {
    console.log('[hub-traffic] read failed:', String(e));
    return json({ ok: false, enabled: true, error: 'storage unavailable' }, 503);
  }
}
