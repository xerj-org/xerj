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
const { buildSearchBody, buildQueryClause, QUERY_TYPES, requestPath, queryWords, queryProblem, queryPlaceholder } = await import('../src/data/search-body.js');
const { deriveRoles, withSearchField, MAX_TEXT_SEARCH_FIELDS } = await import('../src/data/schema-roles.js');
const { sampleQueryToSearch } = await import('../src/data/catalog.js');
const { searchFieldsOf } = await import('../src/data/search-body.js');

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
  const search = { q: 'term sheet', type: 'match', index: '*', filters: { email_from: 'a@b' }, sort: { field: '_score', dir: 'desc' } };
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
  // HYBRID carries no aggregations: a live node answers 400 "aggregations are
  // not supported with hybrid/fusion queries" otherwise, so every Discover
  // hybrid search used to fail.
  assert.equal(buildSearchBody('earnout', 'hybrid', {}, roles, {}).aggs, undefined);
  assert.ok(buildSearchBody('earnout', 'semantic', {}, roles, {}).aggs.by_email_from);
  // …and a facet filter goes into EACH leg: `bool{must: hybrid, filter}` returns
  // 0 hits on a live node and `post_filter` is ignored, both without an error.
  const hf = buildSearchBody('earnout', 'hybrid', { ax_format: 'eml' }, roles, {});
  assert.ok(hf.query.hybrid && !hf.query.bool && !hf.post_filter);
  for (const leg of hf.query.hybrid.queries) {
    assert.deepEqual(leg.query.bool.filter, [{ term: { ax_format: 'eml' } }]);
    // the lexical leg is the multi-field match (below); the other is semantic
    assert.ok((leg.query.bool.must.bool && leg.query.bool.must.bool.should.every((c) => c.match)) || leg.query.bool.must.semantic);
    assert.equal(typeof leg.weight, 'number');
  }
  assert.deepEqual(hf.query.hybrid.fusion, { type: 'rrf', k: 60 });
  assert.ok(buildSearchBody('', 'hybrid', {}, roles, {}).aggs, 'an empty hybrid box is match_all and keeps its facets');
  // log-shaped engine: nothing is hardcoded to autoindex's names
  const logs = deriveRoles({ message: 'text', level: 'keyword', '@timestamp': 'date' });
  assert.deepEqual(buildQueryClause('timeout', 'match', logs), { match: { message: 'timeout' } });
  assert.equal(logs.dateField, '@timestamp');
  const body = buildSearchBody('x', 'match', {}, logs, { aggs: false, size: 5 });
  assert.deepEqual(Object.keys(body).sort(), ['query', 'size', 'track_total_hits']);
  assert.equal(buildSearchBody('x', 'match', {}, logs, { sort: { field: '_score' } }).sort, undefined);
  assert.deepEqual(buildSearchBody('x', 'match', {}, logs, { sort: { field: 'level', dir: 'asc' } }).sort, [{ level: 'asc' }]);
});

