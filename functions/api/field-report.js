// /api/field-report — HTTP FALLBACK for filing an agent field report when the
// canonical GitHub PR path is unavailable.
//
// ──────────────────────────── WHAT THIS IS ──────────────────────────────
// The CANONICAL way to file a field report is a GitHub pull request:
//     xerj feedback --open-pr      →  user-feedback/16-agent-field-reports/
// That path STAYS primary. This endpoint exists ONLY for the agent persona
// that cannot reach it: no shell, no git, no `gh`, no GitHub identity (e.g. a
// sandboxed Claude Code). Proposed in docs/research/llms-txt-2026-09/ as the
// "GitHub-unavailable fallback". Every success response points back at GitHub.
//
// ─────────────────────────────── CONTRACT ───────────────────────────────
//   POST https://xerj.org/api/field-report   (Content-Type: application/json)
//   Body (short strings): { agent, used_for, verdict,   // required
//                           version?, os?, indexed?, notes? }
//   200 { ok:true, stored:true, note:"…GitHub PR is canonical…" }
//   Not POST → 405 · not JSON → 415 · >16 KB → 413 · bad/missing field → 400
//   Same IP within 60s → 429 · daily safety cap reached → 503
//
// ─────────────────────── STORAGE: R2, free-tier-safe ────────────────────
// Stored in Cloudflare R2 (binding FIELD_REPORTS → bucket xerj-field-reports),
// the cheapest storage already granted to the Pages deploy token (the same
// account's Functions use R2 for LEADS/INSTALLS). R2 free tier = 10 GB,
// 1M Class-A ops/mo (writes/list), 10M Class-B ops/mo (reads). Two guards keep
// us far inside it and stop spam from causing a bill:
//   1. RATE LIMIT — ≤1 accepted report per IP per minute (object rl/<ipHash>,
//      body = timestamp; a second request inside 60s → 429, a READ only).
//   2. DAILY CAP — accepted-report counter meta/count-<YYYY-MM-DD>; at
//      DAILY_CAP we DROP new reports with 503 and point to the GitHub PR path.
// An accepted report costs 3 Class-A writes (rate marker + report + counter);
// DAILY_CAP×3 stays far under 1M/mo. Rejected requests do NO writes (at most
// two cheap reads), so a flood cannot burn the write budget or the wallet.
// R2 has no TTL; rl/ and meta/ objects are tiny — a bucket lifecycle rule
// (expire rl/ after 1 day) keeps them swept. Reports under reports/ are kept.
//
// ─────────────────────────────── PRIVACY ────────────────────────────────
// Raw IP is NEVER stored. ip_hash = sha-256(IP + salt). Untrusted input is
// length-capped, control-char stripped, never echoed back as HTML.

const DAILY_CAP = 300;          // ×3 writes = 900/day, far under 1M/mo R2 free
const RATE_WINDOW_MS = 60_000;  // one accepted report per IP per minute
const MAX_BODY = 16 * 1024;     // 16 KB request-body cap

const FIELD_CAPS = { agent:120, used_for:1000, verdict:1000, version:80, os:120, indexed:200, notes:4000 };
const REQUIRED = ['agent','used_for','verdict'];
const OPTIONAL = ['version','os','indexed','notes'];

const CANONICAL_NOTE =
  'Thanks. The canonical path is a GitHub PR: ' +
  'https://github.com/xerj-org/xerj/tree/main/user-feedback/16-agent-field-reports';

const CORS = {
  'access-control-allow-origin': '*',
  'access-control-allow-methods': 'POST, OPTIONS',
  'access-control-allow-headers': 'content-type',
  'access-control-max-age': '86400',
};
const JSON_HEADERS = { 'content-type':'application/json', 'cache-control':'no-store', ...CORS };
const json = (obj, status = 200) => new Response(JSON.stringify(obj), { status, headers: JSON_HEADERS });

