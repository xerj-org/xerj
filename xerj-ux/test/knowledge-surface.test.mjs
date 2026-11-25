// The knowledge surface — one read that answers, the moment indexing
// finishes: how large the corpus is, which data is in it, and what can be
// done with it. These tests pin the DATA (payload → cards/rows/strip),
// never the styling. Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';

// renderCorpus and friends are pure safe-dom; set a fetch stub before the
// imports that could touch the network.
const calls = [];
let respond = () => json(200, {});
globalThis.fetch = async (url, init) => { calls.push(String(url)); return respond(String(url), init); };
function json(status, body) {
  return { status, ok: status >= 200 && status < 300, json: async () => body, text: async () => JSON.stringify(body) };
}

const { parseKnowledge, parseCatalogHits, topFields } = await import('../src/data/catalog.js');
const { renderCorpus, renderCorpusCard, renderRelations, renderCapabilities } = await import('../src/ux/corpus-render.js');
const { corpus } = await import('../src/dashboards/corpus.js');
const { textOf, findAll } = await import('../src/ux/safe-dom.js');

// A payload shaped like GET /_xerj-console/api/v1/knowledge (pinned
// server-side by xerj-console-api/tests/knowledge_surface.rs — the same
// fixture facts, so the two suites cannot drift).
const PAYLOAD = () => ({ data: {
  catalog: true,
  totals: { datasets: 2, records: 105, files: 18, bytes: 3_594_132, docs: 4, relations: 2 },
  datasets: [
    {
      index: 'ax-mail', slug: 'mail', formats: ['eml'], records: 91, junk: 2, files: 14,
      bytes: 482_133, live_docs: 3, store_bytes: 190_000,
      time_field: 'email_date', time_min: '2026-01-05T00:00:00.000Z', time_max: '2026-09-19T00:00:00.000Z',
      semantic_field: 'body',
      fields: [
        { name: 'email_from', type: 'keyword', semantic: false, cardinality: 12, cardinality_overflow: false, null_ratio: 0.0, coverage: 1.0, examples: ['sam@acme.example', 'jo@grid.example'] },
        { name: 'body', type: 'semantic_text', semantic: true, cardinality: 91, cardinality_overflow: false, null_ratio: 0.0, coverage: 1.0, examples: ['term sheet attached'] },
        { name: 'page', type: 'long', semantic: false, cardinality: 4, cardinality_overflow: false, null_ratio: 0.75, coverage: 0.25, examples: ['1', '2'] },
      ],
      sample_queries: ['{"query":{"match":{"body":"term sheet"}},"size":3}'],
      notes: [], run_id: 'run-1',
    },
    { index: 'ax-pdfs', slug: 'pdfs', formats: ['pdf'], records: 14, junk: 0, files: 4, bytes: 3_111_999, live_docs: 1, store_bytes: 88_000, semantic_field: 'text', fields: [], sample_queries: [], notes: [], run_id: 'run-1' },
  ],
  others: [{ index: 'weblogs', docs: 1, store_bytes: 4_096 }],
  relations: [
    { kind: 'key_overlap', a_dataset: 'mail', a_index: 'ax-mail', a_field: 'email_from', b_dataset: 'pdfs', b_index: 'ax-pdfs', b_field: 'title', overlap: 7, containment: 0.58, grade: 'likely', confirmed_values: 7, tested_values: 12, examples: ['sam@acme.example'] },
    { kind: 'time_alignment', a_index: 'ax-mail', a_field: 'email_date', b_index: 'ax-pdfs', b_field: 'title', range_overlap: 0.62, shared_buckets: 31, pearson_r: 0.71, activity_correlated: true },
  ],
  brains: [{ name: 'casefile', nodes_index: 'ax-mail', links: 2 }],
  capabilities: [
    { id: 'search', title: 'Search it', blurb: 'Seven query types.', href: '#/discover', kind: 'console' },
    { id: 'read', title: 'Read the documents', blurb: 'Page through records.', href: '#/reader', kind: 'console' },
    { id: 'graph', title: 'See the links', blurb: "Brain 'casefile' holds 2 links.", href: '#/dashboards/second-brain?brain=casefile', kind: 'console' },
    { id: 'map', title: 'Print the data map', blurb: 'Markdown for agents.', command: 'xerj autoindex map', kind: 'cli' },
  ],
} });

