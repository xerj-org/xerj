// Exercise the shipped script through its DOM click handler, without a browser dependency.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { test } = require('node:test');
const source = fs.readFileSync(path.join(__dirname, '../../landing/interact.js'), 'utf8');

function fixture(modern, succeeds) {
  let click;
  let success = succeeds;
  const timers = new Map();
  const children = [];
  const writes = [];
  const button = {
    textContent: 'COPY THE PROMPT',
    getAttribute: () => 'a "quoted" prompt',
    addEventListener: (_, handler) => { click = handler; },
  };
  const document = {
    documentElement: {},
    querySelectorAll: selector => selector === '[data-copy]' ? [button] : [],
    createElement: () => ({ style: {}, setAttribute() {}, select() {} }),
    body: {
      appendChild: element => children.push(element),
      removeChild: element => children.splice(children.indexOf(element), 1),
    },
    execCommand: command => {
      assert.equal(command, 'copy');
      writes.push(children[0].value);
      if (success === 'throw') throw Error('clipboard denied');
      return success;
    },
  };
  const navigator = modern ? { clipboard: { writeText: text => {
    writes.push(text);
    return success ? Promise.resolve() : Promise.reject(Error('clipboard denied'));
  } } } : {};
  let nextTimer = 0;
  vm.runInNewContext(source, {
    document, navigator,
    setTimeout: handler => { timers.set(++nextTimer, handler); return nextTimer; },
    clearTimeout: id => timers.delete(id),
  });
  return {
    button, children, writes,
    async click() { click(); await Promise.resolve(); await Promise.resolve(); },
    succeed() { success = true; },
    expire() { const pending = [...timers.values()]; timers.clear(); pending.forEach(handler => handler()); },
  };
}

for (const [name, modern, success] of [
  ['modern rejection', true, false],
  ['legacy false result', false, false],
  ['legacy exception', false, 'throw'],
]) {
  test(`${name} reports failure and allows a successful retry`, async () => {
    const page = fixture(modern, success);
    await page.click();
    assert.equal(page.button.textContent, 'COPY FAILED');
    assert.equal(page.children.length, 0);
    page.succeed();
    await page.click();
    assert.equal(page.button.textContent, 'COPIED');
    assert.deepEqual(page.writes, ['a "quoted" prompt', 'a "quoted" prompt']);
    page.expire();
    assert.equal(page.button.textContent, 'COPY THE PROMPT');
  });
}
for (const modern of [true, false]) {
  test(`${modern ? 'modern' : 'legacy'} successful copies restore the original label after repeated clicks`, async () => {
    const page = fixture(modern, true);
    await page.click();
    assert.equal(page.button.textContent, 'COPIED');
    await page.click();
    page.expire();
    assert.equal(page.button.textContent, 'COPY THE PROMPT');
    assert.equal(page.children.length, 0);
  });
}
