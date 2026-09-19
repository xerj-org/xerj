// ux/reader-render.js + ux/corpus-render.js — hostile documents in, inert
// node trees out.  Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';
import {
  detectShape, recordTitle, renderResultCard, renderResultList, renderRecord, renderGraphPanel,
  groupNeighbors, readerHref, edgeLabel, SHAPES,
} from '../src/ux/reader-render.js';
import { renderCorpus, renderCorpusCard, renderSummaryCard, renderCorpusEmpty, EMPTY_COMMAND } from '../src/ux/corpus-render.js';
import { parseCatalogHits, sampleQueryToSearch } from '../src/data/catalog.js';
import { findAll, textOf } from '../src/ux/safe-dom.js';
import {
  PAYLOADS, HOSTILE_HITS, HOSTILE_ID, hostileEmail, hostileAttachment, hostileAttachment2, hostilePdf, hostileCode,
  hostileGeneric, hostileEgo, hostileCatalogHit,
} from './fixtures/hostile.mjs';
import { assertInert, assertShownAsText } from './fixtures/inert.mjs';

test('every record shape renders inert, and shows the hostile text as text', () => {
  const seen = new Set();
  for (const hit of HOSTILE_HITS) {
    const shape = detectShape(hit._source);
    seen.add(shape);
    const related = { attachments: [hostileAttachment, hostileAttachment2], parent: hostileEmail };
    const rec = renderRecord(hit, { related, brain: 'inbox' });
    assertInert(rec, `record:${shape}`);
    const card = renderResultCard(hit, { selectedId: hit._id, brain: 'inbox' });
    assertInert(card, `card:${shape}`);
    // ids travel in attributes and in the reader link, verbatim / encoded.
    assert.equal(card.attrs['data-reader-open'], String(hit._id));
    const qs = new URLSearchParams(card.attrs.href.slice(card.attrs.href.indexOf('?') + 1));
    assert.equal(qs.get('id'), String(hit._id), 'the id round-trips through the hash route');
    assert.equal(qs.get('index'), hit._index);
  }
  assert.deepEqual([...seen].sort(), ['attachment', 'code-symbol', 'email', 'file', 'generic', 'pdf']);
  for (const s of seen) assert.ok(SHAPES.includes(s));
});

