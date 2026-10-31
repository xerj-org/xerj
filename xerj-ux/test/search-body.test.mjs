import test from 'node:test';
import assert from 'node:assert/strict';
import { buildQueryClause, queryProblem } from '../src/data/search-body.js';
import { makeReaderApi } from '../src/data/reader-api.js';

test('RANGE requires a value after the complete comparison operator', () => {
  for (const op of ['>=', '<=', '>', '<']) {
    for (const q of [`page${op}`, ` page ${op}   `]) {
      assert.match(queryProblem(q, 'range') || '', /RANGE needs field>=value/, q);
      assert.deepEqual(buildQueryClause(q, 'range', {}), { match_all: {} }, q);
    }
  }
});

test('Reader refuses incomplete ranges before mapping or search requests', async () => {
  const sent = [];
  const api = makeReaderApi({
    guest: false,
    async mapping() { sent.push('mapping'); return {}; },
    async search() { sent.push('search'); return { hits: { total: { value: 0 }, hits: [] } }; },
  });
  for (const q of ['page>=', 'page<=', 'page>', 'page<', 'page >=   ']) {
    const result = await api.search('ax-docs', { q, type: 'range' });
    assert.match(result.hint || '', /RANGE needs field>=value/, q);
    assert.deepEqual(result.hits, [], q);
    assert.equal(result.total, 0, q);
  }
  assert.deepEqual(sent, [], 'invalid filters must not reach the transport');
});

test('RANGE preserves comparison operators, numeric and date values, and empty browsing', () => {
  for (const [q, field, op, value] of [
    ['page>=2', 'page', 'gte', 2],
    ['page <= 3', 'page', 'lte', 3],
    ['page>0', 'page', 'gt', 0],
    ['score<-1.5', 'score', 'lt', -1.5],
    ['email_date>=2026-01-01', 'email_date', 'gte', '2026-01-01'],
    ['email_date <= now-1d/d', 'email_date', 'lte', 'now-1d/d'],
    ['version.name>beta', 'version.name', 'gt', 'beta'],
  ]) {
    assert.equal(queryProblem(q, 'range'), null, q);
    assert.deepEqual(buildQueryClause(q, 'range', {}), { range: { [field]: { [op]: value } } }, q);
  }
  for (const q of ['', '   ']) {
    assert.equal(queryProblem(q, 'range'), null);
    assert.deepEqual(buildQueryClause(q, 'range', {}), { match_all: {} });
  }
});
