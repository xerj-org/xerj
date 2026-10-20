// GET /api/v1/corpus/<slug> — pack details for one corpus (signed in only).
//
//   401 without a session · 404 unknown slug · 503 when this deploy has no details
//   200 { ok, corpus: { slug, lane, status, refresh, added, repos | repos_summary,
//                       builds?, pins?, pack_stats?, pack_pubkey?, contributors? },
//         signing: { hub_pubkey, scheme }, catalogue: entry | null, generated }
//   builds: pack pipeline runs for this corpus, newest first (date, run url, pin8,
//   result, bytes, defs, stage, error, release tag); pins: each pin the pipeline
//   built, first/last build date and the published tag. catalogue: this corpus's
//   entry in the signed /api/v1/catalogue.json when the site carries one (pack url,
//   sha256, signature url, size), read from the deployed asset, never modified.
import { json, requireUser } from '../../../_lib/session.js';
import { details } from '../../../_lib/details.js';

const SLUG_RE = /^[a-z0-9]([a-z0-9._-]*[a-z0-9])?$/;
const SCHEME = 'Each pack archive is checked against the hub key before extraction: sha256 of the archive, then its ed25519 signature (xerj hub pull does both).';

async function catalogueEntry(env, request, slug) {
  try {
    const r = env.ASSETS ? await env.ASSETS.fetch(new URL('/api/v1/catalogue.json', request.url)) : null;
    if (!r || !r.ok || !(r.headers.get('content-type') || '').includes('json')) return null;
    const cat = await r.json();
    return (cat.corpora || []).find((e) => e && (e.slug === slug || e.id === slug)) || null;
  } catch (_) { return null; }
}

export async function corpusResponse(slug, data, catalogue = null) {
  if (!SLUG_RE.test(slug || '')) return json({ ok: false, error: 'invalid slug' }, 400);
  if (!data) return json({ ok: false, error: 'details are not in this deploy yet' }, 503);
  const c = data.corpora[slug];
  if (!c) return json({ ok: false, error: 'unknown corpus' }, 404);
  return json({ ok: true, corpus: c, signing: { hub_pubkey: data.hub_pubkey || '', scheme: SCHEME }, catalogue, generated: data.generated });
}

export async function onRequestGet({ request, env, params }) {
  const [, deny] = await requireUser(request, env);
  if (deny) return deny;
  const slug = String(params.slug || '').toLowerCase();
  const data = details();
  return corpusResponse(slug, data, data && data.corpora[slug] ? await catalogueEntry(env, request, slug) : null);
}
