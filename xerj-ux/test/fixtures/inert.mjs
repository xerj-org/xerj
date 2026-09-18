// Assertions shared by the render tests: is this safe-dom node tree inert?
import assert from 'node:assert/strict';
import { TAGS, attrAllowed, walk, textOf } from '../../src/ux/safe-dom.js';

/** Tags that load a resource, run script, or submit somewhere. None of them
 *  is in TAGS; listed so a future "just add <img>" fails HERE with a reason. */
export const FORBIDDEN_TAGS = ['script', 'img', 'svg', 'iframe', 'object', 'embed', 'style', 'link', 'meta', 'base', 'form', 'video', 'audio', 'source', 'math', 'template', 'frame', 'frameset', 'applet', 'textarea', 'details', 'animate', 'image'];
export const FORBIDDEN_ATTRS = ['style', 'src', 'srcset', 'srcdoc', 'action', 'formaction', 'background', 'poster', 'ping', 'xlink:href', 'target', 'download', 'is', 'nonce', 'integrity', 'sandbox', 'allow'];

export function assertInert(tree, label = 'tree') {
  let nodes = 0;
  walk(tree, (n) => {
    nodes++;
    assert.ok(TAGS.has(n.tag), `${label}: <${n.tag}> is not an allowed tag`);
    assert.ok(!FORBIDDEN_TAGS.includes(n.tag), `${label}: <${n.tag}> must never be rendered`);
    for (const [k, v] of Object.entries(n.attrs)) {
      assert.ok(attrAllowed(k), `${label}: attribute ${k} is not allowed`);
      assert.ok(!/^on/i.test(k), `${label}: event-handler attribute ${k}`);
      assert.ok(!FORBIDDEN_ATTRS.includes(k.toLowerCase()), `${label}: attribute ${k} must never be set`);
      assert.equal(typeof v, 'string', `${label}: attribute ${k} must be a string`);
      if (k === 'href') assert.ok(v.startsWith('#/'), `${label}: href must be an in-app hash route, got ${JSON.stringify(v.slice(0, 60))}`);
    }
    for (const c of n.children) {
      assert.ok(typeof c === 'string' || (c && c.$ === 'node'), `${label}: child is neither text nor a node`);
    }
  });
  assert.ok(nodes > 0, `${label}: rendered nothing`);
  return textOf(tree);
}

/** The payload must be on the page AS TEXT (so the person sees what the
 *  document really says) — not dropped, and not turned into elements. */
export function assertShownAsText(tree, needle, label = 'tree') {
  const text = textOf(tree);
  assert.ok(text.includes(needle), `${label}: expected the literal text ${JSON.stringify(needle.slice(0, 50))}… to be displayed`);
}
