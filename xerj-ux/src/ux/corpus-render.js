// ============================================================
// XERJ Console — Corpus home rendering (pure)
//
// "What is indexed here?" — one card per dataset. Two sources, one look:
//
//   operator  the autoindex catalog (`autoindex-catalog`, one
//             `doc_kind: dataset` document per dataset — data/catalog.js)
//   guest     the shared index itself (data/reader-api.js#indexSummary): a
//             share is never granted the catalog, which lists every corpus
//             on the node.
//
// Everything on a card can be document-derived — a sample query is built from
// the corpus's own terms, a field example is a document value, a format or a
// dataset slug comes from a file path — so, like the reader, this returns
// safe-dom nodes and never markup. No DOM, no fetch.
// ============================================================

import { h } from './safe-dom.js';
import { readerHref } from './reader-render.js';
import { rankedFields, sampleQueryToSearch, fmtBytes, fmtCount, timeSpan } from '../data/catalog.js';

export const EMPTY_COMMAND = 'xerj brain <folder>';

/** The empty-engine state: exactly one command. Never sample data.
 *  #2409: a fresh data directory looks exactly like lost data, so the
 *  operator also sees which directory this node serves and the
 *  --data-dir/--config hint (both from the knowledge payload; the hint is
 *  the wording `xerj autoindex map` prints on an empty catalog). */
export function renderCorpusEmpty({ guest, dataDir, dataDirHint } = {}) {
  if (guest) return h('div', { class: 'cp-empty' }, 'Nothing is indexed under this share yet.');
  return h('div', { class: 'cp-empty' },
    h('div', { class: 'key' }, 'NOTHING INDEXED YET'),
    h('p', null, 'Point XERJ at a folder — email, PDFs, notes, code — and it becomes typed, searchable datasets with a graph of the links between files. One command:'),
    h('pre', { class: 'cp-cmd mono accent' }, EMPTY_COMMAND),
    h('p', { class: 'faint' }, 'Then reload this page: every dataset it wrote appears here as a card.'),
    dataDir ? h('div', { class: 'cp-datadir' },
      h('div', { class: 'key' }, 'DATA DIRECTORY'),
      h('pre', { class: 'cp-cmd mono' }, dataDir)) : null,
    dataDirHint ? h('p', { class: 'faint cp-datadir-hint' }, dataDirHint) : null);
}

const fact = (k, v) => h('span', { class: 'cp-fact' }, h('span', { class: 'faint' }, k), ' ', v);

/** A field's measured facts, as text: coverage (null = not measured → "—"),
 *  null% when nonzero, cardinality when known. These are the catalog's own
 *  numbers — the same ones `xerj autoindex map` prints for agents. */
function fieldMeta(f) {
  const pct = (v) => (v == null ? '—' : `${Math.round(v * 100)}%`);
  const parts = [pct(f.coverage)];
  if (f.nullRatio != null && f.nullRatio > 0) parts.push(`${Math.round(f.nullRatio * 100)}% null`);
  if (f.cardinality != null) parts.push(f.cardinality === 0 ? 'all same' : `${fmtCount(f.cardinality)} distinct`);
  return parts.join(' · ');
}

/** One field chip. The example values are VISIBLE text (they were
 *  hover-only before, which on touch and on first read is "no examples at
 *  all"), and everything on the chip is data the catalog measured. */
function fieldChip(f) {
  const ex = f.examples && f.examples.length ? h('span', { class: 'cp-field__ex' }, ` e.g. ${f.examples.slice(0, 2).join(' · ')}`) : null;
  return h('span', { class: `cp-field mono${f.semantic ? ' cp-field--sem' : ''}`, title: f.examples && f.examples.length ? f.examples.join(' · ') : '' },
    h('span', { class: 'cp-field__name' }, f.name),
    h('span', { class: 'cp-field__type' }, f.type),
    h('span', { class: 'cp-field__meta faint' }, fieldMeta(f)),
    ex);
}

/** The fields block: the ranked top fields as chips, then EVERY field in a
 *  disclosure when there are more — in the SAME display order, so the table
 *  reads as one ranked list, top chips first. A corpus's structure is the
 *  one thing a person came to see — nothing is capped away. */
function fieldsBlock(card) {
  const ranked = rankedFields(card);
  if (!ranked.length) return null;
  const top = ranked.slice(0, 8);
  const rest = ranked.slice(8);
  return h('div', { class: 'cp-fields' },
    top.map(fieldChip),
    rest.length ? h('details', { class: 'cp-fields__all' },
      h('summary', null, `ALL ${ranked.length} FIELDS`),
      h('div', { class: 'cp-fields' }, rest.map(fieldChip))) : null);
}

