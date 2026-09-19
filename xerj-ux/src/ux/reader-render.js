// ============================================================
// XERJ Console — Reader rendering (pure)
//
// One record in, a node tree out (ux/safe-dom.js). The reader opens ANY
// record `xerj autoindex` / `xerj brain` wrote and renders it by SHAPE,
// detected from the record's own fields (the autoindex extractors —
// engine/crates/xerj-autoindex/src/extract/ — are the contract):
//
//   email        email_subject / email_from / email_to / email_cc / email_date /
//                email_message_id / email_in_reply_to / body   (extract/eml.rs)
//   attachment   attachment_name / attachment_content_type / attachment_bytes,
//                plus the parent's email_* headers and (for PDFs) page + body
//   pdf          page + body + title                          (extract/pdf.rs)
//   code-symbol  name / kind / line / language / path / code  (extract/code.rs)
//   code-file    language / defs / symbols[]                  (extract/code.rs)
//   note         title / body / ax_path  — markdown, docx, text, html
//   file         ax_locator = "file" and no body — the one record autoindex
//                writes per FILE. It carries no text of its own; it is what
//                the file-level graph edges (same_dir, mdlink, pathcite, href)
//                point at, so it lists the records that came out of the file.
//   generic      anything else: the pretty-printed _source
//
// Plus the knowledge graph around it: `GET /_graph/{brain}/ego` edges grouped
// by edge type, each neighbour a link that opens in this same reader.
//
// SECURITY. A record is untrusted input: an .eml can carry `<script>` in its
// subject, an attachment can be NAMED `"><img src=x onerror=…>.pdf`, a body
// can hold `javascript:` URLs, a graph node title is whatever the linked file
// said. Nothing here produces markup. Every function returns safe-dom nodes,
// in which a document string can only ever be a TEXT child or an inert
// attribute value, and the only href is an in-app hash route built by
// `readerHref`. Email HTML is never rendered: the extractor indexes the text
// part, and that text is shown as text. URLs inside a document are shown,
// not linked. No DOM, no fetch: testable under node.
// ============================================================

import { h, highlightChildren, HL_PRE } from './safe-dom.js';

// Vector / passage plumbing the reader never shows (it is huge and meaningless
// to a person); counted, never silently dropped.
const HIDDEN_FIELD = /(_vector|_vector_chunks|_chunks)$|^__xerj_passage_meta__/;

export const SHAPES = ['email', 'attachment', 'pdf', 'code-symbol', 'code-file', 'note', 'file', 'generic'];

/** Which renderer a record gets. Order matters: an attachment record also
 *  carries its parent's email headers, so it is tested first. */
export function detectShape(src) {
  const s = src && typeof src === 'object' ? src : {};
  const has = (k) => s[k] != null && s[k] !== '';
  if (has('attachment_name')) return 'attachment';
  if (has('email_subject') || has('email_from') || has('email_message_id')) return 'email';
  if (typeof s.code === 'string' && has('name') && has('language')) return 'code-symbol';
  if (has('language') && (typeof s.defs === 'string' || Array.isArray(s.symbols))) return 'code-file';
  if (has('page') && Number.isFinite(Number(s.page)) && typeof s.body === 'string') return 'pdf';
  if (typeof s.body === 'string' || typeof s.text === 'string') return 'note';
  if (s.ax_locator === 'file' && has('ax_file')) return 'file';
  return 'generic';
}

/** Coerce any `_source` value to display text. Objects/arrays are JSON, so a
 *  multi-valued header (`email_to: [a, b]`) reads as a list, not [object]. */
export function str(v) {
  if (v == null) return '';
  if (typeof v === 'string') return v;
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  if (Array.isArray(v)) return v.map(str).filter(Boolean).join(', ');
  try { return JSON.stringify(v); } catch { return ''; }
}

/** A one-line title for a record, by shape. Never the raw id unless nothing
 *  better exists. */
export function recordTitle(src, id) {
  const s = src && typeof src === 'object' ? src : {};
  switch (detectShape(s)) {
    case 'attachment':  return str(s.attachment_name);
    case 'email':       return str(s.email_subject) || '(no subject)';
    case 'code-symbol': return `${s.kind ? str(s.kind) + ' ' : ''}${str(s.name)}`;
    case 'code-file':   return str(s.title || s.path || s.ax_path || id);
    case 'pdf':         return `${str(s.title || s.ax_path) || 'document'} · p${str(s.page)}`;
    case 'note':        return str(s.title || s.ax_path || s.path || id);
    case 'file':        return str(s.ax_path || s.title || id);
    default:            return str(s.title || s.name || id);
  }
}

