// ============================================================
// XERJ Console — the ONE search-body builder
//
// Discover previews a request and the xerj backend executes one. PR #923
// review finding 8: they were built by two different functions and drifted
// (the preview emitted a `hybrid` shape the engine does not accept). This
// module is the single source of truth — the preview panel and
// `backends/xerj.js` both call `buildSearchBody`, so what you see is byte-
// for-byte what runs.
//
// Pure: no DOM, no fetch. Field roles come from `schema.js#deriveRoles`
// (the real mapping) so nothing here is hardcoded to `message`/`embedding`.
// ============================================================

/** Query types the console exposes. `knn` is deliberately absent: a raw kNN
 *  needs a query vector the console cannot produce, and a preview that runs
 *  `match_all` while claiming `knn` is exactly the mismatch #923 was faulted
 *  for. `semantic` is the vector path — the engine embeds the query. */
export const QUERY_TYPES = ['match', 'term', 'range', 'prefix', 'phrase', 'semantic', 'hybrid'];

/** How many facet fields to aggregate on (keyword fields from the mapping). */
export const FACET_FIELDS = 3;

/**
 * The fields a MATCH / PHRASE / PREFIX runs over: `roles.searchFields`
 * (schema-roles.js — every text-typed field plus the subject / title /
 * attachment-name fields the mapping has), or just the text field.
 */
export function searchFieldsOf(roles) {
  const textField = roles?.textField || 'body';
  const list = Array.isArray(roles?.searchFields) ? roles.searchFields.filter((f) => typeof f === 'string' && f) : [];
  return list.length ? list : [textField];
}

/**
 * One clause per search field, OR-ed. A single field keeps the plain clause.
 *
 * Why `bool.should` and not `multi_match`: measured on a live node
 * (2026-09-18, the console-reader corpus), `multi_match` with the default
 * `best_fields` type returns the right hits but NO highlight fragments, while
 * `bool.should` of per-field clauses returns the same hits with a fragment
 * per matching field — and the Reader's result cards are built from those
 * fragments.
 */
function overFields(fields, make) {
  if (fields.length === 1) return make(fields[0]);
  return { bool: { should: fields.map(make), minimum_should_match: 1 } };
}

/** The words of a query, the way a text analyzer would cut them: runs of
 *  letters / digits. Used for the keyword-field clause below. */
export function queryWords(text) {
  return String(text || '').split(/[^\p{L}\p{N}]+/u).filter(Boolean);
}

/** `text` as the inside of a wildcard pattern: `*`, `?` and `\` each become
 *  `?` (any ONE character — which the character itself always satisfies), so
 *  nothing a person types can widen the pattern. */
const wildcardLiteral = (text) => String(text).replace(/[*?\\]/g, '?');
const contains = (f, pattern) => ({ wildcard: { [f]: { value: pattern, case_insensitive: true } } });

/**
 * MATCH / PHRASE / PREFIX on a KEYWORD-typed search field.
 *
 * A keyword value is one term, so `match` on it needs the WHOLE string:
 * measured on a live node (2026-09-19, PR #945 review repro corpus, where
 * autoindex typed `email_subject`, `title` and `attachment_name` as keyword)
 * `{"match":{"email_subject":"Lunch"}}` → 0 hits while the subject "Lunch on
 * Friday?" exists, and `match_phrase` "Lunch on" → 0. A case-insensitive
 * `wildcard` finds it: `*lunch*` → 1, `*lunch on*` → 1, `lunch*` → 1, and the
 * engine returns a highlight fragment for it. (`prefix` with
 * `case_insensitive` returned 0 on the same node, hence wildcard there too.)
 *
 *   match   one `*word*` per word, OR-ed like `match`'s default operator;
 *           a record matching more words scores higher
 *   phrase  `*the words as typed*`
 *   prefix  `as typed*`
 *
 * This is a substring test, not token matching: `on` also finds "Duplication".
 */
