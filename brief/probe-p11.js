const puppeteer = require('puppeteer');
const path = require('path');
(async () => {
  const b = await puppeteer.launch({ headless: true, args: ['--no-sandbox'] });
  const p = await b.newPage();
  await p.emulateMediaType('print');
  await p.setViewport({ width: 816, height: 1056 });
  await p.goto('file://' + path.resolve('/home/claude/ai/xerj.ai/brief/_tech-brief.rendered.html'), { waitUntil: 'networkidle0' });
  await p.evaluateHandle('document.fonts.ready');
  const out = await p.evaluate(() => {
    const page = document.querySelectorAll('.page')[10]; // zero-indexed page 11
    const inner = page.querySelector('.grow.flex.col');
    const kids = [...inner.children].map(c => {
      const r = c.getBoundingClientRect();
      return {
        tag: c.tagName + (c.className ? '.' + c.className.split(' ').slice(0,2).join('.') : ''),
        top: Math.round(r.top),
        bottom: Math.round(r.bottom),
        h: Math.round(r.height),
      };
    });
    const pageRect = page.getBoundingClientRect();
    return { pageTop: pageRect.top, pageBottom: pageRect.bottom, kids };
  });
  console.log('page top/bottom:', out.pageTop, out.pageBottom);
  out.kids.forEach((c,i)=>console.log(`  ${i}: ${c.tag.padEnd(30)} top=${c.top} bot=${c.bottom} h=${c.h}`));
  await b.close();
})();
