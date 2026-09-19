// The correctness review of PR #945, replayed against a REAL node: what the
// engine holds (truth), what the first version of the Reader asked for and got
// (before), and what this version shows in real headless Chrome (after).
// Usage: node live-repro.mjs <origin> <admin-key>      → JSON on stdout
import { launch, sleep } from '../../../../xerj-ux/test/browser/cdp.mjs';
import { buildSearchBody, searchFieldsOf } from '../../../../xerj-ux/src/data/search-body.js';
import { deriveRoles } from '../../../../xerj-ux/src/data/schema-roles.js';
import { fieldTypesOf } from '../../../../xerj-ux/src/data/reader-api.js';

const [origin, adminKey] = process.argv.slice(2);
const INDEX = 'ax-docs';
const BRAIN = process.env.BRAIN || 'repro';
const api = async (method, path, body, key = adminKey) => {
  const r = await fetch(origin + path, { method, headers: { authorization: `ApiKey ${key}`, 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text(); let json = null; try { json = JSON.parse(text); } catch { /* */ }
  return { status: r.status, json, text };
};
const total = async (query) => { const r = await api('POST', `/${INDEX}/_search`, { query, size: 0, track_total_hits: true }); return r.json?.hits ? r.json.hits.total.value : `HTTP ${r.status}`; };
const out = { recorded: new Date().toISOString().slice(0, 10) };

// ---- truth ---------------------------------------------------------------
const mapping = (await api('GET', `/${INDEX}/_mapping`)).json;
const types = fieldTypesOf(mapping, INDEX);
out.mapping = Object.fromEntries(['body', 'text', 'email_subject', 'title', 'attachment_name'].map((f) => [f, types[f] || null]));
const truth = await api('POST', `/${INDEX}/_search`, { size: 0, aggs: { p: { terms: { field: 'ax_path', size: 50 }, aggs: { att: { terms: { field: 'attachment_name' } }, mid: { terms: { field: 'email_message_id' } } } } } });
out.truth = Object.fromEntries(truth.json.aggregations.p.buckets.filter((b) => b.key.endsWith('.eml')).sort((a, b) => a.key.localeCompare(b.key))
  .map((b) => [b.key, { records: b.doc_count, attachments: Object.fromEntries(b.att.buckets.map((x) => [x.key, x.doc_count])), messageId: b.mid.buckets.map((x) => x.key)[0] || null }]));

// ---- before: the requests the first version of the Reader made -------------
const oldJoin = async (mid) => {
  const r = await api('POST', `/${INDEX}/_search`, { query: { bool: { filter: [{ term: { email_message_id: mid } }, { exists: { field: 'attachment_name' } }] } }, size: 200 });
  const hits = r.json.hits.hits;
  return { matching: r.json.hits.total.value, returned: hits.length, listedByName: [...new Set(hits.map((h) => h._source.attachment_name))].sort() };
};
const oldMatch = (q) => ({ bool: { should: ['body', 'email_subject', 'title', 'attachment_name'].map((f) => ({ match: { [f]: q } })), minimum_should_match: 1 } });
const catalog = await api('POST', '/autoindex-catalog/_search', { query: { term: { doc_kind: 'dataset' } }, size: 10 });
const samples = (catalog.json?.hits?.hits || []).flatMap((h) => [].concat(h._source.sample_queries_json || [])).map((x) => (typeof x === 'string' ? JSON.parse(x) : x));
const fullText = samples.find((s) => s.class === 'full_text' && s.body?.query?.match);
const [sampleField, sampleText] = fullText ? Object.entries(fullText.body.query.match)[0] : [null, null];
out.before = {
  attachmentJoin_size200_byMessageId: { 'inbox/06-bigpdf.eml': await oldJoin('06-bigpdf.eml@acme.example'), 'inbox/04-samename.eml': await oldJoin('04-samename.eml@acme.example'), 'inbox/03-dup.eml (shares its id with archive/03-dup.eml)': await oldJoin('dup@acme.example') },
  match_on_keyword_subject: { 'match email_subject:Lunch': await total({ match: { email_subject: 'Lunch' } }), 'match email_subject:"Lunch on Friday?" (the whole value)': await total({ match: { email_subject: 'Lunch on Friday?' } }), 'match_phrase email_subject:"Lunch on"': await total({ match_phrase: { email_subject: 'Lunch on' } }) },
  readerMatch_singleTextField: Object.fromEntries(await Promise.all(['Duplicate', 'Lunch', 'nomid', sampleText].filter(Boolean).map(async (q) => [q, await total(oldMatch(q))]))),
  catalogSample: fullText ? { field: sampleField, text: sampleText, catalogBodyVerbatim: await total(fullText.body.query) } : null,
  guestCard_emails_by_message_id: (await api('POST', `/${INDEX}/_search`, { size: 0, aggs: { e: { filter: { bool: { filter: [{ exists: { field: 'email_message_id' } }], must_not: [{ exists: { field: 'attachment_name' } }] } } } } })).json.aggregations.e.doc_count,
};

// ---- the clauses this version sends, measured on the engine ----------------
const roles = deriveRoles(types);
const has = (f, value) => ({ wildcard: { [f]: { value, case_insensitive: true } } });
out.engine = {
  searchFields: searchFieldsOf(roles),
  wildcard_on_keyword_subject: { '*lunch*': await total(has('email_subject', '*lunch*')), '*lunch on*': await total(has('email_subject', '*lunch on*')), 'lunch*': await total(has('email_subject', 'lunch*')), 'prefix + case_insensitive (not used: returns)': await total({ prefix: { email_subject: { value: 'lunch', case_insensitive: true } } }) },
  readerBodies: Object.fromEntries(await Promise.all([['match', 'Duplicate'], ['match', 'Lunch'], ['match', 'nomid'], ['phrase', 'Lunch on'], ['prefix', 'dup'], ['match', sampleText]].filter(([, q]) => q).map(async ([type, q]) => [`${type} ${q}`, await total(buildSearchBody(q, type, {}, roles, { aggs: false }).query)]))),
};

// ---- after: real Chrome, guest mode, the bundled console -------------------
const msgs = (await api('POST', `/${INDEX}/_search`, { query: { term: { ax_locator: 'msg-s0' } }, size: 50, _source: ['ax_path'] })).json.hits.hits;
const idOf = (path) => msgs.find((h) => h._source.ax_path === path)._id;
const mint = await api('POST', '/_security/api_key', { name: 'review-repro-guest', expiration: '1h', role_descriptors: { guest: { indices: [{ names: [INDEX, `.xerj-memory-${BRAIN}-edges`], privileges: ['read'] }] } } });
if (mint.status !== 200) throw new Error('mint failed: ' + mint.text.slice(0, 300));
const record = { api_key: mint.json.encoded, index: INDEX, brain: BRAIN, expires_at: new Date(Date.now() + 3600_000).toISOString(), label: 'review repro' };
const browser = await launch();
try {
  const page = await browser.newPage();
  await page.goto(`${origin}/_xerj-console/src/theme-boot.js`);
  await page.eval(`sessionStorage.setItem('xerj.share', ${JSON.stringify(JSON.stringify(record))}), true`);
  const open = async (path) => {
    await page.goto(`${origin}/_xerj-console/#/reader?index=${INDEX}&id=${encodeURIComponent(idOf(path))}`);
    await page.waitFor(`document.querySelector('[data-shape="email"]') && !/looking/.test(document.querySelector('.rd-read').textContent)`, { label: path, timeoutMs: 20000 });
    await sleep(400);
    return page.eval(`({ head: document.querySelector('[data-rd-block="attachments"] .key')?.textContent || 'no attachment block', attachments: [...document.querySelectorAll('[data-rd-block="attachments"] .rd-att__name')].map((a) => a.textContent), saysIncomplete: !!document.querySelector('[data-rd-block="attachments-truncated"]') })`);
  };
  out.after = { readerAttachmentList: {} };
  for (const p of Object.keys(out.truth)) out.after.readerAttachmentList[p] = await open(p);

  await page.goto(`${origin}/_xerj-console/#/reader?index=${INDEX}&id=${encodeURIComponent(idOf('inbox/04-samename.eml'))}`);
  await page.waitFor(`document.querySelectorAll('[data-rd-block="attachments"] .rd-att').length === 2`, { label: 'two scan.pdf entries' });
  await page.eval(`(document.querySelectorAll('[data-rd-block="attachments"] .rd-att')[1].click(), true)`);
  await page.waitFor(`document.querySelector('[data-shape="attachment"]') && /FROM EMAIL/.test(document.querySelector('[data-guest-main]').textContent)`, { label: 'the second scan.pdf' });
  out.after.secondSameNamedAttachmentOpens = await page.eval(`({ text: document.querySelector('.rd-body').textContent.slice(0, 40), fromEmail: document.querySelector('[data-rd-block="parent"] .rd-att__name').textContent })`);
  const nomidAtt = (await api('POST', `/${INDEX}/_search`, { query: { term: { attachment_name: 'nomid.pdf' } }, size: 1 })).json.hits.hits[0]._id;
  await page.goto(`${origin}/_xerj-console/#/reader?index=${INDEX}&id=${encodeURIComponent(nomidAtt)}`);
  await page.waitFor(`/FROM EMAIL/.test(document.querySelector('[data-guest-main]')?.textContent || '')`, { label: 'parent of an attachment whose email has no Message-ID' });
  out.after.attachmentOfEmailWithoutMessageId_fromEmail = await page.eval(`document.querySelector('[data-rd-block="parent"] .rd-att__name').textContent`);

  const search = async (type, q) => {
    await page.eval(`(() => { const s = document.querySelector('.rd-type'); s.value = ${JSON.stringify(type)}; const i = document.querySelector('.rd-q'); i.value = ${JSON.stringify(q)}; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await page.waitFor(`!/SEARCHING/.test(document.querySelector('[data-rd-slot="list-head"]').textContent)`, { label: `${type} ${q}` });
    await sleep(500);
    return page.eval(`({ head: document.querySelector('[data-rd-slot="list-head"]').textContent, first: [...document.querySelectorAll('.rd-card__title')].slice(0, 2).map((t) => t.textContent), message: document.querySelector('.rd-list') ? null : (document.querySelector('[data-rd-slot="list"] .rd-empty')?.textContent || null) })`);
  };
  out.after.readerSearch = {};
  for (const [type, q] of [['match', 'Duplicate'], ['match', 'Lunch'], ['match', 'nomid'], ['phrase', 'Lunch on'], ['prefix', 'dup'], ['match', sampleText], ['term', 'zebrafish'], ['range', 'hello'], ['term', 'ax_format=eml']].filter(([, q]) => q)) out.after.readerSearch[`${type} ${q}`] = await search(type, q);
  const before = await page.eval(`document.querySelectorAll('.rd-card').length`);
  await page.eval(`(document.querySelector('[data-rd-more]')?.click(), true)`);
  await page.waitFor(`document.querySelectorAll('.rd-card').length > ${before}`, { label: 'SHOW MORE' });
  out.after.showMore = { cardsBefore: before, cardsAfter: await page.eval(`document.querySelectorAll('.rd-card').length`), head: await page.eval(`document.querySelector('[data-rd-slot="list-head"]').textContent`) };

  await page.setHash('#/corpus');
  await page.waitFor(`document.querySelector('[data-corpus-index] .cp-card__nums')`, { label: 'guest corpus' });
  out.after.guestCard = await page.eval(`document.querySelector('[data-corpus-index]').textContent.replace(/\\s+/g, ' ').slice(0, 140)`);
  const flags = await page.eval(`({ pwned: window.__xerjPwned || 0, csp: (window.__cspViolations || []).length })`);
  out.after.inert = { ...flags, consoleApiRequests: page.requests.filter((r) => r.url.includes('/_xerj-console/api/')).length };
  await page.close();
} finally {
  await browser.close();
  await api('DELETE', '/_security/api_key', { ids: [mint.json.id] });
}
console.log(JSON.stringify(out, null, 2));
