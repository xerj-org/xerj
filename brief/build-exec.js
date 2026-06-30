/* XERJ.AI executive brief — HTML → PDF.
   Substitutes {{SVG:name}} markers with file contents from svg/, then renders. */
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');

(async () => {
  const tplPath = path.resolve(__dirname, 'exec-brief.html');
  const svgDir  = path.resolve(__dirname, 'svg');
  const outHtml = path.resolve(__dirname, '_exec-brief.rendered.html');
  const outPdf  = path.resolve(__dirname, 'xerj-exec-brief.pdf');

  let html = fs.readFileSync(tplPath, 'utf8');
  html = html.replace(/\{\{SVG:([a-z_-]+)\}\}/g, (_, name) => {
    const p = path.join(svgDir, name + '.html');
    if (!fs.existsSync(p)) throw new Error('missing SVG: ' + name);
    let svg = fs.readFileSync(p, 'utf8');
    // Strip pixel heights from SVG style attrs so CSS rules win.
    svg = svg.replace(/(<svg\b[^>]*\bstyle=")([^"]*?)(")/g, (m, a, body, c) => {
      const cleaned = body
        .replace(/height\s*:\s*[^;"]+;?/ig, '')
        .replace(/overflow\s*:\s*visible;?/ig, 'overflow:hidden;')
        .trim()
        .replace(/;+$/, '');
      return a + cleaned + c;
    });
    // Extracted files include closers from the original product.html.
    // Only strip trailing </div>s when we also saw a </section> — those
    // </div>s were closing scene-section wrappers that don't exist here.
    // Otherwise (e.g. heatmap.html) the </div>s legitimately close the
    // viz's own structure and must be kept.
    const secIdx = svg.indexOf('</section>');
    if (secIdx !== -1) {
      svg = svg.slice(0, secIdx);
      svg = svg.replace(/(\s*<\/div>)+\s*$/m, '');
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
