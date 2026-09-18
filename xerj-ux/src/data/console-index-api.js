// ============================================================
// XERJ Console — what the engine holds, read with the console SESSION
//
// A signed-in operator has a console session cookie, not an engine API key.
// On an engine started without `--insecure` (the default) every direct
// data-plane call from this page — `GET /_mapping`, `GET /_cat/indices` —
// answers 401, so a console that derives its index list and its query fields
// from those shows an empty picker and queries the wrong fields exactly on
// the deployments that are configured correctly.
//
// The console API already exposes both, behind the session, in-process:
//
//   GET  …/connections/built-in/indices            names + doc counts
//   GET  …/connections/built-in/indices/{i}/fields  name + type (+ semantic)
//
// (xerj-console-api/src/data_sources.rs — system indices and the reserved
// brain namespace are hidden there, so nothing here can list or describe
// one.) Pure data: no DOM. `fetch` is injectable for tests.
// ============================================================

const BASE = '/_xerj-console/api/v1/data-sources/connections/built-in/indices';

// Non-dot indices that are engine plumbing, not something a person searches.
export const INTERNAL_INDICES = new Set(['autoindex-catalog']);

function httpError(status) {
  const e = new Error(`HTTP ${status}`);
  e.status = status;
  e.kind = status === 401 ? 'unauthorized' : status === 403 ? 'forbidden' : status === 404 ? 'not-found' : 'http';
  return e;
}

async function getJson(path, signal, doFetch) {
  const r = await (doFetch || fetch)(path, { signal, credentials: 'same-origin', headers: { accept: 'application/json' } });
  if (!r.ok) throw httpError(r.status);
  return r.json();
}

/** `[{ name, docs }]` for every user index, as the engine reports them. */
export async function listUserIndices(signal, doFetch) {
  const body = await getJson(BASE, signal, doFetch);
  const list = (body && body.data && Array.isArray(body.data.indices)) ? body.data.indices : [];
  return list
    .filter((it) => it && typeof it.name === 'string' && it.name && !it.name.startsWith('.'))
    .map((it) => ({ name: it.name, docs: Number(it.docs) || 0 }));
}

/** `{ field: type }` for one index. A `semantic_text` field is reported by
 *  the engine as `type: "text"` + `semantic: true`; it is named
 *  `semantic_text` here so data/schema-roles.js treats both sources alike. */
export async function fieldTypes(index, signal, doFetch) {
  const body = await getJson(`${BASE}/${encodeURIComponent(index)}/fields`, signal, doFetch);
  const list = (body && body.data && Array.isArray(body.data.fields)) ? body.data.fields : [];
  const out = {};
  for (const f of list) {
    if (!f || typeof f.name !== 'string' || !f.name) continue;
    out[f.name] = f.semantic === true ? 'semantic_text' : String(f.type || 'object');
  }
  return out;
}
