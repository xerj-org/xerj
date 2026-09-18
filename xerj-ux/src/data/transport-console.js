// ============================================================
// XERJ Console — reader transport for the signed-in operator
//
//   search   the session-authenticated panel proxy (one exact index; the
//            proxy refuses patterns, system indices and the reserved brain
//            namespace — see xerj-console-api/src/data_sources.rs).
//   mapping  same-origin `GET /{index}/_mapping`.
//   ego      same-origin `GET /_graph/{brain}/ego`. The console session is
//            NOT an engine credential: on an engine started without
//            `--insecure` this answers 401, which the reader reports as
//            "graph needs an engine API key" instead of pretending there are
//            no links. (The proxy deliberately cannot read a brain — RC10 B1.)
// ============================================================

import { parseBrainIndices } from './brains-probe.js';

const PROXY = '/_xerj-console/api/v1/data-sources/connections/built-in/indices';
const BRAIN_META_ID = '__xerj-brain-meta';
const enc = encodeURIComponent;

function httpError(status) {
  const e = new Error(`HTTP ${status}`);
  e.status = status;
  e.kind = status === 401 ? 'unauthorized' : status === 403 ? 'forbidden' : status === 404 ? 'not-found' : 'http';
  return e;
}

export function makeConsoleTransport() {
  return {
    guest: false,
    async search(index, body, signal) {
      const r = await fetch(`${PROXY}/${enc(index)}/search`, {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'content-type': 'application/json', accept: 'application/json' },
        body: JSON.stringify(body),
        signal,
      });
      if (!r.ok) throw httpError(r.status);
      return r.json(); // the proxy answers in ES `_search` wire shape, unwrapped
    },
    async mapping(index, signal) {
      const r = await fetch(`/${enc(index)}/_mapping`, { signal, credentials: 'same-origin', headers: { accept: 'application/json' } });
      if (!r.ok) throw httpError(r.status);
      return r.json();
    },
    async ego(brain, params, signal) {
      const qs = new URLSearchParams(params);
      const r = await fetch(`/_graph/${enc(brain)}/ego?${qs}`, { signal, credentials: 'same-origin', headers: { accept: 'application/json' } });
      let body = null;
      try { body = await r.json(); } catch { body = null; }
      return { status: r.status, body };
    },
    /** A brain whose meta doc lists `index` in `nodes_index` (what
     *  `xerj brain` writes), or null. Best-effort; never throws. */
    async discoverBrain(index, signal) {
      try {
        const r = await fetch('/_cat/indices/.xerj-memory-*', { signal, credentials: 'same-origin', headers: { accept: 'text/plain, application/json' } });
        if (!r.ok) return null;
        for (const b of parseBrainIndices(await r.text()).slice(0, 32)) {
          const m = await fetch(`/${enc(`.xerj-memory-${b}-edges`)}/_doc/${enc(BRAIN_META_ID)}`, { signal, credentials: 'same-origin', headers: { accept: 'application/json' } });
          if (!m.ok) continue;
          const doc = await m.json();
          const ni = String((doc && doc._source && doc._source.nodes_index) || '');
          if (ni.split(',').map((x) => x.trim()).includes(index)) return b;
        }
      } catch (e) {
        if (e && e.name === 'AbortError') throw e;
      }
      return null;
    },
  };
}
