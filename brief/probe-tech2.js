/* probe — check rendered heights of every page and identify overflow */
const puppeteer = require('puppeteer');
const path = require('path');
(async () => {
  const inPath = path.resolve(__dirname, '_tech-brief.rendered.html');
  const b = await puppeteer.launch({ headless: true, args: ['--no-sandbox'] });
  const p = await b.newPage();
  await p.emulateMediaType('print');
  await p.setViewport({ width: 816, height: 1056 });
  await p.goto('file://' + inPath, { waitUntil: 'networkidle0' });
  await p.evaluateHandle('document.fonts.ready');
  const out = await p.evaluate(() => {
    const pages = [...document.querySelectorAll('.page')];
    return pages.map((pg, i) => {
      const over = pg.scrollHeight > pg.clientHeight;
      const inner = pg.querySelector('.grow.flex.col');
      const innerOver = inner && inner.scrollHeight > inner.clientHeight;
      return {
        i: i+1,
        h: pg.clientHeight,
        sH: pg.scrollHeight,
        over,
        innerH: inner ? inner.clientHeight : 0,
        innerSH: inner ? inner.scrollHeight : 0,
        innerOver,
      };
    });
  });
  for (const pg of out) {
    const tag = pg.over || pg.innerOver ? '  OVER' : '';
    console.log(`page ${String(pg.i).padStart(2)}: h=${pg.h} scrollH=${pg.sH}  inner h=${pg.innerH} scrollH=${pg.innerSH}${tag}`);
  }
  await b.close();
})();
