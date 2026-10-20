// /auth/logout — clear the hub_session cookie and go back (GET link or POST).
// Sessions are stateless signed cookies, so sign-out is clearing the cookie;
// rotating HUB_SESSION_SECRET signs every session out at once.
import { SESSION_COOKIE, clearCookie, safeReturn } from '../_lib/session.js';

export function onRequest({ request }) {
  const ret = safeReturn(new URL(request.url).searchParams.get('return') || '/');
  const h = new Headers({ location: ret, 'cache-control': 'no-store' });
  h.append('set-cookie', clearCookie(SESSION_COOKIE));
  return new Response(null, { status: 302, headers: h });
}
