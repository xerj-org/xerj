// The hosted Corpus Hub API on hub.xerj.org: /api/v1/code/<slug>,
// /api/v1/def/<slug>/<symbol> and /api/v1/search. Plan §6
// (docs/CORPUS_HUB_V2_IMPLEMENTATION.md) puts a xerj node's /_hub/v1/* routes
// (engine/crates/xerj-api/src/hub_api.rs) behind these public paths.
//
// Until a node is configured (Pages variable HUB_NODE_URL, e.g.
// https://node.example:9200) the routes answer an honest JSON 503 that says how
// to answer locally, instead of the static site's HTML page with a 200. With a
// node configured the request is passed through unchanged (GET only).
//
// Either way the request is counted for the corpus page's usage stats through
// recordUsage (functions/_lib/hub-usage.js): template, route and user-agent
// family only, never the raw query or the IP. See that file for the opt-out.
import { recordUsage, SEARCH_CORPUS } from './hub-usage.js';

const SLUG_RE = /^[a-z0-9][a-z0-9._-]{0,99}$/;
const MAX_Q = 4096;
const TARGETS_PATH = '/contributors/stars-targets.json';   // written by gen.py each deploy
let corporaCache = null;
export async function knownCorpora(env, request) {
  if (corporaCache) return corporaCache;
  try {
    const res = env && env.ASSETS ? await env.ASSETS.fetch(new URL(TARGETS_PATH, request.url)) : null;
    if (!res || !res.ok) return null;
    corporaCache = new Set(((await res.json()).corpus || []).map((x) => String(x).toLowerCase()));
    return corporaCache;
  } catch (_) { return null; }
}
const json = (obj, status = 200, extra = {}) => new Response(JSON.stringify(obj), {
  status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store', ...extra },
});

export function hosted(route) {
  return async function onRequest({ request, env, params = {}, waitUntil }) {
    if (request.method !== 'GET' && request.method !== 'HEAD')
      return json({ ok: false, error: 'method not allowed; use GET' }, 405, { allow: 'GET' });
    const url = new URL(request.url);
    const slug = route === 'search' ? SEARCH_CORPUS : String(params.slug || '').toLowerCase();
    if (route !== 'search') {
      const known = SLUG_RE.test(slug) ? await knownCorpora(env, request) : null;
      // An unreadable list fails closed for counting only: the request is still answered.
      if (!SLUG_RE.test(slug) || (known && !known.has(slug))) return json({ ok: false, error: 'unknown corpus' }, 404);
      if (!known) params = { ...params, _uncounted: true };
    }
    const query = route === 'def' ? String(params.symbol || '') : (url.searchParams.get('q') || '');
    if (query.length > MAX_Q) return json({ ok: false, error: 'query too long' }, 400);

    if (!params._uncounted) {
      const rec = recordUsage(env, request, { corpus: slug, route, query });
      if (waitUntil) waitUntil(rec); else await rec;
    }

    const base = String((env && env.HUB_NODE_URL) || '').replace(/\/+$/, '');
    if (!base) {
      const hint = route === 'search'
        ? 'Hosted answers are not available yet. Pull a corpus and ask locally: xerj hub pull <slug>; xerj ask "<question>".'
        : `Hosted answers are not available yet. Pull the pack and ask locally: xerj hub pull ${slug}; xerj ask --corpus ${slug} "<question>".`;
      return json({ ok: false, hosted: false, error: 'hosted hub not deployed', hint }, 503, { 'retry-after': '86400' });
    }
    const path = route === 'search' ? 'search'
      : route === 'def' ? `def/${encodeURIComponent(slug)}/${encodeURIComponent(query)}`
      : `code/${encodeURIComponent(slug)}`;
    try {
      const res = await fetch(`${base}/_hub/v1/${path}${url.search}`, {
        method: request.method,
        headers: { accept: 'application/json', 'x-forwarded-for': request.headers.get('CF-Connecting-IP') || '' },
        signal: AbortSignal.timeout(25_000),
      });
      const h = new Headers();
      for (const k of ['content-type', 'retry-after', 'etag', 'cache-control']) if (res.headers.has(k)) h.set(k, res.headers.get(k));
      return new Response(res.body, { status: res.status, headers: h });
    } catch (e) {
      console.log('[hub-hosted] node unreachable:', String(e));
      return json({ ok: false, hosted: false, error: 'hosted hub node unreachable' }, 502);
    }
  };
}