test('parseKnowledge: the payload → the corpus state, every gate answered', () => {
  const st = parseKnowledge(PAYLOAD());
  assert.equal(st.status, 'ok');
  // gate 1 — size
  assert.equal(st.totals.records, 105);
  assert.equal(st.totals.bytes, 3_594_132);
  assert.equal(st.totals.datasets, 2);
  assert.equal(st.totals.catalog, true);
  // gate 2 — data: datasets in card shape, largest first, with the LIVE counts
  assert.equal(st.datasets[0].index, 'ax-mail');
  assert.equal(st.datasets[0].liveDocs, 3);
  assert.equal(st.datasets[0].storeBytes, 190_000);
  assert.equal(st.datasets[0].fields[2].nullRatio, 0.75);
  assert.deepEqual(st.summaries[0], { index: 'weblogs', records: 1, storeBytes: 4096, emails: 0, attachments: 0, formats: [] });
  // gate 2b — relations, in display shape
  assert.equal(st.relations[0].kind, 'key_overlap');
  assert.equal(st.relations[0].confirmed, 7);
  assert.equal(st.relations[1].kind, 'time_alignment');
  assert.equal(st.relations[1].pearsonR, 0.71);
  // gate 3 — capabilities + brains
  assert.equal(st.capabilities.length, 4);
  assert.equal(st.brains[0].links, 2);
  // a malformed payload degrades to empty, never throws
  const bad = parseKnowledge({ data: null });
  assert.equal(bad.status, 'ok');
  assert.deepEqual(bad.datasets, []);
});

test('a card shows the field facts the catalog measured — as text, not a hover hint', () => {
  const st = parseKnowledge(PAYLOAD());
  const card = renderCorpusCard(st.datasets[0], {});
  const text = textOf(card);
  // the example value is VISIBLE (it lived only in a title attribute before)
  assert.match(text, /sam@acme\.example/);
  // coverage and null% are the catalog's own numbers
  assert.match(text, /75% null/);
  assert.match(text, /25%/); // page coverage
  assert.match(text, /12 distinct/); // email_from cardinality
  // the LIVE count is the headline number, not the run's stale one
  assert.match(text, /3 records/);
  assert.doesNotMatch(text, /91 records/);
});

test('a dataset with more than eight fields folds the rest into an ALL FIELDS disclosure — nothing is capped away', () => {
  const st = parseKnowledge(PAYLOAD());
  const many = { ...st.datasets[0], fields: Array.from({ length: 11 }, (_, i) => ({ name: `f${i}`, type: 'keyword', semantic: false, coverage: 1, cardinality: null, nullRatio: null, examples: [] })) };
  const card = renderCorpusCard(many, {});
  const details = findAll(card, (n) => n.tag === 'details');
  assert.equal(details.length, 1);
  assert.match(textOf(details[0]), /ALL 11 FIELDS/);
  // every field name is reachable in the payload the card renders
  assert.match(textOf(card), /f10/);
});

test('renderRelations: what autoindex inferred — both kinds, and an honest none', () => {
  const st = parseKnowledge(PAYLOAD());
  const rows = renderRelations(st);
  const text = textOf(rows);
  assert.match(text, /ax-mail\.email_from\s+⟷\s+ax-pdfs\.title/);
  assert.match(text, /7 shared values/);
  assert.match(text, /likely/);
  assert.match(text, /7\/12 confirmed by query/);
  assert.match(text, /e\.g\. sam@acme\.example/);
  assert.match(text, /ranges overlap 62%/);
  assert.match(text, /activity r=0\.71 · correlated/);
  // none inferred is stated, not papered over
  assert.match(textOf(renderRelations({ relations: [] })), /No cross-dataset relations were inferred/);
});

