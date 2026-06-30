/* XERJ Engine tech brief — same SVG-injection pipeline as exec. */
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');

(async () => {
  const tplPath = path.resolve(__dirname, 'tech-brief.html');
  const svgDir  = path.resolve(__dirname, 'svg');
  const outHtml = path.resolve(__dirname, '_tech-brief.rendered.html');
  const outPdf  = path.resolve(__dirname, 'xerj-tech-brief.pdf');

  let html = fs.readFileSync(tplPath, 'utf8');
  html = html.replace(/\{\{SVG:([a-z_-]+)\}\}/g, (_, name) => {
    const p = path.join(svgDir, name + '.html');
    if (!fs.existsSync(p)) throw new Error('missing SVG: ' + name);
    let svg = fs.readFileSync(p, 'utf8');
    svg = svg.replace(/(<svg\b[^>]*\bstyle=")([^"]*?)(")/g, (m, a, body, c) => {
      const cleaned = body
        .replace(/height\s*:\s*[^;"]+;?/ig, '')
        .replace(/overflow\s*:\s*visible;?/ig, 'overflow:hidden;')
        .trim()
        .replace(/;+$/, '');
      return a + cleaned + c;
    });
    const secIdx = svg.indexOf('</section>');
    if (secIdx !== -1) svg = svg.slice(0, secIdx);
    // Balance <div> opens vs closes — strip excess trailing </div>s only.
    const opens  = (svg.match(/<div\b/g) || []).length;
    const closes = (svg.match(/<\/div>/g) || []).length;
    let excess = closes - opens;
    while (excess-- > 0) {
      svg = svg.replace(/\s*<\/div>\s*$/, '');
    }
    return svg;
  });
  fs.writeFileSync(outHtml, html);

  const browser = await puppeteer.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--font-render-hinting=none'],
  });
  try {
    const page = await browser.newPage();
    await page.emulateMediaType('print');
    await page.goto('file://' + outHtml, { waitUntil: 'networkidle0', timeout: 60000 });
    await page.evaluateHandle('document.fonts.ready');
    await page.pdf({
      path: outPdf,
      format: 'Letter',
      printBackground: true,
      preferCSSPageSize: true,
      margin: { top: 0, right: 0, bottom: 0, left: 0 },
    });
    console.log('OK:', outPdf, fs.statSync(outPdf).size, 'bytes');
  } finally {
    await browser.close();
  }
})();
