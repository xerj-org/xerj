/* probe layout — report computed heights of each .page's children */
const puppeteer = require('puppeteer');
const path = require('path');

(async () => {
  const inPath = path.resolve(__dirname, '_exec-brief.rendered.html');
  const b = await puppeteer.launch({ headless: true, args: ['--no-sandbox'] });
  const p = await b.newPage();
  await p.emulateMediaType('print');
  await p.setViewport({ width: 816, height: 1056 }); // letter at 96dpi
  await p.goto('file://' + inPath, { waitUntil: 'networkidle0' });
  await p.evaluateHandle('document.fonts.ready');

  const out = await p.evaluate(() => {
    const pages = [...document.querySelectorAll('.page')];
    return pages.map((pg, i) => {
      const r = pg.getBoundingClientRect();
      const children = [...pg.children].map(c => {
        const cr = c.getBoundingClientRect();
        return {
          tag: c.tagName + (c.className ? '.' + c.className.split(' ').slice(0,2).join('.') : ''),
          h: Math.round(cr.height),
          scrollH: c.scrollHeight,
        };
      });
      return { i, h: Math.round(r.height), scrollH: pg.scrollHeight, children };
    });
  });
  console.log(JSON.stringify(out, null, 2));
  await b.close();
})();
