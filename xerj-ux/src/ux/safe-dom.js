// ============================================================
// XERJ Console — safe DOM construction
//
// The corpus home and the reader show OTHER PEOPLE'S DOCUMENTS: an email
// subject, an attachment filename, a PDF page, a highlight fragment, a graph
// node title. Every one of those strings is attacker-controlled (anyone can
// mail you `<img src=x onerror=…>` as a subject), and in guest mode the
// viewer's API key sits in sessionStorage on this origin — one script
// execution is key theft.
//
// The rest of the console builds HTML strings and relies on `esc()` at every
// interpolation. That is correct only while nobody ever forgets one. This
// module removes the HTML parser from the path instead:
//
//   h(tag, attrs, ...children)   a plain-data node. Children are nodes or
//                                STRINGS; a string is always text.
//   mount(container, nodes)      builds real DOM with createElement /
//                                createTextNode / setAttribute only.
//
// Guarantees, by construction rather than by discipline:
//   • no document string is ever parsed as HTML — this file, and everything
//     that renders through it, contains no markup-parsing sink at all
//     (test/safe-dom.test.mjs greps the sources for the full sink list);
//   • tags come from TAGS and attribute names from ATTRS — both closed
//     lists, and both are code constants, never data. An unknown tag or
//     attribute THROWS, so a typo (or someone "just adding onclick") fails
//     the tests instead of shipping;
//   • there is no `style`, no `src`, no `srcset`, no `action`, no `on*`;
//   • an `href` is kept only when it is an in-app hash route (`#/…`).
//     `javascript:`, `data:`, `vbscript:`, `//evil`, `https://…`, a leading
//     space or control character — all dropped, leaving an inert <a>. The
//     reader never links out: a URL found in a document is shown as text.
//
// `h` and the helpers are pure (no DOM), so render functions are testable
// under node; `mount` needs a Document and is exercised in a real browser
// (test/browser/).
// ============================================================

/** Elements a render function may create. Nothing that loads a resource,
 *  runs script, or submits anywhere. */
export const TAGS = new Set([
  'div', 'span', 'p', 'a', 'h1', 'h2', 'h3', 'h4', 'ul', 'ol', 'li',
  'pre', 'code', 'mark', 'article', 'section', 'header', 'footer', 'nav',
  'button', 'b', 'strong', 'em', 'small', 'dl', 'dt', 'dd', 'br', 'hr',
  'label', 'input', 'select', 'option', 'time',
]);

/** Attribute names a render function may set (plus `data-*` / `aria-*`). */
export const ATTRS = new Set([
  'class', 'href', 'title', 'id', 'role', 'tabindex', 'type', 'name',
  'value', 'placeholder', 'for', 'disabled', 'selected', 'autocomplete',
  'spellcheck', 'datetime', 'hidden',
]);

const INPUT_TYPES = new Set(['text', 'search']);
const BUTTON_TYPES = new Set(['button']);

/** Is this attribute name allowed? `on*` can never pass: it is not in ATTRS
 *  and does not start with `data-` / `aria-`. */
export function attrAllowed(name) {
  if (typeof name !== 'string') return false;
  if (ATTRS.has(name)) return true;
  return /^(data|aria)-[a-z0-9-]+$/.test(name);
}

function hasControlChar(s) {
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x20 || c === 0x7f) return true;
  }
  return false;
}

/**
 * The only hrefs the console renders from data: in-app hash routes.
 * Returns the href when safe, null otherwise.
 */
export function safeHref(v) {
  if (typeof v !== 'string') return null;
  // `#/` exactly at position 0 — no leading whitespace/control characters
  // (browsers strip those before resolving a scheme) and no scheme at all.
  if (!v.startsWith('#/')) return null;
  // A fragment cannot change the document's origin or scheme, but keep
  // control characters out of attributes on principle.
  if (hasControlChar(v)) return null;
  return v;
}

/** Build a node. `attrs` may be null. Falsy children are skipped; arrays are
 *  flattened; anything that is not a node becomes TEXT. */
export function h(tag, attrs, ...children) {
  if (!TAGS.has(tag)) throw new Error(`safe-dom: tag <${String(tag)}> is not allowed`);
  const a = {};
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (!attrAllowed(k)) throw new Error(`safe-dom: attribute "${k}" is not allowed`);
    // An href must already BE a string: coercing first would let an object
    // with a crafted toString() choose the URL.
    if (k === 'href' && typeof v !== 'string') continue;
    a[k] = v === true ? '' : String(v);
  }
  if (tag === 'input' && !INPUT_TYPES.has(a.type || 'text')) {
    throw new Error(`safe-dom: <input type="${a.type}"> is not allowed`);
  }
  if (tag === 'button') {
    if (a.type && !BUTTON_TYPES.has(a.type)) throw new Error(`safe-dom: <button type="${a.type}"> is not allowed`);
    a.type = 'button';
  }
  if ('href' in a) {
    const ok = safeHref(a.href);
    if (ok == null) delete a.href; else a.href = ok;
  }
  const c = [];
  const push = (x) => {
    if (x == null || x === false || x === true || x === '') return;
    if (Array.isArray(x)) { x.forEach(push); return; }
    if (isNode(x)) { c.push(x); return; }
    c.push(String(x));
  };
  children.forEach(push);
  return { $: 'node', tag, attrs: a, children: c };
}

