// /api/corpus-feedback — structured reviews of a hub corpus FROM AGENTS.
//
// After `xerj hub pull` or MCP queries against a hub corpus, the xerj client
// sends one review (opt-out: --disable-feedback / XERJ_DISABLE_FEEDBACK=1), and
// agents can send one explicitly through the MCP tool xerj_corpus_feedback.
// This is the "Agent score" on hub.xerj.org corpus pages. It is NOT the human
// hub ★ (functions/api/hub-stars.js) and the two are never mixed.
//
// Built like functions/api/hub-stars.js and functions/api/field-report.js
// (main): R2 storage, per-IP-hash rate limit, daily write cap, raw IP never
// stored, fail-open when the bucket binding is absent.
//
// ─────────────────────────────── CONTRACT ───────────────────────────────
//   POST /api/corpus-feedback   (Content-Type: application/json, <= 4 KB)
//     { corpus:"<slug>", pack:"<pack version or pin>",
//       agent:{family:"claude", version:"4.5"},
//       usefulness:1-5, found:"yes"|"partial"|"no", freshness?:1-5,
//       query_template?:"how to <verb> <term>",      // placeholders, never a raw query
//       coverage_gaps?:["short string", ...],         // <= 5 × 60 chars
//       problems?:{kinds:["error"|"slow"|"stale"|"wrong"], note:"<= 280 chars"},
//       install_id?:"<16-64 random chars, generated locally>", source?:"hub-pull"|"mcp"|"agent"|"cli",
//       basis?:"agent"|"auto" }   // auto = the client derived it from signals, not the agent's judgement
//     A basis:"auto", source:"hub-pull" note may omit usefulness and found: it
//     feeds problem counts (and pull_reports), never the score.
//     An auto review never overwrites an agent review of the same week.
//     200 { ok, stored, replaced, week }       400 invalid · 413 · 415 · 403 cross-origin
//     429 rate limited / weekly volume · 503 daily cap or storage down
//   GET /api/corpus-feedback?corpus=<slug>  → { ok, enabled, corpus, ...aggregate }
//   GET /api/corpus-feedback                → { ok, enabled, scores:{slug:{score,reviews,confidence}} }
//   No CORPUS_FEEDBACK binding → GET {ok:true, enabled:false}; POST 200 stored:false.
//
// ──────────────────────────── ONE VOICE EACH ────────────────────────────
// A voter is sha256(install_id + salt) when the client sends an install id
// (random, made locally, never tied to a person), else sha256(IP or IPv6 /64 + salt).
// One vote per voter per corpus per ISO week: v/<slug>/<week>/<voter>; a later
// vote in the same week OVERWRITES it. The aggregate counts each voter's latest
// vote in the last 26 weeks once (functions/_lib/corpus-feedback.js).
// Anti-stuffing:
//   * one network source may speak for at most IP_VOTERS_PER_CORPUS distinct
//     voters per corpus per week (minting install ids does not multiply votes);
//   * a voter that reviews more than VOLUME_FLAG corpora in one week is flagged
//     (flag/<voter>) and ALL its votes are dropped from aggregates.
//
// ─────────────────────── STORAGE: R2, free-tier-safe ────────────────────
// Binding CORPUS_FEEDBACK → bucket xerj-corpus-feedback. Objects:
//   v/<slug>/<week>/<voter>       the review (JSON; compact copy in customMetadata)
//   rl/<ipHash>                   last accepted vote (ms)
//   meta/count-<YYYY-MM-DD>       accepted votes today (daily cap)
//   vol/<week>/<voter>            distinct corpora this voter reviewed this week
//   ipc/<week>/<slug>/<ipHash>    distinct voters this source spoke for, this corpus/week
//   flag/<voter>                  abnormal volume
// New vote ≤ 5 Class-A writes, replacement 3; DAILY_CAP×5 = 7,500/day ≈ 225k/mo.
// Lifecycle on the bucket: rl/ meta/ 2d, vol/ ipc/ 14d, flag/ 30d, v/ 400d.
// Aggregates are LIST-only (customMetadata) and edge-cached for 5 minutes.
// The raw IP, the raw install id and any problem note are never returned.

import {
  validateReview, isoWeek, weekCutoff, toMeta, fromMeta, aggregate, parseVoteKey,
} from '../_lib/corpus-feedback.js';
import { sourceKey } from './hub-stars.js';

const DAILY_CAP = 1500;
const RATE_WINDOW_MS = 1_000;
const MAX_BODY = 4096;
const IP_VOTERS_PER_CORPUS = 3;
const VOLUME_FLAG = 60;
const TARGETS_PATH = '/contributors/stars-targets.json';
const ORIGIN_OK = /^https:\/\/((www\.)?hub\.xerj\.org|([a-z0-9-]+\.)?xerj-hub\.pages\.dev)$|^http:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/;
const SLUG_RE = /^[a-z0-9][a-z0-9._-]{0,99}$/;

