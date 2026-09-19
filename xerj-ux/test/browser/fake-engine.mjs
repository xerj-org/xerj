// ============================================================
// A fake XERJ node for the browser tests.
//
// Serves the REAL console SPA from xerj-ux/ exactly as the server does
// (`/_xerj-console/…`, same Content-Security-Policy — the string is read out
// of xerj-console-api/src/spa.rs, so the SPA is tested under the policy that
// ships), and answers the data-plane / console-API routes the console calls
// with HOSTILE documents (../fixtures/hostile.mjs).
//
// It records every request, so a test can assert what the console did NOT
// ask for — a guest that never touches `/_xerj-console/api/…` is proven by
// this log, not by reading the code.
// ============================================================

import { createServer } from 'node:http';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { dirname, join, normalize, resolve, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  HOSTILE_HITS, hostileEmail, hostileEgo, hostileCatalogHit, ENGINE_403_BODY, PAYLOADS,
} from '../fixtures/hostile.mjs';
import { miniSearch } from '../fixtures/mini-engine.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const UX_ROOT = resolve(HERE, '..', '..');
const SPA_RS = resolve(UX_ROOT, '..', 'engine', 'crates', 'xerj-console-api', 'src', 'spa.rs');

/** The policy the server really sends, parsed from its source. */
export function shippedCsp() {
  const src = readFileSync(SPA_RS, 'utf8');
  const m = src.match(/pub const CONSOLE_CSP: &str = "([^"]+)";/);
  if (!m) throw new Error('CONSOLE_CSP not found in spa.rs — the browser tests must run under the shipped policy');
  return m[1];
}

const TYPES = { '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8', '.js': 'application/javascript; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon', '.woff2': 'font/woff2' };

export const GUEST_KEY = 'Z3Vlc3Qta2V5LWlkOmd1ZXN0LWtleS1zZWNyZXQtMDEyMzQ1Njc4OQ==';

const MAPPING = {
  'ax-inbox': { mappings: { properties: {
    // `email_subject` is KEYWORD, as autoindex really types it on a mailbox with
    // PDF attachments (PR #945 review: a fake that hardcoded `text` hid that).
    body: { type: 'semantic_text' }, email_subject: { type: 'keyword' }, email_from: { type: 'keyword' }, email_date: { type: 'date' },
    attachment_name: { type: 'keyword' }, email_message_id: { type: 'keyword' }, ax_format: { type: 'keyword' },
    [`field${PAYLOADS.imgOnerror}`]: { type: 'keyword' },
  } } },
};
const FIELDS = Object.entries(MAPPING['ax-inbox'].mappings.properties).map(([name, cfg]) => ({ name, type: cfg.type === 'semantic_text' ? 'text' : cfg.type, semantic: cfg.type === 'semantic_text' }));

/** Is this one of the reader's structural joins (by id, by file, by message
 *  id) rather than the search box? Those are answered by really evaluating the
 *  filter; any text query returns every hostile hit. */
function isJoin(q) {
  if (!q) return false;
  if (q.ids) return true;
  const f = q.bool && Array.isArray(q.bool.filter) ? q.bool.filter : [];
  return f.some((c) => c.term && (c.term.ax_file || c.term.email_message_id));
}

/** Answer a `_search` body the way the engine would, over the hostile hits. */
function searchResponse(body, state) {
  const q = body && body.query;
  // The guest card's email count is a query of its own (reader-api.js
  // EMAIL_MESSAGE_QUERY — not a filter aggregation, #959): evaluate it.
  if (body && body.size === 0 && !body.aggs && q && q.bool && JSON.stringify(q).includes('m*-msg-s0')) return miniSearch(HOSTILE_HITS, body);
  if (isJoin(q)) {
    const resp = miniSearch(HOSTILE_HITS, body);
    // A test can claim the engine holds more attachment records than it returns
    // (one 6000-page PDF), to drive the reader's "list may be incomplete" state.
    const attJoin = JSON.stringify(q).includes('"exists":{"field":"attachment_name"}') && !(q.bool.must_not || []).length;
    if (attJoin && state && state.attachmentRecords) resp.hits.total.value = state.attachmentRecords;
    return resp;
  }
  const wantHl = !!(body && body.highlight);
  const out = HOSTILE_HITS.map((h) => ({ _index: h._index, _id: h._id, _score: h._score, _source: h._source, ...(wantHl && h.highlight ? { highlight: h.highlight } : {}) }));
  const resp = { took: 2, timed_out: false, hits: { total: { value: state && state.searchTotal ? state.searchTotal : out.length, relation: 'eq' }, max_score: 3.2, hits: body && body.size === 0 ? [] : out.slice(0, body && body.size != null ? body.size : 10) } };
  if (body && body.aggs) {
    resp.aggregations = {
      attachments: { doc_count: 2 },
      formats: { buckets: [{ key: `eml${PAYLOADS.imgOnerror}`, doc_count: 3 }, { key: 'pdf', doc_count: 2 }] },
      by__index: { buckets: [{ key: 'ax-inbox', doc_count: out.length }] },
      by_email_from: { buckets: [{ key: hostileEmail._source.email_from, doc_count: 3 }, { key: PAYLOADS.attrBreakout, doc_count: 1 }] },
      by_date: { buckets: [{ key_as_string: '2026-09-01T00:00:00.000Z', doc_count: 3 }] },
    };
  }
  return resp;
}

