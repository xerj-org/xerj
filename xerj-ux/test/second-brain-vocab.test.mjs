// Issue #1100 — the Second Brain is the graph layer over WHATEVER was
// indexed. Its "notes / believed" persona is only truthful for note-like
// corpora (prose, mail); on a source tree (95.8% `code` records) the same
// copy tells a user they are looking at notes and beliefs. These tests pin
// the corpus-aware vocabulary: the classifier, the panel bodies, and the
// dashboard eyebrows must speak the neutral "records / active" frame for a
// code-dominant corpus, and keep the persona for a note-dominant one.
//
// Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';

// second-brain-api.js reads location.hash at import time on some paths; the
// rest of its browser surface is guarded (document, storage, rAF).
globalThis.location = { hash: '' };

const {
  isNoteLikeCorpus, dominantFormat, vocabFor, sbVocab,
} = await import('../src/ux/sb-vocab.js');
const { renderPanelBody } = await import('../src/ux/ego-ledger.js');
const { secondBrain } = await import('../src/dashboards/second-brain.js');

// A realistic code tree — the autoindexed `engine/crates` from the issue:
// overwhelmingly `code`, a little json, a couple of prose files. Many more
// records than a single page so the plurality is the corpus's, not a page's.
const CODE_FORMATS = [
  { format: 'code', count: 9580 },
  { format: 'json', count: 320 },
  { format: 'txt-prose', count: 100 },
];
// A notes / mail corpus: prose and mbox are the strict majority.
const NOTE_FORMATS = [
  { format: 'txt-prose', count: 1400 },
  { format: 'mbox', count: 300 },
  { format: 'code', count: 60 },
];

// ---- the classifier -------------------------------------------------

test('isNoteLikeCorpus: a code-dominant corpus is NOT note-like', () => {
  assert.equal(isNoteLikeCorpus(CODE_FORMATS), false);
});

test('isNoteLikeCorpus: a prose/mail-dominant corpus IS note-like', () => {
  assert.equal(isNoteLikeCorpus(NOTE_FORMATS), true);
});

test('isNoteLikeCorpus: unknown/empty tally defaults to NOT note-like (neutral is the safe frame)', () => {
  assert.equal(isNoteLikeCorpus([]), false);
  assert.equal(isNoteLikeCorpus(null), false);
  assert.equal(isNoteLikeCorpus(undefined), false);
});

test('isNoteLikeCorpus: note formats must be a STRICT majority (a plurality is not enough)', () => {
  // 40% prose, 60% code → neutral.
  assert.equal(isNoteLikeCorpus([{ format: 'txt-prose', count: 40 }, { format: 'code', count: 60 }]), false);
  // otherFormats (records past the agg bucket cap) count as non-note.
  assert.equal(isNoteLikeCorpus([{ format: 'txt-prose', count: 60 }], 60), false);
});

test('dominantFormat picks the largest bucket regardless of order', () => {
  assert.equal(dominantFormat(CODE_FORMATS), 'code');
  assert.equal(dominantFormat([{ format: 'a', count: 1 }, { format: 'b', count: 9 }]), 'b');
  assert.equal(dominantFormat([]), null);
});

test('sbVocab wires the classifier to the vocabulary', () => {
  assert.equal(sbVocab(CODE_FORMATS).noteLike, false);
  assert.equal(sbVocab(CODE_FORMATS).items, 'records');
  assert.equal(sbVocab(NOTE_FORMATS).noteLike, true);
  assert.equal(sbVocab(NOTE_FORMATS).items, 'notes');
});

// ---- panel bodies ---------------------------------------------------