test('renderCapabilities: every entry names its real surface; hrefs are in-app routes', () => {
  const st = parseKnowledge(PAYLOAD());
  const strip = renderCapabilities(st);
  const cards = findAll(strip, (n) => (n.attrs.class || '').split(' ').includes('cp-cap'));
  assert.equal(cards.length, 4);
  const text = textOf(strip);
  assert.match(text, /Search it/);
  assert.match(text, /xerj autoindex map/); // the CLI command is shown verbatim
  for (const c of cards) {
    const a = findAll(c, (n) => n.tag === 'a')[0];
    if (a) assert.match(a.attrs.href, /^#\//, 'a capability link is an in-app hash route');
  }
  // nothing to show → nothing claimed
  assert.equal(renderCapabilities({ capabilities: [] }), null);
});

test('the CORPUS scene answers "how large" in its meta line and adds the two panels', () => {
  const st = parseKnowledge(PAYLOAD());
  const view = corpus.render({ data: st });
  assert.ok(view.meta.some((m) => /105 RECORDS/.test(m)), view.meta.join(' | '));
  assert.ok(view.meta.some((m) => /3\.6 MB|3\.4 MB/.test(m)), view.meta.join(' | '));
  assert.deepEqual(view.panels.map((p) => p.id), ['datasets', 'relations', 'capabilities']);
  // the legacy path (no totals) keeps the one panel it always had
  const legacy = corpus.render({ data: { status: 'ok', datasets: st.datasets, summaries: [], totals: null, relations: [], capabilities: [] } });
  assert.deepEqual(legacy.panels.map((p) => p.id), ['datasets']);
  assert.ok(legacy.meta.some((m) => /105 RECORDS/.test(m)), `the cards' own records sum: ${legacy.meta.join(' | ')}`);
});

test('liveCorpus reads ONE endpoint and keeps the never-mock contract', async () => {
  const { query, NEVER_MOCK } = await import('../src/data/query.js');
  assert.ok(NEVER_MOCK.has('corpus'));
  calls.length = 0;
  respond = (url) => (url.endsWith('/api/v1/knowledge') ? json(200, PAYLOAD()) : json(404, {}));
  const r = await query({ dashId: 'corpus' });
  assert.equal(calls.length, 1, 'exactly one request: the knowledge surface');
  assert.ok(calls[0].endsWith('/_xerj-console/api/v1/knowledge'));
  assert.equal(r.data.datasets.length, 2);
  assert.equal(r.data.relations.length, 2);
  assert.equal(r.data.capabilities.length, 4);
  // engine unreachable → an error and empty lists, never a sample
  calls.length = 0;
  respond = () => { throw new TypeError('Failed to fetch'); };
  const err = await query({ dashId: 'corpus' });
  assert.equal(err.meta.sourceKind, 'live-error');
  assert.equal(err.data.status, 'error');
  assert.deepEqual(err.data.datasets, []);
  assert.deepEqual(err.data.relations, []);
});

// ── #1098: the FIELDS table must be the corpus the catalog measured ────
//
// The screenshot defect: a 95.8%-code corpus opened with `email_date (date)
// 100% · email_subject (keyword) 100% · email_from (keyword) 59% · …` —
// coverage that matches nothing in fields_json — while `code` (0.958) was
// pushed out of the table entirely. The 100/59/57/43 pattern is coverage
// within the corpus's mbox-format SUBSET; these tests pin that neither the
// ranking nor the numbers can present anything but the catalog's own
// field-for-field, number-for-number facts, whichever path served the card.
const CODE_CORPUS_FIELDS = [
  { name: 'code', type: 'text', semantic: false, coverage: 0.958, cardinality: 15436, null_ratio: 0.042, avg_len: 912.0, examples: ['fn short_passage(i: usize) -> String'] },
  { name: 'title', type: 'keyword', semantic: false, coverage: 1.0, cardinality: 500, null_ratio: 0.0, avg_len: 34.0, examples: ['index.rs'] },
  { name: 'body', type: 'semantic_text', semantic: true, coverage: 0.039, cardinality: 602, null_ratio: 0.961, avg_len: 412.5, examples: ['term sheet attached'] },
  { name: 'labels', type: 'keyword', semantic: false, coverage: 1.0, cardinality: 4, null_ratio: 0.0, avg_len: 9.0, examples: ['rust'] },
  { name: 'defs', type: 'text', semantic: false, coverage: 0.032, cardinality: 208, null_ratio: 0.968, avg_len: 64.0, examples: ['pub fn parse_request'] },
  { name: 'mtime', type: 'date', semantic: false, coverage: 1.0, cardinality: 491, null_ratio: 0.0, avg_len: 0.0, examples: [] },
  { name: 'email_date', type: 'date', semantic: false, coverage: 0.0064, cardinality: 99, null_ratio: 0.9936, avg_len: 0.0, examples: [] },
  { name: 'email_subject', type: 'keyword', semantic: false, coverage: 0.0064, cardinality: 97, null_ratio: 0.9936, avg_len: 31.0, examples: ['Re: contract'] },
  // the mbox-subset figures from the screenshot — 59/57/43 must never be
  // able to stand in for the corpus coverage of ANY field
  { name: 'email_from', type: 'keyword', semantic: false, coverage: 0.0064, cardinality: 61, null_ratio: 0.9936, avg_len: 22.0, examples: ['sam@acme.example'] },
  { name: 'email_to', type: 'keyword', semantic: false, coverage: 0.0033, cardinality: 55, null_ratio: 0.9967, avg_len: 22.0, examples: ['jo@grid.example'] },
  { name: 'email_cc', type: 'keyword', semantic: false, coverage: 0.0026, cardinality: 37, null_ratio: 0.9974, avg_len: 20.0, examples: [] },
  { name: 'ax_path', type: 'keyword', semantic: false, coverage: 1.0, cardinality: 500, null_ratio: 0.0, avg_len: 48.0, examples: [] },
  { name: 'ax_format', type: 'keyword', semantic: false, coverage: 1.0, cardinality: 6, null_ratio: 0.0, avg_len: 4.0, examples: [] },
  { name: 'ax_source', type: 'keyword', semantic: false, coverage: 1.0, cardinality: 1, null_ratio: 0.0, avg_len: 5.0, examples: [] },
  { name: 'ax_kind', type: 'keyword', semantic: false, coverage: 1.0, cardinality: 2, null_ratio: 0.0, avg_len: 4.0, examples: [] },
  // a field the catalog did not measure — "—", never a fabricated 100%
  { name: 'unmeasured', type: 'long', semantic: false, coverage: null, cardinality: null, null_ratio: null, avg_len: 0.0, examples: [] },
];

const chipsOf = (card) => findAll(card, (n) => (n.attrs.class || '').split(' ').includes('cp-field'));

test('#1098 the FIELDS table leads with the corpus\'s dominant fields at the catalog\'s own coverage', () => {
  const st = parseKnowledge({ data: { ...PAYLOAD().data, datasets: [{ ...PAYLOAD().data.datasets[0], fields: CODE_CORPUS_FIELDS }] } });
  const card = renderCorpusCard(st.datasets[0], {});
  const chips = chipsOf(card);
  // the top chips (before the ALL-FIELDS disclosure) are the first 8
  assert.equal(chips.length, CODE_CORPUS_FIELDS.length, 'every field renders, in the disclosure when not top');
  const names = chips.map((c) => textOf(findAll(c, (n) => (n.attrs.class || '').includes('cp-field__name'))[0]));
  // the corpus IS its content: code first, body and defs ahead of every
  // email_* field, autoindex plumbing last
  assert.equal(names[0], 'code', `dominant field first: ${names.join(', ')}`);
  for (const dominant of ['code', 'body', 'defs']) {
    for (const email of names.filter((n) => n.startsWith('email_'))) {
      assert.ok(names.indexOf(dominant) < names.indexOf(email), `${dominant} must outrank ${email}: ${names.join(', ')}`);
    }
  }
  const lastThree = names.slice(-3).sort();
  assert.deepEqual(lastThree, ['ax_format', 'ax_kind', 'ax_source'], `plumbing last: ${names.join(', ')}`);
  // number-for-number: each chip shows ITS OWN measured coverage, rounded
  const byName = new Map(CODE_CORPUS_FIELDS.map((f) => [f.name, f]));
  const chipText = (name) => textOf(chips[names.indexOf(name)]);
  for (let i = 0; i < chips.length; i++) {
    const f = byName.get(names[i]);
    const expected = f.coverage == null ? '—' : `${Math.round(f.coverage * 100)}%`;
    assert.ok(textOf(chips[i]).includes(expected), `${names[i]} must show ${expected}: ${textOf(chips[i])}`);
  }
  // the screenshot's mbox-subset mirage is dead: no email field reads 100%,
  // 59/57/43 appear nowhere, and the dominant coverage is on the table
  const text = textOf(card);
  assert.ok(!chipText('email_date').includes('100%'), `email_date renders its own coverage: ${chipText('email_date')}`);
  assert.ok(!chipText('email_subject').includes('100%'), `email_subject renders its own coverage: ${chipText('email_subject')}`);
  for (const bogus of ['59%', '57%', '43%']) assert.ok(!text.includes(bogus), `${bogus} must not render: no field measures it`);
  assert.ok(chipText('code').includes('96%'), 'code 0.958 leads at 96%'); // code 0.958
  // unmeasured coverage renders as "—", never as 100%
  assert.match(chipText('unmeasured'), /—/);
  assert.doesNotMatch(chipText('unmeasured'), /100%/);
});

test('#1098 both card paths agree: the catalog-search fallback ranks and numbers identically', () => {
  // The legacy path parses fields_json (the autoindex catalog document);
  // the knowledge path parses the endpoint payload. The screenshot showed a
  // table that disagreed with BOTH — pin that whichever path serves the
  // card, the same fields lead with the same numbers.
  const specs = CODE_CORPUS_FIELDS.map((f) => ({
    name: f.name, es_type: f.type, semantic: f.semantic === true ? 'lexical-hash-384' : undefined,
    cardinality_est: f.cardinality, null_ratio: f.null_ratio, coverage: f.coverage,
    avg_len: f.avg_len, examples: f.examples,
  }));
  const hits = [{ _id: 'ds:code', _source: {
    doc_kind: 'dataset', index_name: 'ax-docs', record_count: 15436, file_count: 500,
    bytes: 16496072, semantic_field: 'body', fields_json: JSON.stringify(specs),
  } }];
  const legacy = parseCatalogHits(hits)[0];
  const st = parseKnowledge({ data: { ...PAYLOAD().data, datasets: [{ ...PAYLOAD().data.datasets[0], index: 'ax-docs', fields: CODE_CORPUS_FIELDS }] } });
  const knowledgeCard = st.datasets[0];
  const rank = (card) => topFields(card, 8).map((f) => f.name);
  assert.deepEqual(rank(legacy), rank(knowledgeCard), 'both paths lead with the same fields in the same order');
  assert.equal(rank(legacy)[0], 'code');
  // an unmeasured field is null on BOTH paths — the legacy default of 1
  // (which rendered "100%") is gone
  const legacyUnmeasured = legacy.fields.find((f) => f.name === 'unmeasured');
  assert.equal(legacyUnmeasured.coverage, null);
});

// ── #1099: the semantic capability states what the node runs — no invented vectors ──

test('#1099 the semantic row renders the node\'s own facts and never a vector count', () => {
  const lexical = { data: { ...PAYLOAD().data, capabilities: [
    ...PAYLOAD().data.capabilities,
    { id: 'semantic', title: 'Search it semantically',
      blurb: 'Semantic and hybrid over `body` → `body_vector`: 384-D · cosine · lexical feature-hash (built-in, 384-dim, non-neural) — one vector per embedded document. That embedder is NOT neural; start the node with `--embed-mode neural` (or `proxy` / `onnx-experimental`) for neural semantics.',
      href: '#/discover', kind: 'console' },
  ] } };
  const strip = renderCapabilities(parseKnowledge(lexical));
  const text = textOf(strip);
  assert.match(text, /Search it semantically/);
  assert.match(text, /lexical feature-hash/); // the embedder's own label
  assert.match(text, /NOT neural/);
  assert.match(text, /--embed-mode neural/);
  // the honest-claims line: no count of vectors, no READY claim — the node
  // backs neither ("6,546,091 BODY VECTORS · 384-D · COSINE — READY" was
  // computed by nothing the node reports)
  assert.doesNotMatch(text, /\d[\d,.]*\s+VECTORS?/i);
  assert.doesNotMatch(text, /VECTOR COUNT/i);
  assert.doesNotMatch(text, /—\s*READY\b/);
  // and when the node reports no semantic surface, no semantic row renders
  const none = renderCapabilities(parseKnowledge(PAYLOAD()));
  assert.doesNotMatch(textOf(none), /Search it semantically/);
});

test('#1099 a catalog that elected a semantic field the index does not carry says so plainly', () => {
  const missing = { data: { ...PAYLOAD().data, capabilities: [
    ...PAYLOAD().data.capabilities,
    { id: 'semantic', title: 'Search it semantically',
      blurb: "The catalog's last run elected `body` as the semantic field, but index `ax-mail` carries no embedding for it on this node — there are no vectors to match, and semantic/hybrid queries on that field are refused.",
      href: '#/discover', kind: 'console' },
  ] } };
  const text = textOf(renderCapabilities(parseKnowledge(missing)));
  assert.match(text, /no vectors to match/);
  assert.doesNotMatch(text, /\d[\d,.]*\s+VECTORS?/i);
});

test('#2409 the empty state names the data directory the node serves', () => {
  const hint = 'This node serves data directory /srv/xerj/data. If you indexed while the node ran with another --data-dir or --config, restart it with that one: a node reads only its own data directory and does not look for others.';
  const empty = { data: { catalog: false, totals: { datasets: 0, records: 0, files: 0, bytes: 0, docs: 0, relations: 0 },
    datasets: [], others: [], relations: [], brains: [], capabilities: [],
    data_dir: '/srv/xerj/data', data_dir_hint: hint } };
  const st = parseKnowledge(empty);
  assert.equal(st.dataDir, '/srv/xerj/data');
  const text = textOf(renderCorpus(st));
  assert.match(text, /NOTHING INDEXED YET/);
  assert.match(text, /DATA DIRECTORY/);
  assert.ok(text.includes('/srv/xerj/data'), text);
  assert.match(text, /--data-dir or --config/);

  // a viewer: no path in the payload, the hint still renders, no path block
  const viewer = { data: { ...empty.data, data_dir: null,
    data_dir_hint: 'If you indexed while the node ran with another --data-dir or --config, this node may be serving a different data directory; its startup log names the one in use (`server data directory data_dir=...`).' } };
  const vt = textOf(renderCorpus(parseKnowledge(viewer)));
  assert.doesNotMatch(vt, /DATA DIRECTORY/);
  assert.match(vt, /startup log names the one in use/);

  // a share guest never sees either
  const gt = textOf(renderCorpus(st, { guest: true }));
  assert.doesNotMatch(gt, /data directory/i);

  // an older engine without the fields: the empty state is unchanged
  const old = textOf(renderCorpus(parseKnowledge({ data: { ...empty.data, data_dir: undefined, data_dir_hint: undefined } })));
  assert.doesNotMatch(old, /DATA DIRECTORY|--data-dir/);
});
