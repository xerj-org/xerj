// ux/safe-dom.js — the no-HTML-parser DOM builder the corpus home, the
// reader and the guest shell render through.  Run: node --test xerj-ux/test/
import test from 'node:test';
import assert from 'node:assert/strict';
import { h, safeHref, attrAllowed, highlightChildren, highlightRequest, textOf, findAll, TAGS, ATTRS, HL_PRE, HL_POST } from '../src/ux/safe-dom.js';
import { PAYLOADS, HOSTILE_FRAGMENTS } from './fixtures/hostile.mjs';
import { assertInert, FORBIDDEN_TAGS, FORBIDDEN_ATTRS } from './fixtures/inert.mjs';

const CTRL = String.fromCharCode(1);
const TAB = String.fromCharCode(9);
const NL = String.fromCharCode(10);

test('a string child is always text, whatever it contains', () => {
  for (const [name, p] of Object.entries(PAYLOADS)) {
    const n = h('div', { title: p, 'data-x': p }, p);
    assert.deepEqual(n.children, [p], name);
    assert.equal(n.attrs.title, p, 'an attribute value is stored verbatim; setAttribute makes it inert');
    assert.equal(textOf(n), p);
    assertInert(n, name);
  }
});

test('the tag and attribute allow-lists contain nothing that loads, runs or submits', () => {
  for (const t of FORBIDDEN_TAGS) assert.ok(!TAGS.has(t), `<${t}> must not be allowed`);
  for (const a of FORBIDDEN_ATTRS) assert.ok(!ATTRS.has(a), `${a}= must not be allowed`);
  for (const a of ATTRS) assert.ok(!/^on/i.test(a), `${a} looks like an event handler`);
});

test('an unknown tag or attribute throws instead of rendering', () => {
  for (const t of FORBIDDEN_TAGS) assert.throws(() => h(t, null, 'x'), /not allowed/, t);
  for (const a of ['onclick', 'onerror', 'ONLOAD', 'style', 'src', 'srcdoc', 'formaction', 'xlink:href', 'data-', 'data-UPPER', 'aria-x y']) {
    assert.throws(() => h('div', { [a]: 'x' }), /not allowed/, a);
  }
  assert.throws(() => h('input', { type: 'file' }), /not allowed/);
  assert.throws(() => h('input', { type: 'password' }), /not allowed/);
  assert.throws(() => h('button', { type: 'submit' }), /not allowed/);
  assert.equal(h('button', null, 'x').attrs.type, 'button', 'a button can never submit');
  assert.ok(attrAllowed('data-reader-open') && attrAllowed('aria-label'));
  assert.ok(!attrAllowed('onclick') && !attrAllowed('data-') && !attrAllowed(null));
});

test('href survives only as an in-app hash route', () => {
  const bad = [
    PAYLOADS.jsUrl, PAYLOADS.jsUrlObf, PAYLOADS.dataUrl, PAYLOADS.vbUrl,
    'JaVaScRiPt:alert(1)', `${CTRL}javascript:alert(1)`, ' #/reader', `${TAB}#/reader`, '#', '#reader', `#/reader${NL}javascript:alert(1)`,
    '//evil.example/x', 'https://evil.example/', 'http://localhost/', '/_xerj-console/api/v1/me', 'mailto:a@b', 'blob:x', 'file:///etc/passwd',
    '', null, undefined, 7, {}, ['#/x'],
  ];
  for (const v of bad) {
    assert.equal(safeHref(v), null, JSON.stringify(v));
    const a = h('a', { href: v }, 'x');
    assert.ok(!('href' in a.attrs), `href ${JSON.stringify(v)} must be dropped, leaving an inert <a>`);
  }
  assert.equal(safeHref('#/reader?index=a&id=b'), '#/reader?index=a&id=b');
  assert.equal(h('a', { href: '#/corpus' }, 'x').attrs.href, '#/corpus');
});

test('highlight fragments become text + <mark>, never markup', () => {
  for (const frag of HOSTILE_FRAGMENTS) {
    for (const tags of ['markers', 'em']) {
      const kids = highlightChildren(frag, { tags });
      const wrap = h('span', null, kids);
      assertInert(wrap, `fragment ${JSON.stringify(frag.slice(0, 30))} (${tags})`);
      for (const k of kids) {
        if (typeof k === 'string') continue;
        assert.equal(k.tag, 'mark');
        assert.deepEqual(Object.keys(k.attrs), []);
        assert.ok(k.children.every((c) => typeof c === 'string'), 'a <mark> holds text only');
      }
      const text = textOf(wrap);
      assert.ok(!text.includes(HL_PRE) && !text.includes(HL_POST), 'delimiters are never displayed');
    }
  }
  const kids = highlightChildren(`pay the ${HL_PRE}invoice${HL_POST} <b>now</b>`);
  assert.deepEqual(kids.map((k) => (typeof k === 'string' ? k : `[${textOf(k)}]`)), ['pay the ', '[invoice]', ' <b>now</b>']);
  // `<em>` mode matches the two tags literally and nothing else.
  const em = highlightChildren('a <em>b</em> <img src=x onerror=1> <EM>c</EM>', { tags: 'em' });
  assert.deepEqual(em.map((k) => (typeof k === 'string' ? k : `[${textOf(k)}]`)), ['a ', '[b]', ' <img src=x onerror=1> <EM>c</EM>']);
  assert.deepEqual(highlightChildren(null), []);
  assert.equal(findAll(h('span', null, highlightChildren(HOSTILE_FRAGMENTS[1])), (n) => n.tag !== 'span' && n.tag !== 'mark').length, 0);
});

test('the highlight request names our delimiters in BOTH spellings', () => {
  // The ES-compat route reads pre_tags/post_tags; the console search proxy
  // reads pre_tag/post_tag (xerj-console-api/tests/reader_proxy.rs).
  const r = highlightRequest(['body', '', null, 'email_subject']);
  assert.deepEqual(r.pre_tags, [HL_PRE]);
  assert.deepEqual(r.post_tags, [HL_POST]);
  assert.equal(r.pre_tag, HL_PRE);
  assert.equal(r.post_tag, HL_POST);
  assert.deepEqual(Object.keys(r.fields), ['body', 'email_subject']);
  assert.equal(HL_PRE.charCodeAt(0), 0xe000);
  assert.equal(HL_POST.charCodeAt(0), 0xe001);
});