const JSON_HEADERS = { 'content-type': 'application/json', 'cache-control': 'no-store' };
const json = (obj, status = 200, extra = {}) =>
  new Response(JSON.stringify(obj), { status, headers: { ...JSON_HEADERS, ...extra } });

async function sha256hex(input) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}
const salt = (env) => env.CORPUS_FEEDBACK_SALT || env.HUB_STARS_SALT || 'xerj-corpus-feedback-v1';

let targetsCache = null;
async function corpusTargets(env, request) {
  if (targetsCache) return targetsCache;
  try {
    const res = env.ASSETS ? await env.ASSETS.fetch(new URL(TARGETS_PATH, request.url)) : null;
    if (!res || !res.ok) return null;
    const t = await res.json();
    targetsCache = new Set((t.corpus || []).map((x) => String(x).toLowerCase()));
    return targetsCache;
  } catch (_) { return null; }
}
export function _resetForTests() { targetsCache = null; }

async function readInt(bucket, key) {
  try { const o = await bucket.get(key); return o ? (parseInt(await o.text(), 10) || 0) : 0; } catch (_) { return 0; }
}

export async function onRequest(context) {
  const method = context.request.method.toUpperCase();
  if (method === 'GET') return handleGet(context);
  if (method === 'POST') return handlePost(context);
  return json({ ok: false, error: 'method not allowed; use GET or POST' }, 405, { allow: 'GET, POST' });
}

async function listAll(bucket, prefix, maxPages = 20) {
  const out = [];
  let cursor;
  for (let page = 0; page < maxPages; page++) {
    const res = await bucket.list({ prefix, cursor, include: ['customMetadata'] });
    out.push(...res.objects);
    if (!res.truncated) break;
    cursor = res.cursor;
  }
  return out;
}
async function flaggedVoters(bucket) {
  const objs = await listAll(bucket, 'flag/', 5);
  return new Set(objs.map((o) => o.key.slice(5)));
}
function votesFrom(objs) {
  const votes = [];
  for (const o of objs) {
    const k = parseVoteKey(o.key);
    if (!k || !o.customMetadata) continue;
    votes.push({ ...fromMeta(o.customMetadata), ...k });
  }
  return votes;
}

async function handleGet({ request, env, waitUntil }) {
  const url = new URL(request.url);
  const corpus = (url.searchParams.get('corpus') || '').toLowerCase();
  if (corpus && !SLUG_RE.test(corpus)) return json({ ok: false, error: 'invalid corpus slug' }, 400);
  const bucket = env.CORPUS_FEEDBACK;
  if (!bucket) return json({ ok: true, enabled: false });

  const cache = typeof caches !== 'undefined' ? caches.default : null;
  const cacheKey = new Request(`${url.origin}/api/corpus-feedback${corpus ? `?corpus=${corpus}` : ''}`, { method: 'GET' });
  if (cache) { const hit = await cache.match(cacheKey); if (hit) return hit; }

  let body;
  try {
    const flagged = await flaggedVoters(bucket);
    const cutoff = weekCutoff();
    if (corpus) {
      const votes = votesFrom(await listAll(bucket, `v/${corpus}/`));
      body = { ok: true, enabled: true, corpus, ...aggregate(votes, { flagged, cutoff }) };
    } else {
      const byCorpus = new Map();
      for (const v of votesFrom(await listAll(bucket, 'v/'))) {
        if (!byCorpus.has(v.corpus)) byCorpus.set(v.corpus, []);
        byCorpus.get(v.corpus).push(v);
      }
      const scores = {};
      for (const [slug, votes] of byCorpus) {
        const a = aggregate(votes, { flagged, cutoff });
        if (a.reviews) scores[slug] = { score: a.score, reviews: a.reviews, confidence: a.confidence };
      }
      body = { ok: true, enabled: true, prior: { mean: 3, weight: 5 }, scores };
    }
  } catch (e) {
    console.log('[corpus-feedback] list failed:', String(e));
    return json({ ok: false, enabled: true, error: 'storage unavailable' }, 503);
  }
  const resp = new Response(JSON.stringify(body), {
    headers: { 'content-type': 'application/json', 'cache-control': 'public, max-age=300', 'access-control-allow-origin': '*' },
  });
  if (cache) { const p = cache.put(cacheKey, resp.clone()); if (waitUntil) waitUntil(p); else await p; }
  return resp;
}

