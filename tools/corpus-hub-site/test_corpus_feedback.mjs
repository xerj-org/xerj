// node --test tools/corpus-hub-site/test_corpus_feedback.mjs  (Node 18+; no dependencies)
// /api/corpus-feedback: validation, weekly dedup, Bayesian score, k-threshold,
// outlier trimming. Uses an in-memory stand-in for the R2 binding.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { onRequest, _resetForTests } from '../../functions/api/corpus-feedback.js';
import {
  validateReview, normaliseTemplate, normaliseGap, aggregate, bayes, confidence, isoWeek, toMeta, fromMeta,
} from '../../functions/_lib/corpus-feedback.js';

function r2() {
  const m = new Map();
  const obj = (k) => ({ key: k, text: async () => m.get(k).body, customMetadata: m.get(k).meta });
  return {
    m, writes: 0,
    async get(k) { return m.has(k) ? obj(k) : null; },
    async head(k) { return m.has(k) ? obj(k) : null; },
    async put(k, body, o = {}) { this.writes++; m.set(k, { body: String(body), meta: o.customMetadata || {} }); },
    async list({ prefix }) { return { objects: [...m.keys()].filter((k) => k.startsWith(prefix)).sort().map(obj), truncated: false }; },
  };
}
const ASSETS = { fetch: async () => new Response(JSON.stringify({ contributor: [], corpus: ['aider', 'tokio', 'django'] })) };
const ctx = (method, { body, query = '', ip = '1.2.3.4', origin, env, raw }) => ({
  env,
  request: new Request(`https://hub.xerj.org/api/corpus-feedback${query}`, {
    method,
    headers: { 'content-type': 'application/json', 'CF-Connecting-IP': ip, ...(origin ? { origin } : {}) },
    body: raw !== undefined ? raw : body === undefined ? undefined : JSON.stringify(body),
  }),
});
const call = async (...a) => { const r = await onRequest(ctx(...a)); return [r.status, await r.json()]; };
const good = (o = {}) => ({ corpus: 'aider', pack: 'v3@abc123', agent: { family: 'Claude', version: '4.5' },
  usefulness: 4, found: 'yes', freshness: 4, query_template: 'how to <verb> <term>', ...o });
// Each test gets a fresh rate-limit window by using a fresh IP unless it tests the limit.
let ipN = 10; const nextIp = () => `10.0.${(ipN >> 8) & 255}.${ipN++ & 255}`;
const id = (n) => `install-${String(n).padStart(12, '0')}`;

test('validation: required fields, ranges, caps', () => {
  assert.equal(validateReview(null).ok, false);
  assert.match(validateReview(good({ corpus: '../x' })).error, /corpus/);
  assert.match(validateReview(good({ usefulness: 6 })).error, /usefulness/);
  assert.match(validateReview(good({ usefulness: 3.5 })).error, /usefulness/);
  assert.match(validateReview(good({ found: 'maybe' })).error, /found/);
  assert.match(validateReview(good({ freshness: 0 })).error, /freshness/);
  assert.match(validateReview(good({ coverage_gaps: 'x' })).error, /coverage_gaps/);
  assert.match(validateReview(good({ install_id: 'short' })).error, /install_id/);
  const r = validateReview(good({
    coverage_gaps: ['Async Traits!!', 'async traits', 'x'.repeat(500), 'a', 'b1', 'c2', 'd3', 'e4'],
    problems: { kinds: ['slow', 'SLOW', 'bogus', 'stale'], note: 'n'.repeat(1000) },
    agent: { family: 'Claude Code/Opus', version: 'v'.repeat(100) },
  })).review;
  assert.equal(r.family, 'claude-code-opus');
  assert.equal(r.agent_version.length, 40);
  assert.deepEqual(r.problems, ['slow', 'stale']);
  assert.equal(r.problem_note.length, 280);
  assert.equal(r.coverage_gaps.length, 5);
  assert.equal(r.coverage_gaps[0], 'async traits');           // normalised + deduped
  assert.ok(r.coverage_gaps.every((g) => g.length <= 60));
});

