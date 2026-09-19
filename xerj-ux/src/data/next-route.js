// ============================================================
// XERJ Console — "where was I going?" across the sign-in page
//
// A deep link (`/_xerj-console/#/reader?index=…&id=…` — the link llms.txt
// tells an agent to hand a person) opened without a session is redirected to
// `/login`, which used to drop the hash and land on the Corpus home after
// sign-in: the person had to click the link a second time (PR #945 review).
// boot.js stashes the route here before it redirects; login.html takes it
// back out once the passkey ceremony succeeds.
//
// Only an in-console hash route is ever kept or returned — `#/` followed by
// route characters, no scheme, no whitespace — and it is only ever appended to
// `/_xerj-console/`, so it cannot become a redirect anywhere else. It lives in
// sessionStorage: this tab, this visit. Pure: storage is passed in.
// ============================================================

export const NEXT_KEY = 'xerj.next';
const ROUTE_RE = /^#\/[A-Za-z0-9/?&=_%.+~*!'()-]*$/;

/** `hash` when it is an in-console route worth coming back to, else ''. */
export function safeRoute(hash) {
  const h = typeof hash === 'string' ? hash : '';
  if (h.length < 3 || h.length > 2048 || h.includes('//') || !ROUTE_RE.test(h)) return '';
  return h;
}

/** Remember `hash` for after sign-in. A blocked storage is not an error. */
export function stashNext(storage, hash) {
  const route = safeRoute(hash);
  try {
    if (route) storage.setItem(NEXT_KEY, route);
    else storage.removeItem(NEXT_KEY);
  } catch { /* storage blocked: the person lands on the home page, as before */ }
  return route;
}

/** The remembered route ('' when none), forgotten as it is read. */
export function takeNext(storage) {
  let raw = '';
  try { raw = storage.getItem(NEXT_KEY) || ''; storage.removeItem(NEXT_KEY); } catch { return ''; }
  return safeRoute(raw);
}