async function handlePost({ request, env }) {
  const origin = request.headers.get('origin');
  if (origin && !ORIGIN_OK.test(origin)) return json({ ok: false, error: 'cross-origin reviews are not accepted' }, 403);
  const ct = (request.headers.get('content-type') || '').toLowerCase();
  if (!ct.includes('application/json')) return json({ ok: false, error: 'content-type must be application/json' }, 415);
  const declared = parseInt(request.headers.get('content-length') || '0', 10);
  if (Number.isFinite(declared) && declared > MAX_BODY) return json({ ok: false, error: 'body too large' }, 413);
  let raw;
  try { raw = await request.text(); } catch (_) { return json({ ok: false, error: 'could not read body' }, 400); }
  if (raw.length > MAX_BODY) return json({ ok: false, error: 'body too large' }, 413);
  let body;
  try { body = JSON.parse(raw); } catch (_) { return json({ ok: false, error: 'invalid JSON body' }, 400); }
  const v = validateReview(body);
  if (!v.ok) return json({ ok: false, error: v.error }, 400);
  const r = v.review;

  const bucket = env.CORPUS_FEEDBACK;
  if (!bucket) return json({ ok: true, enabled: false, stored: false });

  const t = await corpusTargets(env, request);
  if (!t) return json({ ok: false, enabled: true, error: 'corpus list unavailable' }, 503);
  if (!t.has(r.corpus)) return json({ ok: false, error: 'unknown corpus (not on hub.xerj.org)' }, 400);

  const src = sourceKey(request.headers.get('CF-Connecting-IP'));
  const ipHash = src ? (await sha256hex(src + salt(env))).slice(0, 32) : 'noip';
  const voter = r.install_id
    ? 'i' + (await sha256hex('install:' + r.install_id + salt(env))).slice(0, 31)
    : 's' + ipHash.slice(0, 31);
  const week = isoWeek();
  const voteKey = `v/${r.corpus}/${week}/${voter}`;

  try {
    const prev = await bucket.get(`rl/${ipHash}`);
    if (prev && Date.now() - (parseInt(await prev.text(), 10) || 0) < RATE_WINDOW_MS)
      return json({ ok: false, error: 'rate limited: one review per second per source' }, 429);
  } catch (_) { /* fail open; the daily cap still bounds writes */ }

  let existing = null;
  try { existing = await bucket.head(voteKey); } catch (_) { existing = null; }
  // Never let an automatic signal overwrite what an agent said this week, and
  // never let a pull-only note overwrite any review. Nothing is written.
  if (existing) {
    const prevBasis = (existing.customMetadata || {}).b === 'u' ? 'auto' : 'agent';
    if (r.usefulness === null || (r.basis === 'auto' && prevBasis === 'agent'))
      return json({ ok: true, enabled: true, stored: false, kept: 'earlier review this week', week });
  }

  const volKey = `vol/${week}/${voter}`;
  const ipcKey = `ipc/${week}/${r.corpus}/${ipHash}`;
  let vol = 0; let ipc = 0;
  if (!existing) {
    if (await bucket.head(`flag/${voter}`).catch(() => null))
      return json({ ok: false, error: 'weekly review volume exceeded' }, 429);
    vol = await readInt(bucket, volKey);
    if (vol >= VOLUME_FLAG) {
      try { await bucket.put(`flag/${voter}`, String(Date.now())); } catch (_) { /* best effort */ }
      return json({ ok: false, error: 'weekly review volume exceeded' }, 429);
    }
    ipc = await readInt(bucket, ipcKey);
    if (ipc >= IP_VOTERS_PER_CORPUS)
      return json({ ok: false, error: 'this network already reviewed this corpus this week' }, 429);
  }

  const today = new Date().toISOString().slice(0, 10);
  const cntKey = `meta/count-${today}`;
  const todayN = await readInt(bucket, cntKey);
  if (todayN >= DAILY_CAP) return json({ ok: false, error: 'daily review capacity reached; try again tomorrow' }, 503);

  const now = Date.now();
  const stored = {
    received_at: new Date(now).toISOString(), week, corpus: r.corpus, pack: r.pack,
    agent: { family: r.family, version: r.agent_version }, usefulness: r.usefulness, found: r.found,
    freshness: r.freshness, query_template: r.query_template, coverage_gaps: r.coverage_gaps,
    problems: { kinds: r.problems, note: r.problem_note }, source: r.source, basis: r.basis, has_install_id: !!r.install_id,
  };
  try {
    await bucket.put(`rl/${ipHash}`, String(now));
    await bucket.put(voteKey, JSON.stringify(stored), {
      httpMetadata: { contentType: 'application/json' }, customMetadata: toMeta(r, now),
    });
    await bucket.put(cntKey, String(todayN + 1));
    if (!existing) {
      await bucket.put(volKey, String(vol + 1));
      await bucket.put(ipcKey, String(ipc + 1));
    }
  } catch (e) {
    console.log('[corpus-feedback] R2 write failed:', String(e));
    return json({ ok: false, enabled: true, error: 'storage unavailable' }, 503);
  }
  return json({ ok: true, enabled: true, stored: true, replaced: !!existing, week });
}

