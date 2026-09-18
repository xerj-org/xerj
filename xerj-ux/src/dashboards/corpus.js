// ============================================================
// Section — CORPUS  (the home page after `xerj brain <folder>`)
//
// One card per dataset the autoindex catalog records: what it is, how much,
// what formats, its time span, the top fields with types, and the sample
// queries autoindex wrote for it. On an engine with no datasets: the exact
// one command to run. Data comes from `backends/xerj.js#liveCorpus` (the
// `autoindex-catalog` index, live) and is listed in data/query.js#NEVER_MOCK:
// this page shows what the engine reports, or an error — never a sample.
//
// The panel below is a MOUNT POINT, not markup. Everything on a card can be
// document-derived, so the shell (app.js#mountSafePanels) fills it with nodes
// from ux/corpus-render.js through ux/safe-dom.js. No document string is ever
// interpolated into the HTML this module returns.
// ============================================================

import { fmtCount } from '../data/catalog.js';

export const CORPUS_MOUNT = '<div data-safe-mount="corpus" class="cp-mount"></div>';

export const corpus = {
  id:   'corpus',
  name: 'Corpus',
  section: 'corpus',
  render: ({ data }) => {
    const datasets = data?.datasets || [];
    const summaries = data?.summaries || [];
    const n = datasets.length + summaries.length;
    const total = datasets.reduce((acc, c) => acc + c.records, 0) + summaries.reduce((acc, c) => acc + c.records, 0);
    const meta = data?.status === 'error'
      ? ['CATALOG UNREADABLE']
      : (n ? [`${n} DATASET${n === 1 ? '' : 'S'}`, `${fmtCount(total)} RECORDS`] : ['EMPTY']);
    return {
      title:  'CORPUS',
      kicker: 'WHAT IS INDEXED',
      meta,
      caption: n
        ? 'Everything xerj brain / xerj autoindex wrote to this engine, read live from the autoindex catalog, plus any other index it holds. Open a dataset in the Reader, or run one of the sample queries its catalog entry carries.'
        : '',
      panels: [
        { id: 'datasets', eyebrow: n ? 'ONE CARD PER DATASET · LIVE FROM autoindex-catalog' : 'WHAT IS INDEXED', cols: 12, type: 'corpus',
          render: () => CORPUS_MOUNT,
        },
      ],
    };
  },
};