test('an email shows its headers, body and attachments — all as text', () => {
  const rec = renderRecord(hostileEmail, { related: { attachments: [hostileAttachment, hostileAttachment2] } });
  assertShownAsText(rec, PAYLOADS.script, 'subject');
  assertShownAsText(rec, PAYLOADS.imgOnerror, 'subject/body');
  assertShownAsText(rec, PAYLOADS.svgOnload, 'from');
  assertShownAsText(rec, '<html><body onload=', 'an HTML email is shown as its source, never rendered');
  assertShownAsText(rec, 'https://evil.example/pay', 'a URL in a document is displayed');
  // …and is NOT a link: the only anchors are in-app routes to other records.
  const anchors = findAll(rec, (n) => n.tag === 'a');
  assert.equal(anchors.length, 2, 'one link per attachment, nothing else (no file record was passed)');
  for (const a of anchors) assert.match(a.attrs.href, /^#\/reader\?/);
  // hostile attachment FILENAMES are text inside the link
  assertShownAsText(anchors[0], hostileAttachment._source.attachment_name, 'attachment name');
  assertShownAsText(anchors[1], hostileAttachment2._source.attachment_name, 'javascript:-named attachment');
  assert.ok(textOf(rec).includes('ATTACHMENTS · 2'));
});

test('an attachment shows its name, type and parent email as text', () => {
  const rec = renderRecord(hostileAttachment, { related: { parent: hostileEmail } });
  assertShownAsText(rec, hostileAttachment._source.attachment_name);
  assertShownAsText(rec, 'PDF ATTACHMENT · PAGE 2');
  assertShownAsText(rec, 'FROM EMAIL');
  assert.equal(recordTitle(hostileAttachment._source, 'x'), hostileAttachment._source.attachment_name);
  // a non-numeric attachment_bytes never becomes "NaN MB" or markup
  assert.ok(!textOf(rec).includes('NaN'));
  const noText = renderRecord(hostileAttachment2, {});
  assertShownAsText(noText, 'No text was extracted from this attachment');
});

test('pdf, code and generic records keep hostile text literal', () => {
  assertShownAsText(renderRecord(hostilePdf), PAYLOADS.details);
  assertShownAsText(renderRecord(hostileCode), PAYLOADS.script);
  const gen = renderRecord(hostileGeneric);
  assertShownAsText(gen, `key${PAYLOADS.imgOnerror}`.replace(/"/g, '\\"'), 'a hostile KEY is JSON text');
  assertShownAsText(gen, '1 vector/passage field not shown: body_vector');
  assert.ok(!textOf(gen).includes('0.1'), 'vector values are not dumped');
});

test('result list: honest empty state, highlight fragments inert', () => {
  const empty = renderResultList([], { emptyText: 'Search failed: not permitted. Nothing is shown in its place.' });
  assertInert(empty);
  assert.equal(findAll(empty, (n) => n.tag === 'a').length, 0, 'an empty result renders zero cards');
  const list = renderResultList(HOSTILE_HITS, { selectedId: HOSTILE_ID });
  assertInert(list);
  const marks = findAll(list, (n) => n.tag === 'mark');
  assert.ok(marks.length >= 1, 'highlights are marked');
  for (const m of marks) assert.ok(m.children.every((c) => typeof c === 'string'));
  assert.equal(findAll(list, (n) => n.tag === 'a').length, HOSTILE_HITS.length);
});

test('graph panel: hostile node titles, edge types and ids are inert', () => {
  const groups = groupNeighbors(hostileEgo, HOSTILE_ID);
  assert.equal(groups.length, 3);
  const panel = renderGraphPanel({ status: 'ok', groups, notShown: hostileEgo.not_shown, brain: `inbox${PAYLOADS.script}`, index: 'ax-inbox' });
  assertInert(panel, 'graph');
  assertShownAsText(panel, PAYLOADS.script, 'neighbour title');
  assertShownAsText(panel, '<IMG SRC=X ONERROR=', 'an unknown edge type is displayed (upper-cased) as text');
  const links = findAll(panel, (n) => n.tag === 'a');
  assert.equal(links.length, 3);
  for (const a of links) assert.match(a.attrs.href, /^#\/reader\?/);
  // A neighbour whose ID is a javascript: URL is still only a query-string value.
  const js = links.find((a) => a.attrs['data-reader-open'].startsWith('javascript:'));
  assert.ok(js && js.attrs.href.startsWith('#/reader?'));
  assert.equal(edgeLabel('__proto__'), '  proto  ', 'an inherited key is not a label; the raw type is shown');
  assert.equal(edgeLabel('constructor'), 'constructor', 'prototype keys are not labels');
  // every other status is a plain sentence
  for (const status of ['idle', 'loading', 'no-brain', 'no-links', 'denied', 'error']) {
    for (const guest of [true, false]) {
      assertInert(renderGraphPanel({ status, guest, brain: PAYLOADS.imgOnerror, error: PAYLOADS.script }), `graph:${status}`);
    }
  }
  assert.ok(!textOf(renderGraphPanel({ status: 'denied', guest: true, error: 'HTTP 403 secret-detail' })).includes('secret-detail'),
    'a guest is told the graph is not part of the share — not why the engine refused');
  assert.ok(!textOf(renderGraphPanel({ status: 'no-brain', guest: true, brain: 'x' })).includes('xerj brain'),
    'a guest is not told how to build a brain on someone else\'s engine');
});

test('readerHref cannot be broken out of', () => {
  const href = readerHref({ index: 'a&id=evil#/settings', id: '1#/users?x=<script>', brain: '../../_cat' });
  assert.ok(href.startsWith('#/reader?'));
  const qs = new URLSearchParams(href.slice(href.indexOf('?') + 1));
  assert.deepEqual([...qs.keys()], ['index', 'id', 'brain']);
  assert.equal(qs.get('index'), 'a&id=evil#/settings');
  assert.equal(qs.get('id'), '1#/users?x=<script>');
  assert.ok(!href.includes('<'), 'markup characters are percent-encoded');
});

test('corpus cards: a hostile catalog entry renders inert', () => {
  const cards = parseCatalogHits([hostileCatalogHit, { _source: { doc_kind: 'run', index_name: 'nope' } }, { _source: {} }]);
  assert.equal(cards.length, 1, 'only dataset documents with an index name become cards');
  const card = renderCorpusCard(cards[0], {});
  assertInert(card, 'corpus card');
  assertShownAsText(card, `pdf${PAYLOADS.script}`, 'format');
  assertShownAsText(card, `email_from${PAYLOADS.imgOnerror}`, 'field name');
  // sample-query buttons carry DATA, as JSON, in an attribute.
  const btns = findAll(card, (n) => n.tag === 'button');
  assert.equal(btns.length, 3, 'two text samples and one term sample (the fixture gained a sample written for a field of its own)');
  // …including the FIELD a match sample was written for (PR #945 review)
  assert.deepEqual(btns.map((b) => JSON.parse(b.attrs['data-corpus-query']).field), ['body', undefined, 'ax_format']);
  for (const b of btns) {
    const spec = JSON.parse(b.attrs['data-corpus-query']);
    assert.equal(spec.index, 'ax-inbox');
    assert.equal(typeof spec.q, 'string');
    assert.ok(['match', 'term'].includes(spec.type));
  }
  const grid = renderCorpus({ status: 'ok', datasets: cards, summaries: [{ index: `x${PAYLOADS.imgOnerror}`, records: 3, emails: 1, attachments: 2, formats: [{ key: PAYLOADS.script, count: 3 }] }] });
  assertInert(grid, 'corpus grid');
  assertInert(renderSummaryCard({ index: 'ax-inbox', error: PAYLOADS.script, records: 0, emails: 0, attachments: 0, formats: [] }));
});

test('corpus home never shows a sample: empty → one command, error → the error', () => {
  const empty = renderCorpus({ status: 'ok', datasets: [], summaries: [] });
  assert.ok(textOf(empty).includes(EMPTY_COMMAND));
  assert.equal(findAll(empty, (n) => n.tag === 'article').length, 0);
  const guestEmpty = renderCorpusEmpty({ guest: true });
  assert.ok(!textOf(guestEmpty).includes('xerj brain'), 'a guest is not given operator commands');
  const err = renderCorpus({ status: 'error', error: 'HTTP 500' });
  assert.ok(textOf(err).includes('HTTP 500'));
  assert.ok(textOf(err).includes('Nothing is shown rather than a sample'));
  assert.equal(findAll(err, (n) => n.tag === 'article').length, 0);
  assert.ok(textOf(renderCorpus({})).includes('Reading what is indexed'));
});

test('a catalog sample query becomes a search only when it has a text query', () => {
  // the sample's own field rides along (PR #945 review: dropping it ran a sample over `text` against `body` — 0 results)
  assert.deepEqual(sampleQueryToSearch({ body: { query: { match: { body: { query: 'term sheet' } } } } }), { type: 'match', q: 'term sheet', field: 'body' });
  assert.deepEqual(sampleQueryToSearch({ body: { query: { semantic: { field: 'body', query: 'who approved it' } } } }), { type: 'semantic', q: 'who approved it' });
  assert.deepEqual(sampleQueryToSearch({ body: { query: { bool: { filter: [{ term: { email_from: 'a@b' } }] } } } }), { type: 'term', q: 'email_from=a@b' });
  assert.equal(sampleQueryToSearch({ body: { size: 0, aggs: { x: { terms: { field: 'y' } } } } }), null);
  assert.equal(sampleQueryToSearch({ body: null }), null);
  assert.equal(sampleQueryToSearch(null), null);
});