function sampleButtons(card) {
  const out = [];
  for (const sq of card.sampleQueries || []) {
    const s = sampleQueryToSearch(sq);
    if (!s) continue;
    out.push(h('button', {
      class: 'cp-sq',
      // Consumed by the shell's click handler as DATA (JSON.parse → search
      // state). It is an attribute value: inert whatever it contains.
      'data-corpus-query': JSON.stringify({ index: card.index, type: s.type, q: s.q, ...(s.field ? { field: s.field } : {}) }),
      title: sq.title || '',
    }, h('span', { class: 'cp-sq__type mono' }, s.type.toUpperCase()), h('span', { class: 'cp-sq__q' }, s.q)));
    if (out.length >= 6) break;
  }
  return out;
}

/** What the catalog entry describes: the LAST `xerj brain` / `xerj autoindex`
 *  run over this dataset. The record count is the index's total at the end of
 *  that run; files, bytes, formats and the semantic_text field are that run's
 *  own. A second run over another folder into the same index rewrites the
 *  entry with ITS files (PR #945 review: "91 records · 4 files" after a
 *  14-file corpus), so those facts are labelled as the last run's. */
const LAST_RUN = 'as of the last xerj brain / autoindex run over this dataset (autoindex-catalog)';

/** One dataset card from a catalog entry (data/catalog.js — parseCatalogHits
 *  or parseKnowledge; the latter also carries the engine's LIVE doc count and
 *  store bytes for the index, shown next to the run's own numbers). */
export function renderCorpusCard(card, { brain, discover = true } = {}) {
  const span = timeSpan(card);
  const samples = sampleButtons(card);
  return h('article', { class: 'cp-card', 'data-corpus-index': card.index },
    h('div', { class: 'cp-card__head' },
      h('div', null, h('div', { class: 'key' }, 'DATASET'), h('h2', { class: 'cp-card__name mono' }, card.index)),
      h('div', { class: 'cp-card__nums mono' },
        h('span', { title: card.liveDocs != null ? `${fmtCount(card.liveDocs)} documents in the index now` : LAST_RUN },
          h('b', { class: 'accent' }, fmtCount(card.liveDocs != null ? card.liveDocs : card.records)), ' records'),
        h('span', { title: LAST_RUN }, h('b', null, fmtCount(card.files)), ' files'),
        h('span', { title: card.storeBytes != null ? `${fmtBytes(card.storeBytes)} on disk now` : LAST_RUN }, h('b', null, fmtBytes(card.bytes))),
        h('span', { class: 'faint', title: LAST_RUN }, '· last run'))),
    h('div', { class: 'cp-card__facts mono' },
      card.formats.length ? fact('formats · last run', card.formats.join(', ')) : null,
      span ? fact(card.timeField || 'time', span) : null,
      card.semanticField ? fact('semantic_text field', card.semanticField) : h('span', { class: 'cp-fact faint', title: LAST_RUN }, 'no semantic_text field in the last run'),
      card.junk ? fact('skipped as junk', fmtCount(card.junk)) : null),
    fieldsBlock(card),
    h('div', { class: 'cp-actions' },
      h('a', { class: 'text-btn', href: readerHref({ index: card.index, brain }) }, 'OPEN IN READER'),
      discover ? h('a', { class: 'text-btn', href: '#/discover', 'data-corpus-browse': card.index }, 'BROWSE IN DISCOVER') : null),
    samples.length ? h('div', { class: 'cp-samples' }, h('div', { class: 'key' }, 'TRY A QUERY · FROM THIS DATASET\'S OWN CATALOG ENTRY'), samples) : null);
}

/** One card from an index's own counts (guest mode; also the operator
 *  fallback for an index the catalog does not describe). */
export function renderSummaryCard(sum, { brain } = {}) {
  if (sum.error) {
    return h('article', { class: 'cp-card', 'data-corpus-index': sum.index },
      h('div', { class: 'key' }, 'DATASET'),
      h('h2', { class: 'cp-card__name mono' }, sum.index),
      h('div', { class: 'cp-empty' }, `Could not read this index: ${sum.error}`),
      h('div', { class: 'cp-card__facts mono faint' }, 'This page asks again each time you open it — come back to CORPUS, or reload the tab.'));
  }
  return h('article', { class: 'cp-card', 'data-corpus-index': sum.index },
    h('div', { class: 'cp-card__head' },
      h('div', null, h('div', { class: 'key' }, 'DATASET'), h('h2', { class: 'cp-card__name mono' }, sum.index)),
      h('div', { class: 'cp-card__nums mono' },
        h('span', null, h('b', { class: 'accent' }, fmtCount(sum.records)), ' records'))),
    h('div', { class: 'cp-card__facts mono' },
      sum.emails ? fact('emails', fmtCount(sum.emails)) : null,
      sum.attachments ? fact('attachment records', fmtCount(sum.attachments)) : null,
      sum.formats.length ? fact('formats', sum.formats.map((f) => `${f.key} ${fmtCount(f.count)}`).join(', ')) : null),
    h('div', { class: 'cp-actions' },
      h('a', { class: 'text-btn', href: readerHref({ index: sum.index, brain }) }, 'OPEN IN READER')));
}

