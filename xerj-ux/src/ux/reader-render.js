// ============================================================
// XERJ Console — Reader rendering (pure)
//
// One record in, HTML out. The reader opens ANY record `xerj autoindex` /
// `xerj brain` wrote and renders it by SHAPE, detected from the record's own
// fields (the autoindex extractors — engine/crates/xerj-autoindex/src/extract/
// — are the contract):
//
//   email        email_subject / email_from / email_to / email_cc / email_date /
//                email_message_id / email_in_reply_to / body   (extract/eml.rs)
//   attachment   attachment_name / attachment_content_type / attachment_bytes,
//                plus the parent's email_* headers and (for PDFs) page + body
//   pdf          page + body + title                          (extract/pdf.rs)
//   code-symbol  name / kind / line / language / path / code  (extract/code.rs)
//   code-file    language / defs / symbols[]                  (extract/code.rs)
//   note         title / body / ax_path  — markdown, docx, text, html
//   generic      anything else: the pretty-printed _source
//
// Plus the knowledge graph around it: `GET /_graph/{brain}/ego` edges grouped
// by edge type, each neighbour a link that opens in this same reader.
//
// Every string from `_source` passes through `esc` — a record is untrusted
// input (an .eml can carry `<script>` in its subject); nothing here is
// inserted as raw HTML. No DOM, no fetch: testable under node.
// ============================================================

import { esc } from './text.js';

// Vector / passage plumbing the reader never shows (it is huge and meaningless
// to a person); counted, never silently dropped.
const HIDDEN_FIELD = /(_vector|_vector_chunks|_chunks)$|^__xerj_passage_meta__/;

export const SHAPES = ['email', 'attachment', 'pdf', 'code-symbol', 'code-file', 'note', 'generic'];

/** Which renderer a record gets. Order matters: an attachment record also
 *  carries its parent's email headers, so it is tested first. */
export function detectShape(src) {
  const s = src || {};
  const has = (k) => s[k] != null && s[k] !== '';
  if (has('attachment_name')) return 'attachment';
  if (has('email_subject') || has('email_from') || has('email_message_id')) return 'email';
  if (typeof s.code === 'string' && has('name') && has('language')) return 'code-symbol';
  if (has('language') && (typeof s.defs === 'string' || Array.isArray(s.symbols))) return 'code-file';
  if (Number.isFinite(Number(s.page)) && has('page') && typeof s.body === 'string') return 'pdf';
  if (typeof s.body === 'string' || typeof s.text === 'string') return 'note';
  return 'generic';
}

/** A one-line title for a record, by shape. Never the raw id unless nothing
 *  better exists. */
export function recordTitle(src, id) {
  const s = src || {};
  switch (detectShape(s)) {
    case 'attachment':  return String(s.attachment_name);
    case 'email':       return String(s.email_subject || '(no subject)');
    case 'code-symbol': return `${s.kind ? s.kind + ' ' : ''}${s.name}`;
    case 'code-file':   return String(s.title || s.path || s.ax_path || id || '');
    case 'pdf':         return `${s.title || s.ax_path || 'document'} · p${s.page}`;
    case 'note':        return String(s.title || s.ax_path || s.path || id || '');
    default:            return String(s.title || s.name || id || '');
  }
}

/** Short, human badge for the result list. */
export function shapeBadge(shape, src) {
  const s = src || {};
  switch (shape) {
    case 'attachment':  return s.page != null ? `PDF · p${s.page}` : (String(s.attachment_content_type || 'attachment').split('/').pop());
    case 'email':       return 'EMAIL';
    case 'code-symbol': return String(s.kind || 'symbol').toUpperCase();
    case 'code-file':   return String(s.language || 'code').toUpperCase();
    case 'pdf':         return `PDF · p${s.page}`;
    case 'note':        return String(s.ax_format || 'note').toUpperCase();
    default:            return String(s.ax_format || 'record').toUpperCase();
  }
}

/** The reader deep link for a record. */
export function readerHref({ index, id, brain } = {}) {
  const qs = new URLSearchParams();
  if (index) qs.set('index', index);
  if (id) qs.set('id', id);
  if (brain) qs.set('brain', brain);
  return `#/reader?${qs.toString()}`;
}

function fromName(s) {
  const v = String(s || '');
  return v.replace(/\s*<[^>]*>\s*$/, '').trim() || v;
}
function shortDate(s) {
  const m = String(s || '').match(/(\d{4}-\d{2}-\d{2})/);
  return m ? m[1] : String(s || '').slice(0, 24);
}
function snippet(src) {
  const s = src || {};
  const t = s.body || s.text || s.code || s.defs || s.email_subject || '';
  return String(t).replace(/\s+/g, ' ').slice(0, 180);
}
function fmtBytes(b) {
  const n = Number(b);
  if (!Number.isFinite(n)) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1048576) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}

