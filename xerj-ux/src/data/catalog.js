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
      // Unmeasured coverage is null → "—", never 1 → "100%": a field the
      // catalog did not measure must not read as fully covered (#1098).
      // (`Number(null)` is 0, so null has to be checked before the finite
      // test — a JSON null means "not measured", not "measured zero".)
      coverage: f.coverage == null || !Number.isFinite(Number(f.coverage)) ? null : Number(f.coverage),
      cardinality: num(f.cardinality_est),
      nullRatio: f.null_ratio == null || !Number.isFinite(Number(f.null_ratio)) ? null : Number(f.null_ratio),
      avgLen: Number.isFinite(Number(f.avg_len)) ? Number(f.avg_len) : 0,
      examples: Array.isArray(f.examples) ? f.examples.slice(0, 3).map(str) : [],
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

/** The card's fields in DISPLAY ORDER: the corpus's DOMINANT fields first —
 *  ranked by the share of the corpus's content they hold (the catalog's own
 *  coverage × avg_len), tie-broken by coverage — with autoindex plumbing
 *  (`ax_*`) last. Type must not lead the ranking (#1098): a type-first rank
 *  put every keyword/date field above the text field that IS the corpus, so
 *  a 96%-code corpus opened with 0.6%-coverage email fields and its `code`
 *  field was pushed into the disclosure. The semantic field keeps its accent
 *  styling but no ranking pin — on a notes corpus it dominates on its own. */
export function rankedFields(card) {
  const plumbing = (f) => (f.name.startsWith('ax_') ? 1 : 0);
  const content = (f) => (f.coverage == null ? 0 : f.coverage) * (f.avgLen || 0);
  const cov = (f) => (f.coverage == null ? -1 : f.coverage);
  return [...(card.fields || [])].sort(
    (a, b) => (plumbing(a) - plumbing(b)) || (content(b) - content(a)) || (cov(b) - cov(a)) || a.name.localeCompare(b.name),
  );
}

/** The fields worth showing as a card's top chips. */
export function topFields(card, n = 8) {
  return rankedFields(card).slice(0, n);
}

// ── the knowledge surface ───────────────────────────────────────────
//
// GET /_xerj-console/api/v1/knowledge is ONE read that answers the three
// questions a person has the moment indexing finishes — how large, which
// data, what can I do now. The server (xerj-console-api/src/knowledge.rs)
// joins the catalog's own numbers with the engine's live ones and computes
// the totals, the relations autoindex actually inferred, and a capability
// strip grounded in real routes/commands/endpoints. parseKnowledge()
// normalizes that payload into the SAME card shape parseCatalogHits
// produces (plus the live facts), so the render layer has one shape
// whatever served it.

const knowledgeField = (f) => ({
  name: str(f.name),
  type: str(f.type || 'object'),
  semantic: f.semantic === true,
  coverage: f.coverage == null || !Number.isFinite(Number(f.coverage)) ? null : Number(f.coverage),
  cardinality: Number.isFinite(Number(f.cardinality)) ? Number(f.cardinality) : null,
  nullRatio: f.null_ratio == null || !Number.isFinite(Number(f.null_ratio)) ? null : Number(f.null_ratio),
  avgLen: Number.isFinite(Number(f.avg_len)) ? Number(f.avg_len) : 0,
  examples: Array.isArray(f.examples) ? f.examples.slice(0, 3).map(str) : [],
});

const knowledgeDataset = (d) => ({
  index: str(d.index),
  slug: str(d.slug) || str(d.index).replace(/^ax-/, ''),
  formats: Array.isArray(d.formats) ? d.formats.map(str) : [],
  records: num(d.records),
  junk: num(d.junk),
  files: num(d.files),
  bytes: num(d.bytes),
  liveDocs: Number.isFinite(Number(d.live_docs)) ? Number(d.live_docs) : null,
  storeBytes: Number.isFinite(Number(d.store_bytes)) ? Number(d.store_bytes) : null,
  timeField: str(d.time_field) || null,
  timeMin: str(d.time_min) || null,
  timeMax: str(d.time_max) || null,
  semanticField: str(d.semantic_field) || null,
  fields: (Array.isArray(d.fields) ? d.fields : []).map(knowledgeField),
  sampleQueries: parseSampleQueries(d.sample_queries),
  notes: Array.isArray(d.notes) ? d.notes.map(str) : [],
  runId: str(d.run_id) || null,
});

