/* XERJ architecture brief — same SVG-injection pipeline as exec/tech,
   but renders via headless Chrome directly (no puppeteer dependency). */
const path = require('path');
const fs = require('fs');
const os = require('os');
const { execFileSync } = require('child_process');

(async () => {
  const tplPath = path.resolve(__dirname, 'arch-brief.html');
  const svgDir  = path.resolve(__dirname, 'svg');
  const outHtml = path.resolve(__dirname, '_arch-brief.rendered.html');
  const outPdf  = path.resolve(__dirname, 'xerj-architecture-brief.pdf');

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
    const opens  = (svg.match(/<div\b/g) || []).length;
    const closes = (svg.match(/<\/div>/g) || []).length;
    let excess = closes - opens;
    while (excess-- > 0) {
      svg = svg.replace(/\s*<\/div>\s*$/, '');
    }
    return svg;
  });
  fs.writeFileSync(outHtml, html);

  const tmpProfile = fs.mkdtempSync(path.join(os.tmpdir(), 'xerj-chrome-'));
  try {
    execFileSync('google-chrome', [
      '--headless=new',
      '--disable-gpu',
      '--no-sandbox',
      '--no-pdf-header-footer',
      '--font-render-hinting=none',
      '--virtual-time-budget=10000',
      '--user-data-dir=' + tmpProfile,
      '--print-to-pdf-no-header',
      '--print-to-pdf=' + outPdf,
      'file://' + outHtml,
    ], { stdio: 'inherit' });
    console.log('OK:', outPdf, fs.statSync(outPdf).size, 'bytes');
  } finally {
    fs.rmSync(tmpProfile, { recursive: true, force: true });
  }
})();
