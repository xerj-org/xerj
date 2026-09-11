// ============================================================
// Dashboard — CASE REVIEW  (email + document review)
//
// Auto-activates (registry `requiresLive: 'email-corpus'`) once the engine
// holds an index of parsed emails — i.e. someone ran
// `xerj autoindex <folder-of-.eml>`. Instead of the generic Discover JSON
// table, this reads the way a person does: search by meaning, results as
// email / attachment CARDS, and a reading pane that shows the actual email
// (from / to / subject / body) or the extracted text of a PDF attachment —
// all from each hit's `_source`, no extra fetch.
//
// The searchbox is preset to SEMANTIC over the email index, and the results
// are rendered by this dashboard rather than the shared Hits table.
// ============================================================

import { SearchBox } from '../ux/charts-ops.js';

const QUERY_TYPES = ['semantic', 'match', 'phrase'];

function src(h) { return h?._source || {}; }
function isPdf(h) { return !!src(h).attachment_name; }
function fromName(s) { return String(s || '').replace(/\s*<[^>]*>\s*$/, '').trim() || String(s || ''); }
function shortDate(s) {
  const m = String(s || '').match(/(\d{4}-\d{2}-\d{2})/);
  if (m) return m[1];
  return String(s || '').replace(/\s*[-+]\d{4}.*$/, '').replace(/^[A-Za-z]{3},\s*/, '').trim();
}
function esc(s) { return String(s == null ? '' : s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c])); }
function snippet(h) {
  const s = src(h);
  const t = s.body || s.email_subject || '';
  return String(t).replace(/\s+/g, ' ').slice(0, 180);
}

// One result card — an email or a PDF attachment hit.
function card(h, selectedId) {
  const s = src(h);
  const pdf = isPdf(h);
  const title = pdf ? s.attachment_name : (s.email_subject || s.title || '(no subject)');
  const badge = pdf
    ? `<span class="cr-badge pdf">PDF · p${esc(s.page ?? 1)}</span>`
    : `<span class="cr-badge eml">Email</span>`;
  const sel = h._id === selectedId ? ' sel' : '';
  return `<div class="cr-card${sel}" data-review-open="${esc(h._id)}">
    <div class="cr-card__top">${badge}<span class="cr-card__subj">${esc(title)}</span><span class="cr-card__score">${(h._score ?? 0).toFixed ? (h._score).toFixed(2) : h._score}</span></div>
    <div class="cr-card__meta">${esc(fromName(s.email_from))}${s.email_date ? ' &middot; ' + esc(shortDate(s.email_date)) : ''}</div>
    <div class="cr-card__snip">${esc(snippet(h))}&hellip;</div>
  </div>`;
}

// The reading pane — the selected email or PDF page, rendered like a real reader.
function reader(hit) {
  if (!hit) {
    return `<div class="cr-empty">Search, then open an email or attachment to read it here.</div>`;
  }
  const s = src(hit);
  const hdr = (k, v) => v ? `<div class="cr-hdr"><span class="cr-k">${k}</span><span class="cr-v">${esc(v)}</span></div>` : '';
  if (isPdf(hit)) {
    return `<div class="cr-read">
      <div class="cr-doc-eyebrow">PDF ATTACHMENT &middot; PAGE ${esc(s.page ?? 1)}</div>
      <h2 class="cr-subj">${esc(s.attachment_name)}</h2>
      <div class="cr-hdrs">
        ${hdr('From email', s.email_subject)}
        ${hdr('Sender', fromName(s.email_from))}
        ${hdr('Type', s.attachment_content_type || 'application/pdf')}
      </div>
      <div class="cr-body">${esc(s.body || '(no extracted text on this page)')}</div>
    </div>`;
  }
  return `<div class="cr-read">
    <h2 class="cr-subj">${esc(s.email_subject || '(no subject)')}</h2>
    <div class="cr-hdrs">
      ${hdr('From', s.email_from)}
      ${hdr('To', s.email_to)}
      ${hdr('Cc', s.email_cc)}
      ${hdr('Date', s.email_date)}
    </div>
    <div class="cr-body">${esc(s.body || '(no body)')}</div>
  </div>`;
}

export const caseReview = {
  id:   'case-review',
  name: 'Case Review',
  // Preset the shared search state the first time this dashboard is opened.
  preset: { type: 'semantic', index: 'ai-kb' },
  render: ({ search }) => {
    const r = search?.result;
    const hits = r?.hits || [];
    const selectedId = search?.selectedId;
    const selected = hits.find(h => h._id === selectedId) || null;

    return {
      title:  'CASE REVIEW',
      kicker: 'EMAIL + DOCUMENT REVIEW',
      meta:   ['SEMANTIC', 'EML + PDF'],
      caption: 'Ask the case files in plain English. Results are emails and their attachments — open one to read the message or the document. Meaning-based ranking over the indexed inbox.',
      panels: [
        { id: 'searchbox', eyebrow: 'ASK THE CASE FILES', cols: 12, type: 'searchbox',
          render: () => SearchBox({
            value: search?.q ?? '',
            types: QUERY_TYPES,
            activeType: search?.type ?? 'semantic',
            indices: ['ai-kb'],
            activeIndex: search?.index ?? 'ai-kb',
            filters: {},
          }),
        },
        { id: 'cr-results', eyebrow: `${hits.length} RESULTS · RANKED BY MEANING`, cols: 5, type: 'markdown',
          render: () => hits.length
            ? `<div class="cr-list">${hits.map(h => card(h, selectedId)).join('')}</div>`
            : `<div class="cr-empty">Type a question above and press Enter.</div>`,
        },
        { id: 'cr-reader', eyebrow: 'READER', cols: 7, type: 'markdown',
          render: () => reader(selected),
        },
      ],
    };
  },
};
