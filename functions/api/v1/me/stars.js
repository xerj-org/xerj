// GET /api/v1/me/stars — the signed-in account's hub stars, with star history.
//
//   401 { ok:false, enabled } without a session (or when sign-in is disabled)
//   200 { ok:true, enabled:true, stars: [{ kind, id, href, starred_at, count, history: [[YYYY-MM-DD, n], ...] }] }
//        stars: newest first; history: stars that target received per day since
//        daily history began (functions/api/hub-stars.js day/ keys), oldest first.
//   No HUB_STARS binding → 200 { ok:true, enabled:false, stars: [] }.
//
// Reads only R2 keys under me/<userHash>/ (this account) and the public per-target
// counts. Nothing about other accounts or any IP is returned.
import { json, requireUser } from '../../../_lib/session.js';
import { userHash } from '../../hub-stars.js';

const MAX_ITEMS = 200;        // listed
const MAX_HISTORY = 50;       // newest N get a history (one R2 list each)

async function listAll(bucket, prefix, max, meta = false) {
  const out = [];
  let cursor;
  for (let page = 0; page < 10 && out.length < max; page++) {
    const res = await bucket.list({ prefix, cursor, ...(meta ? { include: ['customMetadata'] } : {}) });
    out.push(...res.objects);
    if (!res.truncated) break;
    cursor = res.cursor;
  }
  return out.slice(0, max);
}

async function pages(env, request) {
  try {
    const r = env.ASSETS ? await env.ASSETS.fetch(new URL('/contributors/stars-targets.json', request.url)) : null;
    return r && r.ok ? ((await r.json()).pages || {}) : {};
  } catch (_) { return {}; }
}

export async function onRequestGet({ request, env }) {
  const [user, deny] = await requireUser(request, env);
  if (deny) return deny;
  const bucket = env.HUB_STARS;
  if (!bucket) return json({ ok: true, enabled: false, stars: [] });
  const uh = await userHash(user, env);
  const prefix = `me/${uh}/`;
  let stars;
  try {
    const objs = await listAll(bucket, prefix, MAX_ITEMS);
    stars = await Promise.all(objs.map(async (o) => {
      const [kind, ...rest] = o.key.slice(prefix.length).split('/');
      const at = await bucket.get(o.key).then((b) => (b ? b.text() : '')).catch(() => '');
      return { kind, id: rest.join('/'), starred_at: at || (o.uploaded ? new Date(o.uploaded).toISOString() : '') };
    }));
    stars.sort((a, b) => (a.starred_at < b.starred_at ? 1 : -1));
    const logins = await pages(env, request);
    for (const s of stars) {
      s.href = s.kind === 'corpus' ? `/corpus/${s.id}.html` : `/contributors/${logins[s.id] || s.id}.html`;
    }
    await Promise.all(stars.map(async (s, i) => {
      const c = await bucket.get(`stars/${s.kind}/${s.id}`).catch(() => null);
      s.count = c ? (parseInt(await c.text(), 10) || 0) : 0;
      if (i >= MAX_HISTORY) return;
      const dp = `day/${s.kind}/${s.id}/`;
      const days = await listAll(bucket, dp, 400, true).catch(() => []);
      s.history = days.map((d) => [d.key.slice(dp.length), parseInt((d.customMetadata || {}).count, 10) || 0])
        .filter(([, n]) => n > 0).sort((a, b) => (a[0] < b[0] ? -1 : 1));
    }));
  } catch (e) {
    console.log('[me/stars] R2 failed:', String(e));
    return json({ ok: false, enabled: true, error: 'storage unavailable' }, 503);
  }
  return json({ ok: true, enabled: true, stars });
}
