// data/reader-api.js#fetchRelated — an email's attachment list, and an
// attachment's parent.  Run: node --test xerj-ux/test/
//
// Every corpus below is the shape a real `xerj brain` run wrote for the review
// of PR #945 (live node, 2026-09-19): autoindex's EML extractor emits
// `msg-s<i>` for the message, and per attachment N `att<N>-s<i>` (text),
// `att<N>-p<page>-s<i>` (a PDF, one record per page section) or `att<N>-card`
// (no extractable text); every record of one .eml shares its `ax_file`; and
// `email_message_id` is stamped only when the message HAS a Message-ID header.
import test from 'node:test';
import assert from 'node:assert/strict';
import { makeReaderApi, ATTACHMENT_RECORDS_READ } from '../src/data/reader-api.js';
import { renderRecord } from '../src/ux/reader-render.js';
import { findAll, textOf } from '../src/ux/safe-dom.js';
import { assertInert } from './fixtures/inert.mjs';
import { miniTransport } from './fixtures/mini-engine.mjs';

const I = 'ax-docs';
const rec = (id, src) => ({ _index: I, _id: id, _source: src });
/** One .eml as autoindex writes it. `atts`: [{ name, pages? , card? }] */
function eml(tag, { mid, subject = tag, atts = [] } = {}) {
  const ax_file = `axf2-${tag}`;
  const base = { ax_file, ax_path: `inbox/${tag}.eml`, ax_format: 'eml' };
  const hdr = { email_subject: subject, email_from: 'dana@acme.example', ...(mid ? { email_message_id: mid } : {}) };
  const out = [
    rec(`${tag}-file`, { ...base, ax_locator: 'file', title: `${tag}.eml` }),
    rec(`${tag}-msg`, { ...base, ...hdr, ax_locator: 'msg-s0', title: subject, body: `body of ${tag}` }),
  ];
  atts.forEach((a, n) => {
    const link = { ...base, ...hdr, attachment_name: a.name, attachment_content_type: a.pages ? 'application/pdf' : 'text/plain', attachment_bytes: 1000 + n };
    if (a.pages) for (let p = 1; p <= a.pages; p++) out.push(rec(`${tag}-att${n}-p${p}`, { ...link, ax_locator: `att${n}-p${p}-s0`, page: p, body: `${a.name} page ${p}`, body_vector: [0.1, 0.2] }));
    else if (a.card) out.push(rec(`${tag}-att${n}-card`, { ...link, ax_locator: `att${n}-card` }));
    else out.push(rec(`${tag}-att${n}`, { ...link, ax_locator: `att${n}-s0`, body: `${a.name} text` }));
  });
  return out;
}
const byId = (docs, id) => docs.find((d) => d._id === id);

test('BLOCKER (PR #945 review): an email with a 300-page PDF and a trailing text file lists BOTH attachments', async () => {
  // Live truth: inbox/06-bigpdf.eml → 303 records, att=[('aaa-big.pdf',300),('zzz-last.txt',1)].
  // The reader fetched 200 attachment records, all of them pages of the PDF,
  // showed "ATTACHMENTS · 1" and said nothing about the rest.
  const docs = eml('06-bigpdf', { mid: '06@acme.example', atts: [{ name: 'aaa-big.pdf', pages: 300 }, { name: 'zzz-last.txt' }] });
  const t = miniTransport(docs);
  const rel = await makeReaderApi(t).fetchRelated(byId(docs, '06-bigpdf-msg'));
  assert.deepEqual(rel.attachments.map((a) => `${a._source.attachment_name}@${a._id}`), ['aaa-big.pdf@06-bigpdf-att0-p1', 'zzz-last.txt@06-bigpdf-att1'], 'every attachment, in the order the email carries them, the PDF at its lowest page');
  assert.equal(rel.attachmentsTruncated, null, '301 records were all read: nothing is missing, so nothing is claimed missing');
  // The join asks for the few fields a list entry shows — never 300 page bodies and their vectors.
  const join = t.calls.find((b) => JSON.stringify(b.query).includes('"exists":{"field":"attachment_name"}') && !JSON.stringify(b.query).includes('must_not'));
  assert.ok(Array.isArray(join._source) && !join._source.includes('body'), 'light _source');
  assert.ok(!rel.attachments.some((a) => 'body_vector' in a._source));
  const tree = renderRecord(byId(docs, '06-bigpdf-msg'), { related: rel });
  assertInert(tree);
  assert.ok(textOf(tree).includes('ATTACHMENTS · 2'));
  assert.ok(textOf(tree).includes('zzz-last.txt'));
});