/** Short, human badge for the result list. */
export function shapeBadge(shape, src) {
  const s = src && typeof src === 'object' ? src : {};
  switch (shape) {
    case 'attachment':  return s.page != null ? `PDF · p${str(s.page)}` : (str(s.attachment_content_type) || 'attachment').split('/').pop().toUpperCase();
    case 'email':       return 'EMAIL';
    case 'code-symbol': return (str(s.kind) || 'symbol').toUpperCase();
    case 'code-file':   return (str(s.language) || 'code').toUpperCase();
    case 'pdf':         return `PDF · p${str(s.page)}`;
    case 'note':        return (str(s.ax_format) || 'note').toUpperCase();
    case 'file':        return `FILE${s.ax_format ? ' · ' + str(s.ax_format).toUpperCase() : ''}`;
    default:            return (str(s.ax_format) || 'record').toUpperCase();
  }
}

/** The reader deep link for a record — always an in-app hash route. Values
 *  go through URLSearchParams, so an id cannot break out of the query. */
export function readerHref({ index, id, brain } = {}) {
  const qs = new URLSearchParams();
  if (index) qs.set('index', String(index));
  if (id) qs.set('id', String(id));
  if (brain) qs.set('brain', String(brain));
  return `#/reader?${qs.toString()}`;
}

function fromName(v) {
  const s = str(v);
  return s.replace(/\s*<[^>]*>\s*$/, '').trim() || s;
}
function shortDate(v) {
  const s = str(v);
  const m = s.match(/(\d{4}-\d{2}-\d{2})/);
  return m ? m[1] : s.slice(0, 24);
}
function snippet(src) {
  const s = src || {};
  const t = s.body || s.text || s.code || s.defs || s.email_subject || '';
  return str(t).replace(/\s+/g, ' ').slice(0, 180);
}
export function fmtBytes(b) {
  const n = Number(b);
  if (b == null || b === '' || !Number.isFinite(n)) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}

// ----- result list ---------------------------------------------------

/** Highlight fragments for a hit → nodes, or null when the engine sent none. */
function highlightBlock(hit) {
  const hl = hit && hit.highlight;
  if (!hl || typeof hl !== 'object') return null;
  const frags = [];
  for (const v of Object.values(hl)) {
    for (const f of Array.isArray(v) ? v : [v]) {
      if (typeof f === 'string' && f) frags.push(f);
      if (frags.length >= 2) break;
    }
    if (frags.length >= 2) break;
  }
  if (!frags.length) return null;
  // Our markers when the engine honoured them; otherwise its default
  // `<em>…</em>`, matched literally. Either way the fragment becomes text
  // nodes and <mark> elements — it is never parsed.
  return frags.map((f) => h('span', { class: 'rd-card__frag' },
    highlightChildren(f.replace(/\s+/g, ' '), { tags: f.includes(HL_PRE) ? 'markers' : 'em' })));
}

/** One clickable result card. `selectedId` marks the open record. */
export function renderResultCard(hit, { selectedId, brain } = {}) {
  const s = (hit && hit._source && typeof hit._source === 'object') ? hit._source : {};
  const shape = detectShape(s);
  const score = Number(hit._score);
  const meta = shape === 'email' || shape === 'attachment'
    ? `${fromName(s.email_from)}${s.email_date ? ' · ' + shortDate(s.email_date) : ''}`
    : str(s.ax_path || s.path || hit._index);
  return h('a', {
    class: `rd-card${hit._id === selectedId ? ' sel' : ''}`,
    href: readerHref({ index: hit._index, id: hit._id, brain }),
    'data-reader-open': str(hit._id),
    'data-reader-index': str(hit._index),
  },
    h('div', { class: 'rd-card__top' },
      h('span', { class: `rd-badge rd-badge--${shape}` }, shapeBadge(shape, s)),
      h('span', { class: 'rd-card__title' }, recordTitle(s, hit._id)),
      Number.isFinite(score) && hit._score != null ? h('span', { class: 'rd-card__score' }, score.toFixed(2)) : null),
    h('div', { class: 'rd-card__meta' }, meta),
    h('div', { class: 'rd-card__snip' }, highlightBlock(hit) || snippet(s)));
}

