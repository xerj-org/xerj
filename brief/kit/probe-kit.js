/* Probe every rendered HTML in kit/src for page overflow. */
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');

(async () => {
  const srcDir = path.resolve(__dirname, 'src');
  const files = fs.readdirSync(srcDir).filter(f => f.endsWith('.html')).sort();
  const browser = await puppeteer.launch({ headless: true, args: ['--no-sandbox'] });
  let anyOver = 0;
  for (const f of files) {
    const page = await browser.newPage();
    await page.emulateMediaType('print');
    await page.setViewport({ width: 816, height: 1056 });
    await page.goto('file://' + path.join(srcDir, f), { waitUntil: 'networkidle0' });
    await page.evaluateHandle('document.fonts.ready');
    const issues = await page.evaluate(() => {
      const pages = [...document.querySelectorAll('.page')];
      return pages.map((pg, i) => {
        const inner = pg.querySelector('.grow.flex.col');
        return {
          i: i + 1,
          ok: pg.scrollHeight === pg.clientHeight
              && (!inner || inner.scrollHeight <= inner.clientHeight),
          pageH: pg.clientHeight,
          pageSH: pg.scrollHeight,
          innerH: inner ? inner.clientHeight : 0,
          innerSH: inner ? inner.scrollHeight : 0,
        };
      });
    });
    const bad = issues.filter(x => !x.ok);
    if (bad.length) {
      console.log(f);
      for (const b of bad) {
        console.log(`  p${b.i}: pageH=${b.pageH} pageSH=${b.pageSH} innerH=${b.innerH} innerSH=${b.innerSH}`);
        anyOver++;
      }
    } else {
      console.log(f + '  ok (' + issues.length + ' pages)');
    }
    await page.close();
  }
  await browser.close();
  if (!anyOver) console.log('\nALL BRIEFS FIT');
})();