/** A key_overlap / time_alignment relation row, in display shape. */
const knowledgeRelation = (r) => ({
  kind: str(r.kind) === 'time_alignment' ? 'time_alignment' : 'key_overlap',
  aIndex: str(r.a_index), aField: str(r.a_field), aDataset: str(r.a_dataset) || null,
  bIndex: str(r.b_index), bField: str(r.b_field), bDataset: str(r.b_dataset) || null,
  grade: str(r.grade),
  overlap: num(r.overlap),
  containment: Number.isFinite(Number(r.containment)) ? Number(r.containment) : null,
  confirmed: Number.isFinite(Number(r.confirmed_values)) ? Number(r.confirmed_values) : null,
  tested: Number.isFinite(Number(r.tested_values)) ? Number(r.tested_values) : null,
  examples: Array.isArray(r.examples) ? r.examples.slice(0, 3).map(str) : [],
  rangeOverlap: Number.isFinite(Number(r.range_overlap)) ? Number(r.range_overlap) : null,
  sharedBuckets: Number.isFinite(Number(r.shared_buckets)) ? Number(r.shared_buckets) : null,
  pearsonR: Number.isFinite(Number(r.pearson_r)) ? Number(r.pearson_r) : null,
  activityCorrelated: r.activity_correlated === true,
});

/**
 * The `{ data: … }` body of GET /_xerj-console/api/v1/knowledge → the
 * corpus state the Corpus home renders: `datasets` (cards, same shape as
 * parseCatalogHits + the engine's live doc counts and store bytes),
 * `summaries` (indices the catalog does not describe), whole-corpus
 * `totals`, `relations`, `capabilities` and `brains`. Pure; throws on
 * nothing (a malformed payload degrades to empty lists, and the caller
 * decides what an unreachable engine means).
 */
export function parseKnowledge(payload) {
  const d = (payload && payload.data && typeof payload.data === 'object') ? payload.data : {};
  const datasets = (Array.isArray(d.datasets) ? d.datasets : []).map(knowledgeDataset);
  datasets.sort((a, b) => (b.records - a.records) || a.index.localeCompare(b.index));
  const summaries = (Array.isArray(d.others) ? d.others : []).map((o) => ({
    index: str(o.index),
    records: num(o.docs),
    storeBytes: Number.isFinite(Number(o.store_bytes)) ? Number(o.store_bytes) : null,
    emails: 0, attachments: 0, formats: [],
  }));
  const t = d.totals || {};
  const totals = {
    datasets: num(t.datasets), records: num(t.records), files: num(t.files),
    bytes: num(t.bytes), docs: num(t.docs), relations: num(t.relations),
    catalog: d.catalog === true,
  };
  const relations = (Array.isArray(d.relations) ? d.relations : []).map(knowledgeRelation);
  const capabilities = (Array.isArray(d.capabilities) ? d.capabilities : [])
    .filter((c) => c && typeof c === 'object' && c.id)
    .map((c) => ({
      id: str(c.id), title: str(c.title), blurb: str(c.blurb),
      href: str(c.href) || null, command: str(c.command) || null, endpoint: str(c.endpoint) || null,
      kind: str(c.kind) || (c.href ? 'console' : 'cli'),
    }));
  const brains = (Array.isArray(d.brains) ? d.brains : []).map((b) => ({
    name: str(b.name), links: num(b.links),
  }));
  // #2409: which data directory the node serves (owner/admin only; null
  // otherwise) and the --data-dir/--config hint, worded server-side as
  // `xerj autoindex map` words it. Shown by the empty state.
  const dataDir = str(d.data_dir) || null;
  const dataDirHint = str(d.data_dir_hint) || null;
  return { status: 'ok', datasets, summaries, totals, relations, capabilities, brains, dataDir, dataDirHint, _live: true };
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