/** The whole list, or an honest empty state. */
export function renderResultList(hits, { selectedId, brain, emptyText } = {}) {
  if (!hits || !hits.length) return h('div', { class: 'rd-empty' }, emptyText || 'No records. Search above, or open one from Discover.');
  return h('div', { class: 'rd-list' }, hits.map((x) => renderResultCard(x, { selectedId, brain })));
}

// ----- the record ----------------------------------------------------

const hdr = (k, v) => {
  const t = str(v);
  return t === '' ? null : h('div', { class: 'rd-hdr' }, h('span', { class: 'rd-k' }, k), h('span', { class: 'rd-v' }, t));
};

function relatedLink(rec, brain, name, meta) {
  return h('a', {
    class: 'rd-att',
    href: readerHref({ index: rec._index, id: rec._id, brain }),
    'data-reader-open': str(rec._id),
    'data-reader-index': str(rec._index),
  }, h('span', { class: 'rd-att__name' }, name), h('span', { class: 'rd-att__meta' }, meta));
}

function attachmentsBlock(related, brain) {
  const attachments = related.attachments;
  if (attachments === undefined) return h('div', { class: 'rd-sub faint' }, 'attachments: looking…');
  // A join that FAILED is not "no attachments".
  if (related.attachmentsError) {
    return h('div', { class: 'rd-sub faint', 'data-rd-block': 'attachments-error' }, `This email's attachments could not be read (${str(related.attachmentsError)}).`);
  }
  if (!attachments || !attachments.length) return null;
  const cut = related.attachmentsTruncated && typeof related.attachmentsTruncated === 'object' ? related.attachmentsTruncated : null;
  return h('div', { class: 'rd-sub', 'data-rd-block': 'attachments' },
    h('div', { class: 'key' }, `ATTACHMENTS · ${attachments.length}${cut ? '+' : ''}`),
    attachments.map((a) => {
      const s = a._source || {};
      const bits = [str(s.attachment_content_type), fmtBytes(s.attachment_bytes), s.page != null ? `page ${str(s.page)}` : '']
        .filter(Boolean).join(' · ');
      return relatedLink(a, brain, str(s.attachment_name) || str(a._id), bits);
    }),
    cut ? h('div', { class: 'rd-honest mono faint', 'data-rd-block': 'attachments-truncated' },
      `This list may be incomplete: this email has ${str(cut.total)} attachment records and the first ${str(cut.read)} were read, so an attachment past them is not listed. Open the file record below for the file's records.`) : null);
}

/** The records that came out of one file (the `file` shape), or — on any
 *  other record — a link up to its file. */
function siblingsBlock(siblings, brain, truncated, total) {
  if (siblings === undefined) return h('div', { class: 'rd-sub faint' }, 'records in this file: looking…');
  if (!siblings || !siblings.length) return h('div', { class: 'rd-sub faint' }, 'No other record came out of this file.');
  return h('div', { class: 'rd-sub', 'data-rd-block': 'siblings' },
    h('div', { class: 'key' }, truncated && Number(total) > siblings.length
      ? `RECORDS IN THIS FILE · ${siblings.length} OF ${str(total)} SHOWN`
      : `RECORDS IN THIS FILE · ${siblings.length}${truncated ? '+' : ''}`),
    siblings.map((r) => {
      const rs = r._source || {};
      return relatedLink(r, brain, recordTitle(rs, r._id), shapeBadge(detectShape(rs), rs));
    }));
}

function fileBlock(fileRecord, brain) {
  if (!fileRecord) return null;
  const fs = fileRecord._source || {};
  return h('div', { class: 'rd-sub', 'data-rd-block': 'file' },
    h('div', { class: 'key' }, 'FROM FILE'),
    relatedLink(fileRecord, brain, str(fs.ax_path || fs.title) || str(fileRecord._id), 'every record from this file'));
}

function parentBlock(parent, brain) {
  if (!parent) return null;
  const s = parent._source || {};
  return h('div', { class: 'rd-sub', 'data-rd-block': 'parent' },
    h('div', { class: 'key' }, 'FROM EMAIL'),
    relatedLink(parent, brain, str(s.email_subject) || '(no subject)',
      `${fromName(s.email_from)}${s.email_date ? ' · ' + shortDate(s.email_date) : ''}`));
}