export async function startFakeEngine() {
  const csp = shippedCsp();
  const state = {
    /** every request: { method, path, query, auth, cookie } */
    log: [],
    /** operator session present? (`/me` → 200) */
    operator: false,
    /** status overrides: { search, mapping, ego, proxySearch, all } → HTTP status */
    fail: {},
    /** An `--insecure` node: the brain list, each brain's meta document and
     *  `ego` answer WITHOUT a key, so the operator's graph path can be driven.
     *  `brains` is the `_cat` order; only `inbox` holds edges for the fixtures. */
    openGraph: false,
    brains: ['inbox'],
    /** claim this many attachment records exist (0 = the truth) */
    attachmentRecords: 0,
    /** claim this many search results exist (0 = the truth) */
    searchTotal: 0,
  };

  const server = createServer(async (req, res) => {
    const url = new URL(req.url, 'http://x');
    const path = decodeURIComponent(url.pathname);
    const auth = req.headers.authorization || null;
    const entry = { method: req.method, path, query: url.search, auth, cookie: !!req.headers.cookie, body: null };
    state.log.push(entry);
    let raw = '';
    for await (const chunk of req) raw += chunk;
    let body = null;
    try { body = raw ? JSON.parse(raw) : null; } catch { body = null; }
    entry.body = body;

    const send = (status, payload, headers = {}) => {
      const text = typeof payload === 'string' ? payload : JSON.stringify(payload);
      res.writeHead(status, { 'content-type': typeof payload === 'string' ? 'text/html; charset=utf-8' : 'application/json', 'x-content-type-options': 'nosniff', ...headers });
      res.end(text);
    };
    const refuse = (status) => send(status, { ...ENGINE_403_BODY, status });

    // ---- the SPA, served as xerj-console-api/src/spa.rs serves it --------
    if (path === '/_xerj-console' ) { res.writeHead(308, { location: '/_xerj-console/' }); res.end(); return; }
    if (path.startsWith('/_xerj-console/') && !path.startsWith('/_xerj-console/api/')) {
      const relPath = path.slice('/_xerj-console/'.length) || 'index.html';
      const file = normalize(join(UX_ROOT, relPath));
      if (!file.startsWith(UX_ROOT) || relPath.startsWith('test/') || !existsSync(file) || !statSync(file).isFile()) { send(404, 'not found'); return; }
      const headers = { 'content-type': TYPES[extname(file)] || 'application/octet-stream', 'x-content-type-options': 'nosniff', 'cache-control': 'no-cache' };
      if (relPath === 'index.html') { headers['content-security-policy'] = csp; headers['referrer-policy'] = 'no-referrer'; }
      res.writeHead(200, headers);
      res.end(readFileSync(file));
      return;
    }
    // A blank same-origin page: lets a test seed sessionStorage before boot.
    if (path === '/favicon.ico') { res.writeHead(404); res.end(); return; }
    if (path === '/__blank') { send(200, '<!doctype html><title>blank</title>'); return; }
    // The CONTROL: the same hostile subject through the unsafe sink, with no
    // policy. If the payloads were duds, or the harness blind, the inertness
    // assertions elsewhere would prove nothing. This page must get pwned.
    if (path === '/__canary') {
      send(200, `<!doctype html><title>canary</title><div id="app"></div><script>
        document.getElementById('app').innerHTML = ${JSON.stringify(hostileEmail._source.email_subject).replace(/</g, '\\u003c')};
      </script>`);
      return;
    }

    if (state.fail.all) { refuse(state.fail.all); return; }

    // ---- console API (operator session) ----------------------------------
    if (path.startsWith('/_xerj-console/api/v1/')) {
      const p = path.slice('/_xerj-console/api/v1'.length);
      if (!state.operator) { send(401, { error: { code: 'unauthorized', message: 'no session' } }); return; }
      if (p === '/me') { send(200, { data: { user: { id: 'u1', email: 'op@example.com', display_name: 'Op', role: 'owner' } } }); return; }
      const DS = '/data-sources/connections/built-in/indices';
      if (p === DS) { send(200, { data: { indices: [{ name: 'ax-inbox', docs: HOSTILE_HITS.length }, { name: 'autoindex-catalog', docs: 1 }], total: 2 } }); return; }
      const m = p.match(/^\/data-sources\/connections\/built-in\/indices\/([^/]+)\/(search|fields)$/);
      if (m && m[2] === 'fields') { send(m[1] === 'ax-inbox' ? 200 : 404, { data: { fields: FIELDS, total: FIELDS.length } }); return; }
      if (m && m[2] === 'search') {
        if (state.fail.proxySearch) { refuse(state.fail.proxySearch); return; }
        if (m[1] === 'autoindex-catalog') { send(200, { took: 1, hits: { total: { value: 1 }, hits: [hostileCatalogHit] } }); return; }
        if (m[1] !== 'ax-inbox') { send(404, { error: { code: 'not_found' } }); return; }
        send(200, searchResponse(body, state));
        return;
      }
      send(404, { error: { code: 'not_found', message: p } });
      return;
    }

    // ---- the graph without a key (an `--insecure` node), when a test asks --
    if (state.openGraph && !auth) {
      if (path === '/_cat/indices/.xerj-memory-*') {
        res.writeHead(200, { 'content-type': 'text/plain; charset=utf-8' });
        res.end(state.brains.map((b) => `green open .xerj-memory-${b}-edges u1 1 0 1 0 1kb 1kb 1kb`).join('\n'));
        return;
      }
      const bm = path.match(/^\/\.xerj-memory-([^/]+)-edges\/_doc\/__xerj-brain-meta$/);
      if (bm) { send(state.brains.includes(bm[1]) ? 200 : 404, { _index: `.xerj-memory-${bm[1]}-edges`, _id: '__xerj-brain-meta', found: true, _source: { nodes_index: 'ax-inbox' } }); return; }
      const gm = path.match(/^\/_graph\/([^/]+)\/ego$/);
      if (gm) { send(200, gm[1] === 'inbox' ? hostileEgo : { node: url.searchParams.get('node'), edges: [], nodes: {}, not_shown: {} }); return; }
    }

    // ---- data plane (API key) --------------------------------------------
    if (auth !== `ApiKey ${GUEST_KEY}`) { refuse(401); return; }
    const dm = path.match(/^\/([^/]+)\/(_search|_count|_mapping)$/);
    if (dm) {
      const [, index, op] = dm;
      const key = op.slice(1);
      if (state.fail[key]) { refuse(state.fail[key]); return; }
      if (index !== 'ax-inbox') { refuse(403); return; }
      if (op === '_mapping') { send(200, MAPPING); return; }
      if (op === '_count') { send(200, { count: HOSTILE_HITS.length }); return; }
      send(200, searchResponse(body, state));
      return;
    }
    if (path === '/_graph/inbox/ego') {
      if (state.fail.ego) { refuse(state.fail.ego); return; }
      send(200, hostileEgo);
      return;
    }
    refuse(403);
  });

  // An OS-assigned port by default; XERJ_TEST_ENGINE_PORT pins it (see cdp.mjs).
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(Number(process.env.XERJ_TEST_ENGINE_PORT) || 0, '127.0.0.1', resolve); });
  const { port } = server.address();
  const origin = `http://127.0.0.1:${port}`;
  return {
    origin, state, csp,
    reset() { state.log.length = 0; state.fail = {}; state.openGraph = false; state.brains = ['inbox']; state.attachmentRecords = 0; state.searchTotal = 0; },
    /** requests that were NOT static SPA assets */
    apiLog() { return state.log.filter((r) => !(r.path.startsWith('/_xerj-console/') && !r.path.startsWith('/_xerj-console/api/')) && !r.path.startsWith('/__') && r.path !== '/favicon.ico'); },
    close: () => new Promise((r) => { server.closeAllConnections?.(); server.close(r); }),
  };
}
