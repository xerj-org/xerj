// ============================================================
// Dashboard — SECOND BRAIN
//
// The relationship layer over documents that already exist: edges are
// ordinary XERJ documents, bi-temporal and soft-delete-only, asserted
// by deterministic detectors (or by hand) with the exact source quote
// as evidence.
//
// Composition, top to bottom (the decided §1.1 order):
//   controls   brain switcher · trail crumbs · FIND
//   map        THE MAP — bounded, deterministic overview (mounted by
//              ux/brain-map.js; this dashboard renders the mount point)
//   scrub      ONE belief-time caret for the whole page — the map
//              restyles and the ledger restates from the same drag
//   stats      counts, kinds, teachers, file types, crossings, and the
//              page's own attributed read log — ran numbers only
//   ego        THE LEDGER — the leaf view; every link's evidence
//   hubs/honesty
//
// The ledger is still the terminal surface: a global force-directed
// hairball stays rejected (see ux/ego-ledger.js). The map above it is
// bounded by construction and routes every terminal click back into
// the ledger.
//
// LIVE DATA ONLY. data/backends/xerj.js routes this dashboard to
// data/second-brain-api.js, which reads the console's own session-authorized
// graph endpoints (`/_xerj-console/api/v1/graph/{brain}/overview` and
// `…/{brain}/ego`, issue #936) and NEVER falls back to mock — an empty or
// unreachable brain renders an honest state that says how to fill it
// (`xerj brain ~/your-folder`), not invented beliefs.
// ============================================================

import { renderPanelBody } from '../ux/ego-ledger.js';
import { sbAfterRender } from '../data/second-brain-api.js';
import { vocabFor } from '../ux/sb-vocab.js';

/** One panel body: renderPanelBody draws the inner HTML; the
 *  `data-sb-body` wrapper is the in-place re-render target the
 *  interaction controller patches after refocus / as-of commits. */
const body = (id, data) => `<div data-sb-body="${id}">${renderPanelBody(id, data)}</div>`;

export const secondBrain = {
  id: 'second-brain',
  name: 'Second Brain',
  render: ({ data, time }) => {
    // Traces + the map mount need real layout; schedule a post-paint
    // measure+draw pass. No-op outside a browser.
    sbAfterRender();
    // #1100: eyebrows speak the corpus's own frame — "notes / believed" for
    // a note-like corpus, the neutral "records / active" frame otherwise.
    const voc = vocabFor(!!(data && data.sb && data.sb.noteLike));
    return {
      title: 'SECOND BRAIN',
      // "EVERY LINK SHOWS ITS EVIDENCE", not "HAS A QUOTE": manual and
      // agent-asserted links may carry no quote, and structural links
      // carry a rationale, not note text. The kicker must not claim
      // more than the schema enforces (spec §7.1, amended 2026-07-30).
      // It says DOCUMENTS, not notes: the corpus is whatever was indexed
      // (mail, PDFs, code), and this page is its graph layer — reached
      // from CORPUS, not a separate "knowledge base".
      kicker: 'THE LINKS BETWEEN YOUR DOCUMENTS · EVERY LINK SHOWS ITS EVIDENCE · REPLAYABLE AT ANY MOMENT',
      // The caption ties this page back to the one knowledge surface: the
      // corpus (sizes, fields, relations, capabilities) lives on CORPUS.
      caption: 'The graph layer of the corpus CORPUS shows: links between the documents you indexed, each with its evidence. Sizes, fields and relations live on CORPUS.',
      meta: [time, 'XERJ-GRAPH'],
      // Eyebrows speak user words — link / believed / retired / taught.
      // Schema vocabulary (edge, src/dst, as-of) stays in the API and
      // in the evidence paper-trail, never in a panel title.
      panels: [
        { id: 'controls', eyebrow: 'YOU ARE HERE · BRAIN, FOCUS, FIND', cols: 12, type: 'controls',
          render: () => body('controls', data) },
        { id: 'map', eyebrow: 'THE MAP · THE WHOLE BRAIN AT A GLANCE', cols: 12, type: 'map',
          render: () => body('map', data) },
        { id: 'scrub', eyebrow: `${voc.timeAxis} · ONE CARET DRIVES THE WHOLE PAGE`, cols: 12, type: 'scrub',
          render: () => body('scrub', data) },
        { id: 'edgesLive', eyebrow: voc.atMoment, cols: 3, type: 'metric',
          render: () => body('edgesLive', data) },
        { id: 'edgesTotal', eyebrow: 'EVER ASSERTED', cols: 3, type: 'metric',
          render: () => body('edgesTotal', data) },
        { id: 'invalidated', eyebrow: 'RETIRED · KEPT FOR REPLAY', cols: 3, type: 'metric',
          render: () => body('invalidated', data) },
        { id: 'detectors', eyebrow: 'WHAT TAUGHT THIS BRAIN', cols: 3, type: 'metric',
          render: () => body('detectors', data) },
        { id: 'typeDist', eyebrow: voc.typeDistEye, cols: 6, type: 'dist',
          render: () => body('typeDist', data) },
        { id: 'edgeTimeline', eyebrow: 'NEW LINKS PER DAY', cols: 6, type: 'series',
          render: () => body('edgeTimeline', data) },
        { id: 'notes', eyebrow: 'WHAT THIS BRAIN HOLDS', cols: 4, type: 'notes',
          render: () => body('notes', data) },
        { id: 'crossings', eyebrow: 'LINKS ACROSS FILE TYPES', cols: 4, type: 'crossings',
          render: () => body('crossings', data) },
        { id: 'reads', eyebrow: 'WHAT THIS VIEW READ · AND WHY', cols: 4, type: 'reads',
          render: () => body('reads', data) },
        { id: 'ego', eyebrow: voc.egoEye, cols: 12, type: 'ego',
          render: () => body('ego', data) },
        { id: 'hubs', eyebrow: 'CENTERS OF GRAVITY', cols: 6, type: 'topn',
          render: () => body('hubs', data) },
        { id: 'notShown', eyebrow: 'WHAT THIS VIEW DID NOT SHOW', cols: 6, type: 'honesty',
          render: () => body('notShown', data) },
      ],
    };
  },
};
