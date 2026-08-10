// /get and /get.ps1 — serve the installer script and count the request.
//
// ─────────────────────────── PRIVACY POSTURE ────────────────────────────
// This counts REQUESTS TO OUR WEB SERVER. It is not telemetry: nothing is
// compiled into the xerj binary, nothing runs on the user's machine, and
// nothing is sent back after the install. `curl` executes no JavaScript, so
// there is no beacon, no cookie and no storage on the client side either.
//
// What is recorded, per request:
//   ts       ISO-8601 timestamp
//   country  request.cf.country — Cloudflare's 2-letter country code
//   ua       User-Agent header, truncated to 200 chars
//   kind     installer | browser | bot | unknown   (derived from ua)
//   os/arch  coarse guess derived from ua only     (may be "unknown")
//   script   "get" or "get.ps1"
//   ver      the XERJ_VERSION-ish ?v= query param, if the caller sent one
//
// What is deliberately NOT recorded:
//   - NO IP address. Not cf-connecting-ip, not x-forwarded-for, not a hash
//     of either. functions/lead.js DOES store `ip`; that was a deliberate
//     divergence, not an oversight — a lead is a person who typed their
//     email in, an installer request is not.
//   - NO cookie, NO Set-Cookie, NO localStorage, NO unique or stable
//     identifier of any kind. Two installs by the same person are
//     indistinguishable from two installs by two people, by design.
//   - NO Referer, NO Accept-Language, NO TLS/JA3 fingerprint, no
//     request.cf fields beyond `country` (city, ASN, postal code and
//     latitude/longitude are all available here and all discarded).
//   - NO Ray ID / request id.
// The result is a count with a coarse platform and country breakdown, and
// nothing that can follow a person between two requests. That is the entire
// intended capability. If you extend this file, keep it that way.
//
// ──────────────────────────── FAIL-OPEN RULE ────────────────────────────
// The install must never depend on the counter. The script body is resolved
// and the Response is built FIRST; the R2 write is handed to waitUntil() so
// it runs after the response is already on its way, and every failure path
// (no binding, throw, timeout) is swallowed. A broken counter degrades to
// "we learn nothing", never to "the install breaks".
//
// ───────────────────────────── DEPLOYMENT ───────────────────────────────
// Bucket bound as INSTALLS (see wrangler.toml). Until that bucket exists the
// function serves the script and logs one line to the Pages console —
// working install, no counting.
//
// landing/_routes.json is what guarantees this function actually owns /get
// rather than the static asset shadowing it. If you delete that file, the
// counter may silently stop being invoked while /get keeps working.

const CT = 'text/plain; charset=utf-8';

// Cache-Control on the installer used to be `public, max-age=300` (set in
// landing/_headers). A cached response never reaches this function, so a
// counter behind a 5-minute cache would undercount by exactly the cache hit
// rate — an unknown number. no-store is the price of the count; the body is
// ~8 KB and `curl | sh` fetches it once per install.
const HEADERS = {
  'content-type': CT,
  'cache-control': 'no-store',
  'x-content-type-options': 'nosniff',
};

// Bot/crawler UA substrings (lowercase). Kept deliberately short: this
// classifies, it does not block. Anything unmatched stays "unknown" rather
// than being quietly counted as a human.
const BOT_HINTS = [
  'bot', 'crawler', 'spider', 'slurp', 'facebookexternalhit', 'preview',
  'monitor', 'uptime', 'scanner', 'python-requests', 'go-http-client',
  'okhttp', 'java/', 'libwww-perl', 'headlesschrome', 'axios', 'node-fetch',
];