test('query template keeps only the shape, never raw terms', () => {
  assert.equal(normaliseTemplate('How to <verb> {term}'), 'how to <verb> <term>');
  assert.equal(normaliseTemplate('how does tokio::spawn handle JoinError 42'), 'how does <term> <num>');
  assert.equal(normaliseTemplate('find "secret project apollo" in src/lib/x.rs for bob@corp.com'),
    'find <str> in <ref> for <ref>');
  assert.equal(normaliseTemplate('https://internal.corp/x?q=1 error'), '<ref> error');
  assert.equal(normaliseTemplate(42), '');
  assert.ok(normaliseTemplate('word '.repeat(200)).length <= 160);
});

test('gap normalisation', () => {
  assert.equal(normaliseGap('  Async   TRAITS!! '), 'async traits');
  assert.equal(normaliseGap('C++ / C# interop'), 'c++ / c# interop');
});

test('no binding: GET disabled, POST accepted but not stored', async () => {
  _resetForTests();
  assert.deepEqual(await call('GET', { query: '?corpus=aider', env: {} }), [200, { ok: true, enabled: false }]);
  const [s, b] = await call('POST', { body: good(), env: {} });
  assert.equal(s, 200); assert.equal(b.stored, false);
});

test('HTTP guards: 415, 413, 403, 400, unknown corpus', async () => {
  const env = { CORPUS_FEEDBACK: r2(), ASSETS };
  const r = await onRequest({ env, request: new Request('https://hub.xerj.org/api/corpus-feedback', { method: 'POST', headers: { 'content-type': 'text/plain' }, body: '{}' }) });
  assert.equal(r.status, 415);
  assert.equal((await call('POST', { raw: 'x'.repeat(5000), env, ip: nextIp() }))[0], 413);
  assert.equal((await call('POST', { body: good(), origin: 'https://evil.example', env, ip: nextIp() }))[0], 403);
  assert.equal((await call('POST', { raw: '{nope', env, ip: nextIp() }))[0], 400);
  const [s, b] = await call('POST', { body: good({ corpus: 'nope' }), env, ip: nextIp() });
  assert.equal(s, 400); assert.match(b.error, /unknown corpus/);
  assert.equal(env.CORPUS_FEEDBACK.writes, 0);
  assert.equal((await call('PUT', { body: good(), env }))[0], 405);
});

test('weekly dedup: latest vote of the same install id this week replaces the earlier one', async () => {
  const env = { CORPUS_FEEDBACK: r2(), ASSETS };
  let [s, b] = await call('POST', { body: good({ install_id: id(1), usefulness: 2 }), env, ip: nextIp() });
  assert.equal(s, 200); assert.equal(b.replaced, false); assert.equal(b.week, isoWeek());
  [s, b] = await call('POST', { body: good({ install_id: id(1), usefulness: 5 }), env, ip: nextIp() });
  assert.equal(s, 200); assert.equal(b.replaced, true);
  const votes = [...env.CORPUS_FEEDBACK.m.keys()].filter((k) => k.startsWith('v/aider/'));
  assert.equal(votes.length, 1);
  const stored = JSON.parse(env.CORPUS_FEEDBACK.m.get(votes[0]).body);
  assert.equal(stored.usefulness, 5);
  assert.ok(!JSON.stringify(stored).includes(id(1)), 'raw install id is never stored');
  assert.ok(!votes[0].includes(id(1)));
  [s, b] = await call('GET', { query: '?corpus=aider', env });
  assert.equal(b.reviews, 1); assert.equal(b.raw_mean, 5);
});

test('rate limit per source, and one network cannot mint install ids to stuff a corpus', async () => {
  const env = { CORPUS_FEEDBACK: r2(), ASSETS };
  const ip = nextIp();
  assert.equal((await call('POST', { body: good({ install_id: id(1) }), env, ip }))[0], 200);
  assert.equal((await call('POST', { body: good({ install_id: id(2) }), env, ip }))[0], 429);   // same second
  const realNow = Date.now;
  try {
    let t = realNow() + 2000;
    Date.now = () => (t += 2000);
    assert.equal((await call('POST', { body: good({ install_id: id(2) }), env, ip }))[0], 200);
    assert.equal((await call('POST', { body: good({ install_id: id(3) }), env, ip }))[0], 200);
    const [s, b] = await call('POST', { body: good({ install_id: id(4) }), env, ip });
    assert.equal(s, 429); assert.match(b.error, /network/);
    // the same voters may still update their own votes
    assert.equal((await call('POST', { body: good({ install_id: id(3), usefulness: 1 }), env, ip }))[0], 200);
    // and the source may review another corpus
    assert.equal((await call('POST', { body: good({ corpus: 'tokio', install_id: id(4) }), env, ip }))[0], 200);
  } finally { Date.now = realNow; }
});

