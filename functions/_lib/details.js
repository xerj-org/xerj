// Lazy access to the signed-in details bundled from functions/_lib/hubdata.js
// (generated at deploy time by tools/corpus-hub-site/private_data.py; a null
// stub in git). Parsed once per isolate, on the first request that needs it.
import RAW from './hubdata.js';

let cache;
export function details(raw = RAW) {
  if (raw !== RAW) return raw ? JSON.parse(raw) : null;      // tests
  if (cache === undefined) {
    try { cache = RAW ? JSON.parse(RAW) : null; } catch (_) { cache = null; }
  }
  return cache;
}

/** A GitHub session whose login is a hub contributor → that contributor's key. */
export function contributorFor(user, data = details()) {
  if (!user || user.provider !== 'github' || !data) return null;
  const k = String(user.login || '').toLowerCase();
  return data.contributors && data.contributors[k] ? k : null;
}
