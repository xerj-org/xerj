// XERJ.AI logo export — SVG (transparent bg) + PNG at 4 sizes,
// for every shape (wordmark · short · mark · systemline) in three
// colour modes: on-dark, on-light, mono (currentColor).
// Naming convention: files include `on-dark` / `on-light` for the
// INTENDED BACKGROUND; actual canvases are transparent.
const puppeteer = require('puppeteer');
const path = require('path');
const fs = require('fs');
const { execSync } = require('child_process');

const OUT = '/tmp/xerj-logos';
const ZIP = '/tmp/xerj-logos.zip';
fs.rmSync(OUT, { recursive: true, force: true });
fs.mkdirSync(OUT, { recursive: true });
fs.mkdirSync(path.join(OUT, 'svg'));
fs.mkdirSync(path.join(OUT, 'png'));

const FONT_STACK = "'Big Shoulders Display','Inter',sans-serif";
const SYS_FONT = "'IBM Plex Sans','Inter',sans-serif";

// -------- SVG BUILDERS (transparent bg, three colour modes) --------

// Colour tokens for each mode · rule/XERJ/.AI
const MODES = {
  'on-dark':  { rule_b: '#f4f2ec', rule_t: '#ffc400', xerj: '#f4f2ec', ai: '#ffc400' },
  'on-light': { rule_b: '#11120f', rule_t: '#a06800', xerj: '#11120f', ai: '#a06800' },
  'mono':     { rule_b: 'currentColor', rule_t: 'currentColor', xerj: 'currentColor', ai: 'currentColor' },
};

function wordmarkSvg(mode) {
  const c = MODES[mode];
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 240" width="800" height="240">
  <line x1="90" y1="200" x2="530" y2="200" stroke="${c.rule_b}" stroke-width="1" stroke-linecap="square"/>
  <line x1="530" y1="40" x2="710" y2="40" stroke="${c.rule_t}" stroke-width="1" stroke-linecap="square"/>
  <text x="530" y="170" font-family="${FONT_STACK}" font-weight="900" font-size="140" letter-spacing="4" text-anchor="end" fill="${c.xerj}">XERJ</text>
  <text x="530" y="170" font-family="${FONT_STACK}" font-weight="900" font-size="140" letter-spacing="4" text-anchor="start" fill="${c.ai}">.AI</text>
</svg>`;
}

function shortSvg(mode) {
  const c = MODES[mode];
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 240" width="600" height="240">
  <line x1="90" y1="40" x2="510" y2="40" stroke="${c.xerj}" stroke-width="1" stroke-linecap="square"/>
  <line x1="90" y1="200" x2="510" y2="200" stroke="${c.xerj}" stroke-width="1" stroke-linecap="square"/>
  <text x="300" y="170" font-family="${FONT_STACK}" font-weight="900" font-size="140" letter-spacing="4" text-anchor="middle" fill="${c.xerj}">XERJ</text>
</svg>`;
}

function markSvg(mode) {
  // For mark variants the X itself always takes the accent colour on dark, ink on light, currentColor in mono.
  const c = MODES[mode];
  // on-dark: yellow X + yellow rules; on-light: ink X + ink rules; mono: currentColor.
  const color = mode === 'on-dark' ? '#ffc400' : mode === 'on-light' ? '#11120f' : 'currentColor';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200" width="200" height="200">
  <line x1="30" y1="24" x2="170" y2="24" stroke="${color}" stroke-width="1" stroke-linecap="square"/>
  <line x1="30" y1="170" x2="170" y2="170" stroke="${color}" stroke-width="1" stroke-linecap="square"/>
  <text x="100" y="140" font-family="${FONT_STACK}" font-weight="900" font-size="120" text-anchor="middle" fill="${color}">X</text>
</svg>`;
}

function systemlineSvg(mode) {
  const c = MODES[mode];
  // Keep the dual-colour faint rule / accent rule split where meaningful.
  const faintBottom = mode === 'on-dark' ? '#3a3836' : mode === 'on-light' ? '#cfcbbf' : 'currentColor';
  const accentTop   = mode === 'on-dark' ? '#ffc400' : mode === 'on-light' ? '#a06800' : 'currentColor';
  const textColor   = mode === 'on-dark' ? '#f4f2ec' : mode === 'on-light' ? '#11120f' : 'currentColor';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 100" width="800" height="100">
  <line x1="210" y1="78" x2="450" y2="78" stroke="${faintBottom}" stroke-width="1" stroke-linecap="square"/>
  <line x1="450" y1="22" x2="600" y2="22" stroke="${accentTop}" stroke-width="1" stroke-linecap="square"/>
  <text x="400" y="58" font-family="${SYS_FONT}" font-weight="600" font-size="20" letter-spacing="4.8" text-anchor="middle" fill="${textColor}">XERJ.AI · OBSERVE</text>
</svg>`;
}

// -------- shape manifest (viewBox + target PNG widths) --------
// aspect = w/h from viewBox; intrinsic height at each width = width / aspect
const shapes = [
  { name: 'wordmark',   aspect: 800/240, build: wordmarkSvg,   widths: [512, 1024, 2048, 4096], darkFallback: '#0b0b0d' },
  { name: 'short',      aspect: 600/240, build: shortSvg,      widths: [512, 1024, 2048, 4096], darkFallback: '#0b0b0d' },
  { name: 'mark',       aspect: 200/200, build: markSvg,       widths: [256, 512, 1024, 2048], darkFallback: '#0b0b0d' },
  { name: 'systemline', aspect: 800/100, build: systemlineSvg, widths: [512, 1024, 2048, 4096], darkFallback: '#0b0b0d' },
];