test('BLOCKER: two attachments with the SAME file name are two entries, each opening its own record', async () => {
  // Live truth: inbox/04-samename.eml → att0-p1-s0 scan.pdf ("firstscan…") and att1-p1-s0 scan.pdf ("secondscan…").
  const docs = eml('04-samename', { mid: '04@acme.example', atts: [{ name: 'scan.pdf', pages: 2 }, { name: 'scan.pdf', pages: 1 }] });
  const rel = await makeReaderApi(miniTransport(docs)).fetchRelated(byId(docs, '04-samename-msg'));
  assert.deepEqual(rel.attachments.map((a) => a._id), ['04-samename-att0-p1', '04-samename-att1-p1']);
  const tree = renderRecord(byId(docs, '04-samename-msg'), { related: rel });
  assert.ok(textOf(tree).includes('ATTACHMENTS · 2'));
  const links = findAll(tree, (n) => n.tag === 'a' && n.attrs['data-reader-open'] && String(n.attrs.class).includes('rd-att')).map((n) => n.attrs['data-reader-open']);
  assert.deepEqual(links.filter((id) => id.includes('-att')), ['04-samename-att0-p1', '04-samename-att1-p1']);
});

test('MAJOR: an email WITHOUT a Message-ID still lists its attachments, and they link back to it', async () => {
  // Live truth: inbox/02-nomid.eml → 5 records, att=[('nomid.pdf',2),('nomid-notes.txt',1)], mid=[].
  const docs = [
    ...eml('02-nomid', { atts: [{ name: 'nomid-notes.txt' }, { name: 'nomid.pdf', pages: 2 }] }),
    ...eml('01-other', { atts: [{ name: 'other.txt' }] }), // another id-less email: must not be pooled
  ];
  const api = makeReaderApi(miniTransport(docs));
  const msg = byId(docs, '02-nomid-msg');
  assert.equal(msg._source.email_message_id, undefined);
  const rel = await api.fetchRelated(msg);
  assert.deepEqual(rel.attachments.map((a) => a._id), ['02-nomid-att0', '02-nomid-att1-p1']);
  const up = await api.fetchRelated(byId(docs, '02-nomid-att1-p2'));
  assert.equal(up.parent && up.parent._id, '02-nomid-msg', 'FROM EMAIL works without a Message-ID');
  assert.equal(up.fileRecord._id, '02-nomid-file');
});

test('MAJOR: two files with the SAME Message-ID do not pool their attachments', async () => {
  // Live truth: inbox/03-dup.eml and archive/03-dup.eml share <dup@acme.example>;
  // each has ONE attachment. The inbox copy listed both.
  const inbox = eml('03-dup-inbox', { mid: 'dup@acme.example', atts: [{ name: 'inbox-only.txt' }] });
  const archive = eml('03-dup-archive', { mid: 'dup@acme.example', atts: [{ name: 'archive-only.txt' }] });
  const docs = [...inbox, ...archive];
  const api = makeReaderApi(miniTransport(docs));
  assert.deepEqual((await api.fetchRelated(byId(docs, '03-dup-inbox-msg'))).attachments.map((a) => a._source.attachment_name), ['inbox-only.txt']);
  assert.deepEqual((await api.fetchRelated(byId(docs, '03-dup-archive-msg'))).attachments.map((a) => a._source.attachment_name), ['archive-only.txt']);
  assert.equal((await api.fetchRelated(byId(docs, '03-dup-archive-att0'))).parent._id, '03-dup-archive-msg', 'an attachment\'s parent is the message in ITS file');
});

test('records that carry no ax_file (not written by autoindex) still join on email_message_id, keyed by name', async () => {
  const docs = [
    rec('e', { email_subject: 's', email_message_id: 'm@x', body: 'b' }),
    rec('big-3', { attachment_name: 'big.pdf', page: 3, email_message_id: 'm@x' }),
    rec('big-1', { attachment_name: 'big.pdf', page: 1, email_message_id: 'm@x' }),
    rec('img', { attachment_name: 'logo.png', email_message_id: 'm@x' }),
    rec('else', { attachment_name: 'x.txt', email_message_id: 'other@x' }),
  ];
  const api = makeReaderApi(miniTransport(docs));
  assert.deepEqual((await api.fetchRelated(byId(docs, 'e'))).attachments.map((a) => a._id), ['big-1', 'img']);
  assert.equal((await api.fetchRelated(byId(docs, 'big-3'))).parent._id, 'e');
  assert.deepEqual((await api.fetchRelated(rec('lonely', { email_subject: 'no id, no file' }))).attachments, []);
});

