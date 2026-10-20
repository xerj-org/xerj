// Counts views of corpus pages (/corpus/<slug> and /corpus/<slug>.html) for the
// Traffic panel, then serves the static page unchanged. The count never delays
// or breaks the page: it runs after the response, through waitUntil, and any
// failure is swallowed. What is stored: functions/_lib/hub-traffic.js.
import { recordView } from '../_lib/hub-traffic.js';
import { knownCorpora } from '../_lib/hub-hosted.js';

const PAGE_RE = /^\/corpus\/([a-z0-9][a-z0-9._-]{0,99}?)(?:\.html)?\/?$/;

export async function onRequest(context) {
  const res = await context.next();
  try {
    const { request, env, waitUntil } = context;
    const m = request.method === 'GET' && res.status === 200 ? PAGE_RE.exec(new URL(request.url).pathname) : null;
    if (m && env && env.HUB_USAGE) {
      const slug = m[1].toLowerCase();
      const p = knownCorpora(env, request).then((known) => (known && known.has(slug) ? recordView(env, request, slug) : 'unknown'));
      if (waitUntil) waitUntil(p); else await p;
    }
  } catch (_) { /* counting is best-effort */ }
  return res;
}
