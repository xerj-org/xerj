/* XERJ.AI product brief — HTML → PDF via puppeteer.
   Renders brief.html at exact Letter size with print CSS. */
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');

(async () => {
  const inPath  = path.resolve(__dirname, 'brief.html');
  const outPath = path.resolve(__dirname, 'xerj-product-brief.pdf');

  if (!fs.existsSync(inPath)) {
    console.error('missing', inPath);
    process.exit(1);
  }

  const browser = await puppeteer.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--font-render-hinting=none'],
  });

  try {
    const page = await browser.newPage();
    await page.emulateMediaType('print');
    await page.goto('file://' + inPath, { waitUntil: 'networkidle0', timeout: 60000 });
    await page.evaluateHandle('document.fonts.ready');

    await page.pdf({
      path: outPath,
      format: 'Letter',
      printBackground: true,
      preferCSSPageSize: true,
      margin: { top: 0, right: 0, bottom: 0, left: 0 },
    });

    const stat = fs.statSync(outPath);
    console.log('OK:', outPath, stat.size, 'bytes');
  } finally {
    await browser.close();
  }
})();
