// Xerj Console auth API client.
//
// Tiny fetch wrapper for the /_xerj-console/api/v1/auth/* surface. Used by
// setup.html, login.html, and the SPA's auth guard. Never imports
// from the rest of the app so it stays loadable on the standalone
// auth pages without dragging the whole bundle in.

const API = '/_xerj-console/api/v1';

/**
 * Low-level fetch wrapper that returns the FULL parsed envelope plus the
 * concurrency etag. Callers that need `meta.etag` (dashboards CRUD →
 * optimistic If-Match) use this; the thin `api()` below is unchanged for
 * every existing call site.
 *
 *   method, path       — as api()
 *   body               — JSON-serialised when defined; a 204 (empty) body
 *                        yields `data: null`
 *   extraHeaders       — merged over the defaults (e.g. `If-Match`)
 *
 * Returns `{ data, meta, etag, status }`. `etag` prefers the `ETag`
 * response header and falls back to `meta.etag` in the body. On a non-2xx
 * the thrown Error carries `.status` and `.code` so callers can branch on
 * 409 (stale etag) / 404 / 403 without string-matching.
 */
export async function apiRaw(method, path, body, extraHeaders) {
  const init = {
    method,
    credentials: 'same-origin',
    headers: { 'accept': 'application/json', ...(extraHeaders || {}) },
  };
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  const r = await fetch(API + path, init);
  let payload = null;
  try { payload = await r.json(); } catch {}
  if (!r.ok) {
    const err = payload && payload.error;
    // The engine emits `{type,reason}`; the shared contract documents
    // `{code,message}`. Accept either so this client survives both.
    const code = err ? (err.type || err.code) : undefined;
    const reason = err
      ? (err.reason || err.message || err.type || err.code || `HTTP ${r.status}`)
      : `HTTP ${r.status}`;
    const e = new Error(code ? `${code}: ${reason}` : reason);
    e.status = r.status;
    e.code = code;
    e.payload = payload;
    throw e;
  }
  const bodyEtag = payload && payload.meta && payload.meta.etag;
  const etag = r.headers.get('ETag') || bodyEtag || null;
  return {
    data: payload && payload.data !== undefined ? payload.data : payload,
    meta: (payload && payload.meta) || null,
    etag,
    status: r.status,
  };
}

export async function api(method, path, body, extraHeaders) {
  const { data } = await apiRaw(method, path, body, extraHeaders);
  return data;
}

// ─── Magic-link redemption ───────────────────────────────────────────────────

export async function redeemMagic(token) {
  return api('POST', '/auth/magic/redeem', { token });
}

// ─── Passkey enrolment ──────────────────────────────────────────────────────

export async function beginEnrol(enrollment_session_id, email, display_name) {
  return api('POST', '/auth/passkey/begin', {
    enrollment_session_id, email, display_name,
  });
}

export async function finishEnrol(
  enrollment_session_id, challenge_id, credential, name, email, display_name
) {
  return api('POST', '/auth/passkey/finish', {
    enrollment_session_id, challenge_id, credential, name, email, display_name,
  });
}

// ─── Login ──────────────────────────────────────────────────────────────────

export async function beginLogin(email) {
  return api('POST', '/auth/login/begin', { email });
}

export async function finishLogin(challenge_id, credential) {
  return api('POST', '/auth/login/finish', { challenge_id, credential });
}

export async function logout() {
  return api('POST', '/auth/logout');
}

export async function me() {
  return api('GET', '/me');
}

// ─── Base64url ↔ ArrayBuffer ────────────────────────────────────────────────
// WebAuthn's JSON wire format encodes binary fields as base64url-no-pad,
// but the browser's `navigator.credentials.create/get` API expects
// ArrayBuffer for `challenge`, `user.id`, credential `id`, etc. These
// helpers convert in both directions.

export function b64uToBuf(s) {
  const pad = '='.repeat((4 - (s.length % 4)) % 4);
  const b64 = (s + pad).replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(b64);
  const buf = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
  return buf.buffer;
}

export function bufToB64u(buf) {
  const bytes = new Uint8Array(buf);
  let bin = '';
  for (let i = 0; i < bytes.byteLength; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}