test('abnormal volume: a voter over the weekly cap is flagged and dropped from aggregates', async () => {
  const bucket = r2(); const env = { CORPUS_FEEDBACK: bucket, ASSETS };
  const w = isoWeek();
  // Seed: an honest voter and a bot that already has an abnormal week.
  await call('POST', { body: good({ install_id: id(7), usefulness: 4 }), env, ip: nextIp() });
  await call('POST', { body: good({ install_id: id(8), usefulness: 1 }), env, ip: nextIp() });
  const botKey = [...bucket.m.keys()].find((k) => k.startsWith('v/aider/') && JSON.parse(bucket.m.get(k).body).usefulness === 1);
  const bot = botKey.split('/').pop();
  bucket.m.set(`vol/${w}/${bot}`, { body: '60', meta: {} });
  const [s] = await call('POST', { body: good({ corpus: 'tokio', install_id: id(8) }), env, ip: nextIp() });
  assert.equal(s, 429);
  assert.ok(bucket.m.has(`flag/${bot}`));
  const [, b] = await call('GET', { query: '?corpus=aider', env });
  assert.equal(b.reviews, 1); assert.equal(b.dropped_outliers, 1); assert.equal(b.raw_mean, 4);
});

test('Bayesian score, confidence and the all-corpora listing', async () => {
  assert.equal(bayes(0, 0), 3);
  assert.equal(Math.round(bayes(10, 2) * 100) / 100, 3.57);     // two 5s
  assert.equal(confidence(0), 'none'); assert.equal(confidence(2), 'very low');
  assert.equal(confidence(5), 'low'); assert.equal(confidence(12), 'medium'); assert.equal(confidence(40), 'high');
  const env = { CORPUS_FEEDBACK: r2(), ASSETS };
  for (let i = 0; i < 5; i++) await call('POST', { body: good({ install_id: id(100 + i), usefulness: 5 }), env, ip: nextIp() });
  await call('POST', { body: good({ corpus: 'tokio', install_id: id(200), usefulness: 2 }), env, ip: nextIp() });
  let [, b] = await call('GET', { query: '?corpus=aider', env });
  assert.equal(b.reviews, 5); assert.equal(b.score, 4); assert.equal(b.confidence, 'low');   // (5*3+25)/10
  [, b] = await call('GET', { query: '?corpus=django', env });
  assert.equal(b.reviews, 0); assert.equal(b.score, null); assert.equal(b.confidence, 'none');
  [, b] = await call('GET', { env });
  assert.deepEqual(Object.keys(b.scores).sort(), ['aider', 'tokio']);
  assert.equal(b.scores.tokio.reviews, 1); assert.equal(b.scores.tokio.score, 2.83);
});

test('aggregate: k>=3 for gaps and family scores, found rate, median freshness, problems', () => {
  const W = isoWeek();
  const mk = (voter, o) => ({ week: W, voter, t: 1, ...fromMeta(toMeta({
    usefulness: 4, found: 'yes', freshness: 3, family: 'claude', coverage_gaps: [], problems: [], ...o }, 1)) });
  const votes = [
    mk('a', { coverage_gaps: ['async traits', 'wasm'], problems: ['slow'], freshness: 5 }),
    mk('b', { coverage_gaps: ['Async Traits'], found: 'partial', freshness: 1 }),
    mk('c', { coverage_gaps: ['async traits', 'wasm'], found: 'no', problems: ['slow', 'stale'] }),
    mk('d', { family: 'gpt', usefulness: 2, freshness: 4 }),
    { ...mk('a', { usefulness: 1 }), week: '2000-W01' },     // older vote of 'a': outside window and superseded
  ];
  const a = aggregate(votes, { cutoff: '2026-W01' });
  assert.equal(a.reviews, 4);
  assert.deepEqual(a.coverage_gaps, [{ gap: 'async traits', reviews: 3 }]);   // 'wasm' has 2 < k
  assert.deepEqual(a.families, [{ family: 'claude', reviews: 3, score: 3.38 }, { family: 'other', reviews: 1, score: null }]);
  assert.deepEqual([a.found.yes, a.found.partial, a.found.no, a.found.rate], [2, 1, 1, 0.63]);
  assert.equal(a.freshness_median, 3.5);
  assert.deepEqual(a.problems, { error: 0, slow: 2, stale: 1, wrong: 0 });
  // latest-per-voter across weeks: a newer week wins
  const b = aggregate([{ ...mk('z', { usefulness: 1 }), week: '2026-W40' }, { ...mk('z', { usefulness: 5 }), week: '2026-W41' }]);
  assert.equal(b.reviews, 1); assert.equal(b.raw_mean, 5);
});