/**
 * The corpus grid.
 *   state: { status: 'loading'|'ok'|'error', datasets?, summaries?, error? }
 */
export function renderCorpus(state = {}, { guest = false, brain } = {}) {
  if (state.status === 'loading' || !state.status) return h('div', { class: 'cp-empty' }, 'Reading what is indexed…');
  if (state.status === 'error') {
    return h('div', { class: 'cp-empty' },
      h('div', { class: 'key' }, 'COULD NOT READ THE CATALOG'),
      h('p', null, String(state.error || 'unknown error')),
      h('p', { class: 'faint' }, 'Nothing is shown rather than a sample: this page only ever lists what the engine reports.'));
  }
  const cards = [
    ...(state.datasets || []).map((c) => renderCorpusCard(c, { brain, discover: !guest })),
    ...(state.summaries || []).map((s) => renderSummaryCard(s, { brain })),
  ];
  if (!cards.length) return renderCorpusEmpty({ guest, dataDir: state.dataDir, dataDirHint: state.dataDirHint });
  return h('div', { class: 'cp-grid' }, cards);
}

// ── relations ───────────────────────────────────────────────────────
//
// What autoindex CORRELATE actually inferred between two datasets — a key
// overlap ("these two datasets share these values: a join exists") or a
// time alignment ("they cover the same period and move together"). Only
// inferred relations are ever listed; an empty list is shown as none,
// never as a guess. The field names and the shared values are
// document-derived, so this goes through safe-dom like everything else.

const pct1 = (v) => (v == null ? null : `${Math.round(v * 100)}%`);

function relationRow(r) {
  const left = `${r.aIndex}.${r.aField}`;
  const right = `${r.bIndex}.${r.bField}`;
  if (r.kind === 'time_alignment') {
    const bits = [
      pct1(r.rangeOverlap) ? `ranges overlap ${pct1(r.rangeOverlap)}` : null,
      r.sharedBuckets ? `${fmtCount(r.sharedBuckets)} shared time buckets` : null,
      r.pearsonR != null ? `activity r=${r.pearsonR.toFixed(2)}${r.activityCorrelated ? ' · correlated' : ''}` : null,
    ].filter(Boolean);
    return h('div', { class: 'cp-rel' },
      h('span', { class: 'cp-rel__pair mono' }, left, '  ⟷  ', right),
      h('span', { class: 'cp-rel__why' }, bits.join(' · ') || 'time-aligned'));
  }
  const why = [
    r.overlap ? `${fmtCount(r.overlap)} shared values` : null,
    r.grade ? r.grade : null,
    r.confirmed != null && r.tested != null ? `${fmtCount(r.confirmed)}/${fmtCount(r.tested)} confirmed by query` : null,
    pct1(r.containment) ? `${pct1(r.containment)} containment` : null,
  ].filter(Boolean).join(' · ');
  return h('div', { class: 'cp-rel' },
    h('span', { class: 'cp-rel__pair mono' }, left, '  ⟷  ', right),
    h('span', { class: 'cp-rel__why' }, why || 'fields overlap'),
    r.examples.length ? h('span', { class: 'cp-rel__ex faint mono' }, `e.g. ${r.examples.slice(0, 3).join(' · ')}`) : null);
}

/** The relations panel: one row per inferred cross-dataset relation. */
export function renderRelations(state = {}) {
  const rels = (state.relations || []).filter((r) => r && r.aIndex && r.bIndex);
  if (!rels.length) {
    return h('div', { class: 'cp-empty' },
      h('p', null, 'No cross-dataset relations were inferred'),
      h('p', { class: 'faint' }, 'Autoindex correlates key-like fields and time spans between datasets; a corpus with no shared keys or dates has none. Nothing is guessed here.'));
  }
  return h('div', { class: 'cp-rels' }, rels.map(relationRow));
}

// ── capabilities ────────────────────────────────────────────────────
//
// "What can I do with this now?" — one entry per REAL surface, each naming
// the console route, CLI command or HTTP endpoint it refers to. The list is
// computed server-side from facts on the node (catalog present, brains
// present); the renderer only lays it out and never adds an entry of its
// own — a capability this page claims that the engine does not have is the
// one dishonesty the console cannot undo.

function capabilityCard(c) {
  const surface = c.href
    ? h('a', { class: 'cp-cap__go mono', href: c.href }, c.href.replace(/^#\//, '').toUpperCase())
    : h('span', { class: 'cp-cap__go mono' }, c.command || c.endpoint || '');
  return h('div', { class: `cp-cap cp-cap--${c.kind}` },
    h('div', { class: 'cp-cap__title' }, c.title),
    h('p', { class: 'cp-cap__blurb' }, c.blurb),
    surface);
}

/** The capability strip. */
export function renderCapabilities(state = {}) {
  const caps = (state.capabilities || []).filter((c) => c && c.title);
  if (!caps.length) return null;
  return h('div', { class: 'cp-caps' }, caps.map(capabilityCard));
}
