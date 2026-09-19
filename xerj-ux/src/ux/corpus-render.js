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
import { topFields, sampleQueryToSearch, fmtBytes, fmtCount, timeSpan } from '../data/catalog.js';

export const EMPTY_COMMAND = 'xerj brain <folder>';

/** The empty-engine state: exactly one command. Never sample data. */
export function renderCorpusEmpty({ guest } = {}) {
  if (guest) return h('div', { class: 'cp-empty' }, 'Nothing is indexed under this share yet.');
  return h('div', { class: 'cp-empty' },
    h('div', { class: 'key' }, 'NOTHING INDEXED YET'),
    h('p', null, 'Point XERJ at a folder — email, PDFs, notes, code — and it becomes typed, searchable datasets with a graph of the links between files. One command:'),
    h('pre', { class: 'cp-cmd mono accent' }, EMPTY_COMMAND),
    h('p', { class: 'faint' }, 'Then reload this page: every dataset it wrote appears here as a card.'));
}

const fact = (k, v) => h('span', { class: 'cp-fact' }, h('span', { class: 'faint' }, k), ' ', v);

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

/** One dataset card from a catalog entry (data/catalog.js#parseCatalogHits). */
export function renderCorpusCard(card, { brain, discover = true } = {}) {
  const fields = topFields(card, 8);
  const span = timeSpan(card);
  const samples = sampleButtons(card);
  return h('article', { class: 'cp-card', 'data-corpus-index': card.index },
    h('div', { class: 'cp-card__head' },
      h('div', null, h('div', { class: 'key' }, 'DATASET'), h('h2', { class: 'cp-card__name mono' }, card.index)),
      h('div', { class: 'cp-card__nums mono' },
        h('span', null, h('b', { class: 'accent' }, fmtCount(card.records)), ' records'),
        h('span', { title: LAST_RUN }, h('b', null, fmtCount(card.files)), ' files'),
        h('span', { title: LAST_RUN }, h('b', null, fmtBytes(card.bytes))),
        h('span', { class: 'faint', title: LAST_RUN }, '· last run'))),
    h('div', { class: 'cp-card__facts mono' },
      card.formats.length ? fact('formats · last run', card.formats.join(', ')) : null,
      span ? fact(card.timeField || 'time', span) : null,
      card.semanticField ? fact('semantic_text field', card.semanticField) : h('span', { class: 'cp-fact faint', title: LAST_RUN }, 'no semantic_text field in the last run'),
      card.junk ? fact('skipped as junk', fmtCount(card.junk)) : null),
    fields.length
      ? h('div', { class: 'cp-fields' }, fields.map((f) => h('span', { class: 'cp-field mono', title: f.examples.join(' · ') },
        h('span', { class: 'cp-field__name' }, f.name), h('span', { class: 'cp-field__type' }, f.type))))
      : null,
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
  if (!cards.length) return renderCorpusEmpty({ guest });
  return h('div', { class: 'cp-grid' }, cards);
}
