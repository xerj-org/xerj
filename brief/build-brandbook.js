/* XERJ.AI Brand Book V1 → PDF.
   Renders a dedicated, print-structured HTML (41 pages of cover /
   colophon / contents / 7 chapters / back cover) to a single PDF.
   This is NOT a print of the website — it's a book. */
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');

const SRC = 'file://' + path.resolve(__dirname, 'brandbook-pdf/brandbook.html');
const OUT = path.resolve(__dirname, 'xerj-brandbook.pdf');

(async () => {
  const browser = await puppeteer.launch({
    headless: true,
    args: [
      '--no-sandbox',
      '--disable-setuid-sandbox',
      '--font-render-hinting=none',
    ],
  });
  try {
    const page = await browser.newPage();
    await page.emulateMediaType('print');
    console.log('→ loading', SRC);
    await page.goto(SRC, { waitUntil: 'networkidle0', timeout: 90000 });
    await page.evaluateHandle('document.fonts.ready');
    await page.evaluate(() => new Promise((r) => setTimeout(r, 400)));

    console.log('→ rendering PDF');
    await page.pdf({
      path: OUT,
      width:  '8.5in',
      height: '11in',
      printBackground: true,
      preferCSSPageSize: true,
      margin: { top: 0, right: 0, bottom: 0, left: 0 },
    });
    const size = fs.statSync(OUT).size;
    console.log('OK:', OUT, `${(size / 1024).toFixed(1)} KB`);
  } finally {
    await browser.close();
  }
})().catch((err) => {
  console.error('FAILED:', err);
  process.exit(1);
});
