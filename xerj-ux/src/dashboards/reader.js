// ============================================================
// Section — READER  (any record, with the knowledge graph around it)
//
// Route `#/reader?index=<i>&id=<id>&brain=<b>`. Search a corpus, open any
// record it holds — an email with its attachments, a PDF page, a note, a code
// symbol — rendered by its shape, beside the records the brain links it to.
// Generalises #923's Case Review (email + PDF only, string-built markup) to
// every record kind autoindex writes.
//
// The panel below is a MOUNT POINT. The reader displays other people's
// documents, so none of it is built as an HTML string: the shell
// (app.js#mountSafePanels) attaches ux/reader-view.js, which renders through
// ux/safe-dom.js. The same view, over a different transport, is what a
// share-link guest gets (guest-app.js).
// ============================================================

export const READER_MOUNT = '<div data-safe-mount="reader" class="rd-mount"></div>';

export const reader = {
  id:   'reader',
  name: 'Reader',
  section: 'reader',
  render: () => ({
    title:  'READER',
    kicker: 'ONE RECORD, AND WHAT LINKS TO IT',
    meta:   [],
    caption: '',
    panels: [
      { id: 'reader', eyebrow: 'SEARCH · RECORD · LINKED RECORDS', cols: 12, type: 'reader',
        render: () => READER_MOUNT,
      },
    ],
  }),
};