const dataFor = (noteLike) => ({
  brain: 'repo',
  overview: { exists: true, edges: { total: 5, live: 3, invalidated: 2 }, types: [], detectors: [], hubs: { in: [], out: [] }, not_shown: {} },
  ego: null,
  metrics: { edgesLive: { formatted: '3', hint: '' } },
  series: { created: [], createdT: [] },
  sb: {
    connected: true, error: null, uiError: null, overviewError: null,
    brains: ['repo'], focus: null, asOf: null, trail: [],
    timeline: { edges: [{ edge_id: 'e1', valid_at: Date.now() - 86_400_000, weight: 1 }] },
    names: {}, openEvidence: new Set(), collapsed: new Set(),
    reads: [], readsTotal: 0, mapStats: null, nodeStats: null,
    noteLike,
  },
});

test('the BELIEVED-NOW tile reads ACTIVE on a code corpus, BELIEVED on a note corpus', () => {
  const neutral = renderPanelBody('edgesLive', dataFor(false));
  assert.match(neutral, /ACTIVE/);
  assert.doesNotMatch(neutral, /BELIEVED/);

  const persona = renderPanelBody('edgesLive', dataFor(true));
  assert.match(persona, /BELIEVED/);
});

test('the belief-time scrubber reads LINK TIME / ACTIVE on a code corpus', () => {
  const neutral = renderPanelBody('scrub', dataFor(false));
  assert.match(neutral, /LINK TIME/);
  assert.match(neutral, /ACTIVE AT THIS MOMENT/);
  assert.doesNotMatch(neutral, /BELIEF TIME/);
  assert.doesNotMatch(neutral, /BELIEVED AT THIS MOMENT/);

  const persona = renderPanelBody('scrub', dataFor(true));
  assert.match(persona, /BELIEF TIME/);
  assert.match(persona, /BELIEVED AT THIS MOMENT/);
});

test('the empty-ledger prompt says RECORD, not NOTE, on a code corpus', () => {
  // ego panel with edges but no ego read → the "nothing in focus" prompt.
  const d = dataFor(false);
  d.overview.edges.total = 5; // hasEdges true → EgoLedger path
  const html = renderPanelBody('ego', d);
  assert.match(html, /NO RECORD IN FOCUS/);
  assert.doesNotMatch(html, /NO NOTE IN FOCUS/);
});

// ---- dashboard eyebrows --------------------------------------------

const eyebrowOf = (panels, id) => (panels.find((p) => p.id === id) || {}).eyebrow;

test('dashboard eyebrows are neutral for a code corpus', () => {
  const view = secondBrain.render({ data: dataFor(false), time: 'now' });
  assert.equal(eyebrowOf(view.panels, 'edgesLive'), 'ACTIVE AT THIS MOMENT');
  assert.equal(eyebrowOf(view.panels, 'typeDist'), 'HOW RECORDS CONNECT');
  assert.equal(eyebrowOf(view.panels, 'ego'), 'THE LEDGER · ONE RECORD, EVERYTHING IT LINKS TO');
  assert.match(eyebrowOf(view.panels, 'scrub'), /^LINK TIME ·/);
});

test('dashboard eyebrows keep the persona for a note corpus', () => {
  const view = secondBrain.render({ data: dataFor(true), time: 'now' });
  assert.equal(eyebrowOf(view.panels, 'edgesLive'), 'BELIEVED AT THIS MOMENT');
  assert.equal(eyebrowOf(view.panels, 'typeDist'), 'HOW NOTES CONNECT');
  assert.equal(eyebrowOf(view.panels, 'ego'), 'THE LEDGER · ONE NOTE, EVERYTHING IT TOUCHES');
  assert.match(eyebrowOf(view.panels, 'scrub'), /^BELIEF TIME ·/);
});

// ---- vocabFor shape -------------------------------------------------

test('vocabFor exposes both frames with the same keys', () => {
  const n = vocabFor(true);
  const d = vocabFor(false);
  for (const k of Object.keys(n)) assert.ok(k in d, `neutral vocab missing ${k}`);
  assert.equal(d.item, 'record');
  assert.equal(d.Items, 'RECORDS');
  assert.equal(n.item, 'note');
});