test('MATCH / PHRASE / PREFIX search every text field plus the subject, title and attachment-name fields the index has — one OR-ed clause per field', () => {
  // PR #945 review: "Lunch" returned 0 results although an email's SUBJECT was
  // "Lunch on Friday?" — only `body` was searched. Now every title-like field
  // the mapping has is searched (and highlighted, reader-api.js).
  const roles = deriveRoles({ body: 'semantic_text', email_subject: 'text', title: 'text', attachment_name: 'text', email_from: 'keyword' });
  assert.deepEqual(roles.searchFields, ['body', 'email_subject', 'title', 'attachment_name']);
  assert.deepEqual(roles.keywordSearchFields, []);
  assert.deepEqual(searchFieldsOf(roles), roles.searchFields);
  const m = buildQueryClause('Lunch', 'match', roles);
  // The subject clause matches the MESSAGE's records: every attachment record
  // carries a copy of its parent's subject, and one hit per PDF page is noise.
  const subj = (c) => ({ bool: { must: [c], must_not: [{ exists: { field: 'attachment_name' } }] } });
  assert.deepEqual(m, { bool: { should: [
    { match: { body: 'Lunch' } }, subj({ match: { email_subject: 'Lunch' } }), { match: { title: 'Lunch' } }, { match: { attachment_name: 'Lunch' } },
  ], minimum_should_match: 1 } });
  assert.deepEqual(buildQueryClause('term sheet', 'phrase', roles).bool.should.map((c) => Object.keys(c.bool ? c.bool.must[0] : c)[0]), ['match_phrase', 'match_phrase', 'match_phrase', 'match_phrase']);
  assert.deepEqual(buildQueryClause('lun', 'prefix', roles).bool.should[1], subj({ prefix: { email_subject: 'lun' } }));
  // an index with no attachments has nothing to keep out of the subject clause
  assert.deepEqual(buildQueryClause('x', 'match', deriveRoles({ body: 'text', email_subject: 'text' })).bool.should[1], { match: { email_subject: 'x' } });
  // the lexical leg of HYBRID is the same clause
  assert.deepEqual(buildQueryClause('Lunch', 'hybrid', roles).hybrid.queries[0].query, m);
  // a filter wraps it once: bool{must: <should-clause>, filter}
  const body = buildSearchBody('Lunch', 'match', { email_from: 'sam@acme.example' }, roles, { aggs: false });
  assert.deepEqual(body.query.bool.must, m);
  // `bool.should` and not `multi_match`: measured on a live node, multi_match
  // (best_fields) returns no highlight fragments; per-field clauses do.
  assert.ok(!JSON.stringify(m).includes('multi_match'));
  // one field → the plain clause, byte for byte as before (Discover's REQUEST panel)
  const one = deriveRoles({ body: 'text', level: 'keyword' });
  assert.deepEqual(one.searchFields, ['body']);
  assert.deepEqual(buildQueryClause('x', 'match', one), { match: { body: 'x' } });
  assert.deepEqual(buildQueryClause('x', 'prefix', one), { prefix: { body: 'x' } });
  assert.deepEqual(buildQueryClause('x y', 'phrase', one), { match_phrase: { body: 'x y' } });
  // no mapping (roles from `{}`): the text field only — nothing is guessed
  assert.deepEqual(buildQueryClause('x', 'match', deriveRoles({})), { match: { body: 'x' } });
  // an empty box is match_all whatever the fields
  assert.deepEqual(buildQueryClause('  ', 'match', roles), { match_all: {} });
  // paging: `from` only when there is an offset
  assert.equal(buildSearchBody('x', 'match', {}, one, { aggs: false }).from, undefined);
  assert.equal(buildSearchBody('x', 'match', {}, one, { aggs: false, from: 25 }).from, 25);
});

test('MAJOR (PR #945 review): a KEYWORD-typed subject / title / file name is searched by the words in it — `match` on a keyword needs the whole string', () => {
  // The mapping autoindex really wrote for the review's mailbox (live node,
  // 2026-09-19): every per-page attachment record copies its parent's subject,
  // the cardinality ratio collapses, and the inferrer picks `keyword`.
  //   {"match":{"email_subject":"Lunch"}}            → 0   ("Lunch on Friday?" exists)
  //   {"wildcard":{"email_subject":{"value":"*lunch*","case_insensitive":true}}} → 1
  // The first version of this fix was tested only against a fake engine that
  // hardcoded `email_subject: text`.
  const roles = deriveRoles({ body: 'text', email_subject: 'keyword', title: 'keyword', attachment_name: 'keyword', email_from: 'keyword', page: 'long' });
  assert.deepEqual(roles.searchFields, ['body', 'email_subject', 'title', 'attachment_name']);
  assert.deepEqual(roles.keywordSearchFields, ['email_subject', 'title', 'attachment_name']);
  const has = (f, value) => ({ wildcard: { [f]: { value, case_insensitive: true } } });
  const subj = (c) => ({ bool: { must: [c], must_not: [{ exists: { field: 'attachment_name' } }] } });
  assert.deepEqual(buildQueryClause('Lunch', 'match', roles), { bool: { should: [
    { match: { body: 'Lunch' } }, subj(has('email_subject', '*Lunch*')), has('title', '*Lunch*'), has('attachment_name', '*Lunch*'),
  ], minimum_should_match: 1 } });
  // several words: OR, like `match` — a record matching more of them scores higher
  assert.deepEqual(buildQueryClause('Lunch on Friday?', 'match', roles).bool.should[2],
    { bool: { should: [has('title', '*Lunch*'), has('title', '*on*'), has('title', '*Friday*')], minimum_should_match: 1 } });
  assert.deepEqual(queryWords('résumé – 设计 v2.pdf'), ['résumé', '设计', 'v2', 'pdf'], 'words are runs of letters/digits in any script');
  // PHRASE is "contains this, as typed"; PREFIX is "starts with this" — both case-insensitive
  assert.deepEqual(buildQueryClause('Lunch on', 'phrase', roles).bool.should[2], has('title', '*Lunch on*'));
  assert.deepEqual(buildQueryClause('Lun', 'prefix', roles).bool.should[3], has('attachment_name', 'Lun*'));
  // nothing a person types can widen the pattern: * ? \ become "any one character"
  assert.deepEqual(buildQueryClause('a*b?c\\d', 'phrase', roles).bool.should[2], has('title', '*a?b?c?d*'));
  assert.deepEqual(buildQueryClause('***', 'match', roles).bool.should[2], has('title', '*???*'));
  // a title-like field of a type there is no clause for is not searched
  assert.deepEqual(deriveRoles({ body: 'text', title: 'long' }).searchFields, ['body']);
});

