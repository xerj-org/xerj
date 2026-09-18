// data/query.js — the views that show a person's OWN DOCUMENTS never fall
// back to the in-memory mock, whatever fails.  Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';

const calls = [];
let respond = () => { throw new TypeError('Failed to fetch'); };
globalThis.fetch = async (url, init) => { calls.push(String(url)); return respond(String(url), init); };
const json = (status, body) => ({ status, ok: status >= 200 && status < 300, json: async () => body, text: async () => JSON.stringify(body) });

const { query, NEVER_MOCK } = await import('../src/data/query.js');
const { resetSchemaCache } = await import('../src/data/schema.js');
const { previewRequest } = await import('../src/dashboards/search-discover.js');
const { buildSearchBody, buildQueryClause, QUERY_TYPES, requestPath } = await import('../src/data/search-body.js');
const { deriveRoles } = await import('../src/data/schema-roles.js');

const LOOKS_FABRICATED = ['hits', 'datasets', 'summaries'];

test('engine unreachable: an error and zero rows — never a sample', async () => {
  respond = () => { throw new TypeError('Failed to fetch'); };
  for (const dashId of ['corpus', 'search-discover']) {
    resetSchemaCache();
    const r = await query({ dashId, search: { q: 'invoice', type: 'match', index: '*' } });
    assert.ok(NEVER_MOCK.has(dashId));
    assert.equal(r.meta.sourceKind, 'live-error', dashId);
    assert.ok(r.data.error, `${dashId} must carry the error`);
    for (const k of LOOKS_FABRICATED) assert.ok(!r.data[k] || r.data[k].length === 0, `${dashId}.${k} must be empty on failure`);
    assert.ok(!r.data.metrics && !r.data.series, `${dashId} must not receive the telemetry mock`);
  }
});

test('"could not ask" and "nothing indexed" read differently', async () => {
  resetSchemaCache();
  respond = () => json(401, {});
  let r = await query({ dashId: 'search-discover', search: { q: '', index: '*' } });
  assert.match(r.data.error, /could not read the index list \(HTTP 401\)/);
  resetSchemaCache();
  respond = (url) => (url.endsWith('/indices') ? json(200, { data: { indices: [{ name: 'autoindex-catalog', docs: 3 }, { name: '.xerj_users', docs: 1 }] } }) : json(404, {}));
  r = await query({ dashId: 'search-discover', search: { q: '', index: '*' } });
  assert.match(r.data.error, /nothing is indexed on this engine yet/);
  assert.deepEqual(r.data.hits, []);
});

test('the reader loads nothing through query() and claims nothing', async () => {
  const n = calls.length;
  const r = await query({ dashId: 'reader' });
  assert.equal(calls.length, n, 'query() makes no request for a self-fetching view');
  assert.equal(r.meta.sourceKind, 'pending');
  assert.ok(!/LIVE/.test(r.meta.sourceLabel));
  assert.ok(!r.data.hits && !r.data.metrics);
});

test('a telemetry dashboard may still use sample data — and is labelled so', async () => {
  const r = await query({ dashId: 'system' });
  assert.equal(r.meta.sourceKind, 'sample');
  assert.match(r.meta.sourceLabel, /SAMPLE DATA/);
});