const modes = ['on-dark', 'on-light', 'mono'];

// -------- write all SVGs --------
const manifest = [];
for (const shape of shapes) {
  fs.mkdirSync(path.join(OUT, 'svg', shape.name), { recursive: true });
  fs.mkdirSync(path.join(OUT, 'png', shape.name), { recursive: true });
  for (const mode of modes) {
    const svg = shape.build(mode);
    const filename = `xerj-${shape.name}.${mode}.svg`;
    const svgPath = path.join(OUT, 'svg', shape.name, filename);
    fs.writeFileSync(svgPath, svg);
    manifest.push({ kind: 'SVG', shape: shape.name, mode, width: 'scalable', height: 'scalable', path: `svg/${shape.name}/${filename}` });
  }
}

// -------- rasterize SVGs to PNGs via puppeteer --------

(async () => {
  const browser = await puppeteer.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox'],
  });
  try {
    const page = await browser.newPage();

    for (const shape of shapes) {
      for (const mode of modes) {
        const svg = shape.build(mode);
        for (const w of shape.widths) {
          const h = Math.round(w / shape.aspect);
          // Host the SVG in an HTML shell · transparent bg · fonts loaded
          const html = `<!DOCTYPE html><html><head>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@900&family=Inter:wght@600;700&family=IBM+Plex+Sans:wght@600&display=swap" rel="stylesheet">
<style>
html,body{margin:0;padding:0;background:transparent;color:${mode === 'on-dark' ? '#f4f2ec' : '#11120f'};}
body{width:${w}px; height:${h}px; display:flex; align-items:center; justify-content:center;}
svg{width:${w}px; height:${h}px; display:block;}
</style>
</head><body>${svg}</body></html>`;

          await page.setViewport({ width: w, height: h, deviceScaleFactor: 1 });
          await page.setContent(html, { waitUntil: 'load', timeout: 60000 });
          await page.evaluateHandle('document.fonts.ready');
          await page.evaluate(() => new Promise(r => setTimeout(r, 120)));

          const filename = `xerj-${shape.name}.${mode}.${w}x${h}.png`;
          const pngPath = path.join(OUT, 'png', shape.name, filename);
          await page.screenshot({ path: pngPath, omitBackground: true, type: 'png' });
          manifest.push({ kind: 'PNG', shape: shape.name, mode, width: w, height: h, path: `png/${shape.name}/${filename}` });
          process.stdout.write(`  ${filename} · ${w}×${h}\n`);
        }
      }
    }
  } finally {
    await browser.close();
  }

  // -------- write README / manifest --------
  const readme = [
    'XERJ.AI · Logo export bundle',
    'Generated ' + new Date().toISOString().slice(0, 10),
    '',
    'NAMING · on-dark / on-light / mono',
    '  on-dark  → for placement on dark surfaces (paper + gold on transparent bg)',
    '  on-light → for placement on light surfaces (ink + ochre on transparent bg)',
    '  mono     → currentColor fill; inherits whatever color you set on the parent',
    '',
    'ALL backgrounds are transparent. The filename tag only indicates the INTENDED surface.',
    '',
    'SHAPES',
    '  wordmark   · XERJ.AI with dual 1px rules (paper under XERJ, yellow over .AI). Use for hero / cover / section titles.',
    '  short      · XERJ with paper rules above + below. Use when .AI suffix cannot fit.',
    '  mark       · X letter with yellow rules top + bottom inside a 200×200 canvas. Use as app icon, favicon, social avatar.',
    '  systemline · XERJ.AI · OBSERVE tracked signature with faint + accent rules. Use for footers, PDF headers, email signatures.',
    '',
    'RASTER SIZES (PNG · transparent bg)',
    '  wordmark   · 512×154 · 1024×307 · 2048×614 · 4096×1229',
    '  short      · 512×205 · 1024×410 · 2048×819 · 4096×1638',
    '  mark       · 256×256 · 512×512  · 1024×1024 · 2048×2048',
    '  systemline · 512×64  · 1024×128 · 2048×256 · 4096×512',
    '',
    'VECTOR · SVG (transparent bg, scalable, no PNG rasters for this tier)',
    '',
    'FILE MANIFEST (' + manifest.length + ' files)',
    '  ────────────────────────────────',
    ...manifest.map(m => `  ${m.kind.padEnd(4)} ${m.shape.padEnd(11)} ${m.mode.padEnd(8)} ${String(m.width).padStart(10)}×${String(m.height).padEnd(10)} ${m.path}`),
  ].join('\n');
  fs.writeFileSync(path.join(OUT, 'README.txt'), readme);

  // -------- zip --------
  try { fs.unlinkSync(ZIP); } catch (_) {}
  execSync(`cd /tmp/xerj-logos && zip -r ${ZIP} . -q`);
  const zipSize = fs.statSync(ZIP).size;
  console.log(`\n${ZIP} · ${(zipSize / 1024).toFixed(1)} KB · ${manifest.length} files`);
})().catch((err) => {
  console.error('FAILED:', err);
  process.exit(1);
});