test('MAJOR (PR #945 review): EVERY text-typed field is searched — a record whose text lives in `text` is findable, and a sample query runs over its own field', () => {
  // Live node, 2026-09-19: autoindex's prose extractors write `body`, its line
  // extractor (Makefile, .ini) writes `text`; both land in ax-docs. The catalog's
  // own sample `{"match":{"text":"makefiletargetword"}}` returned 1 hit while the
  // Reader — which searched `body` only — returned 0 for the same word.
  const roles = deriveRoles({ body: 'text', text: 'semantic_text', notes: 'text', email_subject: 'keyword', ax_path: 'keyword' });
  assert.equal(roles.textField, 'body');
  assert.deepEqual(roles.searchFields, ['body', 'text', 'notes', 'email_subject'], 'preferred names first, then mapping order, then the title-like extras');
  assert.deepEqual(buildQueryClause('makefiletargetword', 'match', roles).bool.should.slice(0, 3), [
    { match: { body: 'makefiletargetword' } }, { match: { text: 'makefiletargetword' } }, { match: { notes: 'makefiletargetword' } },
  ]);
  // the cap bounds the request on a mapping with dozens of text fields, and says so
  const many = Object.fromEntries(Array.from({ length: MAX_TEXT_SEARCH_FIELDS + 5 }, (_, i) => [`t${i}`, 'text']));
  const capped = deriveRoles(many);
  assert.equal(capped.searchFields.length, MAX_TEXT_SEARCH_FIELDS);
  assert.equal(capped.searchFieldsCapped, true);
  assert.equal(roles.searchFieldsCapped, false);
  // a sample query's OWN field is always searched, whatever its rank or type
  assert.deepEqual(withSearchField(capped, `t${MAX_TEXT_SEARCH_FIELDS + 2}`).searchFields.at(-1), `t${MAX_TEXT_SEARCH_FIELDS + 2}`);
  const kw = withSearchField(roles, 'ax_path');
  assert.deepEqual(kw.searchFields.at(-1), 'ax_path');
  assert.ok(kw.keywordSearchFields.includes('ax_path'), 'a keyword sample field gets the contains clause, not `match`');
  assert.equal(withSearchField(roles, 'body'), roles, 'already searched: unchanged');
  assert.equal(withSearchField(roles, 'no_such_field'), roles, 'a field the mapping does not have changes nothing');
  assert.equal(withSearchField(roles, 'page'), roles);
  // and the catalog sample carries its field through (catalog.js)
  assert.deepEqual(sampleQueryToSearch({ body: { query: { match: { text: 'makefiletargetword' } }, size: 3 } }), { type: 'match', q: 'makefiletargetword', field: 'text' });
  assert.deepEqual(sampleQueryToSearch({ body: { query: { match_phrase: { body: { query: 'term sheet' } } } } }), { type: 'phrase', q: 'term sheet', field: 'body' });
  assert.deepEqual(sampleQueryToSearch({ body: { query: { hybrid: { queries: [{ query: { match: { text: 'x' } } }] } } } }), { type: 'hybrid', q: 'x', field: 'text' });
  assert.deepEqual(sampleQueryToSearch({ body: { query: { term: { title: 'A title' } } } }), { type: 'term', q: 'title=A title' });
});

test('minor (PR #945 review): TERM / RANGE with free text is refused with the syntax — it used to run match_all and show every record as a result', () => {
  assert.match(queryProblem('zebrafish', 'term'), /TERM needs field=value/);
  assert.match(queryProblem('hello', 'range'), /RANGE needs field>=value/);
  for (const [q, type] of [['ax_format=eml', 'term'], ['page>=2', 'range'], ['', 'term'], ['   ', 'range'], ['zebrafish', 'match'], ['x', 'semantic']]) {
    assert.equal(queryProblem(q, type), null, `${type} "${q}" runs`);
  }
  assert.match(queryPlaceholder('term'), /field=value/);
  assert.match(queryPlaceholder('range'), /field>=value/);
  assert.match(queryPlaceholder('match'), /search this corpus/);
});
