// ============================================================
// XERJ Console — boot
//
// Decides WHICH console this tab is, before anything else loads:
//
//   guest     sessionStorage['xerj.share'] holds a share record (written by
//             the share guest page). Boot the read-only guest shell and
//             NOTHING else — in particular, none of the console-API calls
//             below are made: a guest has no console session, and must not
//             probe for one.
//   operator  otherwise: the auth guard (GET /_xerj-console/api/v1/me → login
//             on 401), prefs + dashboards hydration, then app.js.
//
// A record that is present but invalid or expired does NOT fall through to
// the operator path (which would bounce a confused guest to a passkey login):
// the guest shell clears it and says the share is over — and leaves a marker
// (no secret in it) so a RELOAD says the same thing. The ended screen offers
// the operator sign-in, which removes the marker.
//
// Lives in a file, not inline in index.html, so the page can be served with
// `script-src 'self'`.
// ============================================================

import { SHARE_KEY, ENDED_KEY } from './data/guest.js';

function hasShareRecord() {
  // A share that has ENDED in this tab (expired, revoked, LEAVE) leaves a
  // non-secret marker behind: a guest who reloads then sees "this share has
  // ended" again, not the operator's passkey login (PR #945 review).
  try { return window.sessionStorage.getItem(SHARE_KEY) != null || window.sessionStorage.getItem(ENDED_KEY) != null; } catch { return false; }
}

async function bootOperator() {
  try {
    const r = await fetch('/_xerj-console/api/v1/me', {
      credentials: 'same-origin',
      headers: { accept: 'application/json' },
    });
    if (r.status === 401) {
      // No reliable "is bootstrapped?" probe yet; default to /login since
      // /setup requires a magic-link token in the fragment. Operators on
      // first boot reach /setup via the stderr banner.
      // Remember the deep link (`#/reader?index=…&id=…`) so sign-in returns
      // to it instead of the home page (data/next-route.js — loaded here, on
      // the operator path only; a guest tab statically imports guest.js alone).
      try {
        const { stashNext } = await import('./data/next-route.js');
        stashNext(window.sessionStorage, window.location.hash);
      } catch { /* the person lands on the home page after sign-in, as before */ }
      window.location.href = '/_xerj-console/login';
      return;
    }
    if (!r.ok) throw new Error('HTTP ' + r.status);
    const body = await r.json();
    window.__xerjConsoleMe = body && body.data ? body.data.user : null;

    // Pull /prefs + /views from the engine and seed localStorage BEFORE
    // app.js boots — app.js reads localStorage at module-load time.
    try {
      const sync = await import('./xerj-console-sync.js');
      await sync.pullAll();
      Promise.resolve().then(() => sync.startPush());
    } catch (e) {
      console.warn('[xerj-console sync]', e);
    }

    // Pull the durable dashboard set before app.js's first paint.
    try {
      const ds = await import('./data/dashboard-store.js');
      await ds.hydrate();
    } catch (e) {
      console.warn('[xerj-console dashboards]', e);
    }
  } catch (e) {
    // Engine unreachable (offline dev, hot-reload mid-flight) — load the SPA
    // anyway so the user sees an error state instead of a redirect loop.
    console.error('[xerj-console auth guard]', e);
  }
  await import('./app.js');
}

if (hasShareRecord()) {
  const { bootGuest } = await import('./guest-app.js');
  bootGuest(window);
} else {
  await bootOperator();
}