test('isoWeek', () => {
  assert.equal(isoWeek(new Date('2026-10-19T12:00:00Z')), '2026-W43');
  assert.equal(isoWeek(new Date('2021-01-03T12:00:00Z')), '2020-W53');
  assert.equal(isoWeek(new Date('2026-01-01T00:00:00Z')), '2026-W01');
});

test('basis: an automatic review never overwrites the agent review of the same week', async () => {
  const env = { CORPUS_FEEDBACK: r2(), ASSETS };
  let [s, b] = await call('POST', { body: good({ install_id: id(300), usefulness: 2, basis: 'auto', source: 'mcp' }), env, ip: nextIp() });
  assert.equal(b.stored, true);
  [s, b] = await call('POST', { body: good({ install_id: id(300), usefulness: 5 }), env, ip: nextIp() });   // agent replaces auto
  assert.equal(b.replaced, true);
  const w = env.CORPUS_FEEDBACK.writes;
  [s, b] = await call('POST', { body: good({ install_id: id(300), usefulness: 1, basis: 'auto', source: 'mcp' }), env, ip: nextIp() });
  assert.equal(s, 200); assert.equal(b.stored, false);
  assert.equal(env.CORPUS_FEEDBACK.writes, w, 'nothing written');
  [, b] = await call('GET', { query: '?corpus=aider', env });
  assert.equal(b.raw_mean, 5); assert.equal(b.agent_reviews, 1);
});

test('pull-only notes: no usefulness, feed problems and pull_reports, never the score', async () => {
  assert.equal(validateReview({ corpus: 'aider', basis: 'auto', source: 'mcp' }).ok, false);   // only hub-pull may omit
  assert.equal(validateReview({ corpus: 'aider', source: 'hub-pull' }).ok, false);              // and only when auto
  const v = validateReview({ corpus: 'aider', basis: 'auto', source: 'hub-pull', problems: { kinds: ['slow'] } });
  assert.equal(v.ok, true); assert.equal(v.review.usefulness, null);
  const env = { CORPUS_FEEDBACK: r2(), ASSETS };
  const pull = (n, kinds = []) => ({ corpus: 'aider', basis: 'auto', source: 'hub-pull', install_id: id(n), problems: { kinds } });
  let [s, b] = await call('POST', { body: pull(400, ['slow']), env, ip: nextIp() });
  assert.equal(b.stored, true);
  await call('POST', { body: good({ install_id: id(401), usefulness: 4 }), env, ip: nextIp() });
  [s, b] = await call('POST', { body: pull(401, ['error']), env, ip: nextIp() });   // does not replace 401's review
  assert.equal(b.stored, false);
  [s, b] = await call('POST', { body: good({ install_id: id(400), usefulness: 2, basis: 'auto', source: 'mcp' }), env, ip: nextIp() });
  assert.equal(b.replaced, true);                                                   // a scored review replaces a pull note
  [, b] = await call('GET', { query: '?corpus=aider', env });
  assert.equal(b.reviews, 2); assert.equal(b.raw_mean, 3);
  // a pull note in a later week is a separate record and does not hide the scored one
  const W = isoWeek();
  const agg = aggregate([
    { week: W, voter: 'p', t: 2, usefulness: null, found: null, freshness: null, family: 'x', coverage_gaps: [], problems: ['slow'], basis: 'auto' },
    { week: '2026-W40', voter: 'p', t: 1, usefulness: 4, found: 'yes', freshness: 3, family: 'x', coverage_gaps: [], problems: ['slow'], basis: 'agent' },
  ], { cutoff: '2026-W01' });
  assert.equal(agg.reviews, 1); assert.equal(agg.pull_reports, 1); assert.equal(agg.problems.slow, 1);
});
