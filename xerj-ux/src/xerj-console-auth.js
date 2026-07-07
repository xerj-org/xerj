// Xerj Console auth API client.
//
// Tiny fetch wrapper for the /_xerj-console/api/v1/auth/* surface. Used by
// setup.html, login.html, and the SPA's auth guard. Never imports
// from the rest of the app so it stays loadable on the standalone
// auth pages without dragging the whole bundle in.

const API = '/_xerj-console/api/v1';

export async function api(method, path, body) {
  const init = {
    method,
    credentials: 'same-origin',
    headers: { 'accept': 'application/json' },
  };
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  const r = await fetch(API + path, init);
  let payload = null;
  try { payload = await r.json(); } catch {}
  if (!r.ok) {
    const reason = payload && payload.error
      ? `${payload.error.type}: ${payload.error.reason}`
      : `HTTP ${r.status}`;
    throw new Error(reason);
  }
  return payload && payload.data !== undefined ? payload.data : payload;
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
