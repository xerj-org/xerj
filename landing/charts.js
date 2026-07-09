/* charts.js — interactive upgrade for the two XERJ hero charts.
 *
 * Progressive enhancement: each <svg class="chart" data-chart="..."> ships a
 * hand-authored STATIC render in index.html (the no-JS fallback). This script,
 * when it runs, replaces that svg's contents with an interactive, data-faithful
 * render and adds an HTML legend + stat card around it. If anything required is
 * missing it bails and the static fallback stays on screen.
 *
 * Vanilla, no deps, no external requests, CSP-safe. Idempotent.
 */
(function () {
  'use strict';

  var SVG_NS = 'http://www.w3.org/2000/svg';
  var TOTAL_EMBEDDINGS = 830;
  var ZOOM_MS = 450;
  var PAD_FRAC = 0.12;              // ~12% padding around a cluster bbox
  var MAX_ZOOM = 2.4;              // cap magnification so dots/labels don't balloon
  var FULL_VB_SCATTER = [0, 0, 1200, 380];

  /* Faithful per-cluster hull outlines from the original static SVG (index.html
   * line 131), keyed by cluster so each hull dims/highlights with its cluster. */
  var HULLS = {
    rag:      { op: '0.55', pts: '93.8,252.0 103.0,263.6 118.2,268.2 167.5,273.3 241.5,265.6 258.3,262.1 263.7,260.2 277.4,250.9 275.7,245.6 250.3,230.8 233.9,225.9 199.6,222.3 110.3,229.6 99.0,233.5 94.4,241.1 93.8,252.0' },
    code:     { op: '0.48', pts: '809.5,290.8 838.4,302.7 901.5,309.2 955.2,308.0 971.3,305.2 1014.9,288.1 1018.5,285.6 1011.6,268.2 997.2,261.8 950.3,250.7 886.5,249.2 846.6,255.9 822.7,263.0 810.4,281.6 809.5,290.8' },
    docqa:    { op: '0.41', pts: '396.6,119.7 409.2,141.2 456.1,151.6 524.3,156.5 564.0,154.8 609.1,146.1 634.0,135.6 640.7,124.8 626.6,106.1 613.8,101.3 576.9,95.6 528.3,90.1 459.2,96.7 410.2,106.3 396.6,119.7' },
    extract:  { op: '0.34', pts: '1002.2,68.6 1026.9,77.8 1045.9,78.0 1106.9,77.9 1160.0,69.6 1141.6,47.7 1105.1,40.0 1045.9,42.1 1017.2,48.5 1002.2,68.6' },
    classify: { op: '0.27', pts: '40.0,54.0 57.7,76.1 93.6,77.6 124.7,76.9 144.7,74.3 160.4,63.0 157.1,52.2 147.6,48.8 114.1,46.0 89.1,44.0 52.2,50.2 40.0,54.0' },
    agent:    { op: '0.20', pts: '618.7,316.0 623.4,325.0 635.8,331.5 667.0,338.7 672.7,339.9 728.6,340.0 781.7,323.6 777.2,312.6 773.0,309.6 756.1,300.9 729.3,299.2 710.8,298.1 667.7,299.9 633.1,308.1 618.7,316.0' }
  };

  /* Hide the redundant hand-authored static caption legend once the interactive
   * chip legend is in place (progressive enhancement — it reappears with JS off). */
  function hideStaticLegend(wrap) {
    var sibs = wrap.parentNode ? wrap.parentNode.children : [];
    for (var i = 0; i < sibs.length; i++) {
      if (sibs[i] !== wrap && sibs[i].classList &&
          sibs[i].classList.contains('series-legend')) {
        sibs[i].style.display = 'none';
      }
    }
  }

  var reduceMotion = false;
  try {
    reduceMotion = window.matchMedia &&
      window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  } catch (e) { /* no matchMedia — treat as motion-ok */ }

  /* ---------- tiny helpers ---------------------------------------------- */

  function svgEl(tag, attrs) {
    var n = document.createElementNS(SVG_NS, tag);
    if (attrs) { for (var k in attrs) { n.setAttribute(k, attrs[k]); } }
    return n;
  }
  function htmlEl(tag, cls) {
    var n = document.createElement(tag);
    if (cls) { n.className = cls; }
    return n;
  }
  function clamp(v, lo, hi) { return v < lo ? lo : (v > hi ? hi : v); }
  function slug(s) {
    return String(s).toLowerCase().replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '');
  }

  /* Wrap the svg in a positioned container so the stat card / reset button can
   * be absolutely placed within the chart's own bounds (never the page). */
  function wrapChart(svg) {
    var wrap = htmlEl('div', 'chart-wrap');
    var parent = svg.parentNode;
    parent.insertBefore(wrap, svg);
    wrap.appendChild(svg);          // moves svg into wrap
    hideStaticLegend(wrap);         // drop the redundant static caption legend
    return wrap;
  }

  /* easeInOutCubic */
  function ease(t) {
    return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
  }

  /* Animate an svg's viewBox from `from` -> `to` (arrays of 4). Cancels any
   * in-flight animation on the same svg. Reduced-motion => jump instantly. */
  function animateViewBox(svg, to, done) {
    if (svg._vbRaf) { cancelAnimationFrame(svg._vbRaf); svg._vbRaf = 0; }
    var from = (svg.getAttribute('viewBox') || '0 0 1200 380')
      .trim().split(/[\s,]+/).map(Number);
    if (reduceMotion) {
      svg.setAttribute('viewBox', to.join(' '));
      if (done) { done(); }
      return;
    }
    var start = 0;
    function step(ts) {
      if (!start) { start = ts; }
      var t = clamp((ts - start) / ZOOM_MS, 0, 1);
      var e = ease(t);
      var vb = [
        from[0] + (to[0] - from[0]) * e,
        from[1] + (to[1] - from[1]) * e,
        from[2] + (to[2] - from[2]) * e,
        from[3] + (to[3] - from[3]) * e
      ];
      svg.setAttribute('viewBox', vb.join(' '));
      if (t < 1) { svg._vbRaf = requestAnimationFrame(step); }
      else { svg._vbRaf = 0; if (done) { done(); } }
    }
    svg._vbRaf = requestAnimationFrame(step);
  }

  /* Bounding box of a cluster's points -> a padded viewBox that preserves the
   * full chart's aspect ratio (so preserveAspectRatio="none" doesn't distort
   * the zoom relative to the un-zoomed view), clamped inside the full canvas. */
  function zoomBoxFor(pts, full) {
    var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (var i = 0; i < pts.length; i++) {
      var x = pts[i][0], y = pts[i][1];
      if (x < minX) { minX = x; } if (x > maxX) { maxX = x; }
      if (y < minY) { minY = y; } if (y > maxY) { maxY = y; }
    }
    var w = Math.max(maxX - minX, 1);
    var h = Math.max(maxY - minY, 1);
    // pad each side
    var px = w * PAD_FRAC, py = h * PAD_FRAC;
    minX -= px; maxX += px; minY -= py; maxY += py;
    w = maxX - minX; h = maxY - minY;
    var cx = (minX + maxX) / 2, cy = (minY + maxY) / 2;
    // expand to the full canvas aspect ratio around the centre
    var aspect = full[2] / full[3];
    if (w / h < aspect) { w = h * aspect; } else { h = w / aspect; }
    // cap magnification: don't zoom tighter than MAX_ZOOM (small clusters would
    // otherwise blow the "·" dots + label up several ×)
    var minW = full[2] / MAX_ZOOM;
    if (w < minW) { w = minW; h = w / aspect; }
    // never larger than the whole canvas
    w = Math.min(w, full[2]); h = Math.min(h, full[3]);
    var x0 = clamp(cx - w / 2, full[0], full[0] + full[2] - w);
    var y0 = clamp(cy - h / 2, full[1], full[1] + full[3] - h);
    return [x0, y0, w, h];
  }

  /* ---------- shared state application ---------------------------------- */

  /* Recompute .is-active / .is-dim across groups + chips, update the stat card
   * and the reset button, from `hoverKey` (transient) and `pinnedKey`. */
  function apply(chart) {
    var activeKey = chart.hoverKey || chart.pinnedKey || null;
    chart.items.forEach(function (it) {
      var on = activeKey != null && it.key === activeKey;
      var dim = activeKey != null && it.key !== activeKey;
      it.group.classList.toggle('is-active', on);
      it.group.classList.toggle('is-dim', dim);
      if (it.chip) {
        it.chip.classList.toggle('is-active', on);
        it.chip.classList.toggle('is-dim', dim);
        it.chip.setAttribute('aria-pressed', chart.pinnedKey === it.key ? 'true' : 'false');
      }
    });
    // stat card
    if (activeKey != null) {
      var it2 = chart.byKey[activeKey];
      chart.card.innerHTML = '';
      var t = htmlEl('span', 'cc-title'); t.textContent = it2.title;
      var m = htmlEl('span', 'cc-meta'); m.textContent = it2.meta;
      chart.card.appendChild(t);
      chart.card.appendChild(m);
      chart.card.classList.add('is-shown');
    } else {
      chart.card.classList.remove('is-shown');
    }
    // reset button (scatter zoom only)
    if (chart.reset) {
      chart.reset.classList.toggle('is-shown', chart.pinnedKey != null);
    }
  }

  /* ---------- renderer: intent-cluster scatter -------------------------- */

  function enhanceClusters(svg) {
    var data = window.XERJ_CLUSTERS;
    if (!data || !data.clusters || !data.clusters.length) { return; } // keep fallback
    var full = (data.viewBox && data.viewBox.length === 4)
      ? data.viewBox.slice() : FULL_VB_SCATTER.slice();

    var wrap = wrapChart(svg);
    var chart = {
      kind: 'clusters', svg: svg, wrap: wrap, full: full,
      items: [], byKey: {}, hoverKey: null, pinnedKey: null
    };

    // Build the interactive render into a fragment FIRST, so a failure mid-build
    // can't blank the static fallback (we only clear once the frag is ready).
    var frag = document.createDocumentFragment();

    data.clusters.forEach(function (cl) {
      var g = svgEl('g', {
        'class': 'cl',
        'data-cluster': cl.key,
        tabindex: '0',
        role: 'button',
        'aria-label': 'Zoom into ' + cl.label + ' cluster, ' + cl.pts.length + ' queries'
      });
      // faint hull outline (behind the dots), matching the static SVG
      var hull = HULLS[cl.key];
      if (hull) {
        g.appendChild(svgEl('polyline', {
          points: hull.pts, fill: 'none', stroke: 'currentColor',
          'stroke-width': '1', 'stroke-opacity': hull.op,
          'vector-effect': 'non-scaling-stroke', 'class': 'cl-hull'
        }));
      }
      // dots
      for (var i = 0; i < cl.pts.length; i++) {
        var p = cl.pts[i];
        // brand-book dot: the "·" glyph in the data font (identical to the
        // original static SVG) — NOT a filled <circle> (reads too large/heavy)
        var dot = svgEl('text', {
          x: p[0], y: p[1], fill: 'currentColor', opacity: p[2],
          'font-family': 'var(--font-data)', 'font-size': '14',
          'text-anchor': 'middle', 'dominant-baseline': 'middle',
          'class': 'cl-dot'
        });
        dot.textContent = '·';
        g.appendChild(dot);
      }
      // cluster label (matches the static render)
      var lbl = svgEl('text', {
        x: cl.cx, y: cl.cy, fill: 'currentColor',
        'font-family': 'var(--font-prose)', 'font-size': '11',
        'font-weight': '700', 'letter-spacing': '1.5',
        'text-anchor': 'middle', 'class': 'cl-label'
      });
      lbl.textContent = cl.label;
      g.appendChild(lbl);
      frag.appendChild(g);

      var pct = (cl.pts.length / TOTAL_EMBEDDINGS * 100).toFixed(1);
      var item = {
        key: cl.key, label: cl.label, group: g, chip: null, pts: cl.pts,
        title: cl.label,
        meta: cl.pts.length + ' queries · ' + pct + '% of ' + TOTAL_EMBEDDINGS,
        count: cl.pts.length, opacity: (cl.pts[0] ? cl.pts[0][2] : 1)
      };
      chart.items.push(item);
      chart.byKey[cl.key] = item;
    });
    while (svg.firstChild) { svg.removeChild(svg.firstChild); } // drop fallback
    svg.appendChild(frag);

    // stat card + reset button (inside wrap => within chart bounds)
    chart.card = htmlEl('div', 'chart-card cc-scatter');
    chart.card.setAttribute('aria-hidden', 'true');
    wrap.appendChild(chart.card);

    chart.reset = htmlEl('button', 'chart-reset');
    chart.reset.type = 'button';
    chart.reset.setAttribute('aria-label', 'Reset zoom to all clusters');
    chart.reset.textContent = '⤢ RESET';
    wrap.appendChild(chart.reset);

    // legend chips (under the chart, inside wrap for single delegation root)
    var legend = htmlEl('div', 'chart-legend');
    legend.setAttribute('role', 'group');
    legend.setAttribute('aria-label', 'Cluster legend');
    chart.items.forEach(function (it) {
      var chip = htmlEl('button', 'chart-chip');
      chip.type = 'button';
      chip.setAttribute('data-chip', it.key);
      chip.setAttribute('aria-pressed', 'false');
      chip.setAttribute('aria-label', 'Zoom into ' + it.label + ', ' + it.count + ' queries');
      var sw = htmlEl('span', 'chip-sw');
      sw.style.opacity = String(it.opacity);
      var tx = htmlEl('span', 'chip-tx');
      tx.textContent = it.label;
      var ct = htmlEl('span', 'chip-ct');
      ct.textContent = it.count;
      chip.appendChild(sw); chip.appendChild(tx); chip.appendChild(ct);
      legend.appendChild(chip);
      it.chip = chip;
    });
    wrap.appendChild(legend);

    // interactions --------------------------------------------------------
    function zoomTo(key) {
      var it = chart.byKey[key];
      if (!it) { return; }
      chart.pinnedKey = key;
      animateViewBox(svg, zoomBoxFor(it.pts, full));
      apply(chart);
    }
    function resetZoom() {
      if (chart.pinnedKey == null) { return; }
      chart.pinnedKey = null;
      chart.hoverKey = null;
      animateViewBox(svg, full.slice());
      apply(chart);
    }

    bindHover(chart);
    wrap.addEventListener('click', function (ev) {
      var chip = closestAttr(ev.target, 'data-chip', wrap);
      var grp = closestAttr(ev.target, 'data-cluster', wrap);
      if (ev.target === chart.reset || chart.reset.contains(ev.target)) {
        resetZoom(); return;
      }
      var key = (chip && chip.getAttribute('data-chip')) ||
                (grp && grp.getAttribute('data-cluster'));
      if (key) { zoomTo(key); return; }
      // empty background while zoomed => reset
      if (chart.pinnedKey != null) { resetZoom(); }
    });
    wrap.addEventListener('keydown', function (ev) {
      var grp = closestAttr(ev.target, 'data-cluster', wrap);
      if ((ev.key === 'Enter' || ev.key === ' ' || ev.key === 'Spacebar') && grp) {
        ev.preventDefault();
        zoomTo(grp.getAttribute('data-cluster'));
      } else if (ev.key === 'Escape') {
        resetZoom();
      }
    });
    // Esc works even when focus has left the chart
    document.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape' && chart.pinnedKey != null) { resetZoom(); }
    });

    svg.dataset.enhanced = '1';
    svg.classList.add('is-interactive');
  }

  /* ---------- renderer: latency multi-series line ----------------------- */

  function enhanceLatency(svg) {
    // Reuse the exact static nodes (verbatim points/labels) — group them.
    // Walk element children in order, grouping [polyline, ...text] runs.
    var series = [];
    var cur = null;
    var kids = [];
    for (var n = svg.firstElementChild; n; n = n.nextElementSibling) {
      kids.push(n);
    }
    kids.forEach(function (node) {
      var tag = node.tagName.toLowerCase();
      if (tag === 'polyline') {
        cur = { poly: node, texts: [] };
        series.push(cur);
      } else if (tag === 'text' && cur) {
        cur.texts.push(node);
      }
    });
    if (!series.length) { return; } // nothing to enhance — keep fallback

    var wrap = wrapChart(svg);
    var chart = {
      kind: 'latency', svg: svg, wrap: wrap,
      items: [], byKey: {}, hoverKey: null, pinnedKey: null
    };

    series.forEach(function (s) {
      var nameNode = null, valueNode = null;
      s.texts.forEach(function (t) {
        if (t.getAttribute('text-anchor') === 'end') { nameNode = t; }
        else { valueNode = t; }
      });
      if (!nameNode) { nameNode = s.texts[0] || null; }
      if (!valueNode) { valueNode = s.texts[s.texts.length - 1] || null; }
      var name = nameNode ? nameNode.textContent.trim() : 'series';
      var value = valueNode ? valueNode.textContent.trim() : '';
      var key = slug(name) || ('s' + chart.items.length);
      var op = s.poly.getAttribute('stroke-opacity') || '1';
      var samples = (s.poly.getAttribute('points') || '')
        .trim().split(/\s+/).filter(Boolean).length;

      var g = svgEl('g', {
        'class': 'ser',
        'data-series': key,
        tabindex: '0',
        role: 'button',
        'aria-label': 'Highlight ' + name + ' latency line'
      });
      // move the real nodes into the group (verbatim reuse)
      g.appendChild(s.poly);
      s.texts.forEach(function (t) { g.appendChild(t); });
      svg.appendChild(g);

      var item = {
        key: key, label: name, group: g, chip: null,
        title: name,
        meta: 'p95 end · ' + value + ' · ' + samples + ' samples',
        opacity: parseFloat(op) || 1, value: value
      };
      chart.items.push(item);
      chart.byKey[key] = item;
    });

    // stat card
    chart.card = htmlEl('div', 'chart-card cc-latency');
    chart.card.setAttribute('aria-hidden', 'true');
    wrap.appendChild(chart.card);
    chart.reset = null; // latency has no zoom/reset

    // legend chips
    var legend = htmlEl('div', 'chart-legend');
    legend.setAttribute('role', 'group');
    legend.setAttribute('aria-label', 'Model legend');
    chart.items.forEach(function (it) {
      var chip = htmlEl('button', 'chart-chip');
      chip.type = 'button';
      chip.setAttribute('data-chip', it.key);
      chip.setAttribute('aria-pressed', 'false');
      chip.setAttribute('aria-label', 'Highlight ' + it.label + ' latency line');
      var sw = htmlEl('span', 'chip-sw chip-sw-line');
      sw.style.opacity = String(it.opacity);
      var tx = htmlEl('span', 'chip-tx');
      tx.textContent = it.label;
      chip.appendChild(sw); chip.appendChild(tx);
      legend.appendChild(chip);
      it.chip = chip;
    });
    wrap.appendChild(legend);

    // interactions --------------------------------------------------------
    function toggleLock(key) {
      chart.pinnedKey = (chart.pinnedKey === key) ? null : key;
      apply(chart);
    }
    function unlock() {
      if (chart.pinnedKey == null) { return; }
      chart.pinnedKey = null;
      apply(chart);
    }

    bindHover(chart);
    wrap.addEventListener('click', function (ev) {
      var chip = closestAttr(ev.target, 'data-chip', wrap);
      var grp = closestAttr(ev.target, 'data-series', wrap);
      var key = (chip && chip.getAttribute('data-chip')) ||
                (grp && grp.getAttribute('data-series'));
      if (key) { toggleLock(key); return; }
      if (chart.pinnedKey != null) { unlock(); }
    });
    wrap.addEventListener('keydown', function (ev) {
      var grp = closestAttr(ev.target, 'data-series', wrap);
      if ((ev.key === 'Enter' || ev.key === ' ' || ev.key === 'Spacebar') && grp) {
        ev.preventDefault();
        toggleLock(grp.getAttribute('data-series'));
      } else if (ev.key === 'Escape') {
        unlock();
      }
    });
    document.addEventListener('keydown', function (ev) {
      if (ev.key === 'Escape' && chart.pinnedKey != null) { unlock(); }
    });

    svg.dataset.enhanced = '1';
    svg.classList.add('is-interactive');
  }

  /* ---------- shared hover/focus wiring (event delegation) -------------- */

  function bindHover(chart) {
    var wrap = chart.wrap;
    var attr = chart.kind === 'clusters' ? 'data-cluster' : 'data-series';

    function keyFrom(target) {
      var chip = closestAttr(target, 'data-chip', wrap);
      if (chip) { return chip.getAttribute('data-chip'); }
      var grp = closestAttr(target, attr, wrap);
      if (grp) { return grp.getAttribute(attr); }
      return null;
    }
    wrap.addEventListener('pointerover', function (ev) {
      var k = keyFrom(ev.target);
      if (k && k !== chart.hoverKey) { chart.hoverKey = k; apply(chart); }
    });
    wrap.addEventListener('pointerout', function (ev) {
      // leaving the whole wrap, or moving to a non-item area
      var to = ev.relatedTarget;
      if (to && wrap.contains(to) && keyFrom(to)) { return; }
      if (chart.hoverKey != null) { chart.hoverKey = null; apply(chart); }
    });
    // keyboard focus mirrors hover
    wrap.addEventListener('focusin', function (ev) {
      var k = keyFrom(ev.target);
      if (k) { chart.hoverKey = k; apply(chart); }
    });
    wrap.addEventListener('focusout', function (ev) {
      var to = ev.relatedTarget;
      if (to && wrap.contains(to) && keyFrom(to)) { return; }
      if (chart.hoverKey != null) { chart.hoverKey = null; apply(chart); }
    });
  }

  /* closest ancestor (inclusive) that has attribute `name`, bounded by `root` */
  function closestAttr(node, name, root) {
    while (node && node !== root && node.nodeType === 1) {
      if (node.hasAttribute && node.hasAttribute(name)) { return node; }
      node = node.parentNode;
    }
    return null;
  }

  /* ---------- boot ------------------------------------------------------ */

  function boot() {
    var charts = document.querySelectorAll('svg.chart[data-chart]');
    for (var i = 0; i < charts.length; i++) {
      var svg = charts[i];
      if (svg.dataset.enhanced === '1') { continue; } // idempotent
      var kind = svg.getAttribute('data-chart');
      try {
        if (kind === 'intent-clusters') { enhanceClusters(svg); }
        else if (kind === 'latency-series') { enhanceLatency(svg); }
      } catch (err) {
        // leave the static fallback intact on any failure
        if (window.console && console.warn) {
          console.warn('[charts] enhance failed for', kind, err);
        }
      }
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