// ----- result list ---------------------------------------------------

/** One clickable result card. `sel` marks the open record. */
export function renderResultCard(hit, { selectedId, brain } = {}) {
  const s = (hit && hit._source) || {};
  const shape = detectShape(s);
  const sel = hit._id === selectedId ? ' sel' : '';
  const score = Number(hit._score);
  const meta = shape === 'email' || shape === 'attachment'
    ? `${esc(fromName(s.email_from))}${s.email_date ? ' · ' + esc(shortDate(s.email_date)) : ''}`
    : esc(s.ax_path || s.path || hit._index || '');
  return `<a class="rd-card${sel}" href="${esc(readerHref({ index: hit._index, id: hit._id, brain }))}" data-reader-open="${esc(hit._id)}">
    <div class="rd-card__top"><span class="rd-badge rd-badge--${esc(shape)}">${esc(shapeBadge(shape, s))}</span><span class="rd-card__title">${esc(recordTitle(s, hit._id))}</span>${Number.isFinite(score) ? `<span class="rd-card__score">${score.toFixed(2)}</span>` : ''}</div>
    <div class="rd-card__meta">${meta}</div>
    <div class="rd-card__snip">${esc(snippet(s))}</div>
  </a>`;
}

/** The whole list, or an honest empty state. */
export function renderResultList(hits, { selectedId, brain, emptyText } = {}) {
  if (!hits || !hits.length) return `<div class="rd-empty">${esc(emptyText || 'No records. Search above, or open one from Discover.')}</div>`;
  return `<div class="rd-list">${hits.map((h) => renderResultCard(h, { selectedId, brain })).join('')}</div>`;
}

// ----- the record ----------------------------------------------------

const hdr = (k, v) => (v == null || v === '' ? '' : `<div class="rd-hdr"><span class="rd-k">${esc(k)}</span><span class="rd-v">${esc(v)}</span></div>`);

function attachmentsBlock(attachments, brain) {
  if (!attachments) return `<div class="rd-sub faint">attachments: looking…</div>`;
  if (!attachments.length) return '';
  const rows = attachments.map((a) => {
    const s = a._source || {};
    const bits = [s.attachment_content_type, s.attachment_bytes != null ? fmtBytes(s.attachment_bytes) : null, s.page != null ? `page ${s.page}` : null]
      .filter(Boolean).map(esc).join(' · ');
    return `<a class="rd-att" href="${esc(readerHref({ index: a._index, id: a._id, brain }))}" data-reader-open="${esc(a._id)}"><span class="rd-att__name">${esc(s.attachment_name || a._id)}</span><span class="rd-att__meta">${bits}</span></a>`;
  }).join('');
  return `<div class="rd-sub"><div class="key">ATTACHMENTS · ${attachments.length}</div>${rows}</div>`;
}

function parentBlock(parent, brain) {
  if (!parent) return '';
  const s = parent._source || {};
  return `<div class="rd-sub"><div class="key">FROM EMAIL</div><a class="rd-att" href="${esc(readerHref({ index: parent._index, id: parent._id, brain }))}" data-reader-open="${esc(parent._id)}"><span class="rd-att__name">${esc(s.email_subject || '(no subject)')}</span><span class="rd-att__meta">${esc(fromName(s.email_from))}${s.email_date ? ' · ' + esc(shortDate(s.email_date)) : ''}</span></a></div>`;
}

function prose(text, fallback) {
  const t = text == null ? '' : String(text);
  return `<div class="rd-body">${esc(t || fallback || '(empty)')}</div>`;
}

function genericSource(src) {
  const shown = {};
  const hidden = [];
  for (const [k, v] of Object.entries(src || {})) {
    if (HIDDEN_FIELD.test(k)) hidden.push(k); else shown[k] = v;
  }
  const note = hidden.length ? `<div class="rd-sub faint mono">${hidden.length} vector/passage field${hidden.length === 1 ? '' : 's'} not shown: ${esc(hidden.join(', '))}</div>` : '';
  return `<pre class="rd-json mono">${esc(JSON.stringify(shown, null, 2))}</pre>${note}`;
}

/**
 * Render the open record. `ctx.related` carries the fetched attachments /
 * parent email (undefined = still loading, [] / null = none).
 */
