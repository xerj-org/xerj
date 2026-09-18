// data/reader-api.js — the file record, and why an email's graph panel walks
// its FILE's links too.  Run: node --test xerj-ux/test/
//
// Shapes below are copied from a real `xerj brain` run over a folder of .eml
// files (docs/usecases/console-reader/): autoindex writes ONE record per file
// (`ax_locator: "file"`, no body) plus one per message / attachment page, and
// its file-level detectors (same_dir, mdlink, pathcite) link the FILE records.
import test from 'node:test';
import assert from 'node:assert/strict';
import { makeReaderApi } from '../src/data/reader-api.js';
import { detectShape, renderRecord, renderGraphPanel, shapeBadge, recordTitle } from '../src/ux/reader-render.js';
import { findAll, textOf } from '../src/ux/safe-dom.js';
import { assertInert } from './fixtures/inert.mjs';
import { PAYLOADS } from './fixtures/hostile.mjs';

const AF = 'axf2-62965698abafaaee54016c8a0d33d97c-9ebe6';
const fileRec = { _index: 'ax-docs', _id: 'file-05', _source: { title: '05.eml', ax_path: `inbox/05${PAYLOADS.imgOnerror}.eml`, ax_file: AF, ax_locator: 'file', ax_format: 'eml', ax_dataset: 'docs' } };
const msg = { _index: 'ax-docs', _id: 'msg-05', _source: { email_subject: 'Overdue invoice', email_from: 'm@evil.example', email_message_id: 'hostile-5@evil.example', body: 'pay now', ax_file: AF, ax_locator: 'msg-s0', ax_format: 'eml' } };
const att = (p, sec = 0) => ({ _index: 'ax-docs', _id: `att-p${p}-s${sec}`, _source: { attachment_name: 'brief.pdf', page: p, body: `page ${p}`, email_message_id: 'hostile-5@evil.example', ax_file: AF, ax_locator: `att0-p${p}-s${sec}` } });
const DOCS = [fileRec, msg, att(10), att(2), att(1), att(3, 1), att(3, 0)];

/** A transport that answers the reader's own queries over DOCS. */
function transport({ egoByNode = {}, guest = false } = {}) {
  const calls = [];
  return {
    calls, guest, brain: guest ? 'casefile' : undefined,
    async search(index, body) {
      calls.push(body);
      const f = body.query.bool ? body.query.bool.filter || [] : [];
      const not = body.query.bool ? body.query.bool.must_not || [] : [];
      const match = (d, c) => (c.term ? Object.entries(c.term).every(([k, v]) => d._source[k] === v) : c.exists ? d._source[c.exists.field] != null : true);
      let hits = body.query.ids ? DOCS.filter((d) => body.query.ids.values.includes(d._id)) : DOCS.filter((d) => f.every((c) => match(d, c)) && !not.some((c) => match(d, c)));
      hits = hits.slice(0, body.size ?? 10);
      return { hits: { total: { value: hits.length }, hits } };
    },
    async ego(brain, params) {
      calls.push({ ego: params.node });
      const body = egoByNode[params.node];
      return body ? { status: 200, body } : { status: 200, body: { edges: [], nodes: {}, not_shown: {} } };
    },
  };
}

