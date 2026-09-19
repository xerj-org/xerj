// The Reader over a MAILBOX: several messages in ONE file, as autoindex's mbox
// ingest (PR #949) writes them — every record shares the mailbox's `ax_file`,
// and each message's locators carry its byte offset (`m812-msg-s0`,
// `m812-att0-p3-s0`). Needs a binary that has the mbox extractor; run by
// run-mbox.sh. Records truth (from the generator), before (the join of
// e1e8dc04 — `ax_file` alone — replayed verbatim), and after (the bundled
// console in headless Chrome, as a guest).
// Usage: node mbox-repro.mjs <origin> <admin-key> <truth.json>   → JSON on stdout
import { readFileSync } from 'node:fs';
import { launch, sleep } from '../../../../xerj-ux/test/browser/cdp.mjs';
import { fieldTypesOf } from '../../../../xerj-ux/src/data/reader-api.js';

const [origin, adminKey, truthPath] = process.argv.slice(2);
const BRAIN = process.env.BRAIN || 'mboxrepro';
const api = async (method, path, body, key = adminKey) => {
  const r = await fetch(origin + path, { method, headers: { authorization: `ApiKey ${key}`, 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const text = await r.text(); let json = null; try { json = JSON.parse(text); } catch { /* */ }
  return { status: r.status, json, text };
};
const out = { recorded: new Date().toISOString().slice(0, 10), truth: JSON.parse(readFileSync(truthPath, 'utf8')) };

// ---- what the engine holds ----------------------------------------------
const found = await api('POST', '/ax-*/_search', { query: { wildcard: { ax_locator: { value: 'm*-msg-s0' } } }, size: 50, _source: ['email_subject', 'ax_locator', 'ax_file', 'email_message_id', 'ax_format'] });
const msgs = (found.json?.hits?.hits || []);
if (!msgs.length) throw new Error('no mailbox message records (m<offset>-msg-s0) — is this binary built with the mbox extractor? ' + found.text.slice(0, 300));
const INDEX = msgs[0]._index;
const types = fieldTypesOf((await api('GET', `/${INDEX}/_mapping`)).json, INDEX);
out.index = INDEX;
out.mapping = Object.fromEntries(['body', 'email_subject', 'title', 'attachment_name', 'ax_locator', 'ax_file'].map((f) => [f, types[f] || null]));
out.messages = msgs.map((h) => ({ subject: h._source.email_subject, locator: h._source.ax_locator, format: h._source.ax_format, messageId: h._source.email_message_id || null }))
  .sort((a, b) => Number(a.locator.slice(1).split('-')[0]) - Number(b.locator.slice(1).split('-')[0]));
out.filesHoldingTheMessages = [...new Set(msgs.map((h) => h._source.ax_file))].length;

// ---- before: the join of e1e8dc04 (ax_file alone), replayed -----------------
const bySubject = (s) => msgs.find((h) => h._source.email_subject === s);
out.before = {};
for (const subject of Object.keys(out.truth)) {
  const h = bySubject(subject);
  if (!h) { out.before[subject] = 'NOT INDEXED'; continue; }
  const r = await api('POST', `/${INDEX}/_search`, { query: { bool: { filter: [{ term: { ax_file: h._source.ax_file } }, { exists: { field: 'attachment_name' } }] } }, sort: [{ ax_locator: 'asc' }], size: 1000, _source: ['attachment_name', 'ax_locator'] });
  out.before[subject] = { attachmentRecordsJoined: r.json.hits.total.value, names: [...new Set(r.json.hits.hits.map((x) => x._source.attachment_name))].sort() };
}
const invAtt = (await api('POST', `/${INDEX}/_search`, { query: { term: { attachment_name: 'invoice.pdf' } }, size: 1, _source: ['ax_file', 'ax_locator'] })).json.hits.hits[0];
const oldParent = await api('POST', `/${INDEX}/_search`, { query: { bool: { filter: [{ term: { ax_file: invAtt._source.ax_file } }], must_not: [{ exists: { field: 'attachment_name' } }, { term: { ax_locator: 'file' } }] } }, sort: [{ ax_locator: 'asc' }], size: 1, _source: ['email_subject'] });
out.before.parentOf_invoice_pdf = oldParent.json.hits.hits[0]?._source?.email_subject || null;
out.before.emailsCountedByMessageId = (await api('POST', `/${INDEX}/_search`, { size: 0, aggs: { e: { filter: { bool: { filter: [{ exists: { field: 'email_message_id' } }], must_not: [{ exists: { field: 'attachment_name' } }] } } } } })).json.aggregations.e.doc_count;
out.engine = {
  'match body Quarterly': (await api('POST', `/${INDEX}/_search`, { query: { match: { body: 'Quarterly' } }, size: 0, track_total_hits: true })).json.hits.total.value,
  'match email_subject Quarterly': (await api('POST', `/${INDEX}/_search`, { query: { match: { email_subject: 'Quarterly' } }, size: 0, track_total_hits: true })).json.hits.total.value,
};

// ---- after: real Chrome, guest mode, the bundled console -------------------
const mint = await api('POST', '/_security/api_key', { name: 'mbox-repro-guest', expiration: '1h', role_descriptors: { guest: { indices: [{ names: [INDEX, `.xerj-memory-${BRAIN}-edges`], privileges: ['read'] }] } } });
if (mint.status !== 200) throw new Error('mint failed: ' + mint.text.slice(0, 300));
const record = { api_key: mint.json.encoded, index: INDEX, brain: BRAIN, expires_at: new Date(Date.now() + 3600_000).toISOString(), label: 'mbox repro' };
const browser = await launch();
try {
  const page = await browser.newPage();
  await page.goto(`${origin}/_xerj-console/src/theme-boot.js`);
  await page.eval(`sessionStorage.setItem('xerj.share', ${JSON.stringify(JSON.stringify(record))}), true`);
  out.after = { readerAttachmentList: {} };
  for (const subject of Object.keys(out.truth)) {
    const h = bySubject(subject);
    if (!h) continue;
    await page.goto(`${origin}/_xerj-console/#/reader?index=${encodeURIComponent(INDEX)}&id=${encodeURIComponent(h._id)}`);
    await page.waitFor(`document.querySelector('[data-shape="email"]') && !/looking/.test(document.querySelector('.rd-read').textContent)`, { label: subject, timeoutMs: 20000 });
    await sleep(500);
    out.after.readerAttachmentList[subject] = await page.eval(`({ head: document.querySelector('[data-rd-block="attachments"] .key')?.textContent || 'no attachment block', attachments: [...document.querySelectorAll('[data-rd-block="attachments"] .rd-att__name')].map((a) => a.textContent), saysIncomplete: !!document.querySelector('[data-rd-block="attachments-truncated"]') })`);
  }
  await page.goto(`${origin}/_xerj-console/#/reader?index=${encodeURIComponent(INDEX)}&id=${encodeURIComponent(invAtt._id)}`);
  await page.waitFor(`/FROM EMAIL/.test(document.querySelector('[data-guest-main]')?.textContent || '')`, { label: 'parent of invoice.pdf' });
  out.after.parentOf_invoice_pdf = await page.eval(`document.querySelector('[data-rd-block="parent"] .rd-att__name').textContent`);
  const search = async (type, q) => {
    await page.eval(`(() => { const s = document.querySelector('.rd-type'); s.value = ${JSON.stringify(type)}; const i = document.querySelector('.rd-q'); i.value = ${JSON.stringify(q)}; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    await page.waitFor(`!/SEARCHING/.test(document.querySelector('[data-rd-slot="list-head"]').textContent)`, { label: `${type} ${q}` });
    await sleep(500);
    return page.eval(`({ head: document.querySelector('[data-rd-slot="list-head"]').textContent, first: [...document.querySelectorAll('.rd-card__title')].slice(0, 3).map((t) => t.textContent) })`);
  };
  out.after.readerSearch = { 'match Quarterly': await search('match', 'Quarterly'), 'match lunch': await search('match', 'lunch'), 'phrase Lunch on': await search('phrase', 'Lunch on') };
  await page.setHash('#/corpus');
  await page.waitFor(`document.querySelector('[data-corpus-index] .cp-card__nums')`, { label: 'guest corpus' });
  out.after.guestCard = await page.eval(`document.querySelector('[data-corpus-index]').textContent.replace(/\\s+/g, ' ').slice(0, 160)`);
  const flags = await page.eval(`({ pwned: window.__xerjPwned || 0, csp: (window.__cspViolations || []).length })`);
  out.after.inert = { ...flags, consoleApiRequests: page.requests.filter((r) => r.url.includes('/_xerj-console/api/')).length };
  await page.close();
} finally {
  await browser.close();
  await api('DELETE', '/_security/api_key', { ids: [mint.json.id] });
}
console.log(JSON.stringify(out, null, 2));