export function renderRecord(hit, ctx = {}) {
  if (!hit) return `<div class="rd-empty">${esc(ctx.emptyText || 'Open a record from the list, from Discover, or from a graph node.')}</div>`;
  const s = hit._source || {};
  const shape = detectShape(s);
  const brain = ctx.brain;
  const related = ctx.related || {};
  const prov = `<div class="rd-prov mono faint">${esc(hit._index || '')} · id ${esc(hit._id || '')}${s.ax_path ? ' · ' + esc(s.ax_path) : ''}</div>`;

  switch (shape) {
    case 'email':
      return `<div class="rd-read" data-shape="email">
        <div class="rd-eyebrow">EMAIL</div>
        <h2 class="rd-title">${esc(s.email_subject || '(no subject)')}</h2>
        <div class="rd-hdrs">${hdr('From', s.email_from)}${hdr('To', s.email_to)}${hdr('Cc', s.email_cc)}${hdr('Date', s.email_date)}${hdr('Message-ID', s.email_message_id)}${hdr('In-Reply-To', s.email_in_reply_to)}</div>
        ${prose(s.body, '(no body text)')}
        ${attachmentsBlock(related.attachments, brain)}
        ${prov}
      </div>`;
    case 'attachment': {
      const kind = s.page != null ? `PDF ATTACHMENT · PAGE ${esc(s.page)}` : 'ATTACHMENT';
      return `<div class="rd-read" data-shape="attachment">
        <div class="rd-eyebrow">${kind}</div>
        <h2 class="rd-title">${esc(s.attachment_name)}</h2>
        <div class="rd-hdrs">${hdr('Type', s.attachment_content_type)}${hdr('Size', s.attachment_bytes != null ? fmtBytes(s.attachment_bytes) : '')}${hdr('Sender', fromName(s.email_from))}${hdr('Sent', s.email_date)}</div>
        ${typeof s.body === 'string' ? prose(s.body, '(no extracted text)') : `<div class="rd-body faint">No text was extracted from this attachment (${esc(s.attachment_content_type || 'binary')}).</div>`}
        ${parentBlock(related.parent, brain)}
        ${prov}
      </div>`;
    }
    case 'pdf':
      return `<div class="rd-read" data-shape="pdf">
        <div class="rd-eyebrow">PDF · PAGE ${esc(s.page)}</div>
        <h2 class="rd-title">${esc(s.title || s.ax_path || 'document')}</h2>
        ${prose(s.body, '(no extracted text on this page)')}
        ${prov}
      </div>`;
    case 'code-symbol':
      return `<div class="rd-read" data-shape="code-symbol">
        <div class="rd-eyebrow">${esc(String(s.language || 'code').toUpperCase())} · ${esc(String(s.kind || 'symbol').toUpperCase())}</div>
        <h2 class="rd-title mono">${esc(s.name)}</h2>
        <div class="rd-hdrs">${hdr('File', s.path || s.ax_path)}${hdr('Line', s.line)}</div>
        <pre class="rd-code mono">${esc(s.code)}</pre>
        ${prov}
      </div>`;
    case 'code-file': {
      const defs = typeof s.defs === 'string'
        ? s.defs.split('\n').filter(Boolean)
        : (Array.isArray(s.symbols) ? s.symbols.map((y) => `${y.kind || ''} ${y.name || ''}${y.line != null ? ' · L' + y.line : ''}`.trim()) : []);
      return `<div class="rd-read" data-shape="code-file">
        <div class="rd-eyebrow">${esc(String(s.language || 'code').toUpperCase())} · FILE</div>
        <h2 class="rd-title mono">${esc(s.title || s.path || s.ax_path || hit._id)}</h2>
        <div class="rd-hdrs">${hdr('Path', s.path || s.ax_path)}${hdr('Declarations', defs.length)}</div>
        ${defs.length ? `<ul class="rd-defs mono">${defs.map((d) => `<li>${esc(d)}</li>`).join('')}</ul>` : ''}
        ${typeof s.body === 'string' ? `<pre class="rd-code mono">${esc(s.body)}</pre>` : ''}
        ${prov}
      </div>`;
    }
    case 'note':
      return `<div class="rd-read" data-shape="note">
        <div class="rd-eyebrow">${esc(String(s.ax_format || 'NOTE').toUpperCase())}</div>
        <h2 class="rd-title">${esc(s.title || s.ax_path || s.path || hit._id)}</h2>
        ${prose(s.body != null ? s.body : s.text, '(empty)')}
        ${prov}
      </div>`;
    default:
      return `<div class="rd-read" data-shape="generic">
        <div class="rd-eyebrow">RECORD</div>
        <h2 class="rd-title">${esc(recordTitle(s, hit._id))}</h2>
        ${genericSource(s)}
        ${prov}
      </div>`;
  }
}

// ----- the graph around it --------------------------------------------

/** Human labels for the edge types autoindex's detectors emit (engine:
 *  xerj-autoindex/src/detect/*.rs), plus the email/thread types the brain
 *  may carry. Unknown types fall back to the raw name. */
