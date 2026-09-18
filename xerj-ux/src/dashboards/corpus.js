// ============================================================
// Section — CORPUS  (the home page after `xerj brain <folder>`)
//
// One card per dataset the autoindex catalog records: what it is, how much,
// what formats, its time span, the top fields with types, and the sample
// queries autoindex wrote for it — each a button that opens Discover with
// that query prefilled and run. On an engine with no datasets: the exact one
// command to run. Data comes from `backends/xerj.js#liveCorpus` (the
// `autoindex-catalog` index, live) — never from a mock.
// ============================================================

import { esc } from '../ux/text.js';
import { topFields, sampleQueryToSearch, fmtBytes, fmtCount, timeSpan } from '../data/catalog.js';
import { readerHref } from '../ux/reader-render.js';

export const EMPTY_COMMAND = 'xerj brain <folder>';

/** The empty-engine state: exactly one command. */
export function renderCorpusEmpty({ guest } = {}) {
  if (guest) return `<div class="cp-empty">Nothing is indexed under the shared index yet.</div>`;
  return `<div class="cp-empty">
    <div class="key">NOTHING INDEXED YET</div>
    <p>Point XERJ at a folder and it becomes typed, searchable datasets with a knowledge graph — one command:</p>
    <pre class="cp-cmd mono accent">${esc(EMPTY_COMMAND)}</pre>
    <p class="faint">Then reload this page: every dataset it wrote appears here as a card.</p>
  </div>`;
}

function sampleButtons(card) {
  const btns = [];
  for (const sq of card.sampleQueries || []) {
    const s = sampleQueryToSearch(sq);
    if (!s) continue;
    btns.push(`<button type="button" class="cp-sq" data-corpus-query="${esc(JSON.stringify({ index: card.index, type: s.type, q: s.q }))}" title="${esc(sq.title || '')}"><span class="cp-sq__type mono">${esc(s.type.toUpperCase())}</span><span class="cp-sq__q">${esc(s.q)}</span></button>`);
  }
  return btns.join('');
}

/** One dataset card. Pure. */
export function renderCorpusCard(card, { brain } = {}) {
  const fields = topFields(card, 8);
  const span = timeSpan(card);
  const samples = sampleButtons(card);
  return `<article class="cp-card" data-corpus-index="${esc(card.index)}">
    <div class="cp-card__head">
      <div>
        <div class="key">DATASET</div>
        <h2 class="cp-card__name mono">${esc(card.index)}</h2>
      </div>
      <div class="cp-card__nums mono">
        <span><b class="accent">${esc(fmtCount(card.records))}</b> records</span>
        <span><b>${esc(fmtCount(card.files))}</b> files</span>
        <span><b>${esc(fmtBytes(card.bytes))}</b></span>
      </div>
    </div>
    <div class="cp-card__facts mono">
      ${card.formats.length ? `<span class="cp-fact"><span class="faint">formats</span> ${card.formats.map(esc).join(', ')}</span>` : ''}
      ${span ? `<span class="cp-fact"><span class="faint">${esc(card.timeField || 'time')}</span> ${esc(span)}</span>` : ''}
      ${card.semanticField ? `<span class="cp-fact"><span class="faint">semantic</span> ${esc(card.semanticField)}</span>` : '<span class="cp-fact faint">no semantic field</span>'}
    </div>
    ${fields.length ? `<div class="cp-fields">${fields.map((f) => `<span class="cp-field mono" title="${esc(f.examples.join(' · '))}"><span class="cp-field__name">${esc(f.name)}</span><span class="cp-field__type">${esc(f.type)}</span></span>`).join('')}</div>` : ''}
    <div class="cp-actions">
      <a class="text-btn" href="#/discover" data-corpus-browse="${esc(card.index)}">BROWSE IN DISCOVER</a>
      <a class="text-btn" href="${esc(readerHref({ index: card.index, brain }))}">OPEN IN READER</a>
    </div>
    ${samples ? `<div class="cp-samples"><div class="key">TRY A QUERY</div>${samples}</div>` : ''}
  </article>`;
}

export const corpus = {
  id:   'corpus',
  name: 'Corpus',
  section: 'corpus',
  render: ({ data, guest, brain }) => {
    const all = data?.datasets || [];
    const datasets = guest ? all.filter((c) => c.index === guest.index) : all;
    const total = datasets.reduce((n, c) => n + c.records, 0);
    const meta = datasets.length
      ? [`${datasets.length} DATASET${datasets.length === 1 ? '' : 'S'}`, `${fmtCount(total)} RECORDS`]
      : ['EMPTY'];
    const caption = data?.error
      ? `The catalog could not be read: ${data.error}`
      : (datasets.length
        ? 'Everything xerj brain / xerj autoindex wrote to this engine, read live from the autoindex catalog. Click a sample query to run it in Discover, or open a dataset in the Reader.'
        : '');
    return {
      title:  guest ? 'SHARED CORPUS' : 'CORPUS',
      kicker: 'WHAT IS INDEXED',
      meta,
      caption,
      panels: [
        { id: 'datasets', eyebrow: datasets.length ? 'ONE CARD PER DATASET · LIVE FROM autoindex-catalog' : 'NOTHING INDEXED', cols: 12, type: 'corpus',
          render: () => (data?.error
            ? `<div class="cp-empty">${esc(data.error)}</div>`
            : (datasets.length
              ? `<div class="cp-grid">${datasets.map((c) => renderCorpusCard(c, { brain })).join('')}</div>`
              : renderCorpusEmpty({ guest }))),
        },
      ],
    };
  },
};