test('the file record is its own shape and lists what came out of the file, in reading order', async () => {
  assert.equal(detectShape(fileRec._source), 'file');
  assert.equal(detectShape({ ax_locator: 'file', ax_file: 'x', body: 'a note that is its own file node' }), 'note', 'a file record WITH text is shown as that text');
  assert.match(shapeBadge('file', fileRec._source), /^FILE · EML$/);
  const api = makeReaderApi(transport());
  const rel = await api.fetchRelated(fileRec);
  assert.deepEqual(rel.siblings.map((h) => h._id), ['msg-05', 'att-p1-s0', 'att-p2-s0', 'att-p3-s0', 'att-p3-s1', 'att-p10-s0'], 'message first, then pages numerically (10 after 3)');
  assert.ok(!rel.siblings.some((h) => h._id === 'file-05'));
  const tree = renderRecord(fileRec, { related: rel });
  assertInert(tree, 'file record');
  assert.ok(textOf(tree).includes('RECORDS IN THIS FILE · 6'));
  assert.ok(textOf(tree).includes(PAYLOADS.imgOnerror), 'a hostile PATH is text');
  assert.equal(findAll(tree, (n) => n.tag === 'a').length, 6);
  assert.equal(recordTitle(fileRec._source, 'x'), fileRec._source.ax_path);
  assert.ok(textOf(renderRecord(fileRec, { related: {} })).includes('looking…'));
});

test('any other record links UP to its file', async () => {
  const api = makeReaderApi(transport());
  const rel = await api.fetchRelated(msg);
  assert.equal(rel.fileRecord._id, 'file-05');
  assert.deepEqual(rel.attachments.map((a) => a._id), ['att-p1-s0'], 'one entry per attachment, at its lowest page');
  const tree = renderRecord(msg, { related: rel });
  assertInert(tree);
  assert.ok(textOf(tree).includes('FROM FILE'));
  const relAtt = await api.fetchRelated(att(2));
  assert.equal(relAtt.fileRecord._id, 'file-05');
  assert.equal(relAtt.parent._id, 'msg-05');
});

test('the graph panel of an email includes its FILE\'s links, and says so', async () => {
  const egoByNode = {
    'msg-05': { edges: [], nodes: {}, not_shown: { dangling_ids: [] } },
    'file-05': {
      edges: [
        { src: 'file-04', dst: 'file-05', type: 'same_dir', hop: 1, edge_id: 'e1' },
        { src: 'file-05', dst: 'file-06', type: 'same_dir', hop: 1, edge_id: 'e2' },
        { src: 'file-05', dst: 'msg-05', type: 'same_dir', hop: 1, edge_id: 'self' },
      ],
      nodes: { 'file-04': { index: 'ax-docs', title: '04.eml' }, 'file-06': { index: 'ax-docs', title: PAYLOADS.script } },
      not_shown: {},
    },
  };
  const t = transport({ egoByNode });
  const api = makeReaderApi(t);
  const g = await api.fetchGraph('casefile', msg, fileRec);
  assert.equal(g.status, 'ok');
  assert.equal(g.viaFile, 2);
  assert.deepEqual(g.groups[0].items.map((i) => `${i.id}:${i.via}`), ['file-04:file', 'file-06:file'], 'the record itself is never its own neighbour');
  assert.deepEqual(t.calls.filter((c) => c.ego).map((c) => c.ego), ['msg-05', 'file-05']);
  const panel = renderGraphPanel({ ...g, brain: 'casefile', index: 'ax-docs' });
  assertInert(panel, 'merged graph');
  assert.match(textOf(panel), /2 linked records · brain casefile · 1 hop · 2 of them are links of the file this record came from/);

  // the file record itself: one walk, nothing marked
  const own = await api.fetchGraph('casefile', fileRec, null);
  assert.equal(own.viaFile, undefined);
  assert.equal(own.groups[0].items.length, 3);
  // no file record known → exactly the record's own answer
  assert.equal((await api.fetchGraph('casefile', msg, null)).status, 'no-links');
});

test('a refused graph is asked for ONCE — not again through the file', async () => {
  const t = transport();
  t.ego = async (b, params) => { t.calls.push({ ego: params.node, nodes: params.include_nodes }); return { status: 403, body: null }; };
  const g = await makeReaderApi(t).fetchGraph('casefile', msg, fileRec);
  assert.equal(g.status, 'denied');
  assert.deepEqual(t.calls.filter((c) => c.ego).map((c) => c.ego), ['msg-05', 'msg-05'], 'one retry without node summaries, then stop');
});