function clean(value, cap) {
  return String(value).replace(/[\u0000-\u001f\u007f]/g, ' ').trim().slice(0, cap);
}
async function sha256hex(input) {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

export async function onRequest(context) {
  const method = context.request.method.toUpperCase();
  if (method === 'OPTIONS') return new Response(null, { status: 204, headers: CORS });
  if (method !== 'POST') return json({ ok:false, error:'method not allowed; use POST' }, 405);
  return handlePost(context);
}

async function handlePost({ request, env }) {
  const ct = (request.headers.get('content-type') || '').toLowerCase();
  if (!ct.includes('application/json')) return json({ ok:false, error:'content-type must be application/json' }, 415);

  const declared = parseInt(request.headers.get('content-length') || '0', 10);
  if (Number.isFinite(declared) && declared > MAX_BODY) return json({ ok:false, error:'body too large' }, 413);

  let bytes;
  try { bytes = await request.arrayBuffer(); } catch (_) { return json({ ok:false, error:'could not read body' }, 400); }
  if (bytes.byteLength > MAX_BODY) return json({ ok:false, error:'body too large' }, 413);

  let body;
  try { body = JSON.parse(new TextDecoder().decode(bytes)); } catch (_) { return json({ ok:false, error:'invalid JSON body' }, 400); }
  if (body === null || typeof body !== 'object' || Array.isArray(body)) return json({ ok:false, error:'body must be a JSON object' }, 400);

  const report = {};
  for (const key of REQUIRED) {
    const v = body[key];
    if (typeof v !== 'string' || clean(v, FIELD_CAPS[key]).length === 0)
      return json({ ok:false, error:`missing or invalid required field: ${key}` }, 400);
    report[key] = clean(v, FIELD_CAPS[key]);
  }
  for (const key of OPTIONAL) {
    if (body[key] === undefined || body[key] === null) { report[key] = ''; continue; }
    if (typeof body[key] !== 'string') return json({ ok:false, error:`field must be a string: ${key}` }, 400);
    report[key] = clean(body[key], FIELD_CAPS[key]);
  }

  // Degrade like functions/lead.js when its bucket is absent: accept, do not
  // store, still return the canonical pointer. (State before the binding wires.)
  const bucket = env.FIELD_REPORTS;
  if (!bucket) { console.log('[field-report] no FIELD_REPORTS R2 binding; not stored'); return json({ ok:true, stored:false, note: CANONICAL_NOTE }); }

  const ip = request.headers.get('CF-Connecting-IP') || '';
  const ipHash = ip ? await sha256hex(ip + (env.FIELD_REPORT_SALT || 'xerj-field-report-v1')) : 'noip';

  // Guard 1: per-IP rate limit (read-only unless we accept below).
  const rlKey = `rl/${ipHash}`;
  try {
    const prev = await bucket.get(rlKey);
    if (prev) {
      const ts = parseInt(await prev.text(), 10) || 0;
      if (Date.now() - ts < RATE_WINDOW_MS) return json({ ok:false, error:'rate limited: one report per minute per source' }, 429);
    }
  } catch (_) { /* fail open; the daily cap still bounds writes */ }

  // Guard 2: daily safety cap (anti-overage).
  const today = new Date().toISOString().slice(0, 10);
  const cntKey = `meta/count-${today}`;
  let count = 0;
  try { const c = await bucket.get(cntKey); if (c) count = parseInt(await c.text(), 10) || 0; } catch (_) { count = 0; }
  if (count >= DAILY_CAP)
    return json({ ok:false, error:'daily capacity reached; please file via the GitHub PR path', note: CANONICAL_NOTE }, 503);

  // Accept: 3 R2 writes.
  const now = new Date();
  const rand = (crypto.randomUUID ? crypto.randomUUID() : `${Math.random()}`).slice(0, 8);
  const key = `reports/${today}/${now.getTime()}-${rand}.json`;
  const stored = {
    received_at: now.toISOString(),
    ip_hash: ip ? ipHash : '',
    agent: report.agent, used_for: report.used_for, verdict: report.verdict,
    version: report.version, os: report.os, indexed: report.indexed, notes: report.notes,
    ua: clean(request.headers.get('user-agent') || '', 300),
  };
  try {
    await bucket.put(rlKey, String(Date.now()));
    await bucket.put(key, JSON.stringify(stored), { httpMetadata: { contentType: 'application/json' } });
    await bucket.put(cntKey, String(count + 1));
  } catch (e) {
    console.log('[field-report] R2 write failed:', String(e));
    return json({ ok:false, error:'storage unavailable; please file via the GitHub PR path', note: CANONICAL_NOTE }, 503);
  }
  return json({ ok:true, stored:true, note: CANONICAL_NOTE });
}
