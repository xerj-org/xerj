// Walks every laid-out element of /osd and asserts it sits on the 32 px matrix.
// Returns a JSON report; run by check_osd_grid.py at several viewport widths.
(() => {
  const CELL = 32, TOL = 0.5, CAP_D = 0.8;
  const wrap = document.querySelector('.wrap');
  const ox = wrap.getBoundingClientRect().left + window.scrollX;
  const oy = 0;
  const W = wrap.getBoundingClientRect().width;
  const mod = (v, m) => { const r = ((v % m) + m) % m; return Math.min(r, m - r); };
  const on = (v, m = CELL, off = 0) => mod(v - off, m) <= TOL;
  const layer = document.getElementById('osd-layer');
  const fails = [], boxes = { total: 0, pass: 0 }, bases = { total: 0, pass: 0 };
  const name = (el) => {
    let s = el.tagName.toLowerCase();
    if (el.id) s += '#' + el.id;
    if (el.classList.length) s += '.' + [...el.classList].join('.');
    const t = (el.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 40);
    return t ? `${s} "${t}"` : s;
  };
  // 1. boxes ---------------------------------------------------------------
  for (const el of document.body.querySelectorAll('*')) {
    if (layer.contains(el) || ['SCRIPT', 'STYLE', 'BR'].includes(el.tagName)) continue;
    if (el.closest('svg') && el.tagName.toLowerCase() !== 'svg') continue;   // svg internals: the svg box is checked
    const cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.display === 'contents') continue;
    if (cs.display === 'inline') continue;                                   // inline runs are text: checked by baseline
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) continue;
    const x = r.left + window.scrollX - ox, y = r.top + window.scrollY - oy;
    const bleed = el.hasAttribute('data-bleed');
    const bad = [];
    if (!bleed && !on(x)) bad.push(`x=${x.toFixed(2)}`);
    if (!on(y)) bad.push(`y=${y.toFixed(2)}`);
    if (!bleed && !on(r.width)) bad.push(`w=${r.width.toFixed(2)}`);
    if (!on(r.height)) bad.push(`h=${r.height.toFixed(2)}`);
    // must stay inside the matrix unless it scrolls inside a clipped parent
    const clipped = el.parentElement && el.parentElement.closest('pre');
    if (!bleed && !clipped && (x < -TOL || x + r.width > W + TOL)) bad.push(`outside matrix ${x.toFixed(0)}..${(x + r.width).toFixed(0)} of ${W}`);
    boxes.total++;
    if (bad.length) fails.push({ kind: 'box', el: name(el), bad: bad.join(' ') }); else boxes.pass++;
  }
  // 2. baselines -------------------------------------------------------------
  // Rules: .td display caps fill cells (cap-top and baseline on cell lines);
  // running text (.tx, small copy buttons) baseline at +24 in its row;
  // nav row (links, OSD label) shares the logo baseline on a cell line;
  // the terminal transcript runs two lines per cell (baselines at +8, +24).
  const rules = [
    ['.td', 0, CELL, true], ['.tx', 24, CELL], ['.copy', 24, CELL], ['.go-btn', 0, CELL],
    ['.nav-links a', 0, CELL], ['.osd-sw .t', 0, CELL], ['.term pre', 8, 16], ['.way pre', 24, CELL],
  ];
  const probe = () => { const s = document.createElement('span'); s.style.cssText = 'display:inline-block;width:0;height:0;vertical-align:baseline;margin:0;padding:0'; s.dataset.probe = '1'; return s; };
  const covered = new Set();
  for (const [sel, off, m, capfill] of rules) {
    for (const el of document.querySelectorAll(sel)) {
      if (layer.contains(el) || covered.has(el)) continue;
      covered.add(el);
      if (el.getBoundingClientRect().height === 0) continue;
      const a = probe(), b = probe();
      el.insertBefore(a, el.firstChild); el.appendChild(b);
      const ya = a.getBoundingClientRect().bottom + window.scrollY - oy;
      const yb = b.getBoundingClientRect().bottom + window.scrollY - oy;
      a.remove(); b.remove();
      const bad = [];
      if (!on(ya, m, off)) bad.push(`first baseline ${ya.toFixed(2)} (mod ${m} want ${off})`);
      if (!on(yb, m, off)) bad.push(`last baseline ${yb.toFixed(2)}`);
      if (capfill) {
        const fs = parseFloat(getComputedStyle(el).fontSize), capTop = ya - fs * CAP_D;
        if (!on(capTop)) bad.push(`cap-top ${capTop.toFixed(2)}`);
      }
      bases.total++;
      if (bad.length) fails.push({ kind: 'baseline', el: name(el), bad: bad.join(' ') }); else bases.pass++;
    }
  }
  // 3. text that no rule owns -------------------------------------------------
  const orphans = [];
  const tw = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = tw.nextNode(); n; n = tw.nextNode()) {
    if (!n.textContent.trim() || layer.contains(n)) continue;
    const p = n.parentElement;
    if (p.closest('script,style')) continue;
    let owned = false;
    for (const el of covered) if (el.contains(n)) { owned = true; break; }
    if (!owned) orphans.push(name(p));
  }
  if (document.documentElement.scrollWidth > innerWidth) fails.push({ kind: 'page', el: 'html', bad: `horizontal overflow: scrollWidth ${document.documentElement.scrollWidth} > ${innerWidth}` });
  return { width: innerWidth, cells: Math.round(wrap.getBoundingClientRect().width / CELL), originX: ox,
           boxes, baselines: bases, unownedText: orphans, fails };
})()