test('Discover: `*` over two indices runs on one and SAYS which', async () => {
  resetSchemaCache();
  let sent = null;
  respond = (url, init) => {
    if (url.endsWith('/indices')) return json(200, { data: { indices: [{ name: 'ax-inbox', docs: 6 }, { name: 'ax-contracts', docs: 2 }, { name: 'autoindex-catalog', docs: 2 }] } });
    if (url.endsWith('/ax-inbox/fields')) return json(200, { data: { fields: [{ name: 'body', type: 'text', semantic: true }, { name: 'email_from', type: 'keyword', semantic: false }, { name: 'email_date', type: 'date', semantic: false }, { name: 'body_vector', type: 'vector', semantic: false }] } });
    if (url.endsWith('/ax-inbox/search')) { sent = JSON.parse(init.body); return json(200, { took: 3, hits: { total: { value: 1 }, max_score: 1.5, hits: [{ _index: 'ax-inbox', _id: 'm1', _score: 1.5, _source: { body: 'x', email_date: '2026-09-01T00:00:00Z' } }] }, aggregations: { by__index: { buckets: [{ key: 'ax-inbox', doc_count: 1 }] }, by_email_from: { buckets: [{ key: 'a@b', doc_count: 1 }] }, by_date: { buckets: [{ key_as_string: '2026-09-01T00:00:00.000Z', doc_count: 1 }] } } }); }
    return json(404, {});
  };
  const search = { q: 'term sheet', type: 'hybrid', index: '*', filters: { email_from: 'a@b' }, sort: { field: '_score', dir: 'desc' } };
  const r = await query({ dashId: 'search-discover', search, filters: search.filters });
  assert.equal(r.data.resolvedIndex, 'ax-inbox');
  assert.equal(r.data.narrowed, true, 'two user indices → the page must say * was narrowed');
  assert.equal(r.data.roles.semanticField, 'body');
  assert.equal(r.data.roles.dateField, 'email_date');
  assert.deepEqual(r.data.facets.email_from, [{ label: 'a@b', value: 'a@b', count: 1 }]);
  assert.deepEqual(r.data.histogram, [{ label: '2026-09-01', value: 1 }]);
  assert.equal(r.data.hits[0]._ts, '2026-09-01T00:00:00Z');

  // #923 finding 8: what the REQUEST panel shows IS what was sent.
  assert.deepEqual(r.data.request, sent);
  const shown = previewRequest(search, { request: r.data.request, resolvedIndex: r.data.resolvedIndex, roles: r.data.roles });
  assert.deepEqual(shown.body, sent);
  assert.equal(shown.path, '/ax-inbox/_search');
  // …and before any answer, the preview is built by the same function.
  const before = previewRequest(search, { roles: r.data.roles });
  assert.deepEqual(before.body, buildSearchBody(search.q, search.type, search.filters, r.data.roles, { sort: search.sort }));
  assert.deepEqual(before.body.query.bool.must, sent.query.bool.must);
  assert.equal(requestPath(null), '/*/_search');
});

test('the query builder: real fields, an engine-accepted hybrid shape, no raw kNN', () => {
  assert.ok(!QUERY_TYPES.includes('knn'), 'the console cannot produce a query vector; it must not offer one');
  const roles = deriveRoles({ body: 'semantic_text', email_subject: 'text', email_from: 'keyword', ax_path: 'keyword', email_message_id: 'keyword', email_date: 'date' });
  assert.equal(roles.textField, 'body');
  assert.equal(roles.semanticField, 'body');
  assert.deepEqual(roles.keywordFields, ['email_from'], 'plumbing / id fields are not facets');
  assert.ok(roles.isEmail);
  const hybrid = buildQueryClause('earnout', 'hybrid', roles).hybrid;
  assert.deepEqual(hybrid.queries.map((e) => Object.keys(e).sort()), [['query', 'weight'], ['query', 'weight']]);
  assert.deepEqual(hybrid.fusion, { type: 'rrf', k: 60 });
  assert.deepEqual(buildQueryClause('  ', 'semantic', roles), { match_all: {} });
  assert.deepEqual(buildQueryClause('email_from = a@b ', 'term', roles), { term: { email_from: 'a@b' } });
  assert.deepEqual(buildQueryClause('page>=3', 'range', roles), { range: { page: { gte: 3 } } });
  assert.deepEqual(buildQueryClause('email_date>2026-01-01', 'range', roles), { range: { email_date: { gt: '2026-01-01' } } });
  // log-shaped engine: nothing is hardcoded to autoindex's names
  const logs = deriveRoles({ message: 'text', level: 'keyword', '@timestamp': 'date' });
  assert.deepEqual(buildQueryClause('timeout', 'match', logs), { match: { message: 'timeout' } });
  assert.equal(logs.dateField, '@timestamp');
  const body = buildSearchBody('x', 'match', {}, logs, { aggs: false, size: 5 });
  assert.deepEqual(Object.keys(body).sort(), ['query', 'size', 'track_total_hits']);
  assert.equal(buildSearchBody('x', 'match', {}, logs, { sort: { field: '_score' } }).sort, undefined);
  assert.deepEqual(buildSearchBody('x', 'match', {}, logs, { sort: { field: 'level', dir: 'asc' } }).sort, [{ level: 'asc' }]);
});
