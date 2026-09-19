// ============================================================
// XERJ Console — reader transport for a shared-link guest
//
// Every call goes through data/guest.js#guestRequest, which builds the URL
// from the share's own validated index / brain names and attaches the key.
// There is deliberately no way to pass a path, another index or another
// brain through this object: `index` arguments that are not part of the
// share are refused before any request is made ('blocked').
// ============================================================

import { guestRequest, GuestError } from './guest.js';

export function makeGuestTransport(share, env) {
  return {
    guest: true,
    brain: share.brain,
    indices: share.indices,
    search: (index, body, signal) => guestRequest(share, 'search', { indices: [index], body, signal }, env),
    mapping: (index, signal) => guestRequest(share, 'mapping', { indices: [index], signal }, env),
    async ego(brain, params, signal) {
      if (brain !== share.brain) throw new GuestError('blocked', 0);
      try {
        return { status: 200, body: await guestRequest(share, 'ego', { query: params, signal }, env) };
      } catch (e) {
        // 403/404 are answers about THIS panel, not the end of the session.
        if (e instanceof GuestError && (e.kind === 'forbidden' || e.kind === 'not-found')) return { status: e.status, body: null };
        throw e;
      }
    },
  };
}