export const EDGE_LABELS = {
  replies_to:    'replies to',
  attachment_of: 'attachment of',
  wikilink:      'wiki-links',
  mdlink:        'markdown links',
  href:          'hyperlinks',
  pathcite:      'cites the path of',
  cratecite:     'cites the crate of',
  sequence:      'next / previous section',
  same_dir:      'same folder',
  shared_term:   'shares rare terms with',
};

export function edgeLabel(type) {
  return EDGE_LABELS[type] || String(type || 'linked').replace(/_/g, ' ');
}

/**
 * An ego response → neighbours grouped by edge type, in edge order. Each
 * item is the OTHER endpoint of an edge touching `focusId`, hydrated from
 * `ego.nodes` when the call asked for `include_nodes`.
 */
export function groupNeighbors(ego, focusId) {
  const groups = new Map();
  const nodes = (ego && ego.nodes) || {};
  const seenPerType = new Map();
  for (const e of (ego && ego.edges) || []) {
    if (!e || (e.src !== focusId && e.dst !== focusId)) continue;
    const out = e.src === focusId;
    const other = out ? e.dst : e.src;
    if (!other || other === focusId) continue;
    const type = String(e.type || 'linked');
    const key = `${type} ${other}`;
    if (seenPerType.has(key)) continue;
    seenPerType.set(key, true);
    const n = nodes[other] || {};
    const item = {
      id: other,
      index: typeof n.index === 'string' ? n.index : null,
      title: (typeof n.title === 'string' && n.title) || (typeof n.path === 'string' && n.path) || other,
      path: typeof n.path === 'string' ? n.path : null,
      preview: typeof n.preview === 'string' ? n.preview : null,
      direction: out ? 'out' : 'in',
      hop: e.hop ?? 1,
      edgeId: e.edge_id,
      weight: e.weight,
      hydrated: !!nodes[other],
    };
    if (!groups.has(type)) groups.set(type, { type, label: edgeLabel(type), items: [] });
    groups.get(type).items.push(item);
  }
  return [...groups.values()];
}

/**
 * The graph panel. `g` is the reader's graph state:
 *   { status: 'idle'|'loading'|'ok'|'no-brain'|'no-links'|'error',
 *     brain, groups, notShown, error, index }
 */
export function renderGraphPanel(g = {}) {
  const brain = g.brain ? esc(g.brain) : '';
  switch (g.status) {
    case 'loading':
      return `<div class="rd-empty">Walking brain <span class="mono">${brain}</span>…</div>`;
    case 'no-brain':
      return `<div class="rd-empty">No brain named <span class="mono">${brain}</span> on this engine (no edges index), so there are no recorded links for this record.<br>Build one: <span class="mono accent">xerj brain &lt;folder&gt;</span> — the links appear here.</div>`;
    case 'no-links':
      return `<div class="rd-empty">Brain <span class="mono">${brain}</span> records no links for this record${g.dangling ? ' (its id is not a node in that brain)' : ''}.</div>`;
    case 'error':
      return `<div class="rd-empty">Graph unavailable: <span class="mono">${esc(g.error || 'unknown error')}</span></div>`;
    case 'ok': {
      const groups = g.groups || [];
      if (!groups.length) return renderGraphPanel({ ...g, status: 'no-links' });
      const total = groups.reduce((n, gr) => n + gr.items.length, 0);
      const cols = groups.map((gr) => `
        <div class="rd-egroup">
          <div class="key">${esc(gr.label.toUpperCase())} · ${gr.items.length}</div>
          ${gr.items.map((it) => `<a class="rd-neigh" href="${esc(readerHref({ index: it.index || g.index, id: it.id, brain: g.brain }))}" data-reader-open="${esc(it.id)}" title="${esc(it.id)}">
            <span class="rd-neigh__dir mono">${it.direction === 'out' ? '→' : '←'}</span>
            <span class="rd-neigh__title">${esc(it.title)}</span>
            ${it.preview ? `<span class="rd-neigh__prev">${esc(it.preview.slice(0, 90))}</span>` : ''}
          </a>`).join('')}
        </div>`).join('');
      const ns = g.notShown || {};
      const clipped = Number(ns.edges_clipped || 0) + Number(ns.frontier_clipped || 0);
      const honesty = `<div class="rd-honest mono faint">${total} linked record${total === 1 ? '' : 's'} · brain ${brain} · 1 hop${clipped ? ` · ${clipped} more not shown (limit)` : ''}${ns.dangling_nodes ? ` · ${esc(ns.dangling_nodes)} link${ns.dangling_nodes === 1 ? '' : 's'} to ids with no document behind them` : ''}</div>`;
      return `<div class="rd-egroups">${cols}</div>${honesty}`;
    }
    default:
      return `<div class="rd-empty">Open a record to see what links to it.</div>`;
  }
}