/** Body text. Always a text node inside a pre-wrapped block — an HTML email
 *  that reached the index as text is displayed as its source, never run. */
function prose(text, fallback) {
  const t = str(text);
  return h('div', { class: t ? 'rd-body' : 'rd-body faint' }, t || fallback || '(empty)');
}

function genericSource(src) {
  const shown = {};
  const hidden = [];
  for (const [k, v] of Object.entries(src || {})) {
    if (HIDDEN_FIELD.test(k)) hidden.push(k); else shown[k] = v;
  }
  let json = '';
  try { json = JSON.stringify(shown, null, 2); } catch { json = '(unprintable record)'; }
  return [
    h('pre', { class: 'rd-json mono' }, json),
    hidden.length
      ? h('div', { class: 'rd-sub faint mono' }, `${hidden.length} vector/passage field${hidden.length === 1 ? '' : 's'} not shown: ${hidden.join(', ')}`)
      : null,
  ];
}

/**
 * Render the open record. `ctx.related` carries the fetched attachments /
 * parent email (`attachments: undefined` = still loading, [] = none).
 */
export function renderRecord(hit, ctx = {}) {
  if (!hit) return h('div', { class: 'rd-empty' }, ctx.emptyText || 'Open a record from the list, from Discover, or from a graph node.');
  const s = (hit._source && typeof hit._source === 'object') ? hit._source : {};
  const shape = detectShape(s);
  const brain = ctx.brain;
  const related = ctx.related || {};
  const prov = h('div', { class: 'rd-prov mono faint' },
    `${str(hit._index)} · id ${str(hit._id)}${s.ax_path ? ' · ' + str(s.ax_path) : ''}`);
  const frame = (eyebrow, title, mono, ...rest) => h('div', { class: 'rd-read', 'data-shape': shape },
    h('div', { class: 'rd-eyebrow' }, eyebrow),
    h('h2', { class: mono ? 'rd-title mono' : 'rd-title' }, title),
    rest, shape === 'file' ? null : fileBlock(related.fileRecord, brain), prov);

  switch (shape) {
    case 'email':
      return frame('EMAIL', str(s.email_subject) || '(no subject)', false,
        h('div', { class: 'rd-hdrs' },
          hdr('From', s.email_from), hdr('To', s.email_to), hdr('Cc', s.email_cc), hdr('Date', s.email_date),
          hdr('Message-ID', s.email_message_id), hdr('In-Reply-To', s.email_in_reply_to)),
        prose(s.body, '(no body text)'),
        attachmentsBlock(related, brain));
    case 'attachment':
      return frame(s.page != null ? `PDF ATTACHMENT · PAGE ${str(s.page)}` : 'ATTACHMENT', str(s.attachment_name), false,
        h('div', { class: 'rd-hdrs' },
          hdr('Type', s.attachment_content_type), hdr('Size', fmtBytes(s.attachment_bytes)),
          hdr('Sender', fromName(s.email_from)), hdr('Sent', s.email_date)),
        typeof s.body === 'string' && s.body
          ? prose(s.body)
          : h('div', { class: 'rd-body faint' }, `No text was extracted from this attachment (${str(s.attachment_content_type) || 'binary'}).`),
        parentBlock(related.parent, brain));
    case 'pdf':
      return frame(`PDF · PAGE ${str(s.page)}`, str(s.title || s.ax_path) || 'document', false,
        prose(s.body, '(no extracted text on this page)'));
    case 'code-symbol':
      return frame(`${(str(s.language) || 'code').toUpperCase()} · ${(str(s.kind) || 'symbol').toUpperCase()}`, str(s.name), true,
        h('div', { class: 'rd-hdrs' }, hdr('File', s.path || s.ax_path), hdr('Line', s.line)),
        h('pre', { class: 'rd-code mono' }, str(s.code)));
    case 'code-file': {
      const defs = typeof s.defs === 'string'
        ? s.defs.split('\n').filter(Boolean)
        : (Array.isArray(s.symbols)
          ? s.symbols.map((y) => `${str(y && y.kind)} ${str(y && y.name)}${y && y.line != null ? ' · L' + str(y.line) : ''}`.trim())
          : []);
      return frame(`${(str(s.language) || 'code').toUpperCase()} · FILE`, str(s.title || s.path || s.ax_path || hit._id), true,
        h('div', { class: 'rd-hdrs' }, hdr('Path', s.path || s.ax_path), hdr('Declarations', defs.length)),
        defs.length ? h('ul', { class: 'rd-defs mono' }, defs.map((d) => h('li', null, d))) : null,
        typeof s.body === 'string' ? h('pre', { class: 'rd-code mono' }, s.body) : null);
    }
    case 'note':
      return frame((str(s.ax_format) || 'NOTE').toUpperCase(), str(s.title || s.ax_path || s.path || hit._id), false,
        prose(s.body != null ? s.body : s.text, '(empty)'));
    case 'file':
      return frame(shapeBadge('file', s), str(s.ax_path || s.title || hit._id), true,
        h('div', { class: 'rd-hdrs' }, hdr('Path', s.ax_path), hdr('Format', s.ax_format), hdr('Dataset', s.ax_dataset)),
        h('div', { class: 'rd-body faint' }, 'This is the record for the file itself — what the brain\'s file-level links point at. Its text lives in the records below.'),
        siblingsBlock(related.siblings, brain, related.siblingsTruncated, related.siblingsTotal));
    default:
      return frame('RECORD', recordTitle(s, hit._id), false, genericSource(s));
  }
}

