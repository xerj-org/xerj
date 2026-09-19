// A small in-memory `_search` for the reader-api tests: bool filter / must_not
// over term · terms · exists · prefix · wildcard · ids, `sort` on one keyword field,
// `from` / `size`, `_source` includes — and, unlike a canned response, a
// `hits.total` that is the number of MATCHES, not the number returned. The
// review of PR #945 found a truncation bug that a transport answering
// `total = hits.length` could never show.
const asList = (v) => (Array.isArray(v) ? v : (v ? [v] : []));

function matches(doc, clause) {
  const s = doc._source || {};
  if (!clause || typeof clause !== 'object') return true;
  if (clause.match_all) return true;
  if (clause.ids) return asList(clause.ids.values).includes(doc._id);
  if (clause.term) return Object.entries(clause.term).every(([k, v]) => s[k] === (v && typeof v === 'object' ? v.value : v));
  if (clause.terms) return Object.entries(clause.terms).every(([k, v]) => asList(v).includes(s[k]));
  if (clause.exists) return s[clause.exists.field] != null && s[clause.exists.field] !== '';
  if (clause.prefix) return Object.entries(clause.prefix).every(([k, v]) => typeof s[k] === 'string' && s[k].startsWith(v && typeof v === 'object' ? v.value : v));
  if (clause.wildcard) {
    // keyword `wildcard`: `*` = any run, `?` = one character, over the WHOLE value
    return Object.entries(clause.wildcard).every(([k, v]) => {
      const pat = v && typeof v === 'object' ? v.value : v;
      const re = new RegExp(`^${String(pat).replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.')}$`, v && v.case_insensitive ? 'is' : 's');
      return typeof s[k] === 'string' && re.test(s[k]);
    });
  }
  if (clause.bool) {
    const b = clause.bool;
    return asList(b.filter).every((c) => matches(doc, c)) && asList(b.must).every((c) => matches(doc, c))
      && !asList(b.must_not).some((c) => matches(doc, c))
      && (!asList(b.should).length || !b.minimum_should_match || asList(b.should).some((c) => matches(doc, c)));
  }
  throw new Error(`mini-engine: unsupported clause ${JSON.stringify(clause)}`);
}

/** Answer one `_search` body over `docs`. */
export function miniSearch(docs, body = {}) {
  let hits = docs.filter((d) => matches(d, body.query || { match_all: {} }));
  const sort = asList(body.sort)[0];
  if (sort) {
    const [field, dir] = typeof sort === 'string' ? [sort, 'asc'] : Object.entries(sort)[0];
    const sign = (dir && typeof dir === 'object' ? dir.order : dir) === 'desc' ? -1 : 1;
    // keyword order: plain code-unit comparison, like the engine ("p10" < "p2")
    hits = [...hits].sort((a, b) => { const x = String(a._source[field] ?? ''), y = String(b._source[field] ?? ''); return sign * (x < y ? -1 : x > y ? 1 : 0); });
  }
  const total = hits.length;
  const from = Number(body.from) || 0;
  const size = body.size == null ? 10 : Number(body.size);
  const inc = Array.isArray(body._source) ? body._source : null;
  const page = hits.slice(from, from + size).map((h) => ({
    _index: h._index, _id: h._id, _score: 1,
    _source: inc ? Object.fromEntries(Object.entries(h._source).filter(([k]) => inc.includes(k))) : h._source,
  }));
  return { took: 1, hits: { total: { value: total, relation: 'eq' }, hits: page } };
}

/** A reader transport over `docs` that records every request body. */
export function miniTransport(docs, { guest = false, brain } = {}) {
  const calls = [];
  return {
    calls, guest, brain,
    async search(index, body) { calls.push(body); return miniSearch(docs.filter((d) => d._index === index), body); },
    async ego() { return { status: 200, body: { edges: [], nodes: {}, not_shown: {} } }; },
  };
}
