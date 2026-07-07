// /lead — capture a work email to R2 (bucket bound as LEADS), and export
// captured leads behind a secret token.
//   POST /lead              {"email": "..."}          -> store one lead
//   GET  /lead?token=SECRET&format=json|csv           -> list all leads
//   GET  /lead                                        -> harmless hint
// The client POST is fire-and-forget, so it must never block the redirect.

const JSON_HEADERS = { 'content-type': 'application/json', 'cache-control': 'no-store' };

export async function onRequestPost({ request, env }) {
  try {
    const body = await request.json().catch(() => ({}));
    const email = String(body.email || '').trim().toLowerCase();
    if (!email || !email.includes('@') || email.length > 254) {
      return new Response(JSON.stringify({ ok: false, error: 'invalid email' }), { status: 400, headers: JSON_HEADERS });
    }
    if (!env.LEADS) {
      console.log('[lead] no R2 binding; email=', email);
      return new Response(JSON.stringify({ ok: true, stored: false }), { headers: JSON_HEADERS });
    }
    const now = new Date().toISOString();
    const record = {
      email,
      ts: now,
      source: String(body.source || 'landing'),
      ua: request.headers.get('user-agent') || '',
      referer: request.headers.get('referer') || '',
      ip: request.headers.get('cf-connecting-ip') || '',
      country: (request.cf && request.cf.country) || '',
    };
    const key = `leads/${now}_${crypto.randomUUID()}.json`;
    await env.LEADS.put(key, JSON.stringify(record), { httpMetadata: { contentType: 'application/json' } });
    return new Response(JSON.stringify({ ok: true, stored: true }), { headers: JSON_HEADERS });
  } catch (e) {
    return new Response(JSON.stringify({ ok: false, error: String(e) }), { status: 500, headers: JSON_HEADERS });
  }
}

export async function onRequestGet({ request, env }) {
  const url = new URL(request.url);
  const token = url.searchParams.get('token');
  // Export requires the secret token AND a configured LEADS_TOKEN.
  if (token && env.LEADS_TOKEN && token === env.LEADS_TOKEN && env.LEADS) {
    const leads = [];
    let cursor;
    do {
      const page = await env.LEADS.list({ prefix: 'leads/', limit: 1000, cursor });
      for (const obj of page.objects) {
        const o = await env.LEADS.get(obj.key);
        if (o) { try { const r = JSON.parse(await o.text()); r._key = obj.key; leads.push(r); } catch (_) {} }
      }
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor);
    leads.sort((a, b) => (a.ts < b.ts ? 1 : -1));
    if (url.searchParams.get('format') === 'csv') {
      const esc = (v) => '"' + String(v == null ? '' : v).replace(/"/g, '""') + '"';
      const rows = [['ts', 'email', 'source', 'country', 'referer', 'ip', 'ua'].join(',')];
      for (const l of leads) rows.push([l.ts, l.email, l.source, l.country, l.referer, l.ip, l.ua].map(esc).join(','));
      return new Response(rows.join('\n'), { headers: { 'content-type': 'text/csv; charset=utf-8', 'cache-control': 'no-store' } });
    }
    return new Response(JSON.stringify({ ok: true, count: leads.length, leads }, null, 2), { headers: JSON_HEADERS });
  }
  return new Response(JSON.stringify({ ok: true, hint: 'POST {"email":"you@company.com"} to capture a lead' }), { headers: JSON_HEADERS });
}
