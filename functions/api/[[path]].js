// Unknown /api/* paths answer a JSON 404 (issue #1895).
//
// Every named route under functions/api/ is more specific than this catch-all,
// so it only sees paths no Function claims. Those may still be static files
// (the signed catalogue, /api/v1/catalogue.json and .sig, is one), so ask the
// asset server first and pass its answer through. Only when the asset server has
// nothing (a 404, or the site's HTML page served in place of a missing file) does
// this answer, and then with a status and a body a client can parse: an agent or
// `xerj ask` reading /api/v1/catalogue.json must never take the homepage for data.
export async function onRequest(context) {
  const res = await context.next();
  const type = (res.headers.get('content-type') || '').toLowerCase();
  if (res.status !== 404 && !(res.status === 200 && type.startsWith('text/html'))) return res;
  const path = new URL(context.request.url).pathname;
  return new Response(JSON.stringify({ ok: false, error: 'not_found', path }) + '\n', {
    status: 404,
    headers: { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' },
  });
}
