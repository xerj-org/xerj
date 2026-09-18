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

/** Inner query clause for a (q, type) pair over the given field roles. */
export function buildQueryClause(q, type, roles) {
  const textField = roles?.textField || 'body';
  const semanticField = roles?.semanticField || textField;
  const text = (q || '').trim();
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
    case 'prefix':   return text ? { prefix: { [textField]: text } } : { match_all: {} };
    case 'phrase':   return text ? { match_phrase: { [textField]: text } } : { match_all: {} };
    case 'semantic': return text ? { semantic: { field: semanticField, query: text, k: 10 } } : { match_all: {} };
    case 'hybrid':   return text ? {
      hybrid: {
        queries: [
          { query: { match: { [textField]: text } }, weight: 1.0 },
          { query: { semantic: { field: semanticField, query: text, k: 10 } }, weight: 0.8 },
        ],
        fusion: { type: 'rrf', k: 60 },
      },
    } : { match_all: {} };
    default:         return text ? { match: { [textField]: text } } : { match_all: {} };
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
