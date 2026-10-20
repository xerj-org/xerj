// /api/hub-usage — aggregate agent usage of each corpus on hub.xerj.org.
//
// Counts come from requests to the hosted hub API (/api/v1/code|def|search,
// functions/_lib/hub-hosted.js), recorded by functions/_lib/hub-usage.js:
// template, route and user-agent family per request; no raw query, no IP, no
// user id. Local `xerj code` / `xerj ask` queries never leave the machine and
// are not counted.
//
// ─────────────────────────────── CONTRACT ───────────────────────────────
//   GET /api/hub-usage?corpus=<slug>
//     → { ok, enabled, corpus, k, window_days, since, d7, d30, agents30,
//         agent_families:{family:n}, routes:{route:n},
//         detail: { templates:[{template,count}], topics:[{term,count}] } | null,
//         detail_requires_signin }
//   GET /api/hub-usage            → { ok, enabled, top:[{corpus,d7,d30}], search:{d7,d30}, total30 }
//   No HUB_USAGE binding          → { ok:true, enabled:false } (pages show "not enabled")
//
// Templates are published only when seen >= k times in the 30-day window, after
// words seen fewer than k times are generalised to <term> (see kTemplates).
//
// ─────────────────────────────── SIGN-IN GATE ───────────────────────────
// Summary numbers (counts, agent families) are always public. The detail block
// (templates, topics) is shown to signed-in visitors only (hub sign-in,
// functions/_lib/session.js). HUB_USAGE_DETAIL_SIGNIN=0 makes it public.
// While sign-in itself is not enabled, nobody sees the detail; the response
// says so (signin_enabled:false) and the page explains it.
//
// Cost: closed days are rolled up once into agg/<day>.json (one list + one put
// per day, ever); later reads are one Class-B get per day, cached per isolate.
import { config as signinConfig, currentUser } from '../_lib/session.js';
import { aggregate, rollup, day, K, WINDOW_DAYS, SEARCH_CORPUS } from '../_lib/hub-usage.js';

const SLUG_RE = /^[a-z0-9][a-z0-9._-]{0,99}$/;
const TODAY_TTL_MS = 120_000;
const json = (obj, status = 200, cache = 'no-store') => new Response(JSON.stringify(obj), {
  status, headers: { 'content-type': 'application/json', 'cache-control': cache },
});

export function detailRequiresSignin(env) { return String((env && env.HUB_USAGE_DETAIL_SIGNIN) ?? '') !== '0'; }

export async function viewerSignedIn(request, env) {
  try { return !!(await currentUser(request, env || {})); } catch (_) { return false; }
}

const closedDays = new Map();          // day → rollup; immutable once the day is over
let todayCache = { day: '', at: 0, roll: null };

async function listDay(bucket, d) {
  const events = [];
  let cursor;
  for (let page = 0; page < 30; page++) {
    const res = await bucket.list({ prefix: `ev/${d}/`, cursor, include: ['customMetadata'] });
    for (const o of res.objects) {
      const corpus = o.key.split('/')[2];
      const m = o.customMetadata || {};
      events.push({ corpus, r: m.r, t: m.t, a: m.a });
    }
    if (!res.truncated) break;
    cursor = res.cursor;
  }
  return rollup(events);
}

export async function loadDays(bucket, now = Date.now()) {
  const today = day(now);
  const days = {};
  for (let i = 1; i < WINDOW_DAYS; i++) {
    const d = day(now - i * 86400000);
    if (closedDays.has(d)) { days[d] = closedDays.get(d); continue; }
    let roll = null;
    const o = await bucket.get(`agg/${d}.json`);
    if (o) { try { roll = JSON.parse(await o.text()); } catch (_) { roll = null; } }
    if (!roll) {
      roll = await listDay(bucket, d);
      await bucket.put(`agg/${d}.json`, JSON.stringify(roll), { httpMetadata: { contentType: 'application/json' } });
    }
    closedDays.set(d, roll);
    days[d] = roll;
  }
  if (todayCache.day !== today || now - todayCache.at > TODAY_TTL_MS)
    todayCache = { day: today, at: now, roll: await listDay(bucket, today) };
  days[today] = todayCache.roll;
  return { today, days };
}

export function _resetCaches() { closedDays.clear(); todayCache = { day: '', at: 0, roll: null }; }

export async function onRequest({ request, env }) {
  if (request.method !== 'GET') return json({ ok: false, error: 'method not allowed; use GET' }, 405);
  const url = new URL(request.url);
  const corpus = (url.searchParams.get('corpus') || '').toLowerCase();
  if (corpus && !SLUG_RE.test(corpus)) return json({ ok: false, error: 'invalid corpus' }, 400);
  const bucket = env && env.HUB_USAGE;
  if (!bucket) return json({ ok: true, enabled: false }, 200, 'public, max-age=300');

  let stats, today;
  try {
    const loaded = await loadDays(bucket);
    today = loaded.today;
    stats = aggregate(loaded.days, today, K);
  } catch (e) {
    console.log('[hub-usage] read failed:', String(e));
    return json({ ok: false, enabled: true, error: 'storage unavailable' }, 503);
  }
  const since = day(Date.parse(today + 'T00:00:00Z') - (WINDOW_DAYS - 1) * 86400000);

  if (!corpus) {
    const top = Object.entries(stats).filter(([c]) => c !== SEARCH_CORPUS)
      .map(([c, s]) => ({ corpus: c, d7: s.d7, d30: s.d30 }))
      .sort((a, b) => b.d30 - a.d30 || b.d7 - a.d7 || (a.corpus < b.corpus ? -1 : 1)).slice(0, 10);
    const s = stats[SEARCH_CORPUS] || { d7: 0, d30: 0 };
    const total30 = Object.values(stats).reduce((n, x) => n + x.d30, 0);
    return json({ ok: true, enabled: true, window_days: WINDOW_DAYS, since, top, search: { d7: s.d7, d30: s.d30 }, total30 },
      200, 'public, max-age=120');
  }

  const s = stats[corpus] || { d7: 0, d30: 0, agents30: 0, agent_families: {}, routes: {}, templates: [], topics: [] };
  const gated = detailRequiresSignin(env);
  const showDetail = !gated || await viewerSignedIn(request, env);
  return json({
    ok: true, enabled: true, corpus, k: K, window_days: WINDOW_DAYS, since,
    d7: s.d7, d30: s.d30, agents30: s.agents30, agent_families: s.agent_families, routes: s.routes,
    detail: showDetail ? { templates: s.templates, topics: s.topics } : null,
    detail_requires_signin: gated, signin_enabled: signinConfig(env || {}).enabled,
  }, 200, gated ? 'private, no-store' : 'public, max-age=120');
}