// ----- the graph around it --------------------------------------------

/** Human labels for the edge types autoindex's detectors emit — one per
 *  `EDGE_TYPE` constant in engine/crates/xerj-autoindex/src/detect/*.rs. Any
 *  other type (a hand-written `/_graph/{brain}/link`) shows its raw name. */
export const EDGE_LABELS = {
  wikilink:    'wiki-links',
  mdlink:      'markdown links',
  href:        'hyperlinks',
  pathcite:    'cites the path of',
  cratecite:   'cites the crate of',
  sequence:    'next / previous section',
  same_dir:    'same folder',
  shared_term: 'shares rare terms with',
};

export function edgeLabel(type) {
  const t = str(type);
  return Object.prototype.hasOwnProperty.call(EDGE_LABELS, t) ? EDGE_LABELS[t] : (t || 'linked').replace(/_/g, ' ');
}

/**
 * An ego response → neighbours grouped by edge type (groups in first-seen
 * order, neighbours by title). Each
 * item is the OTHER endpoint of an edge touching `focusId`, hydrated from
 * `ego.nodes` when the call asked for `include_nodes`.
 */
export function groupNeighbors(ego, focusId) {
  const groups = new Map();
  const nodes = (ego && ego.nodes && typeof ego.nodes === 'object') ? ego.nodes : {};
  const seen = new Set();
  for (const e of (ego && Array.isArray(ego.edges)) ? ego.edges : []) {
    if (!e || (e.src !== focusId && e.dst !== focusId)) continue;
    const out = e.src === focusId;
    const other = out ? e.dst : e.src;
    if (typeof other !== 'string' || !other || other === focusId) continue;
    const type = str(e.type) || 'linked';
    const key = `${type}\n${other}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const n = Object.prototype.hasOwnProperty.call(nodes, other) && nodes[other] ? nodes[other] : null;
    const item = {
      id: other,
      index: n && typeof n.index === 'string' ? n.index : null,
      title: (n && typeof n.title === 'string' && n.title) || (n && typeof n.path === 'string' && n.path) || other,
      path: n && typeof n.path === 'string' ? n.path : null,
      preview: n && typeof n.preview === 'string' ? n.preview : null,
      direction: out ? 'out' : 'in',
      hop: e.hop ?? 1,
      edgeId: e.edge_id,
      weight: e.weight,
      hydrated: !!n,
    };
    if (!groups.has(type)) groups.set(type, { type, label: edgeLabel(type), items: [] });
    groups.get(type).items.push(item);
  }
  // The engine returns edges in no fixed order (two live runs over the same
  // corpus listed the same two neighbours both ways round). Sort, so a record's
  // panel reads the same every time it is opened.
  const out = [...groups.values()];
  for (const g of out) g.items.sort((a, b) => String(a.title).localeCompare(String(b.title)) || String(a.id).localeCompare(String(b.id)));
  return out;
}

/**
 * The graph panel. `g` is the reader's graph state:
 *   { status: 'idle'|'loading'|'ok'|'no-brain'|'no-links'|'denied'|'error',
 *     brain, groups, notShown, error, index, dangling }
 */
export function renderGraphPanel(g = {}) {
  const brain = str(g.brain);
  const mono = (t) => h('span', { class: 'mono' }, t);
  // Every brain that lists this record's index was consulted (reader-api.js
  // #fetchGraph); the panel names all of them, so "no links" can be read as
  // "no links in any of these" and never hides a brain that was not asked.
  const brains = (Array.isArray(g.brains) ? g.brains.map(str).filter(Boolean) : []);
  const several = brains.length > 1;
  const brainsText = (cap) => (several
    ? [cap ? 'Brains ' : 'brains ', mono(brains.join(', '))]
    : [cap ? 'Brain ' : 'brain ', mono(brain || brains[0] || '')]);
  switch (g.status) {
    case 'loading':
      return h('div', { class: 'rd-empty' }, 'Walking ', ...brainsText(false), '…');
    case 'no-brain':
      return h('div', { class: 'rd-empty' },
        (brain || several) ? ['No ', ...brainsText(false), ' on this engine (no edges index), so there are no recorded links for this record.']
          : 'No brain is associated with this index, so there are no recorded links for this record.',
        g.guest ? null : [h('br'), 'Build one: ', h('span', { class: 'mono accent' }, 'xerj brain <folder>'), ' — the links appear here.']);
    case 'no-links':
      return h('div', { class: 'rd-empty' }, ...brainsText(true), several ? ' record no links for this record' : ' records no links for this record',
        g.dangling ? (several ? ' (its id is not a node in any of them).' : ' (its id is not a node in that brain).') : '.');
    case 'denied':
      return h('div', { class: 'rd-empty' },
        g.guest
          ? 'This share does not include the knowledge graph.'
          : ['The graph API refused this console session (', mono(str(g.error) || 'HTTP 401'), '). On an auth-enabled engine the graph needs an engine API key; records and search still work.']);
    case 'error':
      return h('div', { class: 'rd-empty' }, 'Graph unavailable: ', mono(str(g.error) || 'unknown error'));
    case 'ok': {
      const groups = Array.isArray(g.groups) ? g.groups : [];
      if (!groups.length) return renderGraphPanel({ ...g, status: 'no-links' });
      const total = groups.reduce((n, gr) => n + gr.items.length, 0);
      const ns = g.notShown || {};
      const clipped = (Number(ns.edges_clipped) || 0) + (Number(ns.frontier_clipped) || 0);
      const dangling = Number(ns.dangling_nodes) || 0;
      return [
        h('div', { class: 'rd-egroups' }, groups.map((gr) => h('div', { class: 'rd-egroup' },
          h('div', { class: 'key' }, `${str(gr.label).toUpperCase()} · ${gr.items.length}`),
          gr.items.map((it) => h('a', {
            class: 'rd-neigh',
            href: readerHref({ index: it.index || g.index, id: it.id, brain: it.brain || g.brain }),
            'data-reader-open': str(it.id),
            'data-reader-index': str(it.index || g.index),
            title: str(it.id),
          },
            h('span', { class: 'rd-neigh__dir mono' }, it.direction === 'out' ? '→' : '←'),
            it.via === 'file' ? h('span', { class: 'rd-neigh__via mono faint', title: 'a link of the file this record came from' }, 'FILE') : null,
            h('span', { class: 'rd-neigh__title' }, str(it.title)),
            it.preview ? h('span', { class: 'rd-neigh__prev' }, str(it.preview).replace(/\s+/g, ' ').slice(0, 90)) : null))))),
        h('div', { class: 'rd-honest mono faint' },
          `${total} linked record${total === 1 ? '' : 's'} · `,
          several
            ? `brains ${brains.join(', ')} consulted · links in ${(Array.isArray(g.brainsWithLinks) && g.brainsWithLinks.length ? g.brainsWithLinks : [brain]).map(str).join(', ')}`
            : `brain ${brain}`,
          ' · 1 hop',
          g.viaFile ? ` · ${g.viaFile} of them are links of the file this record came from (${str(g.filePath) || 'its file record'}) — file-level detectors link files, not the records inside them` : '',
          clipped ? ` · ${clipped} more not shown (limit)` : '',
          dangling ? ` · ${dangling} link${dangling === 1 ? '' : 's'} to ids with no document behind them` : ''),
      ];
    }
    default:
      return h('div', { class: 'rd-empty' }, 'Open a record to see what links to it.');
  }
}
