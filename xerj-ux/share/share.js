// ============================================================
// XERJ — share-link guest page
//
// The whole guest side of `xerj share`: read the share id from the URL
// fragment, exchange it plus a passcode for a read-only key, then give the
// guest a reading room over the one index that key can reach.
//
// THREE RULES THIS FILE KEEPS — the tests grep for the first and drive a real
// browser at the other two:
//
//   1. Document text is attacker-controlled. It is other people's email. It is
//      only ever put on the page as TEXT — `textContent` / `createTextNode` —
//      and never parsed as markup. There is no `innerHTML`, `outerHTML`,
//      `insertAdjacentHTML`, `document.write`, `eval` or `new Function` in
//      this file, and an HTML email body is shown as the text it is. URLs in
//      documents are not turned into links.
//   2. The share id lives in the URL *fragment*. A browser never sends a
//      fragment to a server, so it reaches no access log and no Referer. This
//      script posts it to the claim route and then removes it from the
//      address bar.
//   3. The session is ONE sessionStorage record, written exactly as
//        sessionStorage['xerj.share'] =
//          JSON.stringify({api_key, index, brain, expires_at, label})
//      — the console's guest mode reads the same record. sessionStorage, not
//      localStorage: it is scoped to this tab and gone when the tab closes.
//
// "Zero-token": search and reading. Nothing here generates text, summarises,
// or calls a model.
// ============================================================
'use strict';