/** Coarse request class from the UA alone. No other input. */
function classify(ua) {
  const s = ua.toLowerCase();
  if (!s) return 'unknown';
  // Order matters: curl/wget/powershell are checked first because some
  // scanners advertise "curl" *and* "bot"; an installer-shaped fetch is the
  // thing we are counting, so it wins ties only when no bot hint is present.
  const looksBot = BOT_HINTS.some((h) => s.includes(h));
  if (!looksBot && /^(curl|wget|fetch)\//.test(s)) return 'installer';
  if (!looksBot && (s.includes('powershell') || s.includes('windowspowershell'))) return 'installer';
  if (looksBot) return 'bot';
  if (s.startsWith('mozilla/')) return 'browser';
  return 'unknown';
}

/**
 * Coarse OS/arch guess from the UA. Returns "unknown" freely — a wrong guess
 * is worse than no guess, because the platform mix is used to decide which
 * targets get built.
 *
 * Note the honest limit: `curl/8.5.0` (the overwhelmingly common installer
 * UA) carries NO platform at all, so most installer rows are unknown/unknown.
 * The platform breakdown that matters still comes from the per-asset release
 * download counts (metrics/release-downloads.jsonl), not from here.
 */
function platform(ua) {
  const s = ua.toLowerCase();
  let os = 'unknown';
  if (s.includes('windows') || s.includes('win32') || s.includes('win64') || s.includes('mingw')) os = 'windows';
  else if (s.includes('mac os') || s.includes('macos') || s.includes('darwin')) os = 'macos';
  else if (s.includes('android')) os = 'android';
  else if (s.includes('linux')) os = 'linux';
  else if (s.includes('freebsd')) os = 'freebsd';

  let arch = 'unknown';
  if (s.includes('aarch64') || s.includes('arm64')) arch = 'aarch64';
  else if (s.includes('x86_64') || s.includes('x64') || s.includes('amd64') || s.includes('win64')) arch = 'x86_64';
  return { os, arch };
}

/** Which script this request is for. Anything else is not ours to serve. */
function scriptFor(pathname) {
  if (pathname === '/get') return 'get';
  if (pathname === '/get.ps1') return 'get.ps1';
  return null;
}

/**
 * Write one immutable record. One object per request: no read-modify-write,
 * so concurrent installs cannot lose each other's counts. Aggregation happens
 * at read time (scripts/adoption-snapshot.sh, or the ?token= export below).
 * Date-prefixed so a range read never has to list the whole bucket.
 */
async function record(env, req, script) {
  if (!env || !env.INSTALLS) {
    console.log('[get] no INSTALLS binding; not counting', script);
    return;
  }
  const ua = (req.headers.get('user-agent') || '').slice(0, 200);
  const { os, arch } = platform(ua);
  const now = new Date();
  const day = now.toISOString().slice(0, 10);
  let ver = '';
  try { ver = (new URL(req.url).searchParams.get('v') || '').slice(0, 40); } catch (_) {}

  const row = {
    ts: now.toISOString(),
    script,
    kind: classify(ua),
    country: (req.cf && req.cf.country) || '',
    os,
    arch,
    ua,
    ...(ver ? { ver } : {}),
  };
  // crypto.randomUUID() here is a KEY, not an identity: it is generated
  // server-side per request and never returned to the client, so it cannot
  // correlate two requests. It exists only so two simultaneous writes do not
  // overwrite one another.
  await env.INSTALLS.put(
    `installs/${day}/${now.toISOString()}_${crypto.randomUUID()}.json`,
    JSON.stringify(row),
    { httpMetadata: { contentType: 'application/json' } },
  );
}

export async function onRequestGet(context) {
  const { request, env } = context;
  const url = new URL(request.url);
  const script = scriptFor(url.pathname);
  if (!script) return new Response('not found\n', { status: 404, headers: HEADERS });

  // Export path: ?token=… returns the aggregate instead of the script. Guarded
  // exactly like lead.js — needs BOTH a caller token and a configured secret,
  // so an unset secret can never open the export.
  const token = url.searchParams.get('token');
  if (token && env && env.INSTALLS_TOKEN && token === env.INSTALLS_TOKEN) {
    return exportStats(env, url);
  }

  // 1. Resolve the script body. env.ASSETS bypasses Functions and reads the
  //    static file straight out of the deployment, so landing/get stays the
  //    single source of truth — this function never carries a copy that can
  //    drift from the file people audit at https://xerj.org/get.
  let body;
  try {
    const asset = await env.ASSETS.fetch(new URL(`/${script}`, url.origin).toString());
    if (!asset.ok) throw new Error(`asset ${script} -> ${asset.status}`);
    body = await asset.text();
  } catch (e) {
    // Nothing sane to fall back to: serving a stale inlined copy of an
    // installer would be worse than failing loudly, and redirecting to a
    // third-party raw-file host would change what `curl | sh` trusts.
    // The body below is valid in BOTH target shells — `#` starts a comment and
    // `exit 1` is a statement in sh and in PowerShell alike — so whichever of
    // the two pipelines receives it stops with a non-zero status and a message,
    // instead of executing a fragment of an error page.
    console.log('[get] asset fetch failed:', String(e));
    return new Response(
      `# xerj: the installer could not be served right now.\n` +
      `# Install directly from the release page instead:\n` +
      `#   https://github.com/xerj-org/xerj/releases/latest\n` +
      `exit 1\n`,
      { status: 503, headers: HEADERS },
    );
  }

  // 2. Count AFTER the body is in hand and OFF the response path. waitUntil
  //    keeps the worker alive for the write without delaying a single byte to
  //    the installer. Errors are swallowed here, on purpose.
  //    (context.waitUntil, not a destructured copy — never rely on the method
  //    staying bound after destructuring.)
  try {
    const p = record(env, request, script).catch((e) => console.log('[get] count failed:', String(e)));
    if (typeof context.waitUntil === 'function') context.waitUntil(p);
  } catch (e) {
    console.log('[get] count dispatch failed:', String(e));
  }

  return new Response(body, { headers: HEADERS });
}

// HEAD is what link checkers and some proxies send. Serve the right headers
// without a body, and do not count it — it is not an install attempt.
export async function onRequestHead(context) {
  const url = new URL(context.request.url);
  if (!scriptFor(url.pathname)) return new Response(null, { status: 404, headers: HEADERS });
  return new Response(null, { headers: HEADERS });
}

/**
 * Token-guarded read-out. Returns aggregates by default (that is all anyone
 * should need); ?format=raw dumps the rows so they can be re-aggregated.
 * Bounded by ?days= (default 30, max 365) so this can never walk an unbounded
 * bucket.
 */
async function exportStats(env, url) {
  const jsonHeaders = { 'content-type': 'application/json', 'cache-control': 'no-store' };
  if (!env.INSTALLS) {
    return new Response(JSON.stringify({ ok: false, error: 'no INSTALLS binding' }), { status: 503, headers: jsonHeaders });
  }
  const days = Math.min(Math.max(parseInt(url.searchParams.get('days') || '30', 10) || 30, 1), 365);
  const today = new Date();
  const wanted = [];
  for (let i = 0; i < days; i++) {
    const d = new Date(today.getTime() - i * 86400000);
    wanted.push(d.toISOString().slice(0, 10));
  }

  const rows = [];
  for (const day of wanted) {
    let cursor;
    do {
      const page = await env.INSTALLS.list({ prefix: `installs/${day}/`, limit: 1000, cursor });
      for (const obj of page.objects) {
        const o = await env.INSTALLS.get(obj.key);
        if (!o) continue;
        try { rows.push(JSON.parse(await o.text())); } catch (_) {}
      }
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor);
  }

  if (url.searchParams.get('format') === 'raw') {
    return new Response(JSON.stringify({ ok: true, days, count: rows.length, rows }, null, 2), { headers: jsonHeaders });
  }

  const bump = (m, k) => { m[k || 'unknown'] = (m[k || 'unknown'] || 0) + 1; };
  const by_day = {}, by_kind = {}, by_country = {}, by_os = {}, by_script = {};
  for (const r of rows) {
    bump(by_day, String(r.ts || '').slice(0, 10));
    bump(by_kind, r.kind);
    bump(by_country, r.country);
    bump(by_os, r.os);
    bump(by_script, r.script);
  }
  const installerish = rows.filter((r) => r.kind === 'installer').length;
  return new Response(JSON.stringify({
    ok: true,
    window_days: days,
    total_requests: rows.length,
    installer_shaped_requests: installerish,
    caveat:
      'A request to /get is a request, not an install. It does not prove the ' +
      'download succeeded, the checksum matched, or the binary was ever run. ' +
      'kind=installer is a User-Agent heuristic and can be spoofed. Compare ' +
      'against metrics/release-downloads.jsonl before quoting either number.',
    by_day, by_script, by_kind, by_os, by_country,
  }, null, 2), { headers: jsonHeaders });
}
