// ============================================================
// XERJ.ai — Export helpers
//
// Real Kibana feedback: GH#1992 "Export Documents as CSV"
// (372 reactions, 571 comments) — the #1 ask of all time.
// GH#1366 "Export chart to image" (178 reactions).
//
// Pure browser, no deps. CSV is built from arrays. PNG is
// built by serializing an SVG and rasterizing through a
// hidden <canvas>.
// ============================================================

/**
 * Build a CSV string from a header row and a body of rows.
 * Cells are escaped per RFC 4180: quoted if they contain
 * comma / quote / newline; embedded quotes doubled.
 */
export function toCsv(headers, rows) {
  const escape = (cell) => {
    const s = cell == null ? '' : String(cell);
    if (/[",\n\r]/.test(s)) return '"' + s.replace(/"/g, '""') + '"';
    return s;
  };
  const lines = [headers.map(escape).join(',')];
  for (const row of rows) lines.push(row.map(escape).join(','));
  return lines.join('\r\n') + '\r\n';
}

/**
 * Trigger a browser download of arbitrary text as a file.
 * Uses an in-memory blob URL so nothing leaves the page.
 */
export function downloadText(filename, mime, text) {
  const blob = new Blob([text], { type: mime + ';charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 1500);
}

/** Convert hits → 4-column CSV (index, id, score, source). */
export function hitsToCsv(hits, columns = ['_index', '_id', '_score', '_ts', '_source']) {
  const headers = columns.map((c) => c.replace(/^_/, ''));
  const rows = hits.map((h) =>
    columns.map((c) => {
      const v = h[c];
      if (typeof v === 'object' && v !== null) return JSON.stringify(v);
      return v;
    })
  );
  return toCsv(headers, rows);
}

/**
 * Export an SVG element to a PNG file.
 *
 * 1. Serialize the live SVG (with computed currentColor and viewBox)
 * 2. Inline it as a data URL in an <img>
 * 3. Render to <canvas> at 2× DPR
 * 4. canvas.toBlob → download
 *
 * Returns a Promise<Blob> so callers can await it for tests.
 */
export async function svgToPng(svgEl, { scale = 2, filename = 'chart.png' } = {}) {
  if (!svgEl) throw new Error('svgToPng: missing SVG element');
  // Clone so we can safely mutate (size, color)
  const clone = svgEl.cloneNode(true);
  const rect = svgEl.getBoundingClientRect();
  const w = Math.max(1, Math.round(rect.width));
  const h = Math.max(1, Math.round(rect.height));
  // Force absolute size for the rasterizer
  clone.setAttribute('width', String(w));
  clone.setAttribute('height', String(h));
  // Inline computed color so currentColor resolves
  const cs = getComputedStyle(svgEl);
  clone.setAttribute('color', cs.color);
  // Add namespace
  if (!clone.getAttribute('xmlns')) {
    clone.setAttribute('xmlns', 'http://www.w3.org/2000/svg');
  }
  // Inject background = page paper, otherwise PNG has transparent bg
  const paper = getComputedStyle(document.documentElement).getPropertyValue('--z-paper').trim() || '#0D0D0D';
  const bg = `<rect width="100%" height="100%" fill="${paper}"/>`;
  // Insert bg at the very start of children
  const inner = clone.innerHTML;
  clone.innerHTML = bg + inner;

  const xml = new XMLSerializer().serializeToString(clone);
  const svg64 = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(xml);

  const img = new Image();
  img.crossOrigin = 'anonymous';

  const blob = await new Promise((resolve, reject) => {
    img.onload = () => {
      const canvas = document.createElement('canvas');
      canvas.width = w * scale;
      canvas.height = h * scale;
      const ctx = canvas.getContext('2d');
      ctx.scale(scale, scale);
      ctx.drawImage(img, 0, 0, w, h);
      canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('toBlob failed'))), 'image/png');
    };
    img.onerror = (e) => reject(new Error('SVG image load failed'));
    img.src = svg64;
  });

  // Trigger download
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  setTimeout(() => URL.revokeObjectURL(url), 1500);

  return blob;
}