(() => {
  const SHARE_KEY = 'xerj.share';
  const PAGE_SIZE = 10;
  // Highlight delimiters. Private-use code points: they cannot be typed into a
  // search box by accident and mean nothing as markup, so a snippet is split on
  // them and rebuilt from text nodes. The engine's default `<em>` tags would
  // arrive glued to unescaped document text — unusable without parsing HTML.
  const HL_PRE = '\uE000';
  const HL_POST = '\uE001';
  // Fields that usually hold the readable body, best first.
  const BODY_FIELDS = ['body', 'text', 'content', 'message', 'description', 'summary'];
  // Fields that usually name the document, best first.
  const TITLE_FIELDS = ['title', 'subject', 'email_subject', 'name', 'attachment_name', 'filename', 'file_name', 'path'];
  // Shown under a hit's title, in this order, when present.
  const META_FIELDS = ['email_from', 'from', 'author', 'email_date', 'date', 'attachment_name', 'ax_path', 'path', '_source_path'];
  const MAX_QUERY_FIELDS = 24;
  const MAX_INLINE_VALUE = 280;

  const $ = (id) => document.getElementById(id);
  const views = { claim: $('view-claim'), ended: $('view-ended'), room: $('view-room') };

  /** In-memory only. `session` mirrors the sessionStorage record. */
  const state = {
    shareId: null,
    session: null,
    plans: [],        // [{index, semanticField, fields}]
    query: '',
    terms: [],
    from: 0,
    total: 0,
    shown: 0,
    expiryTimer: null,
  };

  // ── small DOM helpers (text only) ────────────────────────────────────────

  function show(name) {
    for (const [key, el] of Object.entries(views)) el.hidden = key !== name;
  }

  function el(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = String(text);
    return node;
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  function setStatus(text, bad) {
    const node = $('room-status');
    node.textContent = text || '';
    node.classList.toggle('bad', !!bad);
  }

  // ── the session record ───────────────────────────────────────────────────

  function expiryMs(v) {
    if (v == null || v === '') return null;
    if (typeof v === 'number') return Number.isFinite(v) ? v : null;
    const t = Date.parse(String(v));
    return Number.isFinite(t) ? t : null;
  }

  function loadSession() {
    let raw = null;
    try { raw = sessionStorage.getItem(SHARE_KEY); } catch { return null; }
    if (!raw) return null;
    let obj;
    try { obj = JSON.parse(raw); } catch { return null; }
    if (!obj || typeof obj.api_key !== 'string' || !obj.api_key ||
        typeof obj.index !== 'string' || !obj.index) return null;
    const exp = expiryMs(obj.expires_at);
    if (exp != null && exp <= Date.now()) return null;
    return obj;
  }

  /** The contract, exactly: five keys, nothing else. */
  function saveSession(claim) {
    const record = {
      api_key: claim.api_key,
      index: claim.index,
      brain: claim.brain == null ? null : claim.brain,
      expires_at: claim.expires_at == null ? null : claim.expires_at,
      label: claim.label == null ? null : claim.label,
    };
    sessionStorage.setItem(SHARE_KEY, JSON.stringify(record));
    return record;
  }

  function dropSession() {
    try { sessionStorage.removeItem(SHARE_KEY); } catch { /* storage disabled */ }
    state.session = null;
    state.plans = [];
    if (state.expiryTimer) clearInterval(state.expiryTimer);
    state.expiryTimer = null;
  }

  // ── talking to the node ──────────────────────────────────────────────────

  /** Same-origin only, never cached, never with cookies, never with a Referer. */
  async function call(method, path, body, withKey) {
    const headers = { accept: 'application/json' };
    if (body !== undefined) headers['content-type'] = 'application/json';
    if (withKey && state.session) headers.authorization = `ApiKey ${state.session.api_key}`;
    const resp = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: 'no-store',
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      redirect: 'error',
    });
    let json = null;
    try { json = await resp.json(); } catch { /* not JSON */ }
    return { status: resp.status, json, retryAfter: resp.headers.get('retry-after') };
  }

  function encIndex(index) {
    return String(index).split(',').map((s) => encodeURIComponent(s.trim())).join(',');
  }

  // ── 1. claiming ──────────────────────────────────────────────────────────

  function readShareId() {
    const frag = (location.hash || '').replace(/^#/, '').trim();
    return /^[0-9a-f]{32}$/.test(frag) ? frag : null;
  }

  function ended(title, text, hint) {
    $('ended-title').textContent = title;
    $('ended-text').textContent = text;
    $('ended-hint').textContent = hint || '';
    show('ended');
  }

  async function onClaim(event) {
    event.preventDefault();
    const input = $('passcode');
    const button = $('claim-button');
    const error = $('claim-error');
    const passcode = input.value.trim();
    error.hidden = true;
    if (!passcode) {
      error.textContent = 'Type the passcode you were sent.';
      error.hidden = false;
      input.focus();
      return;
    }
    button.disabled = true;
    let result;
    try {
      result = await call('POST', `/_share/${encodeURIComponent(state.shareId)}/claim`, { passcode }, false);
    } catch {
      button.disabled = false;
      error.textContent = 'Could not reach the computer that shared this. It may be offline — ask the owner to check, then try again.';
      error.hidden = false;
      return;
    }
    button.disabled = false;
    const { status, json } = result;
    if (status === 200 && json && typeof json.api_key === 'string') {
      input.value = '';
      state.session = saveSession(json);
      // The id has done its job; take it out of the address bar.
      try { history.replaceState(null, '', location.pathname); } catch { /* ignore */ }
      state.shareId = null;
      enterRoom();
      return;
    }
    if (status === 401 || status === 403) {
      error.textContent = 'That passcode is not right. Check it and try again — a few wrong tries lock the link for a while.';
    } else if (status === 429) {
      const secs = Number(result.retryAfter);
      const wait = Number.isFinite(secs) && secs > 0
        ? (secs >= 120 ? `about ${Math.ceil(secs / 60)} minutes` : `${secs} seconds`)
        : 'a little while';
      error.textContent = `Too many tries. This link is locked for ${wait}.`;
    } else if (status === 410) {
      const reason = json && json.error && typeof json.error.reason === 'string' ? json.error.reason : '';
      ended(
        'This link is no longer active',
        reason.includes('used up') ? 'It has already been opened as many times as its owner allowed.'
          : reason.includes('revoked') ? 'Its owner has ended it.'
            : 'It has expired.',
        'Ask the person who sent it for a new one.',
      );
      return;
    } else if (status === 404) {
      ended('This link is not recognised', 'The link may be incomplete, or it belongs to a different computer.', 'Check that you copied the whole link, including everything after the # sign.');
      return;
    } else if (status === 409) {
      ended('Sharing is turned off on this computer', 'The owner is running it without authentication, so it will not hand out guest access.', 'Let the person who sent the link know.');
      return;
    } else {
      error.textContent = `Something went wrong (HTTP ${status}). Try again in a moment.`;
    }
    error.hidden = false;
    input.select();
  }

  // ── 2. the reading room ──────────────────────────────────────────────────

  function fmtRemaining(ms) {
    if (ms <= 0) return 'now';
    const s = Math.floor(ms / 1000);
    const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
    if (d > 0) return `in ${d}d ${h}h`;
    if (h > 0) return `in ${h}h ${m}m`;
    if (m > 0) return `in ${m}m`;
    return `in ${s}s`;
  }

  function tickExpiry() {
    const exp = expiryMs(state.session && state.session.expires_at);
    const node = $('room-expiry');
    if (exp == null) { node.textContent = 'read-only access'; return; }
    const left = exp - Date.now();
    if (left <= 0) {
      dropSession();
      ended('Your access has ended', 'This share has expired.', 'Ask the person who sent it for a new link if you still need it.');
      return;
    }
    const when = new Date(exp);
    node.textContent = `access ends ${fmtRemaining(left)} · ${when.toLocaleString()}`;
    node.classList.toggle('soon', left < 15 * 60 * 1000);
  }

  function accessEnded() {
    dropSession();
    ended('Your access has ended', 'The share expired, or its owner ended it.', 'Ask the person who sent it for a new link if you still need it.');
  }

  async function enterRoom() {
    const s = state.session;
    $('room-label').textContent = s.label || s.index;
    document.title = `${s.label || 'Shared documents'} · XERJ`;
    clear($('results'));
    $('more').hidden = true;
    $('doc-pane').hidden = true;
    $('results-pane').hidden = false;
    $('search-mode').textContent = '';
    setStatus('');
    show('room');
    tickExpiry();
    if (!state.session) return; // expired between claim and here
    state.expiryTimer = setInterval(tickExpiry, 20000);
    $('q').focus();
    await learnMappings();
    probeConsole();
  }

  /** Walk an ES mapping `properties` tree into [{name, type}]. */
  function flattenProps(props, prefix, out) {
    for (const [name, def] of Object.entries(props || {})) {
      if (!def || typeof def !== 'object') continue;
      const full = prefix ? `${prefix}.${name}` : name;
      if (def.properties) flattenProps(def.properties, full, out);
      else if (typeof def.type === 'string') out.push({ name: full, type: def.type });
    }
    return out;
  }

  /**
   * One search plan per shared index: which field (if any) is `semantic_text`
   * — that is what makes the `hybrid` query available — and which text fields
   * a plain `multi_match` should cover.
   */
  async function learnMappings() {
    const indices = String(state.session.index).split(',').map((x) => x.trim()).filter(Boolean);
    let mapping = null;
    try {
      const r = await call('GET', `/${encIndex(state.session.index)}/_mapping`, undefined, true);
      if (r.status === 401 || r.status === 403) return accessEnded();
      if (r.status === 200) mapping = r.json;
    } catch { /* fall through: search without a field list */ }
    state.plans = indices.map((index) => {
      const props = mapping && mapping[index] && mapping[index].mappings && mapping[index].mappings.properties;
      const flat = flattenProps(props, '', []).filter((f) => !f.name.endsWith('_vector'));
      const semantic = flat.find((f) => f.type === 'semantic_text');
      const texts = flat.filter((f) => f.type === 'text' || f.type === 'semantic_text').map((f) => f.name);
      const ordered = [
        ...TITLE_FIELDS.filter((f) => texts.includes(f)).map((f) => `${f}^2`),
        ...texts.filter((f) => !TITLE_FIELDS.includes(f)),
      ].slice(0, MAX_QUERY_FIELDS);
      return { index, semanticField: semantic ? semantic.name : null, fields: ordered };
    });
    const hybrid = state.plans.filter((p) => p.semanticField).length;
    $('search-mode').textContent = hybrid === 0
      ? 'Keyword search (BM25). Results are ranked by how well the words match.'
      : hybrid === state.plans.length
        ? 'Hybrid search: keyword matching (BM25) fused with vector similarity from the owner\'s embedder. No text is generated.'
        : 'Hybrid search where the index supports it, keyword search elsewhere. No text is generated.';
    return undefined;
  }

  /**
   * The keyword leg. `multi_match` over the text fields the mapping listed,
   * titles boosted. With no field list (the mapping could not be read) it is
   * `simple_query_string`, which needs none — this engine refuses a
   * `multi_match` without `fields`, and `simple_query_string` never throws on
   * what a person types.
   */
  function lexicalQuery(plan, q) {
    if (plan.fields.length) return { multi_match: { query: q, fields: plan.fields } };
    return { simple_query_string: { query: q } };
  }

  function queryFor(plan, q) {
    if (!plan.semanticField) return lexicalQuery(plan, q);
    return { hybrid: { queries: [
      { query: lexicalQuery(plan, q), weight: 1 },
      { query: { semantic: { field: plan.semanticField, query: q } }, weight: 1 },
    ] } };
  }

  /**
   * Named fields, never `*`: this engine's highlighter does not expand a field
   * pattern, so a wildcard comes back with no highlight at all. The names are
   * the ones being searched (boosts stripped); with no mapping, the usual
   * body and title names.
   */
  function highlightFields(plan) {
    const names = plan.fields.length
      ? plan.fields.map((f) => f.replace(/\^.*$/, ''))
      : [...BODY_FIELDS, ...TITLE_FIELDS];
    const fields = {};
    for (const name of names) fields[name] = {};
    return fields;
  }

  function searchBody(plan, q, from) {
    return {
      from,
      size: PAGE_SIZE,
      track_total_hits: true,
      query: queryFor(plan, q),
      _source: { excludes: ['*_vector'] },
      highlight: {
        pre_tags: [HL_PRE],
        post_tags: [HL_POST],
        fragment_size: 220,
        number_of_fragments: 1,
        fields: highlightFields(plan),
      },
    };
  }

  function termsOf(q) {
    return Array.from(new Set(
      q.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter((t) => t.length >= 2),
    )).slice(0, 12);
  }

  async function runSearch(q, from) {
    if (!state.session) return;
    if (!state.plans.length) await learnMappings();
    if (!state.session) return;
    setStatus('Searching…');
    const perIndex = [];
    try {
      // One request per shared index, each with the query its mapping
      // supports. Scores from different query types are not comparable, so
      // pages are interleaved by rank rather than merged by score.
      for (const plan of state.plans) {
        const r = await call('POST', `/${encodeURIComponent(plan.index)}/_search`, searchBody(plan, q, from), true);
        if (r.status === 401 || r.status === 403) return accessEnded();
        if (r.status !== 200 || !r.json || !r.json.hits) {
          const reason = r.json && r.json.error && r.json.error.reason;
          setStatus(`The search could not be run${reason ? `: ${String(reason).slice(0, 200)}` : ` (HTTP ${r.status})`}.`, true);
          return;
        }
        perIndex.push(r.json.hits);
      }
    } catch {
      setStatus('Could not reach the computer that shared this. It may be offline.', true);
      return;
    }
    const total = perIndex.reduce((n, h) => n + (h.total && typeof h.total.value === 'number' ? h.total.value : 0), 0);
    const lists = perIndex.map((h) => Array.isArray(h.hits) ? h.hits : []);
    const merged = [];
    for (let rank = 0; rank < PAGE_SIZE; rank += 1) {
      for (const list of lists) if (list[rank]) merged.push(list[rank]);
    }
    if (from === 0) { clear($('results')); state.shown = 0; }
    state.query = q;
    state.terms = termsOf(q);
    state.from = from;
    state.total = total;
    for (const hit of merged) $('results').appendChild(renderHit(hit));
    state.shown += merged.length;
    const anyFull = lists.some((l) => l.length === PAGE_SIZE);
    $('more').hidden = !(anyFull && state.shown < total);
    setStatus(total === 0 ? `No documents match “${q}”.` : countLine());
  }

  /**
   * "N matching documents" is only true of a keyword search. The vector leg of
   * a hybrid query scores every document it looks at, so there the count is
   * how many were ranked, not how many contain the words.
   */
  function countLine() {
    const n = state.total.toLocaleString();
    const noun = state.total === 1 ? 'document' : 'documents';
    const what = state.plans.some((p) => p.semanticField) ? `${n} ${noun} ranked, best first` : `${n} matching ${noun}`;
    return `${what} · showing ${state.shown.toLocaleString()}`;
  }

  function firstString(source, names) {
    for (const name of names) {
      const v = source[name];
      if (typeof v === 'string' && v.trim()) return v.trim();
    }
    return null;
  }

  function titleOf(hit) {
    return firstString(hit._source || {}, TITLE_FIELDS) || String(hit._id);
  }

  /** Append `text` to `parent`, wrapping sentinel-delimited runs in <mark>. */
  function appendHighlighted(parent, text) {
    let rest = String(text);
    for (;;) {
      const a = rest.indexOf(HL_PRE);
      if (a < 0) break;
      const b = rest.indexOf(HL_POST, a + 1);
      if (b < 0) break;
      if (a > 0) parent.appendChild(document.createTextNode(rest.slice(0, a)));
      parent.appendChild(el('mark', null, rest.slice(a + 1, b)));
      rest = rest.slice(b + 1);
    }
    // Whatever is left — including a stray, unpaired delimiter — is text.
    const tail = rest.split(HL_PRE).join('').split(HL_POST).join('');
    if (tail) parent.appendChild(document.createTextNode(tail));
  }

  /** Append `text`, marking whole-word-prefix occurrences of the query terms. */
  function appendWithTerms(parent, text, terms) {
    const value = String(text);
    if (!terms.length) { parent.appendChild(document.createTextNode(value)); return; }
    const escaped = terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
    const re = new RegExp(`(${escaped.join('|')})`, 'giu');
    let last = 0;
    let marks = 0;
    for (const m of value.matchAll(re)) {
      if (marks >= 2000) break; // a pathological document must not freeze the tab
      if (m.index > last) parent.appendChild(document.createTextNode(value.slice(last, m.index)));
      parent.appendChild(el('mark', null, m[0]));
      last = m.index + m[0].length;
      marks += 1;
    }
    if (last < value.length) parent.appendChild(document.createTextNode(value.slice(last)));
  }

  /** `size` characters of `text` around the first occurrence of any term. */
  function windowAround(text, terms, size) {
    const value = String(text).replace(/\s+/g, ' ').trim();
    if (value.length <= size) return value;
    const lower = value.toLowerCase();
    let at = -1;
    for (const t of terms) {
      const i = lower.indexOf(t);
      if (i >= 0 && (at < 0 || i < at)) at = i;
    }
    const start = at < 0 ? 0 : Math.max(0, at - Math.floor(size / 3));
    const end = Math.min(value.length, start + size);
    return `${start > 0 ? '…' : ''}${value.slice(start, end)}${end < value.length ? '…' : ''}`;
  }

  function renderHit(hit) {
    const source = hit._source || {};
    const li = el('li');
    const button = el('button', 'hit');
    button.type = 'button';
    button.appendChild(el('span', 'hit-title', titleOf(hit)));
    const meta = META_FIELDS
      .map((f) => (typeof source[f] === 'string' || typeof source[f] === 'number') ? String(source[f]) : null)
      .filter(Boolean)
      .slice(0, 3);
    if (meta.length) button.appendChild(el('span', 'hit-meta', meta.join(' · ').slice(0, 300)));
    const snippet = el('span', 'hit-snippet');
    // A fragment of the body says more than a fragment of the title, which is
    // already on the line above — so body-like fields first.
    const hl = hit.highlight && typeof hit.highlight === 'object' ? hit.highlight : {};
    const order = [...BODY_FIELDS.filter((f) => f in hl), ...Object.keys(hl).filter((f) => !BODY_FIELDS.includes(f) && !TITLE_FIELDS.includes(f)), ...TITLE_FIELDS.filter((f) => f in hl)];
    const fragments = order.flatMap((f) => (Array.isArray(hl[f]) ? hl[f] : [])).filter((f) => typeof f === 'string');
    if (fragments.length) {
      appendHighlighted(snippet, fragments[0].slice(0, 600));
    } else {
      // No highlight came back (the hybrid query returns none): cut a window
      // around the first query term instead of always showing the opening.
      const body = firstString(source, BODY_FIELDS);
      if (body) appendWithTerms(snippet, windowAround(body, state.terms, 280), state.terms);
    }
    if (snippet.firstChild) button.appendChild(snippet);
    // The vector leg of a hybrid query ranks documents that share no word with
    // the query. Say so on the hit, rather than let a reader hunt for a word
    // that is not there.
    const plan = state.plans.find((p) => p.index === hit._index);
    if (plan && plan.semanticField && state.terms.length) {
      const haystack = `${titleOf(hit)} ${firstString(source, BODY_FIELDS) || ''}`.toLowerCase();
      if (!state.terms.some((t) => haystack.includes(t))) {
        button.appendChild(el('span', 'hit-note', 'Does not contain your words — ranked here by vector similarity.'));
      }
    }
    button.addEventListener('click', () => openDoc(hit._index, hit._id));
    li.appendChild(button);
    return li;
  }

  // ── the document view ────────────────────────────────────────────────────

  function isVectorish(name, value) {
    return name.endsWith('_vector') ||
      (Array.isArray(value) && value.length > 16 && value.every((x) => typeof x === 'number'));
  }

  function asText(value) {
    if (value == null) return '';
    if (typeof value === 'string') return value;
    if (typeof value === 'number' || typeof value === 'boolean') return String(value);
    if (Array.isArray(value) && value.every((x) => x == null || typeof x !== 'object')) return value.join(', ');
    try { return JSON.stringify(value, null, 2); } catch { return ''; }
  }

  async function openDoc(index, id) {
    setStatus('Opening…');
    let r;
    try {
      r = await call('GET', `/${encodeURIComponent(index)}/_doc/${encodeURIComponent(id)}?_source_excludes=*_vector`, undefined, true);
    } catch {
      setStatus('Could not reach the computer that shared this. It may be offline.', true);
      return;
    }
    if (r.status === 401 || r.status === 403) return accessEnded();
    if (r.status !== 200 || !r.json || !r.json._source) {
      setStatus(`That document could not be opened (HTTP ${r.status}).`, true);
      return;
    }
    const source = r.json._source;
    $('doc-title').textContent = firstString(source, TITLE_FIELDS) || String(id);

    const meta = $('doc-meta');
    const bodyPane = $('doc-body');
    clear(meta);
    clear(bodyPane);
    const bodyName = BODY_FIELDS.find((f) => typeof source[f] === 'string' && source[f].trim());
    const long = [];
    for (const [name, value] of Object.entries(source)) {
      if (name === bodyName || isVectorish(name, value)) continue;
      const text = asText(value);
      if (!text.trim()) continue;
      if (text.length > MAX_INLINE_VALUE || text.includes('\n')) { long.push([name, text]); continue; }
      meta.appendChild(el('dt', null, name));
      meta.appendChild(el('dd', null, text));
    }
    meta.hidden = !meta.firstChild;
    if (bodyName) {
      const block = el('p', 'doc-text');
      appendWithTerms(block, source[bodyName], state.terms);
      bodyPane.appendChild(block);
    }
    for (const [name, text] of long) {
      bodyPane.appendChild(el('p', 'doc-field-name', name));
      const block = el('p', 'doc-text');
      appendWithTerms(block, text, state.terms);
      bodyPane.appendChild(block);
    }
    if (!bodyPane.firstChild && !meta.firstChild) bodyPane.appendChild(el('p', 'hint', 'This record has no readable fields.'));

    $('results-pane').hidden = true;
    $('doc-pane').hidden = false;
    setStatus('');
    $('doc-back').focus();
    window.scrollTo(0, 0);
    return undefined;
  }

  function closeDoc() {
    $('doc-pane').hidden = true;
    $('results-pane').hidden = false;
    if (state.total) setStatus(countLine());
  }

  // ── sign-out, console hand-off ───────────────────────────────────────────

  function signOut() {
    dropSession();
    clear($('results'));
    clear($('doc-meta'));
    clear($('doc-body'));
    $('doc-title').textContent = '';
    $('q').value = '';
    ended(
      'Signed out',
      'This browser no longer holds your access.',
      'To read again, open the link you were sent. If it was set to open only once, ask its owner for a new one.',
    );
  }

  /** The console's guest mode ships separately; only offer it where it exists. */
  async function probeConsole() {
    try {
      const r = await fetch('/_xerj-console/src/data/guest.js', { method: 'HEAD', cache: 'no-store', credentials: 'omit', referrerPolicy: 'no-referrer' });
      if (!r.ok || !state.session) return;
      const s = state.session;
      const first = String(s.index).split(',')[0];
      $('console-href').href = `/_xerj-console/#/reader?index=${encodeURIComponent(first)}${s.brain ? `&brain=${encodeURIComponent(s.brain)}` : ''}`;
      $('console-link').hidden = false;
    } catch { /* no console guest mode on this node */ }
  }

  // ── boot ─────────────────────────────────────────────────────────────────

  function boot() {
    $('claim-form').addEventListener('submit', onClaim);
    $('search-form').addEventListener('submit', (event) => {
      event.preventDefault();
      const q = $('q').value.trim();
      if (!q) { setStatus('Type something to search for.'); return; }
      closeDoc();
      runSearch(q, 0);
    });
    $('more').addEventListener('click', () => runSearch(state.query, state.from + PAGE_SIZE));
    $('doc-back').addEventListener('click', closeDoc);
    $('signout').addEventListener('click', signOut);

    state.shareId = readShareId();
    const existing = loadSession();
    if (existing && !state.shareId) {
      state.session = existing;
      enterRoom();
      return;
    }
    if (state.shareId) {
      // A fresh link always wins over a stale session from another share.
      dropSession();
      show('claim');
      $('passcode').focus();
      return;
    }
    ended(
      'This link is incomplete',
      'A share link ends with a # sign followed by a long code. That part is missing here.',
      'Copy the whole link from the message you were sent and open it again.',
    );
  }

  boot();
})();