function keywordClause(kind, f, text) {
  if (kind === 'prefix') return contains(f, `${wildcardLiteral(text)}*`);
  if (kind === 'phrase') return contains(f, `*${wildcardLiteral(text)}*`);
  const words = queryWords(text);
  if (words.length <= 1) return contains(f, `*${words.length ? words[0] : wildcardLiteral(text)}*`);
  return { bool: { should: words.map((w) => contains(f, `*${w}*`)), minimum_should_match: 1 } };
}

const TEXT_CLAUSE = { match: 'match', phrase: 'match_phrase', prefix: 'prefix' };

/** The clause for one (kind, field): text fields get the analyzed query,
 *  keyword fields the contains-the-word clause. */
function fieldClause(kind, f, text, roles) {
  const isKeyword = Array.isArray(roles?.keywordSearchFields) && roles.keywordSearchFields.includes(f);
  const clause = isKeyword ? keywordClause(kind, f, text) : { [TEXT_CLAUSE[kind]]: { [f]: text } };
  // Every attachment record carries a COPY of its parent's subject
  // (extract/eml.rs#attach_link), so a subject word would otherwise return the
  // email plus one hit per page of every PDF attached to it — 303 equal-scored
  // hits for one email with a 300-page attachment on the review corpus, the
  // email itself anywhere among them. The subject clause therefore matches the
  // message's own records; an attachment is found by its name and its text.
  if (f === 'email_subject' && roles?.hasAttachments) {
    return { bool: { must: [clause], must_not: [{ exists: { field: 'attachment_name' } }] } };
  }
  return clause;
}

/**
 * Why a (q, type) pair cannot run, or null. TERM and RANGE need
 * `field=value` / `field>=value`; free text used to fall through to
 * `match_all` and present every record as a "result" (PR #945 review).
 * `buildQueryClause` still answers `match_all` for it — a caller that shows
 * results (the Reader) asks this first and shows the message instead.
 */
export function queryProblem(q, type) {
  const text = (q || '').trim();
  if (!text) return null;
  if (type === 'term' && !/^([\w.]+)\s*=\s*(.+)$/.test(text)) return 'TERM needs field=value — for example ax_format=eml. Nothing was searched.';
  if (type === 'range' && !/^([\w.]+)\s*(>=|<=|>|<)\s*(.+)$/.test(text)) return 'RANGE needs field>=value (or >, <=, <) — for example page>=2. Nothing was searched.';
  return null;
}

/** Search-box placeholder for a query type (the syntax, where there is one). */
export function queryPlaceholder(type) {
  if (type === 'term') return 'field=value · for example ax_format=eml · press Enter';
  if (type === 'range') return 'field>=value · for example page>=2 · press Enter';
  return 'search this corpus · press Enter · empty lists records';
}

/** Inner query clause for a (q, type) pair over the given field roles. */
export function buildQueryClause(q, type, roles) {
  const textField = roles?.textField || 'body';
  const semanticField = roles?.semanticField || textField;
  const fields = searchFieldsOf(roles);
  const text = (q || '').trim();
  const over = (kind) => overFields(fields, (f) => fieldClause(kind, f, text, roles));
  switch (type) {
    case 'term': {
      const m = text.match(/^([\w.]+)\s*=\s*(.+)$/);
      return m ? { term: { [m[1]]: m[2].trim() } } : { match_all: {} };
    }
    case 'range': {
      const m = text.match(/^([\w.]+)\s*(>=|<=|>|<)\s*(.+)$/);
      if (!m) return { match_all: {} };
      const [, f, op, raw] = m;
      const k = op === '>=' ? 'gte' : op === '<=' ? 'lte' : op === '>' ? 'gt' : 'lt';
      const v = raw.trim();
      const n = Number(v);
      return { range: { [f]: { [k]: Number.isFinite(n) && v !== '' ? n : v } } };
    }
    case 'prefix':   return text ? over('prefix') : { match_all: {} };
    case 'phrase':   return text ? over('phrase') : { match_all: {} };
    case 'semantic': return text ? { semantic: { field: semanticField, query: text, k: 10 } } : { match_all: {} };
    case 'hybrid':   return text ? {
      hybrid: {
        queries: [
          { query: over('match'), weight: 1.0 },
          { query: { semantic: { field: semanticField, query: text, k: 10 } }, weight: 0.8 },
        ],
        fusion: { type: 'rrf', k: 60 },
      },
    } : { match_all: {} };
    default:         return text ? over('match') : { match_all: {} };
  }
}

