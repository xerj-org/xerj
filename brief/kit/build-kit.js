/* Build script for the XERJ sales kit.
   Renders every .html in /brief/kit/src/ to PDF under /brief/kit/. */
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');

const srcDir = path.resolve(__dirname, 'src');
const outDir = path.resolve(__dirname);
const files = fs.readdirSync(srcDir).filter(f => f.endsWith('.html')).sort();

(async () => {
  const browser = await puppeteer.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--font-render-hinting=none'],
  });
  try {
    for (const f of files) {
      const inPath = path.join(srcDir, f);
      const outPdf = path.join(outDir, 'xerj-' + f.replace(/\.html$/, '.pdf'));
      const page = await browser.newPage();
      await page.emulateMediaType('print');
      await page.goto('file://' + inPath, { waitUntil: 'networkidle0', timeout: 60000 });
      await page.evaluateHandle('document.fonts.ready');
      await page.pdf({
        path: outPdf,
        format: 'Letter',
        printBackground: true,
        preferCSSPageSize: true,
        margin: { top: 0, right: 0, bottom: 0, left: 0 },
      });
      await page.close();
      const stat = fs.statSync(outPdf);
      console.log('OK:', path.basename(outPdf), stat.size, 'bytes');
    }
  } finally {
    await browser.close();
  }
})();