test('whatever cap remains is SAID: past the read limit the list is marked incomplete, with the numbers', async () => {
  const pages = ATTACHMENT_RECORDS_READ + 40;
  const docs = eml('huge', { mid: 'h@x', atts: [{ name: 'huge.pdf', pages }, { name: 'after-the-cap.txt' }] });
  const t = miniTransport(docs);
  const rel = await makeReaderApi(t).fetchRelated(byId(docs, 'huge-msg'));
  assert.deepEqual(rel.attachmentsTruncated, { read: ATTACHMENT_RECORDS_READ, total: pages + 1 });
  assert.ok(rel.attachments.length >= 1);
  const tree = renderRecord(byId(docs, 'huge-msg'), { related: rel });
  assertInert(tree);
  assert.match(textOf(tree), /ATTACHMENTS · \d+\+/);
  assert.ok(textOf(tree).includes(`${pages + 1}`) && /not listed/.test(textOf(tree)), 'says how many records exist and that some attachments may be missing');
  // and the view state keeps it: reader-view.js used to copy four named keys and drop `truncated`
  const { relatedForView } = await import('../src/ux/reader-view.js');
  assert.deepEqual(relatedForView(rel, byId(docs, 'huge-msg')).attachmentsTruncated, rel.attachmentsTruncated);
});

test('a failing join is not an empty list: the email says its attachments could not be read', async () => {
  const docs = eml('05', { mid: '5@x', atts: [{ name: 'a.txt' }] });
  const t = miniTransport(docs);
  const inner = t.search;
  t.search = async (index, body) => { if (JSON.stringify(body.query).includes('attachment_name') && body._source) throw Object.assign(new Error('x'), { kind: 'network' }); return inner(index, body); };
  const rel = await makeReaderApi(t).fetchRelated(byId(docs, '05-msg'));
  assert.deepEqual(rel.attachments, []);
  assert.equal(rel.attachmentsError, 'engine unreachable');
  assert.ok(textOf(renderRecord(byId(docs, '05-msg'), { related: rel })).includes('could not be read'));
});

test('search(): TERM / RANGE free text sends NOTHING and says why; a sample query\'s own field is searched; `size` is passed', async () => {
  const sent = [];
  const t = {
    guest: false,
    async mapping() { return { [I]: { mappings: { properties: { body: { type: 'text' }, text: { type: 'semantic_text' }, email_subject: { type: 'keyword' }, ax_path: { type: 'keyword' } } } } }; },
    async search(index, body) { sent.push(body); return { took: 3, hits: { total: { value: 0 }, hits: [] } }; },
  };
  const api = makeReaderApi(t);
  // PR #945 review: TERM `zebrafish` answered "361 RESULTS" — the whole index, via match_all.
  const refused = await api.search(I, { q: 'zebrafish', type: 'term' });
  assert.deepEqual([refused.hits, refused.total, sent.length], [[], 0, 0]);
  assert.match(refused.hint, /TERM needs field=value/);
  assert.match((await api.search(I, { q: 'hello', type: 'range' })).hint, /RANGE needs/);
  assert.equal(sent.length, 0, 'not one request was made for either');
  await api.search(I, { q: 'ax_path=a/b', type: 'term' });
  assert.deepEqual(sent.at(-1).query, { term: { ax_path: 'a/b' } });
  // every text field, then the sample's field when it is not already among them
  const r = await api.search(I, { q: 'makefiletargetword', type: 'match', size: 50 });
  assert.deepEqual(r.fields, ['body', 'text', 'email_subject']);
  assert.equal(sent.at(-1).size, 50);
  assert.deepEqual(Object.keys(sent.at(-1).highlight.fields), ['body', 'text', 'email_subject']);
  const viaSample = await api.search(I, { q: 'inbox', type: 'match', field: 'ax_path' });
  assert.deepEqual(viaSample.fields, ['body', 'text', 'email_subject', 'ax_path']);
  assert.deepEqual(sent.at(-1).query.bool.should.at(-1), { wildcard: { ax_path: { value: '*inbox*', case_insensitive: true } } });
});

test('minor (PR #945 review): the OPERATOR transport names a dead engine "engine unreachable" — not the browser\'s "Failed to fetch"', async () => {
  const { makeConsoleTransport } = await import('../src/data/transport-console.js');
  const realFetch = globalThis.fetch;
  globalThis.fetch = async () => { throw new TypeError('Failed to fetch'); };
  try {
    const api = makeReaderApi(makeConsoleTransport());
    const r = await api.search(I, { q: 'x' });
    assert.deepEqual([r.hits, r.error, r.kind], [[], 'engine unreachable', 'network']);
    const rec = await api.fetchRecord(I, 'abc');
    assert.deepEqual([rec.hit, rec.error], [null, 'engine unreachable']);
    assert.equal((await api.fetchGraph('b', { _id: 'x', _index: I, _source: {} }, null)).error, 'engine unreachable');
    // an aborted request is still an abort, not a network error
    globalThis.fetch = async () => { throw Object.assign(new Error('aborted'), { name: 'AbortError' }); };
    await assert.rejects(() => makeConsoleTransport().search(I, {}), { name: 'AbortError' });
  } finally { globalThis.fetch = realFetch; }
});
