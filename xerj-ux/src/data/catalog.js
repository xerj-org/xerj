// ============================================================
// XERJ Console — autoindex catalog → corpus cards
//
// `xerj autoindex` / `xerj brain` write one `doc_kind: "dataset"` document per
// dataset into the `autoindex-catalog` index (engine: xerj-autoindex/src/
// catalog.rs#dataset_doc). This module turns those documents into the cards
// the Corpus home renders, and turns a catalog sample query into a Discover
// search. Pure — no fetch, no DOM — so it is testable under node.
// ============================================================

export const CATALOG_INDEX = 'autoindex-catalog';

/** The body that lists every dataset document (one per corpus). */
export function catalogQueryBody() {
  return { query: { term: { doc_kind: 'dataset' } }, size: 200, track_total_hits: true };
}

function num(v) { const n = Number(v); return Number.isFinite(n) ? n : 0; }
function str(v) { return typeof v === 'string' ? v : (v == null ? '' : String(v)); }

function parseFields(fieldsJson) {
  let specs = [];
  try { specs = JSON.parse(fieldsJson || '[]'); } catch { specs = []; }
  if (!Array.isArray(specs)) return [];
  return specs
    .filter((f) => f && typeof f.name === 'string')
    .map((f) => ({
      name: f.name,
      type: str(f.es_type || f.type || 'object'),
      semantic: !!f.semantic,
      coverage: Number.isFinite(Number(f.coverage)) ? Number(f.coverage) : 1,
      cardinality: num(f.cardinality_est),
      examples: Array.isArray(f.examples) ? f.examples.slice(0, 2).map(str) : [],
    }));
}

function parseSampleQueries(arr) {
  const list = Array.isArray(arr) ? arr : (arr ? [arr] : []);
  const out = [];
  for (const item of list) {
    let q = item;
    if (typeof item === 'string') { try { q = JSON.parse(item); } catch { continue; } }
    if (!q || typeof q !== 'object') continue;
    out.push({
      class: str(q.class),
      title: str(q.title),
      request: str(q.request),
      body: q.body && typeof q.body === 'object' ? q.body : null,
      note: str(q.note),
    });
  }
  return out;
}

/** Search hits from the catalog index → one card per dataset, largest first. */
export function parseCatalogHits(hits) {
  const cards = [];
  for (const h of hits || []) {
    const s = (h && h._source) || {};
    if (s.doc_kind && s.doc_kind !== 'dataset') continue;
    const index = str(s.index_name);
    if (!index) continue;
    cards.push({
      id: str(h._id),
      index,
      slug: str(s.slug) || index.replace(/^ax-/, ''),
      formats: Array.isArray(s.formats) ? s.formats.map(str) : (s.formats ? [str(s.formats)] : []),
      records: num(s.record_count),
      junk: num(s.junk_records),
      files: num(s.file_count),
      bytes: num(s.bytes),
      timeField: str(s.time_field) || null,
      timeMin: str(s.time_min) || null,
      timeMax: str(s.time_max) || null,
      semanticField: str(s.semantic_field) || null,
      fields: parseFields(s.fields_json),
      sampleQueries: parseSampleQueries(s.sample_queries_json),
      notes: Array.isArray(s.notes) ? s.notes.map(str) : [],
      runId: str(s.run_id) || null,
    });
  }
  cards.sort((a, b) => (b.records - a.records) || a.index.localeCompare(b.index));
  return cards;
}

/** The fields worth showing on a card: the semantic field first, then the
 *  best-covered keyword/date/text fields; autoindex plumbing (`ax_*`) last. */
export function topFields(card, n = 8) {
  const rank = (f) => {
    if (f.name.startsWith('ax_')) return 9;
    if (f.semantic || f.name === card.semanticField) return 0;
    if (f.type === 'date') return 1;
    if (f.type === 'keyword') return 2;
    if (f.type === 'text' || f.type === 'semantic_text') return 3;
    return 5;
  };
  return [...(card.fields || [])]
    .sort((a, b) => (rank(a) - rank(b)) || (b.coverage - a.coverage) || a.name.localeCompare(b.name))
    .slice(0, n);
}

function firstMatchText(clause) {
  if (!clause || typeof clause !== 'object') return null;
  if (clause.match) {
    const [field, v] = Object.entries(clause.match)[0] || [];
    if (!field) return null;
    return { type: 'match', q: typeof v === 'object' && v ? str(v.query) : str(v), field };
  }
  if (clause.match_phrase) {
    const [field, v] = Object.entries(clause.match_phrase)[0] || [];
    return { type: 'phrase', q: typeof v === 'object' && v ? str(v.query) : str(v), field };
  }
  if (clause.multi_match) return { type: 'match', q: str(clause.multi_match.query) };
  if (clause.semantic) return { type: 'semantic', q: str(clause.semantic.query) };
  if (clause.term) {
    const [field, v] = Object.entries(clause.term)[0] || [];
    return field ? { type: 'term', q: `${field}=${typeof v === 'object' && v ? str(v.value) : str(v)}` } : null;
  }
  if (clause.hybrid && Array.isArray(clause.hybrid.queries)) {
    for (const entry of clause.hybrid.queries) {
      const inner = firstMatchText(entry && (entry.query || entry));
      if (inner && inner.q) return { type: 'hybrid', q: inner.q, ...(inner.field ? { field: inner.field } : {}) };
    }
    return null;
  }
  if (clause.bool) {
    for (const key of ['must', 'should', 'filter']) {
      const list = Array.isArray(clause.bool[key]) ? clause.bool[key] : (clause.bool[key] ? [clause.bool[key]] : []);
      for (const c of list) {
        const inner = firstMatchText(c);
        if (inner && inner.q) return inner;
      }
    }
  }
  return null;
}

/**
 * A catalog sample query → the Reader search state that runs it, or null for
 * an analytics-only sample (aggregations with no text query — there is nothing
 * for a search box to run).
 *
 * `field` is the field the sample was WRITTEN for (`match` / `match_phrase`
 * and the lexical leg of a `hybrid`). The Reader adds it to the fields it
 * searches (schema-roles.js#withSearchField): the first version kept only the
 * text, so a sample over `text` ran over `body` and found nothing while the
 * catalog's own body found five (PR #945 review). A `term` sample already
 * names its field in `q` (`field=value`).
 */
export function sampleQueryToSearch(sq) {
  const body = sq && sq.body;
  if (!body || typeof body !== 'object') return null;
  const found = firstMatchText(body.query);
  if (!found || !found.q) return null;
  return { type: found.type, q: found.q, ...(found.field ? { field: found.field } : {}) };
}

/** Human byte count. */
export function fmtBytes(b) {
  const n = num(b);
  if (n < 1024) return `${n} B`;
  const u = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}

/** Human count with thousands separators. */
export function fmtCount(n) {
  return new Intl.NumberFormat('en-US').format(num(n));
}

/** "2024-01-03 → 2025-08-20" or null. */
export function timeSpan(card) {
  if (!card.timeMin && !card.timeMax) return null;
  const d = (v) => (v ? String(v).slice(0, 10) : '…');
  return `${d(card.timeMin)} → ${d(card.timeMax)}`;
}