export function isNode(x) {
  return !!x && typeof x === 'object' && x.$ === 'node' && typeof x.tag === 'string';
}

/** All text under a node (or list), concatenated — what a person reads. */
export function textOf(node) {
  if (node == null) return '';
  if (Array.isArray(node)) return node.map(textOf).join('');
  if (typeof node === 'string') return node;
  if (!isNode(node)) return '';
  return node.children.map(textOf).join('');
}

/** Depth-first visit of every element node. */
export function walk(node, fn) {
  if (Array.isArray(node)) { node.forEach((n) => walk(n, fn)); return; }
  if (!isNode(node)) return;
  fn(node);
  node.children.forEach((c) => walk(c, fn));
}

/** Every element node matching `pred`, in document order. */
export function findAll(node, pred) {
  const out = [];
  walk(node, (n) => { if (pred(n)) out.push(n); });
  return out;
}

// ----- highlights -----------------------------------------------------

// The reader asks the engine to wrap matches in these two private-use
// characters (U+E000 / U+E001) instead of `<em>`/`</em>`. A highlight
// fragment is raw document text with the markers spliced in — the engine does
// NOT HTML-escape it — so it must never reach an HTML parser. We split on the
// markers and emit text + <mark> elements. A document that itself contains
// the marker characters can at worst cause a stray <mark>; it cannot
// introduce markup.
export const HL_PRE = String.fromCharCode(0xe000);
export const HL_POST = String.fromCharCode(0xe001);

/** The `highlight` block to send with a search so fragments use the markers. */
export function highlightRequest(fields, { fragmentSize = 160, fragments = 2 } = {}) {
  const f = {};
  for (const name of fields || []) {
    if (typeof name === 'string' && name) f[name] = { fragment_size: fragmentSize, number_of_fragments: fragments };
  }
  // Both spellings, because the engine has two request parsers: the ES-compat
  // route (a guest) reads the plural arrays, the console's search proxy (an
  // operator) reads the singular strings and ignores the rest. Sending only
  // one silently falls back to `<em>` on the other route
  // (pinned by xerj-console-api/tests/reader_proxy.rs).
  return { pre_tags: [HL_PRE], post_tags: [HL_POST], pre_tag: HL_PRE, post_tag: HL_POST, fields: f };
}

function stripMarkers(s) {
  return s.split(HL_PRE).join('').split(HL_POST).join('');
}

/**
 * A highlight fragment → children for `h()`: plain strings and <mark> nodes.
 * `tags: 'em'` accepts the engine's default `<em>…</em>` wrapping for
 * responses we did not shape — the two tags are matched LITERALLY and
 * everything else, including any other `<…>`, stays text.
 */
export function highlightChildren(fragment, { tags = 'markers' } = {}) {
  const text = fragment == null ? '' : String(fragment);
  const [pre, post] = tags === 'em' ? ['<em>', '</em>'] : [HL_PRE, HL_POST];
  const out = [];
  let i = 0;
  while (i < text.length) {
    const s = text.indexOf(pre, i);
    if (s < 0) break;
    const e = text.indexOf(post, s + pre.length);
    if (e < 0) break;
    if (s > i) out.push(text.slice(i, s));
    const inner = stripMarkers(text.slice(s + pre.length, e));
    if (inner) out.push(h('mark', null, inner));
    i = e + post.length;
  }
  if (i < text.length) out.push(text.slice(i));
  return out.map((x) => (typeof x === 'string' ? stripMarkers(x) : x)).filter((x) => x !== '');
}

// ----- mount ----------------------------------------------------------

function build(node, doc) {
  if (typeof node === 'string') return doc.createTextNode(node);
  if (!isNode(node)) return doc.createTextNode('');
  // Re-validate at the DOM boundary: a node object that did not come from
  // h() (deserialised, hand-built) gets the same checks.
  if (!TAGS.has(node.tag)) throw new Error(`safe-dom: tag <${node.tag}> is not allowed`);
  const el = doc.createElement(node.tag);
  for (const [k, v] of Object.entries(node.attrs || {})) {
    if (!attrAllowed(k)) throw new Error(`safe-dom: attribute "${k}" is not allowed`);
    if (k === 'href') {
      const ok = safeHref(v);
      if (ok != null) el.setAttribute('href', ok);
      continue;
    }
    el.setAttribute(k, String(v));
  }
  for (const c of node.children || []) el.appendChild(build(c, doc));
  return el;
}

/**
 * Replace `container`'s children with the given node(s). The container is
 * emptied with replaceChildren — never by assigning markup.
 */
export function mount(container, nodes) {
  if (!container) return;
  const doc = container.ownerDocument;
  const frag = doc.createDocumentFragment();
  const list = Array.isArray(nodes) ? nodes : [nodes];
  for (const n of list) {
    if (n == null || n === false || n === '') continue;
    frag.appendChild(build(n, doc));
  }
  container.replaceChildren(frag);
}