/**
 * The full request body: query (+ filters), size, facet aggs on the derived
 * keyword fields, a date_histogram on the derived date field, and a sort when
 * the user picked a real field.
 *
 *   q, type   — SearchBox state
 *   filters   — { field: value } click-to-filter terms
 *   roles     — schema.js#deriveRoles output
 *   opts.size — hits to return (default 25)
 *   opts.from — offset of the first hit (omitted when 0)
 *   opts.aggs — false to omit the facet / histogram aggregations
 *   opts.sort — { field, dir }; `_score` / `_ts` / `_index` / `_id` are
 *               display sorts the engine cannot order by, so they are
 *               omitted (score order is the default).
 */
export function buildSearchBody(q, type, filters, roles, opts = {}) {
  const inner = buildQueryClause(q, type, roles);
  const filterList = Object.entries(filters || {})
    .filter(([, v]) => v != null && v !== '')
    .map(([f, v]) => (Array.isArray(v)
      ? { terms: { [f]: v } }
      : { term: { [f]: v } }));
  // Filters ride INSIDE each leg of a hybrid query. Measured on a live node:
  // `bool{must: hybrid, filter}` is accepted and returns 0 hits, and a
  // top-level `post_filter` beside `hybrid` is accepted and ignored — both
  // wrong without an error — while a filter in each leg filters (17 of 24
  // hits for ax_format=eml in the recorded corpus).
  let query;
  if (!filterList.length) query = inner;
  else if (isHybrid(inner)) {
    query = { hybrid: { ...inner.hybrid, queries: inner.hybrid.queries.map((leg) => ({ ...leg, query: { bool: { must: leg.query, filter: filterList } } })) } };
  } else query = { bool: { must: inner, filter: filterList } };

  const body = { query, size: opts.size ?? 25, track_total_hits: true };
  if (Number(opts.from) > 0) body.from = Number(opts.from);
  // The reader asks for hits only (`opts.aggs === false`); Discover also
  // wants its facet sidebar and histogram — except under HYBRID: the engine
  // refuses aggregations beside a fusion query ("aggregations are not
  // supported with hybrid/fusion queries", HTTP 400, seen on a live node), so
  // sending them would turn every hybrid search into an error.
  if (opts.aggs !== false && !isHybrid(inner)) {
    const aggs = { by__index: { terms: { field: '_index', size: 8 } } };
    for (const f of (roles?.keywordFields || []).slice(0, FACET_FIELDS)) {
      aggs[`by_${f}`] = { terms: { field: f, size: 8 } };
    }
    if (roles?.dateField) {
      aggs.by_date = { date_histogram: { field: roles.dateField, calendar_interval: 'day' } };
    }
    body.aggs = aggs;
  }
  const sort = opts.sort;
  if (sort && sort.field && !sort.field.startsWith('_')) {
    body.sort = [{ [sort.field]: sort.dir === 'asc' ? 'asc' : 'desc' }];
  }
  return body;
}

/** Is this clause a fusion query (no aggregations may ride along)? */
export function isHybrid(clause) {
  return !!(clause && typeof clause === 'object' && clause.hybrid);
}

/** The request line shown above the preview — the path the backend hits. */
export function requestPath(index) {
  return `/${index || '*'}/_search`;
}
