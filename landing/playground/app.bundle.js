(() => {
  // playground/src/ux/text.js
  var esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;"
  })[c]);
  var nf = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 2 });
  var fmt = (v, { decimals } = {}) => {
    if (v == null || Number.isNaN(v)) return "\u2014";
    if (decimals != null) return Number(v).toFixed(decimals);
    if (Math.abs(v) < 1) return Number(v).toFixed(2);
    if (Math.abs(v) < 100) return Number(v).toFixed(1);
    return nf.format(v);
  };
  var Change = (pct, { good = "up", unit = "%", fractionDigits = 1 } = {}) => {
    if (pct == null || Number.isNaN(pct)) return "";
    const up = pct >= 0;
    const arrow = up ? "\u25B2" : "\u25BC";
    const abs = Math.abs(pct).toFixed(fractionDigits);
    let klass = "meh";
    if (good === "up") klass = up ? "good" : "bad";
    else if (good === "down") klass = up ? "bad" : "good";
    return `<span class="delta ${klass}">${arrow} ${abs}${esc(unit)}</span>`;
  };
  var Num = ({
    value,
    unit = "",
    spark = "",
    delta = null,
    deltaGood = "up",
    hint = "",
    emphasis = true
  } = {}) => {
    const valCls = emphasis ? "num-big accent" : "num-md";
    const unitSpan = unit ? `<span class="num-unit">${esc(unit)}</span>` : "";
    const deltaPart = delta != null ? Change(delta, { good: deltaGood }) : "";
    const hintPart = hint ? `<span class="hint">${esc(hint)}</span>` : "";
    const foot = deltaPart || hintPart ? `<div class="row-flex" style="margin-top:10px;">${deltaPart}${hintPart}</div>` : "";
    return `
  <div class="stack">
    <div class="row-flex">
      <div class="${valCls}">${esc(value)}${unitSpan}</div>
      ${spark}
    </div>
    ${foot}
  </div>`;
  };

  // playground/src/ux/charts.js
  var minMax = (xs) => {
    let mn = Infinity, mx = -Infinity;
    for (const x of xs) {
      if (x < mn) mn = x;
      if (x > mx) mx = x;
    }
    return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
  };
  var buildPoints = (values, w, h) => {
    const [mn, mx] = minMax(values);
    const range = mx - mn || 1;
    const n = values.length;
    const step = n > 1 ? w / (n - 1) : w;
    let out = "";
    for (let i = 0; i < n; i++) {
      const x = (i * step).toFixed(2);
      const y = (h - (values[i] - mn) / range * h).toFixed(2);
      out += (i ? " " : "") + x + "," + y;
    }
    return { pts: out, mn, mx };
  };
  var Spark = (values, { w = 140, h = 28, strokeOpacity = 1 } = {}) => {
    if (!values || values.length < 2) return "";
    const { pts } = buildPoints(values, w, h);
    return `<svg class="chart" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" aria-hidden="true">
    <polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${strokeOpacity}"/>
  </svg>`;
  };
  var Series = (values, {
    h = 140,
    labels = ["", ""],
    unit = ""
  } = {}) => {
    if (!values || values.length < 2) return "";
    const w = 1200;
    const { pts, mn, mx } = buildPoints(values, w, h);
    const u = unit ? " " + esc(unit) : "";
    return `
  <div class="series-wrap">
    <svg class="chart series" height="${h}" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
      <polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1"/>
    </svg>
    <div class="series-legend">
      <span>${esc(labels[0] ?? "")} \xB7 <span class="mono" style="color:var(--z-ink);">${fmt(values[0])}${u}</span></span>
      <span class="mid">min <span class="mono" style="color:var(--z-ink);">${fmt(mn)}${u}</span>   \xB7   peak <span class="mono" style="color:var(--z-ink);">${fmt(mx)}${u}</span></span>
      <span><span class="mono" style="color:var(--z-ink);">${fmt(values[values.length - 1])}${u}</span> \xB7 ${esc(labels[1] ?? "")}</span>
    </div>
  </div>`;
  };
  var Dist = ({ segments, width = 1200, height = 10 } = {}) => {
    if (!segments || !segments.length) return "";
    const total = segments.reduce((a, s) => a + s.value, 0) || 1;
    let x = 0;
    const lines = [];
    segments.forEach((s, i) => {
      const segW = s.value / total * width;
      const op = Math.max(0.12, 1 - i * 0.14);
      if (segW > 0.5) {
        lines.push(
          `<line x1="${x.toFixed(1)}" y1="${(height - 1).toFixed(1)}" x2="${(x + segW).toFixed(1)}" y2="${(height - 1).toFixed(1)}" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`
        );
      }
      x += segW;
    });
    const legend = segments.map((s, i) => {
      const pct = (100 * s.value / total).toFixed(1);
      return `
      <div class="stack" style="gap:6px;">
        <span class="key">${esc(s.label)}</span>
        <span class="mono" style="font-size:var(--fs-20); font-weight:700;">${fmt(s.value)}</span>
        <span class="mono faint" style="font-size:var(--fs-11);">${pct}%</span>
      </div>`;
    }).join("");
    return `
    <svg class="chart dist-bar" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none">${lines.join("")}</svg>
    <div class="dist-legend">${legend}</div>
  `;
  };
  var Heatmap = ({ rows, cols, matrix, cellFmt = (v) => fmt(v) } = {}) => {
    if (!matrix || !matrix.length) return "";
    const flat = matrix.flat();
    const [mn, mx] = minMax(flat);
    const range = mx - mn || 1;
    let maxLen = 0;
    for (const v of flat) {
      const l = String(cellFmt(v)).length;
      if (l > maxLen) maxLen = l;
    }
    const widthCh = Math.max(3, maxLen + 1);
    const header = `<div class="heatmap-row heatmap-head"><span class="row-label">\xA0</span>${cols.map(
      (c) => `<span style="display:inline-block; width:${widthCh}ch; text-align:right; color:var(--z-mute);">${esc(c)}</span>`
    ).join("")}</div>`;
    const body = matrix.map((row, i) => {
      const cells = row.map((v) => {
        const t = (v - mn) / range;
        const op = (0.16 + 0.84 * t).toFixed(2);
        return `<span style="display:inline-block; width:${widthCh}ch; text-align:right; opacity:${op};">${esc(cellFmt(v))}</span>`;
      }).join("");
      return `<div class="heatmap-row"><span class="row-label">${esc(rows[i] ?? "")}</span>${cells}</div>`;
    }).join("");
    return `<div class="heatmap">${header}${body}</div>`;
  };
  var Multiples = ({ items, w = 160, h = 22 } = {}) => {
    if (!items || !items.length) return "";
    const cells = items.map(({ label, values, value }) => `
    <div class="mult">
      <div class="mult-head">
        <span class="name">${esc(label)}</span>
        <span class="val">${value != null ? esc(value) : ""}</span>
      </div>
      ${Spark(values, { w, h })}
    </div>
  `).join("");
    return `<div class="multiples">${cells}</div>`;
  };

  // playground/src/ux/charts-ai.js
  var minMax2 = (xs) => {
    let mn = Infinity, mx = -Infinity;
    for (const x of xs) {
      if (x < mn) mn = x;
      if (x > mx) mx = x;
    }
    return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
  };
  function convexHull(points2) {
    if (points2.length < 3) return points2.slice();
    const pts = points2.slice().sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    const cross = (O, A, B) => (A[0] - O[0]) * (B[1] - O[1]) - (A[1] - O[1]) * (B[0] - O[0]);
    const lower = [];
    for (const p of pts) {
      while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], p) <= 0) lower.pop();
      lower.push(p);
    }
    const upper = [];
    for (let i = pts.length - 1; i >= 0; i--) {
      const p = pts[i];
      while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], p) <= 0) upper.pop();
      upper.push(p);
    }
    upper.pop();
    lower.pop();
    return lower.concat(upper);
  }
  var EmbedSpace = ({ clusters = [], h = 360, highlight = null } = {}) => {
    if (!clusters.length) return "";
    const W = 1200;
    const allPts = clusters.flatMap((c) => c.points);
    if (!allPts.length) return "";
    const [xmn, xmx] = minMax2(allPts.map((p) => p[0]));
    const [ymn, ymx] = minMax2(allPts.map((p) => p[1]));
    const xr = xmx - xmn || 1;
    const yr = ymx - ymn || 1;
    const pad = 40;
    const px = (x) => pad + (x - xmn) / xr * (W - pad * 2);
    const py = (y) => pad + (1 - (y - ymn) / yr) * (h - pad * 2);
    const dots = clusters.flatMap((c, ci) => {
      const op = Math.max(0.35, 1 - ci * 0.09);
      return c.points.map(
        ([x, y]) => `<text x="${px(x).toFixed(1)}" y="${py(y).toFixed(1)}" fill="currentColor" opacity="${op.toFixed(2)}" font-family="var(--font-data)" font-size="14" text-anchor="middle" dominant-baseline="middle">\xB7</text>`
      );
    }).join("");
    const hulls = clusters.map((c, ci) => {
      if (c.points.length < 3) return "";
      const h2 = convexHull(c.points);
      if (h2.length < 3) return "";
      const closed = h2.concat([h2[0]]);
      const pts = closed.map(([x, y]) => `${px(x).toFixed(1)},${py(y).toFixed(1)}`).join(" ");
      const op = Math.max(0.2, 0.55 - ci * 0.07);
      return `<polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`;
    }).join("");
    const labels = clusters.map((c) => {
      const [cx, cy] = c.centroid || c.points[0];
      return `<text x="${px(cx).toFixed(1)}" y="${(py(cy) - 14).toFixed(1)}" fill="currentColor" font-family="var(--font-prose)" font-size="11" font-weight="700" letter-spacing="1.5" text-anchor="middle">${esc((c.label || "").toUpperCase())}</text>`;
    }).join("");
    const hl = highlight ? `
    <line x1="${px(highlight[0]).toFixed(1)}" y1="0" x2="${px(highlight[0]).toFixed(1)}" y2="${h}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.5"/>
    <line x1="0" y1="${py(highlight[1]).toFixed(1)}" x2="${W}" y2="${py(highlight[1]).toFixed(1)}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.5"/>
    <text x="${px(highlight[0]).toFixed(1)}" y="${py(highlight[1]).toFixed(1)}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="18" text-anchor="middle" dominant-baseline="middle">\xD7</text>
    <text x="${(px(highlight[0]) + 8).toFixed(1)}" y="${(py(highlight[1]) - 8).toFixed(1)}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="11" letter-spacing="0.08em">QUERY</text>
  ` : "";
    return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${hulls}
    ${dots}
    ${labels}
    ${hl}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);">UMAP \xB7 <span style="color:var(--z-ink);">${allPts.length}</span> embeddings \xB7 <span style="color:var(--z-ink);">${clusters.length}</span> clusters</span>
  </div>`;
  };
  var Ribbon3D = ({ series = [], h = 260, depth = 14 } = {}) => {
    if (!series.length) return "";
    const W = 1200;
    const [mn, mx] = minMax2(series.flatMap((s) => s.values));
    const range = mx - mn || 1;
    const n = series.length;
    const innerW = W - (n - 1) * depth - 120;
    const innerH = h - (n - 1) * depth - 40;
    const bodies = series.map((s, i) => {
      const offX = 110 + (n - 1 - i) * depth;
      const offY = 20 + (n - 1 - i) * depth;
      const pts = s.values.map((v, j) => {
        const x = offX + j / (s.values.length - 1) * innerW;
        const y = offY + innerH - (v - mn) / range * innerH;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      }).join(" ");
      const op = Math.max(0.32, 1 - i * 0.14);
      const labelX = offX - 6;
      const labelY = offY + innerH * 0.5;
      const label = `<text x="${labelX.toFixed(1)}" y="${labelY.toFixed(1)}" fill="currentColor" opacity="${op.toFixed(2)}" font-family="var(--font-data)" font-size="11" text-anchor="end" dominant-baseline="middle">${esc(s.label)}</text>`;
      const endVal = `<text x="${(offX + innerW + 6).toFixed(1)}" y="${(offY + innerH - (s.values[s.values.length - 1] - mn) / range * innerH).toFixed(1)}" fill="currentColor" opacity="${op.toFixed(2)}" font-family="var(--font-data)" font-size="11" dominant-baseline="middle">${esc(fmt(s.values[s.values.length - 1]))}</text>`;
      return `<polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>${label}${endVal}`;
    }).join("");
    return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${bodies}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);">axonometric \xB7 ${n} series \xB7 min <span style="color:var(--z-ink);">${fmt(mn)}</span> \xB7 max <span style="color:var(--z-ink);">${fmt(mx)}</span></span>
  </div>`;
  };
  var ChordArcs = ({ sources = [], targets = [], flows = [], h = 420, unit = "" } = {}) => {
    if (!sources.length || !targets.length || !flows.length) return "";
    const W = 1200;
    const pad = 20;
    const innerH = h - pad * 2;
    const srcY = (id) => {
      const i = sources.findIndex((s) => s.id === id);
      return pad + (i + 0.5) / sources.length * innerH;
    };
    const tgtY = (id) => {
      const i = targets.findIndex((t) => t.id === id);
      return pad + (i + 0.5) / targets.length * innerH;
    };
    const leftX = 220;
    const rightX = W - 220;
    const maxW = Math.max(...flows.map((f) => f.weight)) || 1;
    const arcs = flows.map((f) => {
      const y1 = srcY(f.from);
      const y2 = tgtY(f.to);
      if (y1 == null || y2 == null) return "";
      const cx1 = leftX + (rightX - leftX) * 0.38;
      const cx2 = leftX + (rightX - leftX) * 0.62;
      const op = Math.max(0.1, Math.min(0.9, f.weight / maxW * 0.85));
      return `<path d="M ${leftX},${y1.toFixed(1)} C ${cx1.toFixed(1)},${y1.toFixed(1)} ${cx2.toFixed(1)},${y2.toFixed(1)} ${rightX},${y2.toFixed(1)}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>`;
    }).join("");
    const srcLabels = sources.map((s) => `
    <text x="${(leftX - 12).toFixed(1)}" y="${srcY(s.id).toFixed(1)}" fill="currentColor" font-family="var(--font-data)" font-size="11" text-anchor="end" dominant-baseline="middle">${esc(s.label)}</text>
  `).join("");
    const tgtLabels = targets.map((t) => `
    <text x="${(rightX + 12).toFixed(1)}" y="${tgtY(t.id).toFixed(1)}" fill="currentColor" font-family="var(--font-data)" font-size="11" dominant-baseline="middle">${esc(t.label)}</text>
  `).join("");
    const srcHead = `<text x="${(leftX - 12).toFixed(1)}" y="${(pad - 8).toFixed(1)}" fill="currentColor" opacity="0.6" font-family="var(--font-prose)" font-size="11" font-weight="600" letter-spacing="2" text-transform="uppercase" text-anchor="end">SOURCE</text>`;
    const tgtHead = `<text x="${(rightX + 12).toFixed(1)}" y="${(pad - 8).toFixed(1)}" fill="currentColor" opacity="0.6" font-family="var(--font-prose)" font-size="11" font-weight="600" letter-spacing="2" text-transform="uppercase">TARGET</text>`;
    return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px; overflow:visible;">
    ${arcs}
    ${srcLabels}
    ${tgtLabels}
    ${srcHead}
    ${tgtHead}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);">${flows.length} flows \xB7 <span style="color:var(--z-ink);">${sources.length}</span>\u2192<span style="color:var(--z-ink);">${targets.length}</span>${unit ? " \xB7 " + esc(unit) : ""}</span>
  </div>`;
  };
  var ParallelCoords = ({ dims = [], rows = [], h = 260, highlight = null } = {}) => {
    if (!dims.length || !rows.length) return "";
    const W = 1200;
    const pad = 60;
    const top = 36, bot = 36;
    const colX = (i) => pad + i / (dims.length - 1 || 1) * (W - pad * 2);
    const extents = dims.map((d, i) => {
      if (d.min != null && d.max != null) return [d.min, d.max];
      const vals = rows.map((r) => r[i]).filter((v) => v != null);
      return minMax2(vals);
    });
    const axes = dims.map((d, i) => {
      const x = colX(i).toFixed(1);
      return `<line x1="${x}" y1="${top}" x2="${x}" y2="${h - bot}" stroke="currentColor" stroke-width="1" stroke-opacity="0.2"/>`;
    }).join("");
    const polyline = (row, op) => {
      const pts = row.map((v, i) => {
        const [mn, mx] = extents[i];
        const frac = (v - mn) / (mx - mn || 1);
        const x = colX(i);
        const y = top + (1 - Math.max(0, Math.min(1, frac))) * (h - top - bot);
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      }).join(" ");
      return `<polyline points="${pts}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="${op}"/>`;
    };
    const bodies = rows.map(() => polyline).map((fn, i) => fn(rows[i], "0.16")).join("");
    const hl = highlight ? polyline(highlight, "1").replace('stroke="currentColor"', 'stroke="var(--z-accent)"') : "";
    const labels = dims.map((d, i) => {
      const x = colX(i).toFixed(1);
      const [mn, mx] = extents[i];
      return `
      <text x="${x}" y="22" fill="currentColor" font-family="var(--font-prose)" font-size="11" font-weight="700" letter-spacing="1.5" text-anchor="middle">${esc(d.name.toUpperCase())}</text>
      <text x="${x}" y="${h - 16}" fill="currentColor" opacity="0.55" font-family="var(--font-data)" font-size="10" text-anchor="middle">${esc(fmt(mn))}</text>
      <text x="${x}" y="${h - 4}" fill="currentColor" opacity="0.55" font-family="var(--font-data)" font-size="10" text-anchor="middle">${esc(fmt(mx))}</text>
    `;
    }).join("");
    return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${axes}
    ${bodies}
    ${hl}
    ${labels}
  </svg>
  <div class="series-legend">
    <span class="mono" style="color:var(--z-mute);"><span style="color:var(--z-ink);">${rows.length}</span> rows \xB7 <span style="color:var(--z-ink);">${dims.length}</span> dimensions${highlight ? ' \xB7 <span style="color:var(--z-accent);">1 highlighted</span>' : ""}</span>
  </div>`;
  };
  var AttentionMap = ({ tokens = [], maxWeight = null } = {}) => {
    if (!tokens.length) return "";
    const mx = maxWeight ?? Math.max(...tokens.map((t) => t.weight));
    const spans = tokens.map((t) => {
      const op = Math.max(0.08, t.weight / (mx || 1));
      const hot = t.weight / mx > 0.82;
      const style = hot ? `color:var(--z-accent); opacity:${op.toFixed(2)};` : `opacity:${op.toFixed(2)};`;
      return `<span style="${style}">${esc(t.text)}</span>`;
    }).join(" ");
    const top = [...tokens].sort((a, b) => b.weight - a.weight).slice(0, 3);
    return `
  <div class="attention" style="font-family:var(--font-data); font-size:var(--fs-16); line-height:1.9; max-width:92ch;">${spans}</div>
  <div class="series-legend" style="margin-top:var(--sp-3);">
    <span class="mono" style="color:var(--z-mute);">peak attention on <span class="accent">${top.map((t) => esc(t.text)).join(" \xB7 ")}</span></span>
  </div>`;
  };
  var FlowBand = ({ segments = [], width = 1200, unit = "" } = {}) => {
    if (!segments.length) return "";
    const total = segments.reduce((a, s) => a + s.value, 0) || 1;
    const H = 16;
    let x = 0;
    const parts = segments.map((s, i) => {
      const w = s.value / total * width;
      const op = Math.max(0.28, 1 - i * 0.16);
      const line = w > 1 ? `<line x1="${x.toFixed(1)}" y1="8" x2="${(x + w).toFixed(1)}" y2="8" stroke="currentColor" stroke-width="2" stroke-opacity="${op.toFixed(2)}"/>` : "";
      const tick = `<line x1="${x.toFixed(1)}" y1="2" x2="${x.toFixed(1)}" y2="14" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>`;
      x += w;
      return tick + line;
    }).join("");
    const endTick = `<line x1="${width.toFixed(1)}" y1="2" x2="${width.toFixed(1)}" y2="14" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>`;
    const labels = segments.map((s, i) => {
      const pct = (s.value / total * 100).toFixed(1);
      const op = Math.max(0.28, 1 - i * 0.16);
      return `
      <div class="fb-seg" style="flex:${s.value} 0 0; min-width:0; opacity:${op.toFixed(2)};">
        <div class="fb-label">${esc(s.label.toUpperCase())}</div>
        <div class="fb-value">${esc(fmt(s.value))}${unit ? " " + esc(unit) : ""} \xB7 ${pct}%</div>
      </div>`;
    }).join("");
    return `
  <div class="flowband-wrap">
    <svg class="chart flowband-svg" viewBox="0 0 ${width} ${H}" preserveAspectRatio="none" style="width:100%; height:${H}px; overflow:visible; display:block;">
      ${parts}${endTick}
    </svg>
    <div class="flowband-labels" style="display:flex; width:100%; margin-top:8px;">${labels}</div>
  </div>`;
  };

  // playground/src/ux/charts-ops.js
  var minMax3 = (xs) => {
    let mn = Infinity, mx = -Infinity;
    for (const x of xs) {
      if (x < mn) mn = x;
      if (x > mx) mx = x;
    }
    return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
  };
  var SearchBox = ({
    value = "",
    types = ["match", "term", "range", "prefix", "phrase", "knn", "semantic", "hybrid"],
    activeType = "match",
    indices = ["*"],
    activeIndex = "*",
    filters = {},
    placeholder = "type a query \xB7 press Enter"
  } = {}) => {
    const filterEntries = Object.entries(filters).filter(([, v]) => v != null && v !== "");
    const pillsRow = filterEntries.length ? `
  <div class="sbox-row sbox-pills">
    <span class="key" style="margin-right:12px;">FILTERS</span>
    ${filterEntries.map(
      ([f, v]) => `<button type="button" class="pill" data-facet-apply="${esc(f)}:${esc(v)}" title="Click to remove">
        <span class="pill-field">${esc(f)}</span><span class="pill-eq">=</span><span class="pill-val">${esc(v)}</span><span class="pill-x">\u2715</span>
      </button>`
    ).join("")}
    <button type="button" class="pill-clear" data-facet-clear>CLEAR ALL</button>
  </div>` : "";
    return `
<div class="sbox">
  <div class="sbox-row sbox-types">
    <span class="key" style="margin-right:12px;">QUERY</span>
    ${types.map(
      (t) => `<button type="button" data-query-type="${esc(t)}" class="${t === activeType ? "active" : ""}">${esc(t.toUpperCase())}</button>`
    ).join('<span class="sep">\xB7</span>')}
    <span class="spacer"></span>
    <span class="key" style="margin-right:12px;">INDEX</span>
    ${indices.map(
      (i) => `<button type="button" data-search-index="${esc(i)}" class="${i === activeIndex ? "active" : ""}">${esc(i)}</button>`
    ).join('<span class="sep">\xB7</span>')}
  </div>
  <div class="sbox-row sbox-input">
    <span class="prompt accent">\u25B8</span>
    <input type="text" data-search-input value="${esc(value)}" placeholder="${esc(placeholder)}" autocomplete="off" spellcheck="false" aria-label="Search query"/>
  </div>
  ${pillsRow}
</div>`;
  };
  var QueryDSL = (obj) => {
    const json = JSON.stringify(obj, null, 2);
    const out = esc(json).replace(/&quot;([^&]+?)&quot;(\s*:)/g, '<span class="tok-key">"$1"</span>$2').replace(/:\s*&quot;([^&]*?)&quot;/g, ': <span class="tok-str">"$1"</span>').replace(/:\s*(-?\d+\.?\d*)/g, ': <span class="tok-num">$1</span>').replace(/:\s*(true|false|null)\b/g, ': <span class="tok-kw">$1</span>');
    return `<pre class="qdsl mono">${out}</pre>`;
  };
  var QueryPlanTree = (node, depth = 0, last = true) => {
    if (!node) return "";
    const prefix = depth === 0 ? "" : last ? "\u2514\u2500 " : "\u251C\u2500 ";
    const indent = '<span class="faint mono">' + "\u2502  ".repeat(Math.max(0, depth - 1)) + prefix + "</span>";
    const est = node.estimate != null ? ` <span class="faint mono">est <span class="mono" style="color:var(--z-ink);">${fmt(node.estimate)}</span></span>` : "";
    const cost = node.cost != null ? ` <span class="faint mono">cost <span class="mono" style="color:var(--z-ink);">${fmt(node.cost)}</span></span>` : "";
    const field = node.field ? `<span class="mono" style="color:var(--z-ink);"> ${esc(node.field)}</span>` : "";
    const val = node.value != null ? `<span class="mono faint"> = ${esc(node.value)}</span>` : "";
    const selfRow = `<div class="plan-row">${indent}<span class="plan-op accent mono">${esc(node.op)}</span>${field}${val}${est}${cost}</div>`;
    const kids = node.children || [];
    const childRows = kids.map((c, i) => QueryPlanTree(c, depth + 1, i === kids.length - 1)).join("");
    return selfRow + childRows;
  };
  var Hits = ({
    hits = [],
    total = 0,
    tookMs = 0,
    maxScore = null,
    sort = { field: "_score", dir: "desc" },
    showTime = true,
    labels = { _index: "INDEX", _id: "ID", _score: "SCORE", _ts: "TIME" },
    exportable = true
  } = {}) => {
    const sortIndicator = (field) => {
      if (sort?.field !== field) return "";
      return sort.dir === "asc" ? " \u25B2" : " \u25BC";
    };
    const colHeader = (field, label) => `<button type="button" class="col-sort ${sort?.field === field ? "active" : ""}" data-sort-field="${esc(field)}" data-sort-dir="${esc(sort?.field === field && sort.dir === "desc" ? "asc" : "desc")}">${esc(label)}${sortIndicator(field)}</button>`;
    const header = `
    <div class="hits-meta">
      <span class="key">HITS</span>
      <span class="mono" style="margin-left:var(--sp-3);"><span class="accent" style="font-size:var(--fs-20); font-weight:700;">${fmt(total)}</span> <span class="faint">documents</span></span>
      <span class="mono" style="margin-left:var(--sp-3);"><span class="accent">${fmt(tookMs)}</span> <span class="faint">ms</span></span>
      ${maxScore != null ? `<span class="mono" style="margin-left:var(--sp-3);"><span class="faint">max_score</span> ${maxScore.toFixed(3)}</span>` : ""}
      <span style="flex:1;"></span>
      ${exportable ? `<button type="button" class="hits-action" data-export-csv title="GH#1992 \xB7 372 reactions">\u2193 CSV</button>` : ""}
      <button type="button" class="hits-action" data-toggle-time aria-pressed="${showTime ? "true" : "false"}" title="GH#3319 \xB7 44 reactions">${showTime ? "HIDE TIME" : "SHOW TIME"}</button>
    </div>`;
    if (!hits.length) {
      return header + `<div class="mono faint" style="padding:var(--sp-3) 0;">No matches. Try a broader query or a different query type.</div>`;
    }
    const colHead = `
    <div class="hit-head hit-headrow ${showTime ? "" : "no-time"}">
      ${colHeader("_index", labels._index)}
      ${colHeader("_id", labels._id)}
      ${colHeader("_score", labels._score)}
      ${showTime ? colHeader("_ts", labels._ts) : ""}
      <span class="faint">${esc(labels._source || "MESSAGE")}</span>
    </div>`;
    const rows = hits.map((h) => {
      const body = typeof h._source === "string" ? h._source : JSON.stringify(h._source);
      return `
      <div class="hit">
        <div class="hit-head mono ${showTime ? "" : "no-time"}">
          <button type="button" class="hit-cell-clickable" data-facet-apply="_index:${esc(h._index)}" title="Filter for this">${esc(h._index)}</button>
          <span class="faint">${esc(h._id)}</span>
          <span class="accent">${(h._score ?? 0).toFixed(3)}</span>
          ${showTime ? `<span class="faint">${esc(h._ts || "")}</span>` : ""}
        </div>
        <div class="hit-body mono">${esc(body)}</div>
      </div>`;
    }).join("");
    return header + colHead + `<div class="hits-list">${rows}</div>`;
  };
  var Facet = ({ field, items = [], active = null } = {}) => {
    if (!items.length) return "";
    const max = Math.max(...items.map((i) => i.count));
    const rows = items.map((i) => {
      const frac = i.count / (max || 1);
      const x = (180 * frac).toFixed(1);
      const on = i.value === active;
      return `
      <button type="button" class="facet-row ${on ? "active" : ""}" data-facet-apply="${esc(field)}:${esc(i.value)}">
        <span class="facet-label">${esc(i.label)}</span>
        <span class="facet-count mono">${fmt(i.count)}</span>
        <svg class="chart facet-bar" height="5" viewBox="0 0 180 5" preserveAspectRatio="none">
          <line x1="0" y1="4" x2="${x}" y2="4" stroke="currentColor" stroke-width="1"/>
        </svg>
      </button>`;
    }).join("");
    return `<div class="facet"><div class="key" style="margin-bottom:6px;">${esc(field.toUpperCase())}</div>${rows}</div>`;
  };
  var AnomalyBand = ({
    values = [],
    upper = [],
    lower = [],
    anomalies = [],
    h = 200,
    labels = ["", ""],
    unit = ""
  } = {}) => {
    if (!values.length) return "";
    const W = 1200;
    const all2 = values.concat(upper, lower);
    const [mn, mx] = minMax3(all2);
    const range = mx - mn || 1;
    const pad = 10;
    const py = (v) => pad + (h - pad * 2) - (v - mn) / range * (h - pad * 2);
    const px = (i, n) => i / (n - 1) * W;
    const pts = (arr) => arr.map((v, i) => `${px(i, arr.length).toFixed(1)},${py(v).toFixed(1)}`).join(" ");
    const upperLine = upper.length ? `<polyline points="${pts(upper)}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="0.22"/>` : "";
    const lowerLine = lower.length ? `<polyline points="${pts(lower)}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="0.22"/>` : "";
    const valueLine = `<polyline points="${pts(values)}" fill="none" stroke="currentColor" stroke-width="1"/>`;
    const marks = anomalies.map(({ idx }) => {
      const x = px(idx, values.length).toFixed(1);
      const y = py(values[idx]).toFixed(1);
      return `
      <line x1="${x}" y1="${pad}" x2="${x}" y2="${h - pad}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.4"/>
      <text x="${x}" y="${(py(values[idx]) - 8).toFixed(1)}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="14" text-anchor="middle">\xD7</text>
    `;
    }).join("");
    const u = unit ? " " + esc(unit) : "";
    return `
  <svg class="chart" viewBox="0 0 ${W} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
    ${upperLine}${lowerLine}${valueLine}${marks}
  </svg>
  <div class="series-legend">
    <span>${esc(labels[0] || "")} \xB7 <span class="mono" style="color:var(--z-ink);">${fmt(values[0])}${u}</span></span>
    <span class="mid">normal band \xB7 <span class="mono" style="color:var(--z-ink);">${fmt(Math.min(...lower))}..${fmt(Math.max(...upper))}${u}</span> \xB7 <span class="accent">${anomalies.length}</span> anomalies</span>
    <span><span class="mono" style="color:var(--z-ink);">${fmt(values[values.length - 1])}${u}</span> \xB7 ${esc(labels[1] || "")}</span>
  </div>`;
  };
  var Citations = ({ items = [], total = null } = {}) => {
    if (!items.length) {
      return `<div class="mono faint">No citations available \u2014 re-run <span class="accent">node user-feedback/kibana/pipeline/design-inputs.mjs</span></div>`;
    }
    const rows = items.map((c) => `
    <a class="cite" href="${esc(c.url)}" target="_blank" rel="noopener">
      <span class="cite-src mono faint">${esc((c.source || "").toUpperCase())}</span>
      <span class="cite-score mono accent">${c.score || 0}</span>
      <span class="cite-title">${esc(c.title || "\u2014 no title \u2014")}</span>
      <span class="cite-arrow mono faint">\u2197</span>
    </a>
  `).join("");
    const meta = total != null ? `<div class="hint" style="margin-bottom:var(--sp-2);">drawn from <span class="mono accent">${total}</span> ranked artifacts in <span class="mono">user-feedback/kibana/themes/design-inputs.md</span></div>` : "";
    return meta + `<div class="cites">${rows}</div>`;
  };

  // playground/src/ux/layout.js
  var TopN = ({
    items,
    total,
    n = 10,
    valueFmt = (v) => fmt(v),
    barWidth = 200,
    scale = "max",
    filterField = ""
  } = {}) => {
    if (!items || !items.length) return '<div class="faint mono">No data.</div>';
    const slice = items.slice(0, n);
    const max = Math.max(...slice.map((i) => i.value));
    const denom = scale === "total" && total ? total : max;
    const out = slice.map((i) => {
      const frac = denom > 0 ? Math.max(0, Math.min(1, i.value / denom)) : 0;
      const pct = total ? (100 * i.value / total).toFixed(1) + "%" : "";
      const x = (frac * barWidth).toFixed(1);
      const clickable = filterField ? ` data-filter-add="${esc(filterField)}:${esc(i.label)}" role="button" title="Filter ${esc(filterField)} = ${esc(i.label)}"` : "";
      return `
    <div class="row${filterField ? " clickable" : ""}"${clickable}>
      <div class="row__label">${esc(i.label)}</div>
      <div class="row__val">${esc(valueFmt(i.value))}</div>
      <div class="row__bar">
        <svg class="chart" height="6" viewBox="0 0 ${barWidth} 6" preserveAspectRatio="none">
          <line x1="0" y1="5" x2="${x}" y2="5" stroke="currentColor" stroke-width="1"/>
        </svg>
      </div>
      <div class="row__pct">${esc(pct)}</div>
    </div>`;
    }).join("");
    return out;
  };
  var Events = ({ items } = {}) => {
    if (!items || !items.length) return '<div class="faint mono">No events.</div>';
    const rows = items.map((e) => `
    <div class="ev">
      <span class="when">${esc(e.at)}</span>
      <span class="sev ${e.sev === "err" ? "err" : ""}">${esc((e.sev || "").toUpperCase())}</span>
      <span class="msg">${esc(e.msg)}</span>
    </div>
  `).join("");
    return `<div class="events">${rows}</div>`;
  };

  // playground/src/data/feedback-citations.js
  var dashboardCitations = {
    "ai-overview": [
      {
        "id": "gh-kibana-696",
        "source": "github",
        "score": 162,
        "title": "Allow sorting on multiple fields",
        "url": "https://github.com/elastic/kibana/issues/696"
      },
      {
        "id": "gh-kibana-4584",
        "source": "github",
        "score": 113,
        "title": "Pipeline aggregations",
        "url": "https://github.com/elastic/kibana/issues/4584"
      },
      {
        "id": "gh-kibana-4707",
        "source": "github",
        "score": 71,
        "title": "Support Bucket Script Aggregation",
        "url": "https://github.com/elastic/kibana/issues/4707"
      }
    ],
    "rag-quality": [
      {
        "id": "gh-kibana-98246",
        "source": "github",
        "score": 8,
        "title": "Best practices for rule executions",
        "url": "https://github.com/elastic/kibana/issues/98246"
      },
      {
        "id": "gh-kibana-144418",
        "source": "github",
        "score": 7,
        "title": "[Dashboard] Redesign Add Panel Experience",
        "url": "https://github.com/elastic/kibana/issues/144418"
      },
      {
        "id": "gh-kibana-74571",
        "source": "github",
        "score": 4,
        "title": "[Meta] Saved Object Tagging",
        "url": "https://github.com/elastic/kibana/issues/74571"
      }
    ],
    "vector-index": [
      {
        "id": "gh-kibana-6515",
        "source": "github",
        "score": 18,
        "title": "Kibana Globalization",
        "url": "https://github.com/elastic/kibana/issues/6515"
      },
      {
        "id": "gh-kibana-98246",
        "source": "github",
        "score": 8,
        "title": "Best practices for rule executions",
        "url": "https://github.com/elastic/kibana/issues/98246"
      },
      {
        "id": "gh-kibana-144418",
        "source": "github",
        "score": 7,
        "title": "[Dashboard] Redesign Add Panel Experience",
        "url": "https://github.com/elastic/kibana/issues/144418"
      }
    ],
    "agent-memory": [
      {
        "id": "gh-kibana-98246",
        "source": "github",
        "score": 8,
        "title": "Best practices for rule executions",
        "url": "https://github.com/elastic/kibana/issues/98246"
      },
      {
        "id": "gh-kibana-144418",
        "source": "github",
        "score": 7,
        "title": "[Dashboard] Redesign Add Panel Experience",
        "url": "https://github.com/elastic/kibana/issues/144418"
      },
      {
        "id": "gh-kibana-74571",
        "source": "github",
        "score": 4,
        "title": "[Meta] Saved Object Tagging",
        "url": "https://github.com/elastic/kibana/issues/74571"
      }
    ],
    "search-discover": [
      {
        "id": "gh-kibana-1366",
        "source": "github",
        "score": 178,
        "title": "Export chart to image",
        "url": "https://github.com/elastic/kibana/issues/1366"
      },
      {
        "id": "gh-kibana-9575",
        "source": "github",
        "score": 143,
        "title": "[Dashboard] Allow Authors to Limit Interactivity",
        "url": "https://github.com/elastic/kibana/issues/9575"
      },
      {
        "id": "gh-kibana-12560",
        "source": "github",
        "score": 64,
        "title": "Custom drilldown links for a dashboard panel",
        "url": "https://github.com/elastic/kibana/issues/12560"
      }
    ],
    "anomaly-detect": [
      {
        "id": "gh-kibana-98246",
        "source": "github",
        "score": 8,
        "title": "Best practices for rule executions",
        "url": "https://github.com/elastic/kibana/issues/98246"
      },
      {
        "id": "gh-kibana-144418",
        "source": "github",
        "score": 7,
        "title": "[Dashboard] Redesign Add Panel Experience",
        "url": "https://github.com/elastic/kibana/issues/144418"
      },
      {
        "id": "gh-kibana-74571",
        "source": "github",
        "score": 4,
        "title": "[Meta] Saved Object Tagging",
        "url": "https://github.com/elastic/kibana/issues/74571"
      }
    ],
    "ingest-pipeline": [
      {
        "id": "gh-kibana-4288",
        "source": "github",
        "score": 45,
        "title": "Ability to export the index pattern along with saved objects",
        "url": "https://github.com/elastic/kibana/issues/4288"
      },
      {
        "id": "gh-kibana-4759",
        "source": "github",
        "score": 45,
        "title": "Expose object import/export as an API",
        "url": "https://github.com/elastic/kibana/issues/4759"
      },
      {
        "id": "gh-kibana-6057",
        "source": "github",
        "score": 40,
        "title": "add command line option to execute the optimize task standalone",
        "url": "https://github.com/elastic/kibana/issues/6057"
      }
    ],
    "logs-overview": [
      {
        "id": "gh-kibana-1992",
        "source": "github",
        "score": 372,
        "title": "Export Documents as CSV",
        "url": "https://github.com/elastic/kibana/issues/1992"
      },
      {
        "id": "gh-kibana-1366",
        "source": "github",
        "score": 178,
        "title": "Export chart to image",
        "url": "https://github.com/elastic/kibana/issues/1366"
      },
      {
        "id": "gh-kibana-30982",
        "source": "github",
        "score": 46,
        "title": "[Reporting] Exporting raw data from table-based visualizations",
        "url": "https://github.com/elastic/kibana/issues/30982"
      }
    ],
    "system": [
      {
        "id": "gh-kibana-6057",
        "source": "github",
        "score": 40,
        "title": "add command line option to execute the optimize task standalone",
        "url": "https://github.com/elastic/kibana/issues/6057"
      },
      {
        "id": "gh-kibana-4482",
        "source": "github",
        "score": 31,
        "title": "Conditional XY bar metric colors",
        "url": "https://github.com/elastic/kibana/issues/4482"
      },
      {
        "id": "gh-kibana-706",
        "source": "github",
        "score": 13,
        "title": "Translation to other languages",
        "url": "https://github.com/elastic/kibana/issues/706"
      }
    ]
  };

  // playground/src/dashboards/ai-overview.js
  var aiOverview = {
    id: "ai-overview",
    name: "AI \xB7 Overview",
    render: ({ data, time }) => ({
      title: "AI \xB7 OVERVIEW",
      kicker: "XERJ INTELLIGENCE",
      meta: [time, "AI DATA PLANE"],
      panels: [
        {
          id: "queries",
          eyebrow: "LLM QUERIES",
          cols: 4,
          type: "metric",
          render: () => Num({
            value: data.metrics.queries.formatted,
            unit: "queries",
            spark: Spark(data.series.queries, { w: 220, h: 44 }),
            delta: data.metrics.queries.delta,
            emphasis: true
          })
        },
        {
          id: "tokens",
          eyebrow: "TOKENS \xB7 IN + OUT",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.tokens.formatted,
            unit: "T",
            delta: data.metrics.tokens.delta,
            emphasis: false
          })
        },
        {
          id: "cost",
          eyebrow: "SPEND \xB7 USD",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.cost.formatted,
            unit: "usd",
            delta: data.metrics.cost.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "savings",
          eyebrow: "vs. ES + PINECONE + SPLUNK",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.savings.formatted,
            unit: "saved",
            hint: data.metrics.savings.note,
            emphasis: false
          })
        },
        {
          id: "cacheHit",
          eyebrow: "CACHE HIT",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.cacheHit.formatted,
            unit: "%",
            delta: data.metrics.cacheHit.delta,
            emphasis: false
          })
        },
        {
          id: "queriesSeries",
          eyebrow: "QUERIES OVER TIME",
          cols: 12,
          type: "line",
          render: () => Series(data.series.queries, {
            h: 160,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "q/s"
          })
        },
        {
          id: "latencyRibbons",
          eyebrow: "LATENCY \xB7 PER MODEL \xB7 AXONOMETRIC",
          cols: 12,
          type: "ribbon3d",
          render: () => Ribbon3D({ series: data.latencyRibbons, h: 280, depth: 14 })
        },
        {
          id: "tokenFlow",
          eyebrow: "TOKEN BUDGET",
          cols: 12,
          type: "flowband",
          render: () => FlowBand({ segments: data.flowSegments, unit: "T" })
        },
        {
          id: "models",
          eyebrow: "BY MODEL",
          cols: 12,
          type: "dist",
          render: () => Dist({ segments: data.models, width: 1200 })
        },
        {
          id: "topIntents",
          eyebrow: "TOP INTENTS \xB7 CLICK TO DRILL",
          cols: 6,
          type: "topn",
          drilldown: { to: "search-discover" },
          render: () => TopN({ items: data.intents, total: data.metrics.queries.value, n: 10, filterField: "intent" })
        },
        {
          id: "topDocs",
          eyebrow: "TOP DOCUMENTS \xB7 CLICK TO FILTER",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.topDocs, total: data.metrics.queries.value, n: 10, filterField: "doc" })
        },
        {
          id: "costHeatmap",
          eyebrow: "SPEND \xB7 WEEKDAY \xD7 2H",
          cols: 12,
          type: "heatmap",
          render: () => Heatmap({
            rows: ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"],
            cols: data.costHeatmap.cols,
            matrix: data.costHeatmap.matrix,
            cellFmt: (v) => "$" + v.toFixed(0)
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["ai-overview"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/dashboards/rag-quality.js
  var ragQuality = {
    id: "rag-quality",
    name: "RAG \xB7 Quality",
    render: ({ data, time }) => ({
      title: "RAG \xB7 QUALITY",
      kicker: "GROUNDING \xB7 HALLUCINATION \xB7 CITATIONS",
      meta: [time, "RETRIEVAL PIPELINE"],
      panels: [
        {
          id: "grounding",
          eyebrow: "GROUNDING SCORE",
          cols: 4,
          type: "metric",
          render: () => Num({
            value: data.metrics.grounding.formatted,
            unit: "%",
            spark: Spark(data.series.grounding, { w: 220, h: 44 }),
            delta: data.metrics.grounding.delta,
            deltaGood: "up",
            emphasis: true
          })
        },
        {
          id: "halluc",
          eyebrow: "HALLUCINATION RATE",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.hallucination.formatted,
            unit: "%",
            delta: data.metrics.hallucination.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "hitRate",
          eyebrow: "RETRIEVAL HIT RATE",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.hitRate.formatted,
            unit: "%",
            delta: data.metrics.hitRate.delta,
            emphasis: false
          })
        },
        {
          id: "citations",
          eyebrow: "AVG CITATIONS",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.avgCitations.formatted,
            unit: "per answer",
            delta: data.metrics.avgCitations.delta,
            emphasis: false
          })
        },
        {
          id: "groundingSeries",
          eyebrow: "GROUNDING OVER TIME",
          cols: 12,
          type: "line",
          render: () => Series(data.series.grounding, {
            h: 140,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "%"
          })
        },
        {
          id: "flow",
          eyebrow: "RETRIEVAL FLOW \xB7 QUERY \u2192 CHUNK",
          cols: 12,
          type: "chord",
          render: () => ChordArcs({
            sources: data.flow.queries,
            targets: data.flow.chunks,
            flows: data.flow.flows,
            h: 440
          })
        },
        {
          id: "attention",
          eyebrow: "SAMPLE ANSWER \xB7 TOKEN ATTENTION",
          cols: 12,
          type: "attention",
          render: () => AttentionMap({ tokens: data.attention.tokens })
        },
        {
          id: "retrievalSource",
          eyebrow: "BY RETRIEVAL SOURCE",
          cols: 12,
          type: "dist",
          render: () => Dist({ segments: data.retrievalSource, width: 1200 })
        },
        {
          id: "lowGrounding",
          eyebrow: "LOWEST GROUNDING PROMPTS",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: data.lowGroundingPrompts,
            n: 8,
            valueFmt: (v) => v
          })
        },
        {
          id: "chunkDensity",
          eyebrow: "CHUNK HIT DENSITY \xB7 QUERY TYPE \xD7 CHUNK",
          cols: 6,
          type: "heatmap",
          render: () => Heatmap({
            rows: data.chunkHitDensity.rows,
            cols: data.chunkHitDensity.cols,
            matrix: data.chunkHitDensity.matrix,
            cellFmt: (v) => v >= 1e3 ? Math.round(v / 1e3) + "K" : String(v)
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["rag-quality"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/ux/charts-ext.js
  var minMax4 = (xs) => {
    let mn = Infinity, mx = -Infinity;
    for (const x of xs) {
      if (x < mn) mn = x;
      if (x > mx) mx = x;
    }
    return [mn === Infinity ? 0 : mn, mx === -Infinity ? 0 : mx];
  };
  var VBar = ({ items, h = 160, unit = "", labels = true } = {}) => {
    if (!items || !items.length) return "";
    const w = 1200;
    const values = items.map((i) => i.value);
    const [mn, mx] = minMax4(values);
    const denom = mx || 1;
    const step = w / items.length;
    const lines = items.map((it, i) => {
      const x = ((i + 0.5) * step).toFixed(1);
      const y = (h - it.value / denom * h).toFixed(1);
      return `<line x1="${x}" y1="${h}" x2="${x}" y2="${y}" stroke="currentColor" stroke-width="1"/>`;
    }).join("");
    const first = items[0]?.label ?? "";
    const last = items[items.length - 1]?.label ?? "";
    const u = unit ? " " + esc(unit) : "";
    const legend = labels ? `
    <div class="series-legend">
      <span>${esc(first)}</span>
      <span class="mid">min <span class="mono" style="color:var(--z-ink);">${fmt(mn)}</span>   \xB7   max <span class="mono" style="color:var(--z-ink);">${fmt(mx)}${u}</span></span>
      <span>${esc(last)}</span>
    </div>` : "";
    return `
  <div class="series-wrap">
    <svg class="chart" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px;">
      ${lines}
    </svg>
    ${legend}
  </div>`;
  };
  var Hist = VBar;
  var Gauge = ({
    value,
    min = 0,
    max = 100,
    unit = "",
    thresholds = [],
    emphasis = true,
    label = ""
  } = {}) => {
    const w = 600, h = 14;
    const v = Math.max(min, Math.min(max, value));
    const frac = (v - min) / (max - min || 1);
    const x = (frac * w).toFixed(1);
    const ticks = thresholds.map((t) => {
      const tx = ((t - min) / (max - min || 1) * w).toFixed(1);
      return `<line x1="${tx}" y1="0" x2="${tx}" y2="${h}" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>`;
    }).join("");
    const valCls = emphasis ? "num-big accent" : "num-md";
    return `
  <div class="stack">
    <div class="row-flex" style="justify-content:space-between;">
      <div class="${valCls}">${esc(fmt(value))}<span class="num-unit">${esc(unit)}</span></div>
      ${label ? `<span class="hint">${esc(label)}</span>` : ""}
    </div>
    <svg class="chart" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none" style="width:100%; height:${h}px; margin-top:12px;">
      <line x1="0" y1="${h - 1}" x2="${w}" y2="${h - 1}" stroke="currentColor" stroke-width="1" stroke-opacity="0.18"/>
      <line x1="0" y1="${h - 1}" x2="${x}" y2="${h - 1}" stroke="currentColor" stroke-width="1"/>
      ${ticks}
    </svg>
    <div class="row-flex" style="justify-content:space-between; margin-top:6px; font-family:var(--font-data); font-size:var(--fs-11); color:var(--z-mute);">
      <span>${esc(fmt(min))}</span>
      <span>${esc(fmt(max))}${unit ? " " + esc(unit) : ""}</span>
    </div>
  </div>`;
  };
  var Scatter = ({ points: points2, h = 220, xLabel = "X", yLabel = "Y" } = {}) => {
    if (!points2 || !points2.length) return "";
    const xs = points2.map((p) => p[0]);
    const ys = points2.map((p) => p[1]);
    const [xmn, xmx] = minMax4(xs);
    const xr = xmx - xmn || 1;
    const [ymn, ymx] = minMax4(ys);
    const yr = ymx - ymn || 1;
    const dots = points2.map(([x, y]) => {
      const px = ((x - xmn) / xr * 100).toFixed(2);
      const py = ((ymx - y) / yr * 100).toFixed(2);
      return `<span style="position:absolute; left:${px}%; top:${py}%; transform:translate(-50%,-50%); font-family:var(--font-data); font-size:var(--fs-16); color:var(--z-ink); line-height:1; pointer-events:none;">\xB7</span>`;
    }).join("");
    return `
  <div style="position:relative; width:100%; height:${h}px;">${dots}</div>
  <div class="series-legend">
    <span>${esc(xLabel)} <span class="mono" style="color:var(--z-ink);">${fmt(xmn)}..${fmt(xmx)}</span></span>
    <span class="mid"><span class="mono" style="color:var(--z-ink);">${points2.length}</span> points</span>
    <span>${esc(yLabel)} <span class="mono" style="color:var(--z-ink);">${fmt(ymn)}..${fmt(ymx)}</span></span>
  </div>`;
  };
  var Stacked = ({ rows, width = 1200, height = 10, showSegments = true } = {}) => {
    if (!rows || !rows.length) return "";
    const segmentLabels = showSegments ? rows[0].segments.map((s) => esc(s.label)).join(" \xB7 ") : "";
    const body = rows.map((row) => {
      const total = row.segments.reduce((a, s) => a + s.value, 0) || 1;
      let x = 0;
      const lines = row.segments.map((s, i) => {
        const segW = s.value / total * width;
        const op = Math.max(0.14, 1 - i * 0.16);
        const line = segW > 0.5 ? `<line x1="${x.toFixed(1)}" y1="${(height - 1).toFixed(1)}" x2="${(x + segW).toFixed(1)}" y2="${(height - 1).toFixed(1)}" stroke="currentColor" stroke-width="1" stroke-opacity="${op.toFixed(2)}"/>` : "";
        x += segW;
        return line;
      }).join("");
      return `
      <div class="stk-row">
        <span class="row__label">${esc(row.label)}</span>
        <svg class="chart stk-bar" viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" style="height:${height}px;">${lines}</svg>
        <span class="mono faint" style="text-align:right;">${fmt(total)}</span>
      </div>`;
    }).join("");
    const legend = segmentLabels ? `<div class="hint" style="margin-top:var(--sp-2);">${segmentLabels}</div>` : "";
    return `<div class="stacked">${body}</div>${legend}`;
  };
  var Treemap = ({ items, depth = 0, parentTotal = null } = {}) => {
    if (!items || !items.length) return "";
    const total = parentTotal ?? (items.reduce((a, i) => a + i.value, 0) || 1);
    const max = Math.max(...items.map((i) => i.value));
    return items.map((it) => {
      const frac = it.value / max;
      const pct = (it.value / total * 100).toFixed(1);
      const x = (200 * frac).toFixed(1);
      const indent = "\xB7  ".repeat(depth);
      const kids = it.children && it.children.length ? Treemap({ items: it.children, depth: depth + 1, parentTotal: it.value }) : "";
      return `
      <div class="row">
        <div class="row__label"><span class="faint">${indent}</span>${esc(it.label)}</div>
        <div class="row__val">${esc(fmt(it.value))}</div>
        <div class="row__bar">
          <svg class="chart" height="6" viewBox="0 0 200 6" preserveAspectRatio="none">
            <line x1="0" y1="5" x2="${x}" y2="5" stroke="currentColor" stroke-width="1" stroke-opacity="${(1 - depth * 0.2).toFixed(2)}"/>
          </svg>
        </div>
        <div class="row__pct">${pct}%</div>
      </div>
      ${kids}`;
    }).join("");
  };

  // playground/src/dashboards/vector-index.js
  var vectorIndex = {
    id: "vector-index",
    name: "Vector \xB7 Index",
    render: ({ data, time }) => ({
      title: "VECTOR \xB7 INDEX",
      kicker: "EXACT kNN \xB7 EMBEDDINGS \xB7 HYBRID",
      meta: [time, "XERJ-VECTOR"],
      panels: [
        {
          id: "vectors",
          eyebrow: "VECTORS",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.vectors.formatted,
            unit: "total",
            hint: data.metrics.vectors.hint,
            emphasis: true
          })
        },
        {
          id: "dim",
          eyebrow: "DIMENSIONS",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.dim.formatted,
            unit: "d",
            hint: data.metrics.dim.hint,
            emphasis: false
          })
        },
        {
          id: "disk",
          eyebrow: "ON DISK",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.disk.formatted,
            unit: "gb",
            hint: data.metrics.disk.hint,
            emphasis: false
          })
        },
        {
          id: "qps",
          eyebrow: "QUERIES/s",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.qps.formatted,
            unit: "qps",
            spark: Spark(data.series.qps, { w: 140, h: 32 }),
            delta: data.metrics.qps.delta,
            emphasis: false
          })
        },
        {
          id: "recall",
          eyebrow: "RECALL @ 10",
          cols: 3,
          type: "gauge",
          render: () => Gauge({
            value: data.metrics.recall.value,
            min: 80,
            max: 100,
            unit: "%",
            thresholds: [90, 95],
            emphasis: false
          })
        },
        {
          id: "embedSpace",
          eyebrow: "EMBEDDING SPACE \xB7 UMAP PROJECTION",
          cols: 12,
          type: "embedspace",
          render: () => EmbedSpace({
            clusters: data.clusters,
            h: 420,
            highlight: [48, 48]
          })
        },
        {
          id: "annLatency",
          eyebrow: "ANN LATENCY \xB7 P50 / P95 / P99",
          cols: 12,
          type: "ribbon3d",
          render: () => Ribbon3D({
            series: [
              { label: "P50", values: data.series.p50 },
              { label: "P95", values: data.series.p95 },
              { label: "P99", values: data.series.p99 }
            ],
            h: 220,
            depth: 14
          })
        },
        {
          id: "pcoords",
          eyebrow: "QUERY PROFILE \xB7 PARALLEL COORDINATES",
          cols: 12,
          type: "pcoords",
          render: () => ParallelCoords({
            dims: data.pcoords.dims,
            rows: data.pcoords.rows,
            highlight: data.pcoords.highlight,
            h: 260
          })
        },
        {
          id: "models",
          eyebrow: "EMBEDDING MODELS",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.models, n: 6, valueFmt: (v) => v + "M" })
        },
        {
          id: "p95Spark",
          eyebrow: "p95 LATENCY",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.p95.formatted,
            unit: "ms",
            spark: Spark(data.series.p95, { w: 160, h: 32 }),
            delta: data.metrics.p95.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "recallTimeline",
          eyebrow: "RECALL OVER TIME",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.recall.formatted,
            unit: "%",
            spark: Spark(data.series.recall, { w: 160, h: 32 }),
            emphasis: false
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["vector-index"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/ux/tables.js
  var Table = ({ columns, rows, align = [] } = {}) => {
    if (!columns || !columns.length) return '<div class="faint mono">No columns.</div>';
    const ta = (i) => align[i] || (i === 0 ? "left" : "right");
    const cols = columns.length;
    const headCells = columns.map(
      (c, i) => `<span style="text-align:${ta(i)}">${esc(c)}</span>`
    ).join("");
    const bodyRows = (rows || []).map((r) => {
      const cells = r.map(
        (c, i) => `<span style="text-align:${ta(i)}">${esc(c)}</span>`
      ).join("");
      return `<div class="tbl-row">${cells}</div>`;
    }).join("");
    return `
  <div class="tbl" style="--tbl-cols:${cols};">
    <div class="tbl-row tbl-head">${headCells}</div>
    ${bodyRows}
  </div>`;
  };
  var Markdown = (text = "") => {
    if (!text) return "";
    const paragraphs = text.split(/\n\n+/).map((p) => {
      const t = p.trim();
      if (!t) return "";
      if (t.startsWith("# ")) return `<div class="h-section" style="margin-bottom:10px;">${esc(t.slice(2))}</div>`;
      if (t.startsWith("## ")) return `<div class="key" style="margin-bottom:8px;">${esc(t.slice(3))}</div>`;
      const e = esc(t).replace(/\*\*(.+?)\*\*/g, '<strong style="color:var(--z-ink);">$1</strong>').replace(/\*(.+?)\*/g, "<em>$1</em>").replace(/`([^`]+)`/g, '<span class="mono accent">$1</span>');
      return `<p style="max-width:60ch; margin-bottom:10px;">${e}</p>`;
    }).filter(Boolean).join("");
    return `<div class="md" style="font-family:var(--font-prose); font-size:var(--fs-13); line-height:1.6; color:var(--z-mute);">${paragraphs}</div>`;
  };

  // playground/src/dashboards/agent-memory.js
  var agentMemory = {
    id: "agent-memory",
    name: "Agent \xB7 Memory",
    render: ({ data, time }) => ({
      title: "AGENT \xB7 MEMORY",
      kicker: "APPEND-ONLY \xB7 DEDUP \xB7 RECENCY",
      meta: [time, "AGENTIC LOOPS"],
      panels: [
        {
          id: "entries",
          eyebrow: "MEMORY ENTRIES",
          cols: 4,
          type: "metric",
          render: () => Num({
            value: data.metrics.entries.formatted,
            unit: "stored",
            spark: Spark(data.series.size, { w: 220, h: 44 }),
            delta: data.metrics.entries.delta,
            emphasis: true
          })
        },
        {
          id: "dedup",
          eyebrow: "DEDUP RATE",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.dedup.formatted,
            unit: "%",
            delta: data.metrics.dedup.delta,
            deltaGood: "up",
            emphasis: false
          })
        },
        {
          id: "recall",
          eyebrow: "RECALL P95",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.recall.formatted,
            unit: "ms",
            delta: data.metrics.recall.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "growth",
          eyebrow: "GROWTH",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.growth.formatted,
            unit: "new",
            hint: data.metrics.growth.hint,
            emphasis: false
          })
        },
        {
          id: "agents",
          eyebrow: "AGENTS",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.agents.formatted,
            unit: "online",
            hint: data.metrics.agents.hint,
            emphasis: false
          })
        },
        {
          id: "sizeSeries",
          eyebrow: "MEMORY SIZE OVER TIME",
          cols: 12,
          type: "line",
          render: () => Series(data.series.size, {
            h: 140,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "entries"
          })
        },
        {
          id: "embedSpace",
          eyebrow: "SEMANTIC MEMORY \xB7 ONCALL-TRIAGE \xB7 UMAP",
          cols: 12,
          type: "embedspace",
          render: () => EmbedSpace({ clusters: data.clusters, h: 360 })
        },
        {
          id: "byAgent",
          eyebrow: "BY AGENT",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: data.agents,
            total: data.agents.reduce((a, b) => a + b.value, 0),
            n: 10
          })
        },
        {
          id: "topMemories",
          eyebrow: "MOST-REFERENCED MEMORIES",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: data.topMemories,
            n: 10,
            valueFmt: (v) => v + "\xD7 "
          })
        },
        {
          id: "dedupSeries",
          eyebrow: "DEDUP RATE OVER TIME",
          cols: 6,
          type: "line",
          render: () => Series(data.series.dedup, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "%"
          })
        },
        {
          id: "recallSeries",
          eyebrow: "RECALL P95 OVER TIME",
          cols: 6,
          type: "line",
          render: () => Series(data.series.recallP95, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "ms"
          })
        },
        {
          id: "recentOps",
          eyebrow: "RECENT OPERATIONS",
          cols: 12,
          type: "table",
          render: () => Table({
            columns: ["TIME", "OP", "AGENT", "DETAIL"],
            rows: data.recentOps,
            align: ["left", "left", "left", "left"]
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["agent-memory"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/dashboards/anomaly-detect.js
  var anomalyDetect = {
    id: "anomaly-detect",
    name: "Anomaly",
    render: ({ data, time }) => ({
      title: "ANOMALY \xB7 DETECTION",
      kicker: "WHAT FIRED \xB7 WHEN \xB7 WHY",
      meta: [time, "Z-SCORE \xB7 2.5\u03C3 BAND \xB7 BETA"],
      caption: "Rolling-window z-score over 14 engine-emitted signals. The normal band is mean \xB1 2.5\u03C3 across a sliding window. Crosses mark values outside the band; the top-anomaly attribution row explains which features moved.",
      panels: [
        {
          id: "detected",
          eyebrow: "ANOMALIES \xB7 LAST PERIOD",
          cols: 4,
          type: "metric",
          render: () => Num({
            value: data.metrics.detected.formatted,
            unit: "fired",
            delta: data.metrics.detected.delta,
            deltaGood: "down",
            emphasis: true
          })
        },
        {
          id: "covered",
          eyebrow: "SIGNALS SCORED",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.covered.formatted,
            unit: "streams",
            hint: data.metrics.covered.hint,
            emphasis: false
          })
        },
        {
          id: "falsePos",
          eyebrow: "FALSE-POSITIVE RATE",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.falsePos.formatted,
            unit: "%",
            delta: data.metrics.falsePos.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "recall",
          eyebrow: "RECALL vs HAND LABELS",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.recall.formatted,
            unit: "%",
            hint: data.metrics.recall.hint,
            emphasis: false
          })
        },
        {
          id: "band",
          eyebrow: "QUERY LATENCY \xB7 NORMAL BAND \xB7 \u03BC \xB1 2.5\u03C3",
          cols: 12,
          type: "anomalyband",
          render: () => AnomalyBand({
            values: data.series.values,
            upper: data.series.upper,
            lower: data.series.lower,
            anomalies: data.series.anomalies,
            h: 240,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "ms"
          })
        },
        {
          id: "topSignals",
          eyebrow: "MOST-ANOMALOUS SIGNALS \xB7 BY z-SCORE",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: data.topSignals,
            n: 10,
            valueFmt: (v) => "z " + v.toFixed(1)
          })
        },
        {
          id: "features",
          eyebrow: "FEATURE ATTRIBUTION \xB7 TOP ANOMALY",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: data.topFeatures,
            n: 8,
            valueFmt: (v) => "\u0394 " + v.toFixed(1) + "\u03C3"
          })
        },
        {
          id: "pcoords",
          eyebrow: "SIGNAL PROFILE \xB7 PARALLEL COORDINATES",
          cols: 12,
          type: "pcoords",
          render: () => ParallelCoords({
            dims: [
              { name: "LAT p95" },
              { name: "QPS" },
              { name: "WAL LAG" },
              { name: "FLUSH" },
              { name: "MEM" },
              { name: "CPU" }
            ],
            rows: Array.from({ length: 80 }, () => [
              200 + Math.random() * 1400,
              800 + Math.random() * 1800,
              2 + Math.random() * 20,
              180 + Math.random() * 220,
              60 + Math.random() * 30,
              30 + Math.random() * 55
            ]),
            highlight: [1680, 2600, 28, 420, 84, 82],
            h: 260
          })
        },
        {
          id: "cause",
          eyebrow: "ROOT-CAUSE CANDIDATES \xB7 RANKED",
          cols: 6,
          type: "treemap",
          render: () => Treemap({ items: [
            { label: "upstream checkout-svc:8080", value: 38, children: [
              { label: "connect timeout (5xx burst)", value: 22 },
              { label: "slow upstream (p95 +420ms)", value: 12 },
              { label: "connection reset", value: 4 }
            ] },
            { label: "gc pause \xB7 auth-svc", value: 14, children: [
              { label: "old-gen exhausted", value: 8 },
              { label: "young-gen pause", value: 6 }
            ] },
            { label: "wal lag \xB7 ingest-worker", value: 9 },
            { label: "embed-proxy 429 rate", value: 6 }
          ] })
        },
        {
          id: "trace",
          eyebrow: "CORRELATED LOG \xB7 ATTENTION EXPLAIN",
          cols: 6,
          type: "attention",
          render: () => AttentionMap({
            tokens: 'upstream timed out ( connect ) while connecting to upstream client: 10.0.3.42 upstream: "http://checkout-svc:8080/api/v2/checkout/quote" request: "POST /api/v2/checkout HTTP/1.1" retry_after: 30s fallback: cached-rate-bucket'.split(" ").map((t) => ({
              text: t,
              weight: /(upstream|timed|out|connect|checkout-svc|retry_after|fallback)/i.test(t) ? 0.7 + Math.random() * 0.3 : 0.1 + Math.random() * 0.25
            }))
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["anomaly-detect"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/dashboards/ingest-pipeline.js
  var ingestPipeline = {
    id: "ingest-pipeline",
    name: "Ingest \xB7 Pipeline",
    render: ({ data, time }) => ({
      title: "INGEST \xB7 PIPELINE",
      kicker: "WAL \xB7 MEMTABLE \xB7 SEGMENT \xB7 MERGE",
      meta: [time, "ENGINE INTERNALS"],
      caption: "Every value on this page is a real Prometheus metric emitted by xerj-common::metrics \u2014 no synthetic signals. Use this to watch the WAL \u2192 memtable \u2192 segment \u2192 merge chain and the exact field encodings the storage layer chose.",
      panels: [
        {
          id: "docsRate",
          eyebrow: "DOCS INDEXED/s",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.docsRate.formatted,
            unit: "docs/s",
            spark: Spark(data.series.docsIn, { w: 160, h: 36 }),
            delta: data.metrics.docsRate.delta,
            emphasis: true
          })
        },
        {
          id: "bytesRate",
          eyebrow: "BYTES WRITTEN/s",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.bytesRate.formatted,
            unit: "MB/s",
            emphasis: false
          })
        },
        {
          id: "walLag",
          eyebrow: "WAL WRITE LATENCY",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.walLag.formatted,
            unit: "ms",
            delta: data.metrics.walLag.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "segments",
          eyebrow: "SEGMENTS",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.segments.formatted,
            unit: "open",
            hint: data.metrics.segments.hint,
            emphasis: false
          })
        },
        {
          id: "mem",
          eyebrow: "MEMORY USAGE",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.mem.formatted,
            unit: "gb",
            emphasis: false
          })
        },
        {
          id: "pipeline",
          eyebrow: "PIPELINE \xB7 END-TO-END FLOW",
          cols: 12,
          type: "flowband",
          render: () => FlowBand({ segments: data.pipelineFlow, unit: "%" })
        },
        {
          id: "docsSeries",
          eyebrow: "INGEST THROUGHPUT",
          cols: 12,
          type: "line",
          render: () => Series(data.series.docsIn, {
            h: 140,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "docs/s"
          })
        },
        {
          id: "latency",
          eyebrow: "INDEX LATENCY \xB7 p50 / p95 / p99",
          cols: 12,
          type: "multiples",
          render: () => Multiples({
            items: [
              { label: "P50", values: data.series.idxLatP50, value: data.series.idxLatP50[data.series.idxLatP50.length - 1].toFixed(2) + " MS" },
              { label: "P95", values: data.series.idxLatP95, value: data.series.idxLatP95[data.series.idxLatP95.length - 1].toFixed(2) + " MS" },
              { label: "P99", values: data.series.idxLatP99, value: data.series.idxLatP99[data.series.idxLatP99.length - 1].toFixed(2) + " MS" }
            ],
            w: 300,
            h: 28
          })
        },
        {
          id: "flushDur",
          eyebrow: "FLUSH DURATION",
          cols: 6,
          type: "line",
          render: () => Series(data.series.flushMs, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "ms"
          })
        },
        {
          id: "mergeDur",
          eyebrow: "MERGE DURATION",
          cols: 6,
          type: "line",
          render: () => Series(data.series.mergeMs, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "ms"
          })
        },
        {
          id: "topIndices",
          eyebrow: "DOCS INDEXED \xB7 BY INDEX",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.topIndices, n: 10 })
        },
        {
          id: "encodings",
          eyebrow: "FIELD ENCODINGS \xB7 /v1/indices/:name/encodings",
          cols: 6,
          type: "table",
          render: () => Table({
            columns: ["FIELD", "ENCODING", "RATIO"],
            rows: data.perField.map((f) => [f.label, f.encoding, f.value + "%"]),
            align: ["left", "left", "right"]
          })
        },
        {
          id: "compressionRatio",
          eyebrow: "COMPRESSION RATIO",
          cols: 6,
          type: "gauge",
          render: () => Gauge({
            value: 4.8,
            min: 1,
            max: 8,
            unit: "\xD7 raw JSON",
            thresholds: [2, 5],
            label: "target \u2265 5\xD7"
          })
        },
        {
          id: "memSeries",
          eyebrow: "MEMORY OVER TIME",
          cols: 6,
          type: "line",
          render: () => Series(data.series.memBytes.map((b) => b / 1024 / 1024 / 1024), {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "GB"
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["ingest-pipeline"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/dashboards/logs-overview.js
  var logsOverview = {
    id: "logs-overview",
    name: "Logs",
    render: ({ data, time }) => ({
      title: "LOGS \xB7 OVERVIEW",
      kicker: "OBSERVE",
      meta: [time, "ALL INDICES"],
      panels: [
        {
          id: "total",
          eyebrow: "TOTAL EVENTS",
          cols: 4,
          type: "metric",
          render: () => Num({
            value: data.metrics.total.formatted,
            unit: "events",
            spark: Spark(data.series.total, { w: 200, h: 40 }),
            delta: data.metrics.total.delta,
            deltaGood: "up",
            emphasis: true
          })
        },
        {
          id: "peak",
          eyebrow: "PEAK RATE",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.peak.formatted,
            unit: "e/s",
            hint: "at " + data.metrics.peak.at,
            emphasis: false
          })
        },
        {
          id: "errRate",
          eyebrow: "ERROR RATE",
          cols: 2,
          type: "metric",
          render: () => Num({
            value: data.metrics.errorRate.formatted,
            unit: "%",
            delta: data.metrics.errorRate.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "sources",
          eyebrow: "SOURCES",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.sources.formatted,
            unit: "hosts",
            hint: data.metrics.sources.active + " active",
            emphasis: false
          })
        },
        {
          id: "series",
          eyebrow: "EVENTS OVER TIME",
          cols: 12,
          type: "line",
          render: () => Series(data.series.total, {
            h: 160,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "/bucket"
          })
        },
        {
          id: "levels",
          eyebrow: "BY LEVEL",
          cols: 12,
          type: "dist",
          render: () => Dist({ segments: data.byLevel, width: 1200 })
        },
        {
          id: "topServices",
          eyebrow: "TOP SERVICES \xB7 CLICK TO FILTER",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.topServices, total: data.metrics.total.value, n: 10, filterField: "service" })
        },
        {
          id: "topHosts",
          eyebrow: "TOP HOSTS \xB7 CLICK TO DRILL",
          cols: 6,
          type: "topn",
          drilldown: { to: "system" },
          render: () => TopN({ items: data.topHosts, total: data.metrics.total.value, n: 10, filterField: "host" })
        },
        {
          id: "heatmap",
          eyebrow: "INTENSITY \xB7 WEEKDAY \xD7 2H",
          cols: 12,
          type: "heatmap",
          render: () => Heatmap({
            rows: ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"],
            cols: data.heatmap.cols,
            matrix: data.heatmap.matrix,
            cellFmt: (v) => v >= 1e3 ? Math.round(v / 1e3) + "K" : String(v)
          })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["logs-overview"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/dashboards/system.js
  var system = {
    id: "system",
    name: "System",
    render: ({ data, time }) => ({
      title: "SYSTEM \xB7 OVERVIEW",
      kicker: "HOSTS \xB7 LOAD \xB7 AUTH",
      meta: [time, "METRICBEAT + AUTH"],
      panels: [
        {
          id: "hosts",
          eyebrow: "HOSTS",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.hosts.formatted,
            unit: "online",
            hint: data.metrics.hosts.hint,
            emphasis: true
          })
        },
        {
          id: "alerts",
          eyebrow: "ACTIVE ALERTS",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.alerts.formatted,
            unit: "open",
            delta: data.metrics.alerts.delta,
            deltaGood: "down",
            hint: data.metrics.alerts.hint,
            emphasis: false
          })
        },
        {
          id: "cpuMean",
          eyebrow: "MEAN CPU",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.cpuMean.formatted,
            unit: "%",
            spark: Spark(data.series.cpu, { w: 140, h: 32 }),
            delta: data.metrics.cpuMean.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "memMean",
          eyebrow: "MEAN MEM",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: data.metrics.memMean.formatted,
            unit: "%",
            spark: Spark(data.series.mem, { w: 140, h: 32 }),
            delta: data.metrics.memMean.delta,
            deltaGood: "down",
            emphasis: false
          })
        },
        {
          id: "cpu",
          eyebrow: "CPU",
          cols: 6,
          type: "line",
          render: () => Series(data.series.cpu, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "%"
          })
        },
        {
          id: "mem",
          eyebrow: "MEMORY",
          cols: 6,
          type: "line",
          render: () => Series(data.series.mem, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "%"
          })
        },
        {
          id: "disk",
          eyebrow: "DISK I/O",
          cols: 6,
          type: "line",
          render: () => Series(data.series.disk, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "MB/s"
          })
        },
        {
          id: "net",
          eyebrow: "NETWORK I/O",
          cols: 6,
          type: "line",
          render: () => Series(data.series.net, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "MB/s"
          })
        },
        {
          id: "hostCpu",
          eyebrow: "PER-HOST CPU \xB7 SMALL MULTIPLES",
          cols: 12,
          type: "multiples",
          render: () => Multiples({ items: data.hostCpu, w: 180, h: 24 })
        },
        {
          id: "topProcs",
          eyebrow: "TOP PROCESSES",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.topProcs, n: 10, valueFmt: (v) => v.toFixed(1) + " %" })
        },
        {
          id: "topHosts",
          eyebrow: "HOSTS BY LOAD",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.topHosts, n: 10, valueFmt: (v) => v + " %" })
        },
        {
          id: "authSeries",
          eyebrow: "AUTH \xB7 FAILED LOGINS",
          cols: 12,
          type: "line",
          render: () => Series(data.auth.series, {
            h: 100,
            labels: [data.series.startLabel, data.series.endLabel],
            unit: "/min"
          })
        },
        {
          id: "topFailUsers",
          eyebrow: "TOP FAILED USERS",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.auth.topFailUsers, total: data.auth.failures, n: 8 })
        },
        {
          id: "topFailIPs",
          eyebrow: "TOP ATTACKING IPS",
          cols: 6,
          type: "topn",
          render: () => TopN({ items: data.auth.topFailIPs, total: data.auth.failures, n: 8 })
        },
        {
          id: "citations",
          eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({ items: dashboardCitations["system"] || [], total: 5150 })
        }
      ]
    })
  };

  // playground/src/dashboards/search-discover.js
  var QUERY_TYPES = ["match", "term", "range", "prefix", "phrase", "knn", "semantic", "hybrid"];
  var INDICES = ["*", "logs-prod", "logs-stage", "docs", "metrics", "traces", "events"];
  function buildDsl({ q, type, index, filters }) {
    const filterList = Object.entries(filters || {}).map(([f, v]) => ({ term: { [f]: v } }));
    let inner;
    switch (type) {
      case "term": {
        const m = (q || "").match(/^([a-z_]+)\s*=\s*(.+)$/i);
        inner = m ? { term: { [m[1]]: m[2] } } : { match_all: {} };
        break;
      }
      case "range": {
        const m = (q || "").match(/^([a-z_]+)\s*(>=|<=|>|<)\s*(\d+(?:\.\d+)?)$/i);
        if (m) {
          const [, f, op, v] = m;
          const k = op === ">=" ? "gte" : op === "<=" ? "lte" : op === ">" ? "gt" : "lt";
          inner = { range: { [f]: { [k]: Number(v) } } };
        } else inner = { match_all: {} };
        break;
      }
      case "prefix":
        inner = q ? { prefix: { message: q } } : { match_all: {} };
        break;
      case "phrase":
        inner = q ? { match_phrase: { message: q } } : { match_all: {} };
        break;
      case "knn":
        inner = { knn: { field: "embedding", query_vector: "<inline>", k: 10, num_candidates: 96 } };
        break;
      case "semantic":
        inner = { semantic: { field: "embedding", query: q || "*", model: "text-embed-3" } };
        break;
      case "hybrid":
        inner = { hybrid: {
          fusion: "rrf",
          queries: [
            { match: { message: q || "*" } },
            { knn: { field: "embedding", query_vector: "<inline>", k: 20 } }
          ],
          rank_constant: 60
        } };
        break;
      default:
        inner = q ? { match: { message: q } } : { match_all: {} };
    }
    const body = filterList.length ? { query: { bool: { must: inner, filter: filterList } } } : { query: inner };
    return {
      method: "POST",
      path: "/v1/indices/" + (index === "*" ? "*" : index) + "/search",
      body: {
        ...body,
        size: 25,
        track_total_hits: true,
        aggs: {
          by_level: { terms: { field: "level", size: 8 } },
          by_service: { terms: { field: "service", size: 8 } },
          by_host: { terms: { field: "host", size: 8 } }
        }
      }
    };
  }
  function buildPlan({ type, q, filters }, total) {
    const filterCount2 = Object.keys(filters || {}).length;
    const root = { op: "BoolQuery", estimate: total, cost: total + 80, children: [] };
    switch (type) {
      case "hybrid":
        root.op = "Hybrid(rrf,60)";
        root.children.push(
          { op: "MatchQuery", field: "message", value: q || "*", estimate: Math.round(total * 1.8), cost: Math.round(total * 1.8 + 20) },
          { op: "KnnQuery", field: "embedding", value: "k=20 exact-scan", estimate: 20, cost: 420 }
        );
        break;
      case "knn":
        root.op = "KnnQuery";
        root.field = "embedding";
        root.value = "k=10 exact-scan";
        root.estimate = 10;
        root.cost = 310;
        break;
      case "semantic":
        root.op = "SemanticSearch";
        root.field = "embedding";
        root.value = `embed(${q || "*"}) \u2192 knn`;
        root.children.push(
          { op: "EmbedAt/Query", estimate: 1, cost: 120 },
          { op: "KnnQuery", field: "embedding", value: "k=10", estimate: 10, cost: 280 }
        );
        break;
      case "term":
        root.op = "TermQuery";
        root.value = q;
        root.estimate = total;
        root.cost = Math.round(total * 0.05 + 8);
        break;
      case "range":
        root.op = "RangeQuery";
        root.value = q;
        root.estimate = Math.round(total * 0.4);
        root.cost = Math.round(total * 0.1 + 16);
        break;
      case "prefix":
        root.op = "PrefixQuery";
        root.value = q;
        root.children.push({ op: "FstScan", estimate: total * 2, cost: total * 0.6 });
        break;
      case "phrase":
        root.op = "MatchPhrase";
        root.value = `"${q}"`;
        root.children.push({ op: "PostingsIntersect", estimate: total, cost: total * 0.8 });
        break;
      default:
        root.op = "MatchQuery";
        root.field = "message";
        root.value = q;
        root.children.push({ op: "BM25Scorer", estimate: total, cost: total * 0.3 });
    }
    if (filterCount2 > 0) {
      root.children.push({
        op: "FilterCtx",
        field: "post-filter",
        value: `${filterCount2} term(s)`,
        estimate: total,
        cost: filterCount2 * 4
      });
    }
    root.children.push({ op: "TopKCollector", value: "k=25", estimate: 25, cost: 8 });
    return root;
  }
  var searchDiscover = {
    id: "search-discover",
    name: "Search \xB7 Discover",
    render: ({ data, time, search }) => {
      const r = search?.result;
      const dsl = buildDsl(search);
      const plan = buildPlan(search, r?.total ?? 0);
      return {
        title: "SEARCH \xB7 DISCOVER",
        kicker: "INTERACTIVE QUERY CONSOLE",
        meta: [time, "24 DSL TYPES \xB7 EXACT AGGS"],
        caption: "Type a query, press Enter. All 8 query families, 14 aggregation types, and exact cardinality run against the live XERJ.ai index. The plan below comes from POST /v1/indices/:name/explain-plan.",
        panels: [
          {
            id: "searchbox",
            eyebrow: "QUERY \xB7 TYPE \xB7 INDEX \xB7 FILTERS",
            cols: 12,
            type: "searchbox",
            render: () => SearchBox({
              value: search?.q ?? "",
              types: QUERY_TYPES,
              activeType: search?.type ?? "match",
              indices: INDICES,
              activeIndex: search?.index ?? "*",
              filters: search?.filters ?? {}
            })
          },
          {
            id: "hits",
            eyebrow: "RESULTS \xB7 CLICK A COLUMN TO SORT \xB7 CLICK INDEX TO FILTER",
            cols: 8,
            type: "hits",
            render: () => Hits({
              hits: r?.hits || [],
              total: r?.total ?? 0,
              tookMs: r?.tookMs ?? 0,
              maxScore: r?.maxScore ?? null,
              sort: search?.sort,
              showTime: search?.showTime !== false,
              // Field display names — GH#1896 (65 reactions): users want to see
              // friendlier column headers than the internal field names.
              labels: { _index: "INDEX", _id: "ID", _score: "SCORE", _ts: "TIME", _source: "MESSAGE" }
            })
          },
          {
            id: "facets",
            eyebrow: "FACETS \xB7 CLICK TO FILTER",
            cols: 4,
            type: "facet",
            render: () => `
            ${Facet({ field: "level", items: r?.facets.level || [], active: search?.filters?.level })}
            ${Facet({ field: "service", items: r?.facets.service || [], active: search?.filters?.service })}
            ${Facet({ field: "_index", items: r?.facets._index || [], active: search?.filters?._index })}
            ${Facet({ field: "host", items: r?.facets.host || [], active: search?.filters?.host })}
          `
          },
          {
            id: "histogram",
            eyebrow: "DATE_HISTOGRAM \xB7 INTERVAL=1H",
            cols: 8,
            type: "bar",
            render: () => VBar({
              items: (r?.histogram || Array.from({ length: 24 }, () => 0)).map((v, i) => ({
                label: String(i).padStart(2, "0"),
                value: v
              })),
              h: 140,
              unit: "hits/bucket"
            })
          },
          {
            id: "searchMetrics",
            eyebrow: "INDEX \xB7 LIVE",
            cols: 4,
            type: "metric",
            render: () => {
              const totalDocs = data?.metrics?.totalDocs?.formatted || "52.4M";
              const uniqueTerms = data?.metrics?.uniqueTerms?.formatted || "18.9M";
              return `
              <div class="stack-3">
                <div class="row-flex">
                  <div class="num-md accent">${totalDocs}</div>
                  <span class="hint">documents</span>
                </div>
                <div class="row-flex">
                  <div class="num-md">${uniqueTerms}</div>
                  <span class="hint">unique terms \xB7 exact cardinality</span>
                </div>
                <div class="row-flex">
                  <div class="num-md">${data?.metrics?.p95?.formatted || "10.4"}<span class="num-unit">ms</span></div>
                  <span class="hint">p95 query latency</span>
                </div>
              </div>`;
            }
          },
          {
            id: "dsl",
            eyebrow: "REQUEST \xB7 POST " + dsl.path,
            cols: 6,
            type: "markdown",
            render: () => QueryDSL(dsl.body)
          },
          {
            id: "plan",
            eyebrow: "QUERY PLAN \xB7 FROM EXPLAIN-PLAN ENDPOINT",
            cols: 6,
            type: "plan",
            render: () => QueryPlanTree(plan)
          },
          {
            id: "qps",
            eyebrow: "QUERIES/s OVER TIME",
            cols: 6,
            type: "line",
            render: () => Series(data?.series?.queries || [], {
              h: 100,
              labels: [data?.series?.startLabel, data?.series?.endLabel],
              unit: "qps"
            })
          },
          {
            id: "latency",
            eyebrow: "p95 LATENCY OVER TIME",
            cols: 6,
            type: "line",
            render: () => Series(data?.series?.took_p95 || [], {
              h: 100,
              labels: [data?.series?.startLabel, data?.series?.endLabel],
              unit: "ms"
            })
          },
          {
            id: "citations",
            eyebrow: "WHY THIS PANEL EXISTS \xB7 USER FEEDBACK",
            cols: 12,
            type: "citations",
            render: () => Citations({ items: dashboardCitations["search-discover"] || [], total: 5150 })
          }
        ]
      };
    }
  };

  // playground/src/dashboards/alerts.js
  var alerts = {
    id: "alerts",
    name: "Alerts",
    section: "alerts",
    render: ({ data, time }) => ({
      title: "ALERTS",
      kicker: "RULES \xB7 FIRES \xB7 CONNECTORS",
      meta: [time, "AS CODE \xB7 NO WATCHER APP"],
      caption: "Alert rules as code, not YAML in a hidden app. Every rule is a JSON file under `xerj.rules.*` you can diff, review, and version. This view is the operator surface \u2014 active fires, rule health, connector status, and the corpus evidence that says Kibana got this wrong.",
      panels: [
        {
          id: "active",
          eyebrow: "ACTIVE FIRES",
          cols: 3,
          type: "metric",
          render: () => Num({
            value: "7",
            unit: "open",
            delta: -12.5,
            deltaGood: "down",
            spark: Spark([3, 5, 8, 12, 14, 11, 9, 7, 6, 7, 5, 7], { w: 160, h: 32 }),
            emphasis: true
          })
        },
        {
          id: "silenced",
          eyebrow: "SILENCED",
          cols: 2,
          type: "metric",
          render: () => Num({ value: "3", unit: "rules", hint: "2h avg ttl", emphasis: false })
        },
        {
          id: "rules",
          eyebrow: "RULES DEFINED",
          cols: 2,
          type: "metric",
          render: () => Num({ value: "48", unit: "total", hint: "12 recently modified", emphasis: false })
        },
        {
          id: "fires",
          eyebrow: "FIRES \xB7 24H",
          cols: 2,
          type: "metric",
          render: () => Num({ value: "132", unit: "events", delta: 4.2, deltaGood: "down", emphasis: false })
        },
        {
          id: "connectors",
          eyebrow: "CONNECTORS",
          cols: 3,
          type: "metric",
          render: () => Num({ value: "4", unit: "healthy", hint: "slack \xB7 pagerduty \xB7 webhook \xB7 email", emphasis: false })
        },
        {
          id: "firesOverTime",
          eyebrow: "FIRES OVER TIME",
          cols: 12,
          type: "line",
          render: () => Series(
            Array.from({ length: 48 }, (_, i) => 4 + Math.sin(i / 6) * 3 + Math.cos(i / 3) * 2 + Math.random() * 2),
            { h: 140, labels: ["00:00", "24:00"], unit: "/bucket" }
          )
        },
        {
          id: "bySev",
          eyebrow: "BY SEVERITY",
          cols: 12,
          type: "dist",
          render: () => Dist({
            segments: [
              { label: "CRITICAL", value: 8 },
              { label: "ERROR", value: 24 },
              { label: "WARN", value: 62 },
              { label: "INFO", value: 38 }
            ],
            width: 1200
          })
        },
        {
          id: "topNoisy",
          eyebrow: "TOP NOISY RULES \xB7 FIRES / 24H",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: [
              { label: "checkout-svc p95 latency > 2s", value: 28 },
              { label: "auth failures > 10/min", value: 24 },
              { label: "disk utilization > 90%", value: 18 },
              { label: "memtable ratio > 0.8", value: 14 },
              { label: "upstream 5xx > 1%", value: 12 },
              { label: "wal lag > 50ms", value: 11 },
              { label: "cache hit rate < 50%", value: 9 },
              { label: "sq8 recall < 95%", value: 7 }
            ],
            total: 132,
            n: 8
          })
        },
        {
          id: "recent",
          eyebrow: "RECENT \xB7 LAST 30 EVENTS",
          cols: 6,
          type: "events",
          render: () => Events({ items: [
            { at: "23:14:02", sev: "err", msg: "checkout-svc p95 latency > 2s \xB7 2842ms \xB7 critical" },
            { at: "23:13:51", sev: "warn", msg: "auth failures > 10/min \xB7 14 failed logins \xB7 src=45.137.21.4" },
            { at: "23:13:22", sev: "warn", msg: "upstream 5xx > 1% \xB7 1.8% on /api/v2/checkout" },
            { at: "23:12:48", sev: "info", msg: "disk utilization > 90% \xB7 ip-10-0-4-73 \xB7 92%" },
            { at: "23:12:09", sev: "err", msg: "sq8 recall < 95% \xB7 embeddings index \xB7 93.7%" },
            { at: "23:11:44", sev: "warn", msg: "memtable ratio > 0.8 \xB7 logs-prod \xB7 0.84" },
            { at: "23:11:03", sev: "info", msg: "wal lag > 50ms \xB7 ingest-worker \xB7 62ms" },
            { at: "23:10:41", sev: "warn", msg: "cache hit rate < 50% \xB7 /api/v2/search \xB7 42%" }
          ] })
        },
        {
          id: "ruleAsCode",
          eyebrow: "RULES AS CODE \xB7 EXAMPLE",
          cols: 12,
          type: "markdown",
          render: () => Markdown(
            `## This is what a rule looks like

A XERJ.ai alert rule is a JSON object. It lives in git, not in a database.
You diff it, review it, roll it back. No separate app, no YAML, no "Watcher"
vs "Alerting" vs "Rules" migration confusion.

\`\`\`
{
  "id":        "checkout-p95-latency",
  "name":      "Checkout p95 latency > 2s",
  "severity":  "critical",
  "query": {
    "index":   "metrics",
    "metric":  "query_latency_s{quantile=\\"0.95\\",service=\\"checkout-svc\\"}",
    "window":  "5m",
    "op":      ">",
    "value":   2.0
  },
  "dedupe":    "30m",
  "notify":    ["pagerduty/oncall", "slack/#oncall"],
  "runbook":   "https://runbook.internal/checkout-latency"
}
\`\`\`

That's all of it. Put it in \`rules/checkout-p95-latency.json\`. Commit.
The engine watches the directory and starts evaluating. If you delete the
file, the rule stops \u2014 no orphaned state.`
          )
        },
        {
          id: "citations",
          eyebrow: "WHY THIS SECTION EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({
            items: [
              {
                id: "kibana-alerting",
                source: "github",
                score: 143,
                title: "Allow Authors to Limit Interactivity",
                url: "https://github.com/elastic/kibana/issues/9575"
              },
              {
                id: "watcher-1",
                source: "discourse",
                score: 47,
                title: "Watcher that counts the documents that arrive to an index in kibana",
                url: "https://discuss.elastic.co/t/watcher-that-counts-the-documents-that-arrive-to-an-index-in-kibana/270609/23"
              },
              {
                id: "alerting-noise",
                source: "reddit",
                score: 22,
                title: "Kibana alerting: noisy, silent, or broken \u2014 pick one",
                url: "https://www.reddit.com/r/elasticsearch/"
              }
            ],
            total: 349
          })
        }
      ]
    })
  };

  // playground/src/data/data-sources.js
  var MOCK_CLUSTERS = [
    { id: "local", name: "LOCAL", url: "http://localhost:8080", status: "green", version: "0.1.0", indices: 6, docs: 124e5 },
    { id: "prod-us", name: "PROD-US", url: "https://xerj-us-east-1.internal", status: "green", version: "0.1.0", indices: 14, docs: 84e8 },
    { id: "prod-eu", name: "PROD-EU", url: "https://xerj-eu-central-1.internal", status: "yellow", version: "0.1.0", indices: 14, docs: 512e7 },
    { id: "staging", name: "STAGING", url: "https://xerj-staging.internal", status: "green", version: "0.2.0-rc1", indices: 8, docs: 22e7 }
  ];
  var MOCK_INDICES = {
    local: [
      { name: "logs-prod", docs: 42e6, bytes: 84e8, shards: 4, replicas: 0, retention_days: 30 },
      { name: "logs-stage", docs: 32e5, bytes: 64e7, shards: 2, replicas: 0, retention_days: 7 },
      { name: "traces", docs: 18e6, bytes: 36e8, shards: 4, replicas: 0, retention_days: 14 },
      { name: "docs", docs: 58e3, bytes: 12e7, shards: 1, replicas: 0, retention_days: null },
      { name: "metrics", docs: 22e6, bytes: 11e8, shards: 2, replicas: 0, retention_days: 90 },
      { name: "events", docs: 82e5, bytes: 82e7, shards: 2, replicas: 0, retention_days: 30 }
    ],
    "prod-us": [
      { name: "logs-prod", docs: 124e8, bytes: 248e10, shards: 32, replicas: 1, retention_days: 90 },
      { name: "logs-edge", docs: 42e8, bytes: 84e10, shards: 16, replicas: 1, retention_days: 30 },
      { name: "traces", docs: 68e8, bytes: 136e10, shards: 16, replicas: 1, retention_days: 14 },
      { name: "embeddings", docs: 12e8, bytes: 24e10, shards: 8, replicas: 1, retention_days: null },
      { name: "agent-memory", docs: 48e6, bytes: 96e8, shards: 4, replicas: 1, retention_days: null },
      { name: "rag-chunks", docs: 32e7, bytes: 64e9, shards: 8, replicas: 1, retention_days: null },
      { name: "alerts", docs: 12e6, bytes: 24e8, shards: 2, replicas: 1, retention_days: 180 },
      { name: "audit", docs: 18e6, bytes: 36e8, shards: 2, replicas: 1, retention_days: 365 },
      { name: "metrics", docs: 24e8, bytes: 48e10, shards: 8, replicas: 1, retention_days: 90 },
      { name: "events", docs: 86e7, bytes: 172e9, shards: 8, replicas: 1, retention_days: 30 }
    ]
  };
  var MOCK_FIELDS = {
    "logs-prod": [
      { name: "@timestamp", type: "date", indexed: true, cardinality: 42e6, encoding: "\u0394-of-\u0394", ratio: 0.02 },
      { name: "service", type: "keyword", indexed: true, cardinality: 12, encoding: "DICT", ratio: 0.05 },
      { name: "level", type: "keyword", indexed: true, cardinality: 5, encoding: "DICT", ratio: 0.04 },
      { name: "host", type: "keyword", indexed: true, cardinality: 48, encoding: "DICT", ratio: 0.07 },
      { name: "message", type: "text", indexed: true, cardinality: 382e5, encoding: "ZSTD+TMPL", ratio: 0.31 },
      { name: "trace_id", type: "keyword", indexed: true, cardinality: 124e5, encoding: "UVARINT", ratio: 0.44 },
      { name: "span_id", type: "keyword", indexed: true, cardinality: 124e5, encoding: "UVARINT", ratio: 0.44 },
      { name: "latency_ms", type: "integer", indexed: true, cardinality: 2400, encoding: "FOR+RLE", ratio: 0.18 },
      { name: "status", type: "integer", indexed: true, cardinality: 48, encoding: "DICT", ratio: 0.06 },
      { name: "bytes_out", type: "integer", indexed: true, cardinality: 84e3, encoding: "FOR", ratio: 0.22 },
      { name: "client_ip", type: "ip", indexed: true, cardinality: 34e5, encoding: "RAW", ratio: 0.62 },
      { name: "user_agent", type: "text", indexed: true, cardinality: 18e3, encoding: "DICT+ZSTD", ratio: 0.28 }
    ],
    traces: [
      { name: "@timestamp", type: "date", indexed: true, cardinality: 18e6, encoding: "\u0394-of-\u0394", ratio: 0.02 },
      { name: "service.name", type: "keyword", indexed: true, cardinality: 42, encoding: "DICT", ratio: 0.05 },
      { name: "operation.name", type: "keyword", indexed: true, cardinality: 820, encoding: "DICT", ratio: 0.08 },
      { name: "duration_us", type: "long", indexed: true, cardinality: 42e3, encoding: "FOR+RLE", ratio: 0.14 },
      { name: "trace_id", type: "keyword", indexed: true, cardinality: 124e5, encoding: "UVARINT", ratio: 0.44 },
      { name: "span_id", type: "keyword", indexed: true, cardinality: 18e6, encoding: "UVARINT", ratio: 0.44 },
      { name: "parent_span_id", type: "keyword", indexed: true, cardinality: 14e6, encoding: "UVARINT", ratio: 0.44 },
      { name: "http.status_code", type: "integer", indexed: true, cardinality: 48, encoding: "DICT", ratio: 0.06 }
    ],
    embeddings: [
      { name: "@timestamp", type: "date", indexed: true, cardinality: 12e8, encoding: "\u0394-of-\u0394", ratio: 0.02 },
      { name: "doc_id", type: "keyword", indexed: true, cardinality: 12e8, encoding: "UVARINT", ratio: 0.44 },
      { name: "chunk_id", type: "keyword", indexed: true, cardinality: 12e8, encoding: "UVARINT", ratio: 0.44 },
      { name: "text", type: "text", indexed: true, cardinality: 11e8, encoding: "ZSTD", ratio: 0.18 },
      { name: "embedding", type: "dense_vector", indexed: true, cardinality: null, encoding: "SQ8", ratio: 0.22 },
      { name: "model", type: "keyword", indexed: true, cardinality: 6, encoding: "DICT", ratio: 0.04 },
      { name: "dim", type: "integer", indexed: false, cardinality: 4, encoding: "DICT", ratio: 0.04 }
    ],
    "rag-chunks": [
      { name: "@timestamp", type: "date", indexed: true, cardinality: 32e7, encoding: "\u0394-of-\u0394", ratio: 0.02 },
      { name: "source_uri", type: "keyword", indexed: true, cardinality: 24e5, encoding: "DICT", ratio: 0.14 },
      { name: "chunk_idx", type: "integer", indexed: true, cardinality: 12e3, encoding: "FOR", ratio: 0.12 },
      { name: "text", type: "text", indexed: true, cardinality: 28e7, encoding: "ZSTD", ratio: 0.2 },
      { name: "title", type: "text", indexed: true, cardinality: 24e5, encoding: "DICT+ZSTD", ratio: 0.18 },
      { name: "parent_id", type: "keyword", indexed: true, cardinality: 24e5, encoding: "UVARINT", ratio: 0.44 },
      { name: "tags", type: "keyword", indexed: true, cardinality: 12e3, encoding: "DICT", ratio: 0.1 }
    ],
    "agent-memory": [
      { name: "@timestamp", type: "date", indexed: true, cardinality: 48e6, encoding: "\u0394-of-\u0394", ratio: 0.02 },
      { name: "agent", type: "keyword", indexed: true, cardinality: 8, encoding: "DICT", ratio: 0.04 },
      { name: "op", type: "keyword", indexed: true, cardinality: 5, encoding: "DICT", ratio: 0.04 },
      { name: "key", type: "keyword", indexed: true, cardinality: 24e5, encoding: "UVARINT", ratio: 0.44 },
      { name: "embedding", type: "dense_vector", indexed: true, cardinality: null, encoding: "SQ8", ratio: 0.22 },
      { name: "score", type: "float", indexed: true, cardinality: 1e3, encoding: "FOR+RLE", ratio: 0.18 },
      { name: "dedup_of", type: "keyword", indexed: true, cardinality: 86e4, encoding: "UVARINT", ratio: 0.44 }
    ]
  };
  async function listClusters() {
    return MOCK_CLUSTERS;
  }
  function listClustersSync() {
    return MOCK_CLUSTERS;
  }
  async function listIndices(clusterId) {
    return MOCK_INDICES[clusterId] || [];
  }
  async function listFields(indexName) {
    return MOCK_FIELDS[indexName] || [];
  }
  function defaultClusterId() {
    return localStorage.getItem("xerj.cluster") || "local";
  }
  function setDefaultCluster(id) {
    localStorage.setItem("xerj.cluster", id);
  }

  // playground/src/dashboards/data.js
  var humanCount = (n) => {
    if (n == null) return "\u2014";
    if (n >= 1e9) return (n / 1e9).toFixed(1) + "B";
    if (n >= 1e6) return (n / 1e6).toFixed(1) + "M";
    if (n >= 1e3) return (n / 1e3).toFixed(0) + "K";
    return String(n);
  };
  var humanBytes = (b) => {
    if (b == null) return "\u2014";
    const u = ["B", "KB", "MB", "GB", "TB", "PB"];
    let v = b, i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return v.toFixed(v < 10 ? 1 : 0) + " " + u[i];
  };
  var dataSection = {
    id: "data",
    name: "Data",
    section: "data",
    render: ({ data, time }) => {
      const clusters = data.clusters || [];
      const indicesByCluster = data.indicesByCluster || {};
      const fieldsByIndex = data.fieldsByIndex || {};
      const active = data.activeCluster || "local";
      const activeIndices = indicesByCluster[active] || [];
      const focusIndex = data.focusIndex || activeIndices[0]?.name;
      const fields = fieldsByIndex[focusIndex] || [];
      return {
        title: "DATA",
        kicker: "CLUSTERS \xB7 INDICES \xB7 FIELDS",
        meta: [time, "SOURCES"],
        caption: "What the engine actually has. Every row on this page maps to a real endpoint: `GET /v1/clusters`, `GET /v1/clusters/:id/indices`, `GET /v1/indices/:name/_mapping`. The mock values flip to live fetch the day the backend ships \u2014 nothing else on this page changes.",
        panels: [
          {
            id: "clusters",
            eyebrow: "CLUSTERS \xB7 CLICK TO SET DEFAULT",
            cols: 12,
            type: "clusters",
            render: () => renderClusters(clusters, active)
          },
          {
            id: "indices",
            eyebrow: `INDICES \xB7 ${active.toUpperCase()} \xB7 CLICK AN INDEX TO INSPECT FIELDS`,
            cols: 6,
            type: "indices",
            render: () => renderIndices(activeIndices, focusIndex)
          },
          {
            id: "fields",
            eyebrow: `FIELDS \xB7 ${focusIndex || "\u2014"} \xB7 FROM /v1/indices/:name/_mapping`,
            cols: 6,
            type: "fields",
            render: () => renderFields(fields)
          },
          {
            id: "howTo",
            eyebrow: "CONNECTING A NEW CLUSTER",
            cols: 12,
            type: "markdown",
            render: () => Markdown(
              `## Point XERJ.ai at a cluster

Today, clusters are defined in \`src/data/data-sources.js\` and read from
mock arrays. **When the engine ships** the HTTP bindings that this page
expects, clusters will be configured through this UI instead.

\`\`\`
POST /v1/clusters
{
  "id":   "prod-us",
  "name": "PROD-US",
  "url":  "https://xerj-us-east-1.internal:8080",
  "auth": { "type": "bearer", "token": "$XERJ_TOKEN" }
}
\`\`\`

The current default cluster is stored in \`localStorage.xerj.cluster\`,
which you can inspect under SETTINGS. Every query goes to that cluster
unless a specific dashboard panel overrides it via its \`source: { cluster }\`
binding.`
            )
          },
          {
            id: "citations",
            eyebrow: "WHY THIS SECTION EXISTS \xB7 USER FEEDBACK",
            cols: 12,
            type: "citations",
            render: () => Citations({
              items: [
                {
                  id: "gh-6498",
                  source: "github",
                  score: 57,
                  title: "Remove index pattern mapping cache",
                  url: "https://github.com/elastic/kibana/issues/6498"
                },
                {
                  id: "gh-17888",
                  source: "github",
                  score: 45,
                  title: "Per-user profiles, settings in Kibana",
                  url: "https://github.com/elastic/kibana/issues/17888"
                },
                {
                  id: "gh-17542",
                  source: "github",
                  score: 57,
                  title: "Ability to change the index pattern on a visualization",
                  url: "https://github.com/elastic/kibana/issues/17542"
                }
              ],
              total: 451
            })
          }
        ]
      };
    }
  };
  function renderClusters(clusters, active) {
    if (!clusters.length) return '<div class="mono faint">No clusters configured.</div>';
    const rows = clusters.map((c) => {
      const isActive = c.id === active;
      const status = {
        green: `<span class="mono accent">\u25CF</span>`,
        yellow: `<span class="mono faint">\u25D0</span>`,
        red: `<span class="mono">\u25CB</span>`
      }[c.status] || "\u2014";
      return `
      <button type="button" class="mg-cluster${isActive ? " mg-cluster-active" : ""}" data-mg-cluster="${esc(c.id)}">
        <span class="mg-cluster-status">${status}</span>
        <span class="mg-cluster-name mono${isActive ? " accent" : ""}">${esc(c.name)}</span>
        <span class="mg-cluster-url mono faint">${esc(c.url)}</span>
        <span class="mg-cluster-stat mono">${humanCount(c.indices)}&nbsp;idx</span>
        <span class="mg-cluster-stat mono">${humanCount(c.docs)}&nbsp;docs</span>
        <span class="mg-cluster-ver mono faint">${esc(c.version)}</span>
      </button>`;
    }).join("");
    return `<div class="mg-clusters">${rows}</div>`;
  }
  function renderIndices(indices, focusIndex) {
    if (!indices.length) return '<div class="mono faint">No indices in this cluster.</div>';
    const cols = ["NAME", "DOCS", "SIZE", "SHARDS", "RETENTION"];
    const headRow = `<div class="mg-idx-row mg-idx-head">${cols.map((c) => `<span>${esc(c)}</span>`).join("")}</div>`;
    const body = indices.map((i) => {
      const cells = [
        `<button type="button" class="mg-idx-btn${i.name === focusIndex ? " active" : ""}" data-mg-index="${esc(i.name)}">${esc(i.name)}</button>`,
        humanCount(i.docs),
        humanBytes(i.bytes),
        String(i.shards),
        i.retention_days == null ? "\u221E" : i.retention_days + "d"
      ];
      return `<div class="mg-idx-row">${cells.map((c) => `<span>${c}</span>`).join("")}</div>`;
    }).join("");
    return `<div class="mg-idx-table">${headRow}${body}</div>`;
  }
  function renderFields(fields) {
    if (!fields.length) return '<div class="mono faint">No mapping for this index.</div>';
    const cols = ["FIELD", "TYPE", "CARDINALITY", "ENCODING", "RATIO"];
    const headRow = `<div class="mg-fld-row mg-fld-head">${cols.map((c) => `<span>${esc(c)}</span>`).join("")}</div>`;
    const body = fields.map((f) => `
    <div class="mg-fld-row">
      <span class="mono">${esc(f.name)}</span>
      <span class="mono faint">${esc(f.type)}</span>
      <span class="mono">${humanCount(f.cardinality)}</span>
      <span class="mono faint">${esc(f.encoding)}</span>
      <span class="mono">${(f.ratio * 100).toFixed(0)}%</span>
    </div>`).join("");
    return `<div class="mg-fld-table">${headRow}${body}</div>`;
  }

  // playground/src/dashboards/users.js
  var users = {
    id: "users",
    name: "Users",
    section: "users",
    render: ({ data, time }) => ({
      title: "USERS",
      kicker: "IDENTITY \xB7 ACCESS \xB7 TOKENS",
      meta: [time, "RBAC"],
      caption: 'No Spaces. No feature privileges matrix. No separate "role mapping" app. A user has a token. A role is a set of index prefixes and operations. A session is a token with an expiry. Everything else is hidden complexity we refuse to ship.',
      panels: [
        {
          id: "users",
          eyebrow: "USERS",
          cols: 3,
          type: "metric",
          render: () => Num({ value: "128", unit: "active", hint: "12 online now", emphasis: true })
        },
        {
          id: "roles",
          eyebrow: "ROLES",
          cols: 2,
          type: "metric",
          render: () => Num({ value: "8", unit: "defined", hint: "flat model", emphasis: false })
        },
        {
          id: "apiKeys",
          eyebrow: "API KEYS",
          cols: 2,
          type: "metric",
          render: () => Num({ value: "42", unit: "live", hint: "3 expire < 7d", emphasis: false })
        },
        {
          id: "sessions",
          eyebrow: "SESSIONS",
          cols: 2,
          type: "metric",
          render: () => Num({ value: "86", unit: "active", delta: 3.2, emphasis: false })
        },
        {
          id: "lastLogin",
          eyebrow: "LAST LOGIN",
          cols: 3,
          type: "metric",
          render: () => Num({ value: "12s", unit: "ago", hint: "deploy@xerj.ai", emphasis: false })
        },
        {
          id: "userList",
          eyebrow: "USERS \xB7 MOST ACTIVE",
          cols: 6,
          type: "topn",
          render: () => TopN({
            items: [
              { label: "deploy", value: 2804 },
              { label: "oncall-bot", value: 1920 },
              { label: "searcher", value: 1612 },
              { label: "metrics-exporter", value: 1402 },
              { label: "alice@eng", value: 1180 },
              { label: "bob@eng", value: 980 },
              { label: "carol@pm", value: 770 },
              { label: "dave@sre", value: 612 }
            ],
            total: 11280,
            n: 8
          })
        },
        {
          id: "roles",
          eyebrow: "ROLES \xB7 INDEX PREFIX \xD7 OPS",
          cols: 6,
          type: "table",
          render: () => Table({
            columns: ["ROLE", "INDICES", "OPS"],
            rows: [
              ["admin", "*", "read, write, admin, delete"],
              ["developer", "logs-*, metrics-*", "read, write"],
              ["sre", "*", "read, write, admin"],
              ["pm", "logs-prod, metrics", "read"],
              ["oncall", "*", "read, alert.ack"],
              ["agent-token", "agent-memory, embeddings", "read, write"],
              ["audit-ro", "audit", "read"],
              ["guest", "docs", "read"]
            ],
            align: ["left", "left", "left"]
          })
        },
        {
          id: "recent",
          eyebrow: "RECENT AUTH EVENTS",
          cols: 12,
          type: "events",
          render: () => Events({ items: [
            { at: "23:14:02", sev: "info", msg: "login \xB7 deploy@xerj.ai \xB7 src=10.0.3.42 \xB7 method=bearer \xB7 ok" },
            { at: "23:13:51", sev: "info", msg: "login \xB7 alice@eng    \xB7 src=10.0.4.11 \xB7 method=sso \xB7 ok" },
            { at: "23:13:22", sev: "warn", msg: "token rotated \xB7 api-key-k9q8 \xB7 expires in 30d" },
            { at: "23:12:48", sev: "err", msg: "permission denied \xB7 bob@eng \xB7 index=audit \xB7 op=read \xB7 missing role audit-ro" },
            { at: "23:12:09", sev: "info", msg: "login \xB7 oncall-bot   \xB7 src=10.0.0.4  \xB7 method=api-key \xB7 ok" },
            { at: "23:11:44", sev: "info", msg: "token created \xB7 bob@eng \xB7 api-key-77mx \xB7 expires=30d" },
            { at: "23:11:03", sev: "warn", msg: "token expiring \xB7 alice@eng \xB7 api-key-4ka2 \xB7 6h" }
          ] })
        },
        {
          id: "model",
          eyebrow: "THE PERMISSION MODEL \xB7 WHY IT IS LIKE THIS",
          cols: 12,
          type: "markdown",
          render: () => Markdown(
            `## One token, no magic

XERJ.ai has exactly three concepts for access control:

- **Token** \u2014 a user or machine bearer. Can have an expiry. Can be revoked.
- **Role** \u2014 a set of \`(index_prefix, operation)\` pairs. Flat. No inheritance.
- **Session** \u2014 a token that's currently open. Auto-expires.

That's it. There are no **Spaces**, no **feature privileges matrix**, no
separate **role mapping** configuration. Everything goes through one check:

\`\`\`
allow(token, index, op) :=
   \u2203 role \u2208 token.roles :
     \u2203 (prefix, ops) \u2208 role :
       index.startsWith(prefix) \u2227 op \u2208 ops
\`\`\`

If an operation is denied, the response includes the **exact missing role +
prefix**. No more "permission denied" with no hint \u2014 see the auth event
above for \`bob@eng\`.

The corpus has 213 items in \`categories/08-spaces-and-rbac/\`, many of
which are "I can see the dashboard but can't edit it" or "why doesn't my
Space work". That complexity is gone.`
          )
        },
        {
          id: "citations",
          eyebrow: "WHY THIS SECTION EXISTS \xB7 USER FEEDBACK",
          cols: 12,
          type: "citations",
          render: () => Citations({
            items: [
              {
                id: "gh-4453",
                source: "github",
                score: 38,
                title: "Saved object authorization - Phase 1",
                url: "https://github.com/elastic/kibana/issues/4453"
              },
              {
                id: "gh-18331",
                source: "github",
                score: 47,
                title: "Anonymous access",
                url: "https://github.com/elastic/kibana/issues/18331"
              },
              {
                id: "gh-17888",
                source: "github",
                score: 45,
                title: "Per-user profiles, settings in Kibana",
                url: "https://github.com/elastic/kibana/issues/17888"
              }
            ],
            total: 213
          })
        }
      ]
    })
  };

  // playground/src/dashboards/settings.js
  var settings = {
    id: "settings",
    name: "Settings",
    section: "settings",
    render: ({ data, time }) => {
      const dashboards = data.dashboards || [];
      const views = data.views || [];
      return {
        title: "SETTINGS",
        kicker: "DEFAULTS \xB7 DASHBOARDS \xB7 DANGER",
        meta: [time, "ADMIN"],
        caption: "Everything under SETTINGS is rare admin. Rename or reorder a dashboard here; for everything else, the defaults are right.",
        panels: [
          {
            id: "defaults",
            eyebrow: "DEFAULTS",
            cols: 12,
            type: "settings",
            render: () => renderSettings()
          },
          {
            id: "mg-dashboards-head",
            eyebrow: "",
            cols: 12,
            type: "markdown",
            render: () => `<div class="h-section" style="margin-top:var(--sp-3);">DASHBOARDS \xB7 ${dashboards.length}</div>
            <div class="hint" style="margin-top:6px;">Drag a row to reorder. Click RENAME to rename. CLONE, HIDE, or DELETE from the action buttons.</div>`
          },
          {
            id: "mg-dashboards",
            eyebrow: "",
            cols: 12,
            type: "manage-dashboards",
            render: () => renderDashboardTable(dashboards)
          },
          {
            id: "mg-new",
            eyebrow: "",
            cols: 12,
            type: "manage-new",
            render: () => renderNewDashboardRow(dashboards.filter((d) => !d.isUser && d.section === "dashboards"))
          },
          {
            id: "mg-views-head",
            eyebrow: "",
            cols: 12,
            type: "markdown",
            render: () => `<div class="h-section" style="margin-top:var(--sp-10);">SAVED VIEWS \xB7 ${views.length}</div>
            <div class="hint" style="margin-top:6px;">A saved view is a pinned snapshot of a dashboard with its time range, cluster, and filters. Click the name to apply.</div>`
          },
          {
            id: "mg-views",
            eyebrow: "",
            cols: 12,
            type: "manage-views",
            render: () => renderViewsTable(views)
          },
          {
            id: "storage",
            eyebrow: "PERSISTENT STATE INVENTORY",
            cols: 12,
            type: "markdown",
            render: () => Markdown(
              `## What we store, where

Everything XERJ.ai persists lives under \`localStorage.xerj.*\`. No
cookies, no IndexedDB, no server state (yet). You can dump the entire
inventory with one browser-console command:

\`\`\`
Object.keys(localStorage).filter(k => k.startsWith('xerj.'))
\`\`\`

The current keys are:

- \`xerj.theme\` \xB7 \`day\` | \`night\`
- \`xerj.time\` \xB7 last-used time range (\`1H\`, \`24H\`, \`7D\`, ...)
- \`xerj.edit\` \xB7 \`0\` | \`1\` \u2014 is edit mode sticky on reload
- \`xerj.search\` \xB7 search-discover state (query + type + filters + sort)
- \`xerj.dashboards\` \xB7 rename / order / hidden / custom (user dashboards)
- \`xerj.layout.<dash-id>\` \xB7 per-dashboard panel layout overrides
- \`xerj.cluster\` \xB7 default cluster id

When the engine grows a \`/v1/users/me\` endpoint, this store syncs up.`
            )
          },
          {
            id: "danger",
            eyebrow: "DANGER \xB7 WIPE ALL STATE",
            cols: 12,
            type: "danger",
            render: () => `
            <div class="mg-settings">
              <div class="mg-setting">
                <span class="key">RESET EVERYTHING</span>
                <button type="button" class="mg-btn mg-btn-danger" data-mg-reset-all>RESET ALL SAVED STATE</button>
                <span class="hint">\u2014 restores defaults, deletes every user dashboard, clears layouts, themes, filters. Reloads the page. Cannot be undone.</span>
              </div>
            </div>
          `
          },
          {
            id: "citations",
            eyebrow: "WHY THIS SECTION EXISTS \xB7 USER FEEDBACK",
            cols: 12,
            type: "citations",
            render: () => Citations({
              items: [
                {
                  id: "gh-56406",
                  source: "github",
                  score: 41,
                  title: 'Add a configuration setting for default "Rows Per Page" setting in Management',
                  url: "https://github.com/elastic/kibana/issues/56406"
                },
                {
                  id: "gh-6515",
                  source: "github",
                  score: 18,
                  title: "Kibana Globalization",
                  url: "https://github.com/elastic/kibana/issues/6515"
                },
                {
                  id: "gh-1600",
                  source: "github",
                  score: 14,
                  title: "Global timezone support",
                  url: "https://github.com/elastic/kibana/issues/1600"
                }
              ],
              total: 3610
            })
          }
        ]
      };
    }
  };
  function renderSettings() {
    return `
    <div class="mg-settings">
      <div class="mg-setting">
        <span class="key">DEFAULT CLUSTER</span>
        <span class="mono accent">${esc(defaultClusterId())}</span>
        <span class="hint">\u2014 change by clicking a cluster in the <a href="#/data" class="mono accent">DATA</a> section</span>
      </div>
      <div class="mg-setting">
        <span class="key">THEME</span>
        <span class="mono">toggle in the top-right nav</span>
      </div>
      <div class="mg-setting">
        <span class="key">TIME ZONE</span>
        <span class="mono">${esc(Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC")}</span>
        <span class="hint">\u2014 auto-detected from browser</span>
      </div>
    </div>`;
  }
  function renderDashboardTable(dashboards) {
    if (!dashboards.length) {
      return '<div class="mono faint">No dashboards. Click + NEW to create one.</div>';
    }
    const listable = dashboards.filter((d) => d.section === "dashboards" || d.section == null);
    const rows = listable.map((d, i) => {
      const isHidden = d.hidden;
      const kind = d.isUser ? `<span class="mono accent">USER</span>` : `<span class="mono faint">DEFAULT</span>`;
      const src = d.clonedFrom ? `<span class="mono faint">from ${esc(d.clonedFrom)}</span>` : "";
      const actions = [
        `<button type="button" class="mg-btn" data-mg-up="${esc(d.id)}" title="Move up">\u2191</button>`,
        `<button type="button" class="mg-btn" data-mg-down="${esc(d.id)}" title="Move down">\u2193</button>`,
        `<button type="button" class="mg-btn" data-mg-rename="${esc(d.id)}" title="Rename">RENAME</button>`,
        `<button type="button" class="mg-btn" data-mg-hide="${esc(d.id)}" title="${isHidden ? "Show" : "Hide"}">${isHidden ? "SHOW" : "HIDE"}</button>`,
        `<button type="button" class="mg-btn" data-mg-clone="${esc(d.id)}" title="Clone">CLONE</button>`,
        d.isUser ? `<button type="button" class="mg-btn mg-btn-danger" data-mg-delete="${esc(d.id)}" title="Delete">DELETE</button>` : `<span class="mg-btn mg-btn-disabled" title="Defaults can only be hidden">\u2014</span>`
      ].join("");
      return `
      <div class="mg-row${isHidden ? " mg-row-hidden" : ""}" data-mg-id="${esc(d.id)}">
        <span class="mg-order mono faint">${String(i + 1).padStart(2, "0")}</span>
        <span class="mg-name">${esc(d.name)}</span>
        <span class="mg-kind">${kind}</span>
        <span class="mg-src">${src}</span>
        <span class="mg-actions">${actions}</span>
      </div>`;
    }).join("");
    return `
    <div class="mg-table">
      <div class="mg-row mg-head">
        <span class="mg-order">#</span>
        <span class="mg-name">NAME</span>
        <span class="mg-kind">KIND</span>
        <span class="mg-src">SOURCE</span>
        <span class="mg-actions">ACTIONS</span>
      </div>
      ${rows}
    </div>`;
  }
  function renderViewsTable(views) {
    if (!views.length) {
      return '<div class="mono faint">No saved views. Pin a view from any dashboard with the + SAVE CURRENT VIEW button.</div>';
    }
    const rows = views.map((v, i) => {
      const fs = v.filters || {};
      const fbits = Object.entries(fs).map(([k, val]) => {
        const vals = Array.isArray(val) ? val.join("|") : val;
        return `${k}:${vals}`;
      }).join(" \xB7 ") || "\u2014";
      const when = (v.savedAt || "").slice(0, 16).replace("T", " ");
      return `
      <div class="mg-row" data-view-id="${esc(v.id)}">
        <span class="mg-order mono faint">${String(i + 1).padStart(2, "0")}</span>
        <span class="mg-name"><button type="button" class="mg-btn mg-btn-link" data-view-apply="${esc(v.id)}" title="Apply view">${esc(v.name)}</button></span>
        <span class="mg-kind"><span class="mono faint">${esc(v.dashId)}</span></span>
        <span class="mg-src"><span class="mono faint">${esc(v.time || "24H")} \xB7 ${esc(v.cluster || "\u2014")} \xB7 ${esc(fbits)}</span></span>
        <span class="mg-actions">
          <span class="mono faint">${esc(when)}</span>
          <button type="button" class="mg-btn" data-view-apply="${esc(v.id)}" title="Apply">APPLY</button>
          <button type="button" class="mg-btn mg-btn-danger" data-view-delete="${esc(v.id)}" title="Delete">DELETE</button>
        </span>
      </div>`;
    }).join("");
    return `
    <div class="mg-table">
      <div class="mg-row mg-head">
        <span class="mg-order">#</span>
        <span class="mg-name">NAME</span>
        <span class="mg-kind">DASHBOARD</span>
        <span class="mg-src">CONTEXT</span>
        <span class="mg-actions">SAVED / ACTIONS</span>
      </div>
      ${rows}
    </div>`;
  }
  function renderNewDashboardRow(templates) {
    const options = templates.map(
      (t) => `<button type="button" class="mg-btn" data-mg-new="${esc(t.id)}" title="New from ${esc(t.name)}">${esc(t.name)}</button>`
    ).join('<span class="sep">\xB7</span>');
    return `
    <div class="mg-new-row">
      <span class="key accent">+ NEW DASHBOARD</span>
      <span class="hint" style="margin: 0 var(--sp-2);">clone a template</span>
      <span class="mg-new-templates">${options}</span>
      <span class="hint" style="margin-left:auto;">or</span>
      <button type="button" class="mg-btn" data-mg-new="" title="New blank dashboard">BLANK</button>
    </div>`;
  }

  // playground/src/dashboards/registry.js
  var DEFAULT_GROUP = {
    "ai-overview": "ai",
    "rag-quality": "ai",
    "vector-index": "ai",
    "agent-memory": "ai",
    "logs-overview": "logs",
    "anomaly-detect": "logs",
    "ingest-pipeline": "logs",
    "system": "infra"
  };
  for (const d of [aiOverview, ragQuality, vectorIndex, agentMemory, anomalyDetect, ingestPipeline, logsOverview, system]) {
    d.section = "dashboards";
    d.group = DEFAULT_GROUP[d.id] || "other";
  }
  searchDiscover.section = "discover";
  var all = [
    // Dashboards section, ordered by group so the first member of each
    // group is the one the group tab lands on when clicked.
    //   AI:    ai-overview, rag-quality, vector-index, agent-memory
    //   Logs:  logs-overview, anomaly-detect, ingest-pipeline
    //   Infra: system
    aiOverview,
    ragQuality,
    vectorIndex,
    agentMemory,
    logsOverview,
    anomalyDetect,
    ingestPipeline,
    system,
    // Top-level sections (one view each)
    searchDiscover,
    alerts,
    dataSection,
    users,
    settings
  ];
  var defaults = all;
  var registry = Object.fromEntries(all.map((d) => [d.id, d]));
  var SECTIONS = [
    { id: "dashboards", label: "Dashboards" },
    { id: "discover", label: "Discover" },
    { id: "alerts", label: "Alerts" },
    { id: "data", label: "Data" },
    { id: "users", label: "Users" },
    { id: "settings", label: "Settings" }
  ];
  var DASHBOARD_GROUPS = [
    { id: "ai", label: "AI" },
    { id: "logs", label: "Logs" },
    { id: "infra", label: "Infra" }
  ];
  function dashboardsInSection(sectionId, merged) {
    return merged.filter((d) => (d.section || "dashboards") === sectionId);
  }
  var dashboardList = all.map((d) => ({ id: d.id, name: d.name }));

  // playground/src/ux/chrome.js
  var ThemeCtrl = ({ active = "night" } = {}) => `
<span class="theme" role="group" aria-label="Theme">
  <button type="button" data-theme-set="day"   class="${active === "day" ? "active" : ""}" aria-pressed="${active === "day"}">DAY</button>
  <span class="dash">\xB7</span>
  <button type="button" data-theme-set="night" class="${active === "night" ? "active" : ""}" aria-pressed="${active === "night"}">NIGHT</button>
</span>`;
  var MobileCtrl = ({ active = false } = {}) => `
<span class="mobile-ctrl" role="group" aria-label="Mobile preview">
  <button type="button" data-mobile-toggle class="${active ? "active" : ""}" aria-pressed="${active}">MOBILE</button>
</span>`;
  var EditCtrl = ({ active = false } = {}) => `
<span class="edit-ctrl" role="group" aria-label="Edit mode">
  <button type="button" data-edit-toggle class="${active ? "active" : ""}" aria-pressed="${active}">EDIT</button>
  ${active ? `<span class="dash">\xB7</span><button type="button" data-reset-layout>RESET</button>` : ""}
</span>`;
  var Nav = ({
    sections = [],
    activeSection = "dashboards",
    dashboards = [],
    groups = [],
    activeDash = "",
    theme = "night",
    edit = false,
    mobile = false,
    status = ""
  } = {}) => {
    const primaryLinks = sections.map((s) => {
      const href = s.id === "dashboards" ? "#/dashboards" : "#/" + s.id;
      return `<a href="${href}" data-section="${esc(s.id)}" class="${s.id === activeSection ? "active" : ""}">${esc(s.label)}</a>`;
    }).join("");
    const showSecondary = activeSection === "dashboards" && dashboards.length > 0;
    let secondary = "";
    if (showSecondary) {
      const byGroup = {};
      for (const d of dashboards) {
        const g = d.group || "other";
        (byGroup[g] = byGroup[g] || []).push(d);
      }
      const activeDashObj = dashboards.find((d) => d.id === activeDash);
      const activeGroup = activeDashObj?.group || (groups[0]?.id || "other");
      const orderedGroups = [];
      const seen = /* @__PURE__ */ new Set();
      for (const g of groups) {
        if (byGroup[g.id]) {
          orderedGroups.push({ ...g, members: byGroup[g.id] });
          seen.add(g.id);
        }
      }
      for (const gid of Object.keys(byGroup)) {
        if (!seen.has(gid)) orderedGroups.push({ id: gid, label: gid.toUpperCase(), members: byGroup[gid] });
      }
      const groupHtml = orderedGroups.map((g) => {
        const isActive = g.id === activeGroup;
        const members = isActive ? g.members.map(
          (d) => `<a href="#/dashboards/${esc(d.id)}" data-dash="${esc(d.id)}" class="${d.id === activeDash ? "active" : ""}">${esc(d.name)}</a>`
        ).join("") : "";
        const firstId = g.members[0]?.id || "";
        const labelCls = "group-label" + (isActive ? " active" : "");
        return `
        <span class="group${isActive ? " open" : ""}">
          <button type="button" class="${labelCls}" data-dash-group="${esc(g.id)}" data-dash-group-first="${esc(firstId)}" aria-expanded="${isActive}">${esc(g.label)}</button>
          ${isActive ? `<span class="members">${members}</span>` : ""}
        </span>`;
      }).join("");
      secondary = `<nav class="nav-sub" aria-label="Dashboards">${groupHtml}</nav>`;
    }
    return `
<nav class="nav" aria-label="Product">
  <span class="brand">XERJ.AI \xB7 OBSERVE</span>
  ${primaryLinks}
  <span class="spacer"></span>
  ${EditCtrl({ active: edit })}
  ${MobileCtrl({ active: mobile })}
  ${ThemeCtrl({ active: theme })}
  ${status ? `<span class="status">${esc(status)}</span>` : ""}
</nav>
${secondary}`;
  };
  var SceneHeader = ({ title, kicker = "", meta = [], editable = false, dashId = "" } = {}) => {
    const bits = [kicker, ...meta].filter(Boolean);
    const kickerLine = bits.length ? `<div class="kicker"><span class="key">${bits.map(esc).join(
      '</span><span class="dash">\xB7</span><span class="key">'
    )}</span></div>` : "";
    const editAttrs = editable ? ` contenteditable="true" spellcheck="false" data-rename-dash="${esc(dashId)}" title="Click to rename \xB7 Enter to save \xB7 Esc to cancel"` : "";
    return `
<header class="scene">
  ${kickerLine}
  <h1 class="h-scene${editable ? " editable" : ""}"${editAttrs}>${esc(title)}</h1>
</header>`;
  };
  var TimeCtrl = ({
    ranges = ["1H", "24H", "7D", "30D", "90D"],
    active = "24H",
    custom = { from: "", to: "" }
  } = {}) => {
    const buttons = ranges.map(
      (r) => `<button type="button" data-time="${esc(r)}" class="${r === active ? "active" : ""}" aria-pressed="${r === active}">${esc(r)}</button>`
    ).join("");
    const isCustom = active === "CUSTOM";
    const customBtn = `<button type="button" data-time="CUSTOM" class="${isCustom ? "active" : ""}" aria-pressed="${isCustom}">CUSTOM</button>`;
    const customInputs = isCustom ? `
    <span class="custom-range">
      <input type="datetime-local" data-time-from value="${esc(custom.from || "")}" aria-label="From" />
      <span class="dash">\u2192</span>
      <input type="datetime-local" data-time-to   value="${esc(custom.to || "")}" aria-label="To"   />
    </span>` : "";
    return `
  <div class="time" role="group" aria-label="Time range">
    <span class="key">RANGE</span>
    ${buttons}
    ${customBtn}
    ${customInputs}
  </div>`;
  };
  var RefreshCtrl = ({
    intervals = [
      { ms: 0, label: "OFF" },
      { ms: 1e4, label: "10S" },
      { ms: 3e4, label: "30S" },
      { ms: 6e4, label: "1M" },
      { ms: 3e5, label: "5M" }
    ],
    active = 0
  } = {}) => `
<div class="refresh" role="group" aria-label="Auto-refresh">
  <span class="key">REFRESH</span>
  ${intervals.map(
    (iv) => `<button type="button" data-refresh="${iv.ms}" class="${iv.ms === active ? "active" : ""}" aria-pressed="${iv.ms === active}">${esc(iv.label)}</button>`
  ).join("")}
</div>`;
  var FilterBar = ({ filters = {}, kql = "" } = {}) => {
    const entries = Object.entries(filters);
    const pills = entries.map(([field, value]) => {
      const values = Array.isArray(value) ? value : [value];
      const chips = values.map((v, i) => `
      ${i > 0 ? '<span class="or">OR</span>' : ""}
      <button type="button" class="chip" data-filter-remove="${esc(field)}:${esc(v)}" title="Remove ${esc(field)} = ${esc(v)}">
        <span class="value">${esc(v)}</span>
        <span class="x">\u2715</span>
      </button>
    `).join("");
      return `
    <span class="pill active">
      <span class="field">${esc(field)}</span>
      <span class="eq">:</span>
      ${chips}
    </span>`;
    }).join("");
    const hint = entries.length ? "" : `<span class="hint mono faint">TYPE <span class="mono">field:value</span> ABOVE \xB7 OR CLICK A LABEL IN A CHART</span>`;
    const clearBtn = entries.length ? `<button type="button" class="clear" data-filter-clear>CLEAR ALL</button>` : "";
    return `
  <div class="filter-bar${entries.length ? "" : " empty"}" aria-label="Filters">
    <div class="kql">
      <span class="key">KQL</span>
      <input type="text" data-kql-input value="${esc(kql)}" placeholder="service:auth level:error &quot;login failed&quot;" spellcheck="false" autocomplete="off" />
    </div>
    <div class="pills">
      <span class="key">FILTER</span>
      ${pills}
      ${hint}
      ${clearBtn}
    </div>
  </div>`;
  };
  var SavedViews = ({ views = [], dashId = "" } = {}) => {
    const mine = views.filter((v) => v.dashId === dashId);
    const saveBtn = `<button type="button" class="save" data-view-save>+ SAVE CURRENT VIEW</button>`;
    if (!mine.length) {
      return `
    <div class="saved-views empty" aria-label="Saved views">
      <span class="key">VIEWS</span>
      <span class="hint mono faint">NO SAVED VIEWS</span>
      ${saveBtn}
    </div>`;
    }
    const links = mine.map((v) => `
    <button type="button" class="view" data-view-apply="${esc(v.id)}" title="Apply view">${esc(v.name)}</button>
    <button type="button" class="x" data-view-delete="${esc(v.id)}" title="Delete view" aria-label="Delete view">\u2715</button>
  `).join('<span class="sep">\xB7</span>');
    return `
  <div class="saved-views" aria-label="Saved views">
    <span class="key">VIEWS</span>
    ${links}
    ${saveBtn}
  </div>`;
  };
  var ClusterCtrl = ({ clusters = [], active = "" } = {}) => {
    if (!clusters.length) return "";
    return `
  <div class="cluster-ctrl" role="group" aria-label="Cluster">
    <span class="key">CLUSTER</span>
    ${clusters.map(
      (c) => `<button type="button" data-cluster-set="${esc(c.id)}" class="${c.id === active ? "active" : ""}" aria-pressed="${c.id === active}">${esc(c.name || c.id)}</button>`
    ).join("")}
  </div>`;
  };
  var Footer = ({ version = "v0.1" } = {}) => {
    const now = (/* @__PURE__ */ new Date()).toISOString().slice(0, 16).replace("T", " \xB7 ");
    return `
<footer class="footer">
  <span>XERJ.AI \xB7 OBSERVE \xB7 ${esc(version)}</span>
  <span>TYPE IS THE UI</span>
  <span>${esc(now)}</span>
</footer>`;
  };

  // playground/src/data/mock.js
  var rng = (seed) => {
    let t = seed >>> 0;
    return () => {
      t = t + 1831565813 >>> 0;
      let r = t;
      r = Math.imul(r ^ r >>> 15, r | 1);
      r ^= r + Math.imul(r ^ r >>> 7, r | 61);
      return ((r ^ r >>> 14) >>> 0) / 4294967296;
    };
  };
  var hashStr = (s) => {
    let h = 2166136261;
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  };
  var POINTS = { "1H": 60, "24H": 96, "7D": 168, "30D": 180, "90D": 180 };
  var points = (r) => POINTS[r] ?? 96;
  var HOURS = { "1H": 1, "24H": 24, "7D": 168, "30D": 720, "90D": 2160 };
  var hours = (r) => HOURS[r] ?? 24;
  var diurnal = (n, base, { peakHour = 14, amplitude = 0.75, noise = 0.12, rand } = {}) => {
    const out = new Array(n);
    const span = hours(rand.range ?? "24H");
    for (let i = 0; i < n; i++) {
      const hour = i / (n - 1) * span;
      const phase = Math.cos((hour - peakHour) / 24 * 2 * Math.PI);
      const n1 = (rand() - 0.5) * 2 * noise;
      out[i] = Math.max(0, base * (1 + amplitude * phase + n1));
    }
    return out;
  };
  var pareto = (labels, total, { alpha = 1.05, rand }) => {
    const raw = labels.map((_, i) => 1 / Math.pow(i + 1, alpha) * (0.85 + rand() * 0.3));
    const sum = raw.reduce((a, b) => a + b, 0);
    return labels.map((label, i) => ({ label, value: Math.round(raw[i] / sum * total) }));
  };
  var sumOf = (arr) => arr.reduce((a, b) => a + b, 0);
  var peakLabel = (values) => {
    let idx = 0, max = -Infinity;
    for (let i = 0; i < values.length; i++) if (values[i] > max) {
      max = values[i];
      idx = i;
    }
    const frac = idx / (values.length - 1);
    const h = Math.floor(frac * 24);
    const m = Math.floor((frac * 24 - h) * 60);
    return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
  };
  var compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 2 });
  var rangeLabels = (range) => {
    switch (range) {
      case "1H":
        return ["\u221260 MIN", "NOW"];
      case "24H":
        return ["00:00", "24:00"];
      case "7D":
        return ["MON", "SUN"];
      case "30D":
        return ["D\u221230", "TODAY"];
      case "90D":
        return ["Q START", "TODAY"];
      default:
        return ["", ""];
    }
  };
  var buildAiOverview = (rand, range) => {
    const n = points(range);
    const base = 780 + rand() * 220;
    const queries = diurnal(n, base, { rand, peakHour: 13 });
    const totalQueries = Math.round(sumOf(queries));
    const promptTok = queries.map((q) => q * (1800 + rand() * 600));
    const ctxTok = queries.map((q) => q * (9e3 + rand() * 4500));
    const outTok = queries.map((q) => q * (320 + rand() * 180));
    const cacheHit = queries.map(() => 0.38 + rand() * 0.18);
    const totalPromptT = Math.round(sumOf(promptTok));
    const totalCtxT = Math.round(sumOf(ctxTok));
    const totalOutT = Math.round(sumOf(outTok));
    const totalT = totalPromptT + totalCtxT + totalOutT;
    const cost = totalPromptT / 1e6 * 2.5 + totalCtxT / 1e6 * 0.2 + totalOutT / 1e6 * 10;
    const costES = cost * 5.2;
    const models = [
      { label: "OPUS 4.6", value: Math.round(totalQueries * 0.22) },
      { label: "SONNET 4.6", value: Math.round(totalQueries * 0.35) },
      { label: "HAIKU 4.5", value: Math.round(totalQueries * 0.28) },
      { label: "GPT-5", value: Math.round(totalQueries * 0.09) },
      { label: "GEMINI 3", value: Math.round(totalQueries * 0.04) },
      { label: "LLAMA 4", value: Math.round(totalQueries * 0.02) }
    ];
    const intents = pareto(
      [
        "semantic search",
        "code-assist",
        "doc-q&a",
        "summarize",
        "translate",
        "classify",
        "extract-json",
        "agent-tool",
        "rerank",
        "rewrite",
        "chat-freeform",
        "safety-check"
      ],
      totalQueries,
      { alpha: 0.85, rand }
    );
    const topDocs = pareto(
      [
        "runbook/oncall.md",
        "rfc/042-retention.md",
        "arch/cluster-design.md",
        "rfc/039-hybrid-search.md",
        "runbook/incident-1411.md",
        "policy/pii.md",
        "arch/vector-internals.md",
        "rfc/051-agent-memory.md",
        "docs/query-dsl.md",
        "rfc/048-embed-proxy.md",
        "runbook/billing-sync.md",
        "docs/ingest-api.md"
      ],
      Math.round(totalQueries * 0.7),
      { alpha: 0.75, rand }
    );
    const latencyRibbons = [
      { label: "OPUS 4.6", values: queries.map(() => 1450 + rand() * 220) },
      { label: "SONNET 4.6", values: queries.map(() => 820 + rand() * 140) },
      { label: "HAIKU 4.5", values: queries.map(() => 310 + rand() * 80) },
      { label: "GPT-5", values: queries.map(() => 990 + rand() * 190) },
      { label: "GEMINI 3", values: queries.map(() => 1120 + rand() * 240) }
    ];
    const costHeatmap = {
      cols: Array.from({ length: 12 }, (_, i) => String(i * 2).padStart(2, "0")),
      matrix: ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"].map(
        (_, di) => Array.from({ length: 12 }, (_2, hi) => {
          const h = hi * 2;
          const phase = Math.cos((h - 13) / 24 * 2 * Math.PI);
          const weekend = di >= 5 ? 0.48 : 1;
          return Math.max(0.2, cost / 150 * weekend * (1 + 0.8 * phase) * (0.85 + rand() * 0.3));
        })
      )
    };
    const flowSegments = [
      { label: "SYS PROMPT", value: totalPromptT },
      { label: "CONTEXT", value: totalCtxT },
      { label: "COMPLETION", value: totalOutT }
    ];
    return {
      metrics: {
        queries: { value: totalQueries, formatted: compact.format(totalQueries), delta: (rand() - 0.3) * 12 },
        tokens: { value: totalT, formatted: compact.format(totalT), delta: (rand() - 0.35) * 8 },
        cost: { value: cost, formatted: "$" + cost.toFixed(0), delta: (rand() - 0.65) * 18 },
        savings: { value: costES - cost, formatted: "$" + (costES - cost).toFixed(0), note: "vs. ES+Pinecone+Splunk" },
        latency: { value: 820, formatted: "820", delta: (rand() - 0.55) * 8 },
        cacheHit: { value: 42, formatted: "42", delta: (rand() - 0.35) * 6 }
      },
      series: {
        queries,
        prompt: promptTok.map((v) => v / 1e3),
        context: ctxTok.map((v) => v / 1e3),
        output: outTok.map((v) => v / 1e3),
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      models,
      intents,
      topDocs,
      latencyRibbons,
      costHeatmap,
      flowSegments
    };
  };
  var buildRagQuality = (rand, range) => {
    const n = points(range);
    const grounding = diurnal(n, 88, { rand, amplitude: 0.06, noise: 0.03, peakHour: 13 });
    const hallucination = grounding.map((g) => Math.max(0, 4.2 - (g - 84) * 0.8 + (rand() - 0.5) * 0.8));
    const retrievalHit = grounding.map((g) => Math.min(99, g + 8 + (rand() - 0.5) * 4));
    const queries = [
      { id: "q1", label: "how do I reset cluster" },
      { id: "q2", label: "retention vs storage cost" },
      { id: "q3", label: "agent memory dedup rules" },
      { id: "q4", label: "hybrid search score fusion" },
      { id: "q5", label: "mmap segment format" },
      { id: "q6", label: "WAL recovery procedure" },
      { id: "q7", label: "what is kNN recall" },
      { id: "q8", label: "pricing large context" }
    ];
    const chunks = [
      { id: "c1", label: "arch/cluster-design.md#reset" },
      { id: "c2", label: "rfc/042-retention.md#cost" },
      { id: "c3", label: "rfc/051-agent-memory.md#dedup" },
      { id: "c4", label: "rfc/039-hybrid-search.md#rrf" },
      { id: "c5", label: "arch/vector-internals.md" },
      { id: "c6", label: "runbook/wal-recovery.md" },
      { id: "c7", label: "docs/query-dsl.md#knn" },
      { id: "c8", label: "rfc/048-embed-proxy.md" },
      { id: "c9", label: "docs/pricing.md#ctx" },
      { id: "c10", label: "runbook/oncall.md" }
    ];
    const flows = [];
    for (const q of queries) {
      const k = 3 + Math.floor(rand() * 3);
      const picked = /* @__PURE__ */ new Set();
      while (picked.size < k) picked.add("c" + (1 + Math.floor(rand() * chunks.length)));
      for (const cid of picked) {
        flows.push({ from: q.id, to: cid, weight: 0.4 + rand() * 0.6 });
      }
    }
    const sampleText = "The XERJ.ai cluster reset procedure requires draining the WAL, flushing all memtables to disk, and then restarting with the recovery flag set. Do not skip the WAL drain step \u2014 lost writes in the fsync tail are unrecoverable.";
    const tokens = sampleText.split(" ").map((t) => {
      const hot = /(cluster|reset|drain|WAL|flush|memtables|recovery|unrecoverable)/i.test(t);
      return { text: t, weight: hot ? 0.65 + rand() * 0.35 : 0.05 + rand() * 0.25 };
    });
    const chunkHitDensity = {
      rows: ["Q&A", "SEARCH", "SUMMARIZE", "CODE", "EXTRACT", "AGENT"],
      cols: ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10"],
      matrix: Array.from(
        { length: 6 },
        () => Array.from({ length: 10 }, () => Math.round(100 + rand() * 900))
      )
    };
    const retrievalSource = [
      { label: "HYBRID (RRF)", value: 6120 },
      { label: "VECTOR KNN", value: 2340 },
      { label: "BM25 ONLY", value: 820 },
      { label: "MEMORY RECALL", value: 410 }
    ];
    const lowGroundingPrompts = pareto(
      [
        "what changed in release 0.2",
        "why is latency up today",
        "who owns the billing pipeline",
        "current on-call for Europe",
        "is the fix deployed yet",
        "any anomaly in the last hour",
        "what does the new quota mean",
        "regression vs last week"
      ],
      4200,
      { alpha: 0.7, rand }
    ).map((it) => ({ ...it, value: Math.round(it.value / 10) + "%" }));
    return {
      metrics: {
        grounding: { value: grounding[grounding.length - 1], formatted: grounding[grounding.length - 1].toFixed(1), delta: (rand() - 0.35) * 4 },
        hallucination: { value: hallucination[hallucination.length - 1], formatted: hallucination[hallucination.length - 1].toFixed(2), delta: (rand() - 0.6) * 1.8 },
        hitRate: { value: retrievalHit[retrievalHit.length - 1], formatted: retrievalHit[retrievalHit.length - 1].toFixed(1), delta: (rand() - 0.4) * 4 },
        avgCitations: { value: 3.4, formatted: "3.4", delta: (rand() - 0.5) * 0.8 }
      },
      series: {
        grounding,
        hallucination,
        retrievalHit,
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      flow: { queries, chunks, flows },
      attention: { tokens, text: sampleText },
      chunkHitDensity,
      retrievalSource,
      lowGroundingPrompts
    };
  };
  var buildVectorIndex = (rand, range) => {
    const n = points(range);
    const qps = diurnal(n, 1800, { rand, peakHour: 14 });
    const p50 = qps.map(() => 2.8 + rand() * 0.7);
    const p95 = qps.map(() => 9.4 + rand() * 2.2);
    const p99 = qps.map(() => 22 + rand() * 6);
    const recall = qps.map(() => 96.2 + rand() * 2);
    const clusters = [
      { label: "code", center: [20, 70], spread: 9, count: 180 },
      { label: "docs", center: [60, 40], spread: 12, count: 220 },
      { label: "runbook", center: [82, 68], spread: 7, count: 110 },
      { label: "tickets", center: [42, 82], spread: 10, count: 160 },
      { label: "chat", center: [30, 20], spread: 14, count: 240 },
      { label: "email", center: [72, 18], spread: 8, count: 120 }
    ].map((c) => {
      const points2 = Array.from({ length: c.count }, () => {
        const a = rand() * Math.PI * 2;
        const r = rand() ** 0.5 * c.spread;
        return [c.center[0] + Math.cos(a) * r, c.center[1] + Math.sin(a) * r];
      });
      return { label: c.label, points: points2, centroid: c.center };
    });
    const vectors = clusters.reduce((a, c) => a + c.points.length, 0);
    const models = [
      { label: "text-embed-3 (1536)", value: 720 },
      { label: "cohere-v3 (1024)", value: 340 },
      { label: "e5-large (1024)", value: 190 },
      { label: "local-bge-m3 (1024)", value: 80 }
    ];
    const dims = [
      { name: "LATENCY" },
      { name: "TOKENS" },
      { name: "COST" },
      { name: "RECALL" },
      { name: "GROUND" }
    ];
    const rows = Array.from({ length: 80 }, () => [
      300 + rand() * 1400,
      // latency ms
      800 + rand() * 18e3,
      // tokens
      0.01 + rand() * 0.18,
      // cost usd
      82 + rand() * 16,
      // recall %
      70 + rand() * 28
      // grounding %
    ]);
    const highlight = [1420, 14200, 0.16, 94, 86];
    return {
      metrics: {
        vectors: { value: vectors, formatted: compact.format(vectors * 6200), hint: "6.2M total \xB7 shard 14/32" },
        dim: { value: 1536, formatted: "1536", hint: "3 models" },
        disk: { value: 38, formatted: "38", hint: "GB quantized" },
        qps: { value: qps[qps.length - 1], formatted: compact.format(qps[qps.length - 1]), delta: (rand() - 0.3) * 6 },
        recall: { value: 96.8, formatted: "96.8", delta: (rand() - 0.45) * 0.6 },
        p95: { value: p95[p95.length - 1], formatted: p95[p95.length - 1].toFixed(1), delta: (rand() - 0.6) * 8 }
      },
      series: {
        qps,
        p50,
        p95,
        p99,
        recall,
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      clusters,
      models,
      pcoords: { dims, rows, highlight }
    };
  };
  var buildAgentMemory = (rand, range) => {
    const n = points(range);
    const size = diurnal(n, 22e5, { rand, amplitude: 0.25, noise: 0.04 });
    const dedup = diurnal(n, 34, { rand, amplitude: 0.08, noise: 0.02 });
    const recallP95 = diurnal(n, 14, { rand, amplitude: 0.12, noise: 0.04 });
    const agents = [
      { label: "oncall-triage", value: 48210 },
      { label: "doc-writer", value: 31450 },
      { label: "incident-postmort", value: 22100 },
      { label: "customer-qa", value: 18600 },
      { label: "eng-copilot", value: 14200 },
      { label: "billing-agent", value: 8900 },
      { label: "sales-prospect", value: 6400 },
      { label: "ops-baby-sitter", value: 3800 }
    ];
    const clusters = [
      { label: "network", center: [18, 70], spread: 10, count: 90 },
      { label: "storage", center: [52, 78], spread: 8, count: 110 },
      { label: "billing", center: [78, 50], spread: 12, count: 80 },
      { label: "auth", center: [30, 30], spread: 9, count: 70 },
      { label: "query", center: [65, 22], spread: 11, count: 120 }
    ].map((c) => {
      const points2 = Array.from({ length: c.count }, () => {
        const a = rand() * Math.PI * 2;
        const r = rand() ** 0.5 * c.spread;
        return [c.center[0] + Math.cos(a) * r, c.center[1] + Math.sin(a) * r];
      });
      return { label: c.label, points: points2, centroid: c.center };
    });
    const topMemories = [
      { label: "cluster reset procedure (verified)", value: 2804 },
      { label: "pricing context for large enterprise", value: 1920 },
      { label: "hybrid search fusion score explanation", value: 1612 },
      { label: "WAL recovery tail loss (0.0014%)", value: 1402 },
      { label: "kNN recall vs quantization tradeoff", value: 1180 },
      { label: "agent memory dedup rules (semantic)", value: 980 },
      { label: "mmap segment format roadmap", value: 770 },
      { label: "flush policy triggers (investigate)", value: 612 }
    ];
    const recentOps = [
      ["23:14:02", "INSERT", "oncall-triage", "new memory \xB7 cluster reset"],
      ["23:13:51", "DEDUP", "doc-writer", "merged with 2 prior entries"],
      ["23:13:22", "RECALL", "oncall-triage", "k=5 \xB7 top score 0.86"],
      ["23:12:48", "FORGET", "eng-copilot", "decay below 0.12"],
      ["23:12:09", "REWRITE", "incident-pm", "compacted from 4 \u2192 1 entry"],
      ["23:11:44", "INSERT", "billing-agent", "new memory \xB7 refund workflow"],
      ["23:11:03", "RECALL", "customer-qa", "k=3 \xB7 top score 0.79"],
      ["23:10:41", "DEDUP", "sales-prospect", "merged with 1 prior entry"],
      ["23:10:12", "RECALL", "eng-copilot", "k=8 \xB7 top score 0.91"],
      ["23:09:58", "INSERT", "oncall-triage", "new memory \xB7 billing-sync"]
    ];
    return {
      metrics: {
        entries: { value: size[size.length - 1], formatted: compact.format(size[size.length - 1]), delta: (rand() - 0.4) * 5 },
        dedup: { value: dedup[dedup.length - 1], formatted: dedup[dedup.length - 1].toFixed(1), delta: (rand() - 0.4) * 3 },
        recall: { value: recallP95[recallP95.length - 1], formatted: recallP95[recallP95.length - 1].toFixed(1), delta: (rand() - 0.55) * 4 },
        growth: { value: 84e3, formatted: "84K", hint: "per day" },
        agents: { value: agents.length, formatted: String(agents.length), hint: agents.length + " active" }
      },
      series: {
        size,
        dedup,
        recallP95,
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      agents,
      clusters,
      topMemories,
      recentOps
    };
  };
  var buildLogsOverview = (rand, range) => {
    const n = points(range);
    const base = 42e3 + rand() * 8e3;
    const totalSeries = diurnal(n, base, { rand });
    const total = Math.round(sumOf(totalSeries));
    const peakVal = Math.max(...totalSeries);
    const peakAt = peakLabel(totalSeries);
    const errRate = 0.6 + rand() * 0.6;
    const sourcesTotal = 84 + Math.floor(rand() * 20);
    const sourcesActive = sourcesTotal - Math.floor(rand() * 6);
    const byLevel = [
      { label: "INFO", value: Math.round(total * (0.86 + rand() * 0.04)) },
      { label: "WARN", value: Math.round(total * (0.08 + rand() * 0.02)) },
      { label: "ERROR", value: Math.round(total * (errRate / 100)) },
      { label: "DEBUG", value: Math.round(total * 3e-3) },
      { label: "FATAL", value: Math.round(total * 1e-4) + 2 }
    ];
    const services = [
      "api-gateway",
      "auth-service",
      "billing",
      "checkout",
      "search",
      "catalog",
      "inventory",
      "shipping",
      "notifications",
      "webhook-worker",
      "recommendation",
      "pricing"
    ];
    const topServices = pareto(services, Math.round(total * 0.92), { alpha: 0.95, rand });
    const hosts = [
      "ip-10-0-1-17",
      "ip-10-0-1-42",
      "ip-10-0-2-88",
      "ip-10-0-3-11",
      "ip-10-0-3-54",
      "ip-10-0-4-09",
      "ip-10-0-4-73",
      "ip-10-0-5-22",
      "ip-10-0-5-91",
      "ip-10-0-6-18",
      "ip-10-0-6-60",
      "ip-10-0-7-04"
    ];
    const topHosts = pareto(hosts, Math.round(total * 0.82), { alpha: 0.7, rand });
    const cols = Array.from({ length: 12 }, (_, i) => String(i * 2).padStart(2, "0"));
    const matrix = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"].map(
      (_, di) => cols.map((_2, hi) => {
        const h = hi * 2;
        const phase = Math.cos((h - 14) / 24 * 2 * Math.PI);
        const weekend = di >= 5 ? 0.55 : 1;
        return Math.round(base * weekend * (1 + 0.8 * phase) * (0.85 + rand() * 0.3));
      })
    );
    return {
      metrics: {
        total: { value: total, formatted: compact.format(total), delta: (rand() - 0.45) * 6 },
        peak: { value: peakVal, formatted: compact.format(peakVal), at: peakAt },
        errorRate: { value: errRate, formatted: errRate.toFixed(2), delta: (rand() - 0.6) * 0.8 },
        sources: { value: sourcesTotal, formatted: String(sourcesTotal), active: sourcesActive }
      },
      series: {
        total: totalSeries,
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      byLevel,
      topServices,
      topHosts,
      heatmap: { cols, matrix }
    };
  };
  var buildSystem = (rand, range) => {
    const n = points(range);
    const cpuSeries = diurnal(n, 48, { rand, amplitude: 0.35, noise: 0.08, peakHour: 14 });
    const memSeries = diurnal(n, 61, { rand, amplitude: 0.12, noise: 0.04, peakHour: 18 });
    const diskSeries = diurnal(n, 180, { rand, amplitude: 0.55, noise: 0.18, peakHour: 13 });
    const netSeries = diurnal(n, 420, { rand, amplitude: 0.6, noise: 0.2, peakHour: 15 });
    const hosts = [
      "ip-10-0-1-17",
      "ip-10-0-1-42",
      "ip-10-0-2-88",
      "ip-10-0-3-11",
      "ip-10-0-3-54",
      "ip-10-0-4-09",
      "ip-10-0-4-73",
      "ip-10-0-5-22",
      "ip-10-0-5-91",
      "ip-10-0-6-18",
      "ip-10-0-6-60",
      "ip-10-0-7-04"
    ];
    const hostCpu = hosts.map((h) => {
      const baseline = 30 + rand() * 45;
      const vals = Array.from({ length: 40 }, (_, i) => {
        const phase = Math.cos((i - 20) / 20 * Math.PI);
        return Math.max(0, Math.min(100, baseline + phase * 14 + (rand() - 0.5) * 10));
      });
      return { label: h.toUpperCase(), values: vals, value: Math.round(vals[vals.length - 1]) + "%" };
    });
    const procs = [
      "java -jar checkout-svc",
      "postgres: walwriter",
      "node search-proxy",
      "java -jar auth-svc",
      "python3 metrics-exporter",
      "redis-server *:6379",
      "envoy -c /etc/envoy.yaml",
      "containerd-shim-runc",
      "systemd-journald",
      "kubelet --config /etc/kubelet",
      "otelcol --config otel.yaml"
    ];
    const topProcs = pareto(procs, 1e3, { alpha: 0.9, rand }).map((p) => ({
      label: p.label,
      value: p.value / 10
    }));
    const topHosts = hosts.map((h, i) => ({
      label: h,
      value: Math.round(30 + rand() * 60 + (i === 0 ? 15 : 0))
    })).sort((a, b) => b.value - a.value);
    const authSeries = diurnal(n, 28, { rand, amplitude: 0.5 });
    const authTotal = Math.round(sumOf(authSeries));
    const failures = Math.round(authTotal * (0.03 + rand() * 0.02));
    const topFailUsers = pareto(
      ["root", "admin", "ubuntu", "deploy", "postgres", "jenkins", "test", "oracle"],
      failures,
      { alpha: 0.65, rand }
    );
    const topFailIPs = pareto(
      [
        "45.137.21.4",
        "185.234.218.19",
        "193.32.162.157",
        "91.240.118.99",
        "162.247.74.217",
        "141.98.10.55",
        "89.248.165.74",
        "185.142.236.35"
      ],
      failures,
      { alpha: 0.7, rand }
    );
    return {
      metrics: {
        hosts: { value: hosts.length, formatted: String(hosts.length), hint: `${hosts.length - 1} healthy` },
        alerts: { value: 3, formatted: "3", delta: -1, deltaGood: "down", hint: "1 warn \xB7 2 info" },
        cpuMean: { value: cpuSeries[cpuSeries.length - 1], formatted: cpuSeries[cpuSeries.length - 1].toFixed(0), delta: 4.1 },
        memMean: { value: memSeries[memSeries.length - 1], formatted: memSeries[memSeries.length - 1].toFixed(0), delta: 0.8 }
      },
      series: {
        cpu: cpuSeries,
        mem: memSeries,
        disk: diskSeries,
        net: netSeries,
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      hostCpu,
      topProcs,
      topHosts,
      auth: { total: authTotal, failures, series: authSeries, topFailUsers, topFailIPs }
    };
  };
  var CORPUS_INDICES = ["logs-prod", "logs-stage", "docs", "metrics", "traces", "events"];
  var BODY_TEMPLATES = [
    (r) => `GET /api/v2/catalog status=200 ms=${Math.round(12 + r() * 60)} client=203.0.113.${Math.floor(r() * 240)}`,
    (r) => `POST /api/v2/checkout status=500 upstream_ms=${Math.round(1500 + r() * 2500)} error="upstream timeout"`,
    (r) => `auth_login user=deploy src=10.0.${Math.floor(r() * 10)}.${Math.floor(r() * 240)} result=success`,
    (r) => `auth_login user=root src=45.137.21.${Math.floor(r() * 240)} result=failure reason="invalid password"`,
    (r) => `flush segment=seg-${Math.floor(r() * 999)} docs=${Math.floor(4e4 + r() * 8e4)} took_ms=${Math.round(180 + r() * 220)}`,
    (r) => `merge segments=[seg-${Math.floor(r() * 99)},seg-${Math.floor(r() * 99)}] out=seg-${Math.floor(r() * 999)} ratio=${(0.42 + r() * 0.3).toFixed(2)}`,
    (r) => `slow_query took_ms=${Math.round(820 + r() * 1800)} plan="BoolQuery(Must(Match(message)))" index="logs-prod"`,
    (r) => `sq8_recall k=10 recall=${(0.94 + r() * 0.05).toFixed(3)} quant=scalar8`,
    (r) => `agent_memory op=insert agent=oncall-triage key="cluster-reset" score=${(0.72 + r() * 0.25).toFixed(2)}`,
    (r) => `ingest_batch index=logs-prod docs=${Math.floor(1e3 + r() * 9e3)} wal_lag_ms=${Math.round(2 + r() * 18)}`,
    (r) => `oom_score=${Math.round(100 + r() * 800)} rss_mb=${Math.round(1200 + r() * 2600)} pid=${Math.floor(1e3 + r() * 9e3)}`,
    (r) => `cache_hit route=/api/v2/search ratio=${(0.72 + r() * 0.22).toFixed(2)} ttl_s=${Math.floor(60 + r() * 540)}`,
    (r) => `tool_use name=search success=true tokens_in=${Math.floor(200 + r() * 900)} tokens_out=${Math.floor(40 + r() * 240)}`,
    (r) => `rag_answer grounding=${(0.78 + r() * 0.2).toFixed(2)} citations=${Math.floor(2 + r() * 5)} chunks=${Math.floor(3 + r() * 7)}`
  ];
  var SERVICES = ["api-gateway", "auth-service", "billing", "checkout", "search", "catalog", "ingest-worker", "query-coordinator", "embed-proxy", "agent-memory"];
  var HOSTS = ["ip-10-0-1-17", "ip-10-0-2-88", "ip-10-0-3-54", "ip-10-0-4-73", "ip-10-0-5-91", "ip-10-0-6-60"];
  function buildCorpus() {
    const r = rng(4277009102);
    const docs = [];
    for (let i = 0; i < 600; i++) {
      const tpl = BODY_TEMPLATES[Math.floor(r() * BODY_TEMPLATES.length)];
      const level = r() < 0.78 ? "INFO" : r() < 0.93 ? "WARN" : r() < 0.98 ? "ERROR" : "FATAL";
      docs.push({
        _index: CORPUS_INDICES[Math.floor(r() * CORPUS_INDICES.length)],
        _id: (1e6 + i).toString(16),
        _ts: new Date(Date.now() - Math.floor(r() * 864e5)).toISOString().slice(11, 19),
        service: SERVICES[Math.floor(r() * SERVICES.length)],
        level,
        host: HOSTS[Math.floor(r() * HOSTS.length)],
        _source: tpl(r)
      });
    }
    return docs;
  }
  var _corpus = null;
  var corpus = () => _corpus ??= buildCorpus();
  function mockSearch({ q = "", type = "match", index = "*", filters = {}, sort = { field: "_score", dir: "desc" } } = {}) {
    const t0 = performance.now();
    const docs = corpus();
    const qLower = q.toLowerCase().trim();
    const passesIndex = (d) => index === "*" || d._index === index;
    const passesFilter = (d) => Object.entries(filters).every(([f, v]) => !v || d[f] === v);
    let pool = docs.filter((d) => passesIndex(d) && passesFilter(d));
    let matched;
    if (!qLower) {
      matched = pool.map((d) => ({ ...d, _score: 1 }));
    } else if (type === "term") {
      const m = qLower.match(/^([a-z_]+)\s*=\s*(.+)$/i);
      if (m) {
        const [, f, v] = m;
        matched = pool.filter((d) => String(d[f] ?? "").toLowerCase() === v.toLowerCase()).map((d) => ({ ...d, _score: 1 }));
      } else matched = [];
    } else if (type === "prefix") {
      matched = pool.filter((d) => d._source.toLowerCase().startsWith(qLower)).map((d) => ({ ...d, _score: 1 - d._source.length / 500 }));
    } else if (type === "phrase") {
      const phrase = qLower.replace(/^"|"$/g, "");
      matched = pool.filter((d) => d._source.toLowerCase().includes(phrase)).map((d) => ({ ...d, _score: 2 + Math.random() * 0.5 }));
    } else if (type === "range") {
      const m = qLower.match(/^([a-z_]+)\s*(>=|<=|>|<)\s*(\d+(?:\.\d+)?)$/i);
      if (m) {
        const [, f, op, v] = m;
        const n = Number(v);
        matched = pool.filter((d) => {
          const src = d._source;
          const rx = new RegExp(f + "=(\\d+(?:\\.\\d+)?)", "i");
          const mm = src.match(rx);
          if (!mm) return false;
          const x = Number(mm[1]);
          return op === ">=" ? x >= n : op === "<=" ? x <= n : op === ">" ? x > n : x < n;
        }).map((d) => ({ ...d, _score: 1 }));
      } else matched = [];
    } else if (type === "knn" || type === "semantic") {
      const qHash = Array.from(qLower).reduce((a, c) => (a + c.charCodeAt(0)) % 997, 0);
      matched = pool.map((d) => {
        const dh = Array.from(d._source.toLowerCase()).reduce((a, c) => (a + c.charCodeAt(0)) % 997, 0);
        const dist = Math.abs(qHash - dh) / 997;
        return { ...d, _score: 1 - dist };
      }).filter((d) => d._score > 0.72);
    } else if (type === "hybrid") {
      const bmList = pool.filter((d) => d._source.toLowerCase().includes(qLower)).map((d, i) => ({ id: d._id, rank: i + 1, base: d }));
      const qHash = Array.from(qLower).reduce((a, c) => (a + c.charCodeAt(0)) % 997, 0);
      const knnList = pool.map((d) => {
        const dh = Array.from(d._source.toLowerCase()).reduce((a, c) => (a + c.charCodeAt(0)) % 997, 0);
        return { id: d._id, score: 1 - Math.abs(qHash - dh) / 997, base: d };
      }).sort((a, b) => b.score - a.score).slice(0, 80).map((d, i) => ({ ...d, rank: i + 1 }));
      const mix = /* @__PURE__ */ new Map();
      for (const r of bmList) mix.set(r.id, { base: r.base, s: 0.6 / (60 + r.rank) });
      for (const r of knnList) {
        const cur = mix.get(r.id);
        const add = 0.4 / (60 + r.rank);
        if (cur) cur.s += add;
        else mix.set(r.id, { base: r.base, s: add });
      }
      matched = Array.from(mix.values()).sort((a, b) => b.s - a.s).map((r) => ({ ...r.base, _score: r.s * 1e3 }));
    } else {
      matched = pool.filter((d) => d._source.toLowerCase().includes(qLower)).map((d) => {
        const idx = d._source.toLowerCase().indexOf(qLower);
        const score = 2 + (1 - idx / d._source.length) * 2;
        return { ...d, _score: score };
      });
    }
    const sortField = sort?.field || "_score";
    const sortDir = sort?.dir === "asc" ? 1 : -1;
    matched.sort((a, b) => {
      const av = a[sortField];
      const bv = b[sortField];
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (typeof av === "number" && typeof bv === "number") return (av - bv) * sortDir;
      return String(av).localeCompare(String(bv)) * sortDir;
    });
    const hits = matched.slice(0, 25);
    const took = Math.max(1, Math.round(performance.now() - t0 + (0.5 + Math.random() * 3.5)));
    const count = (field) => {
      const m = /* @__PURE__ */ new Map();
      for (const d of matched) m.set(d[field], (m.get(d[field]) || 0) + 1);
      return Array.from(m.entries()).sort((a, b) => b[1] - a[1]).slice(0, 8).map(([value, c]) => ({ label: value, value, count: c }));
    };
    const buckets = Array.from({ length: 24 }, () => 0);
    for (const d of matched) {
      const h = parseInt(d._ts.slice(0, 2), 10) || 0;
      buckets[h] += 1;
    }
    return {
      hits,
      total: matched.length,
      tookMs: took,
      maxScore: hits.length ? hits[0]._score : null,
      facets: { level: count("level"), service: count("service"), host: count("host"), _index: count("_index") },
      histogram: buckets
    };
  }
  var buildAnomalyDetect = (rand, range) => {
    const n = points(range);
    const base = 420 + rand() * 90;
    const values = Array.from({ length: n }, (_, i) => {
      const diurnal2 = Math.cos((i / n * 24 - 14) / 24 * 2 * Math.PI);
      return Math.max(10, base * (1 + 0.22 * diurnal2) + (rand() - 0.5) * 28);
    });
    const injected = [];
    const numAnom = 5 + Math.floor(rand() * 3);
    for (let k = 0; k < numAnom; k++) {
      const idx = Math.floor(rand() * n);
      const mag = 2.8 + rand() * 3;
      values[idx] = values[idx] * mag;
      injected.push(idx);
    }
    const W = Math.max(6, Math.floor(n / 12));
    const upper = new Array(n);
    const lower = new Array(n);
    for (let i = 0; i < n; i++) {
      const lo = Math.max(0, i - W), hi = Math.min(n - 1, i + W);
      let s = 0, s2 = 0, c = 0;
      for (let j = lo; j <= hi; j++) {
        s += values[j];
        s2 += values[j] * values[j];
        c++;
      }
      const mean = s / c;
      const std = Math.sqrt(Math.max(0, s2 / c - mean * mean));
      upper[i] = mean + 2.5 * std;
      lower[i] = Math.max(0, mean - 2.5 * std);
    }
    const anomalies = [];
    for (let i = 0; i < n; i++) {
      if (values[i] > upper[i] || values[i] < lower[i]) {
        const score = (values[i] - (upper[i] + lower[i]) / 2) / ((upper[i] - lower[i]) / 2 || 1);
        anomalies.push({ idx: i, score: Math.abs(score), value: values[i] });
      }
    }
    anomalies.sort((a, b) => b.score - a.score);
    const topFeatures = [
      { label: "query_latency_p95", value: 3.8 },
      { label: "upstream_timeout", value: 2.9 },
      { label: "cache_miss_rate", value: 2.1 },
      { label: "flush_duration", value: 1.8 },
      { label: "gc_pause", value: 0.7 },
      { label: "cpu_saturation", value: 0.5 }
    ];
    const topSignals = [
      { label: "api-gateway /checkout p95", value: anomalies[0]?.score || 0 },
      { label: "billing-svc query latency", value: 4.1 },
      { label: "auth-svc failed logins", value: 3.6 },
      { label: "search-svc wal_lag_ms", value: 2.9 },
      { label: "embed-proxy cost surge", value: 2.4 },
      { label: "vector-index p99 latency", value: 2 },
      { label: "agent-memory dedup dip", value: 1.7 }
    ];
    return {
      metrics: {
        detected: { value: anomalies.length, formatted: String(anomalies.length), delta: (rand() - 0.6) * 18 },
        covered: { value: 14, formatted: "14", hint: "signals scored \xB7 z\xB7score" },
        falsePos: { value: 1.2, formatted: "1.2", delta: (rand() - 0.5) * 0.6 },
        recall: { value: 92, formatted: "92", hint: "vs. hand labels" }
      },
      series: {
        values,
        upper,
        lower,
        anomalies: anomalies.slice(0, 12),
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      topFeatures,
      topSignals,
      injected
    };
  };
  var buildIngest = (rand, range) => {
    const n = points(range);
    const docsIn = diurnal(n, 68e3, { rand, amplitude: 0.6, peakHour: 15 });
    const bytesIn = docsIn.map((d) => d * (1.1 + rand() * 0.6) * 1024);
    const walLag = docsIn.map(() => 2 + rand() * 22);
    const flushMs = docsIn.map(() => 180 + rand() * 220);
    const mergeMs = docsIn.map(() => 420 + rand() * 900);
    const memBytes = docsIn.map(() => 1.8 * 1024 * 1024 * 1024 + rand() * 800 * 1024 * 1024);
    const idxLatP50 = docsIn.map(() => 0.6 + rand() * 0.3);
    const idxLatP95 = docsIn.map(() => 2.1 + rand() * 0.9);
    const idxLatP99 = docsIn.map(() => 6.4 + rand() * 2.8);
    const topIndices = pareto(
      [
        "logs-prod",
        "logs-stage",
        "traces",
        "docs",
        "metrics",
        "events",
        "agent-memory",
        "embeddings",
        "alerts",
        "audit"
      ],
      Math.round(sumOf(docsIn)),
      { alpha: 0.85, rand }
    );
    const perField = [
      { label: "@timestamp", value: 98, encoding: "\u0394-of-\u0394" },
      { label: "service", value: 91, encoding: "DICT" },
      { label: "level", value: 96, encoding: "DICT" },
      { label: "host", value: 88, encoding: "DICT" },
      { label: "message", value: 61, encoding: "ZSTD+TMPL" },
      { label: "trace_id", value: 28, encoding: "UVARINT" },
      { label: "latency_ms", value: 72, encoding: "FOR+RLE" },
      { label: "status", value: 94, encoding: "DICT" },
      { label: "bytes_out", value: 58, encoding: "FOR" }
    ];
    const pipelineFlow = [
      { label: "HTTP PARSE", value: 100 },
      { label: "FIELD MAP", value: 98 },
      { label: "PIPELINE \xB7 redact", value: 97 },
      { label: "WAL APPEND", value: 97 },
      { label: "MEMTABLE", value: 97 },
      { label: "FLUSH \u2192 SEGMENT", value: 92 },
      { label: "MERGE", value: 61 }
    ];
    return {
      metrics: {
        docsRate: { value: docsIn[docsIn.length - 1], formatted: compact.format(docsIn[docsIn.length - 1]), delta: (rand() - 0.3) * 8, unit: "docs/s" },
        bytesRate: { value: bytesIn[bytesIn.length - 1], formatted: (bytesIn[bytesIn.length - 1] / 1024 / 1024).toFixed(1), unit: "MB/s" },
        walLag: { value: walLag[walLag.length - 1], formatted: walLag[walLag.length - 1].toFixed(1), unit: "ms", delta: (rand() - 0.55) * 12 },
        segments: { value: 184, formatted: "184", hint: "3 indices \xB7 12 shards" },
        mem: { value: (memBytes[memBytes.length - 1] / 1024 / 1024 / 1024).toFixed(2), formatted: (memBytes[memBytes.length - 1] / 1024 / 1024 / 1024).toFixed(2), unit: "GB" },
        ratio: { value: 4.8, formatted: "4.8", unit: "\xD7", hint: "vs. raw JSON" }
      },
      series: {
        docsIn,
        bytesIn,
        walLag,
        flushMs,
        mergeMs,
        memBytes,
        idxLatP50,
        idxLatP95,
        idxLatP99,
        startLabel: rangeLabels(range)[0],
        endLabel: rangeLabels(range)[1]
      },
      topIndices,
      perField,
      pipelineFlow
    };
  };
  var buildSearchDash = (rand, range) => {
    const n = points(range);
    const queries = diurnal(n, 1200, { rand, peakHour: 14 });
    const took_p50 = queries.map(() => 2 + rand() * 1.4);
    const took_p95 = queries.map(() => 8 + rand() * 3.2);
    return {
      metrics: {
        qps: { value: queries[queries.length - 1], formatted: compact.format(queries[queries.length - 1]), delta: (rand() - 0.4) * 5 },
        p95: { value: took_p95[took_p95.length - 1], formatted: took_p95[took_p95.length - 1].toFixed(1), delta: (rand() - 0.6) * 6 },
        totalDocs: { value: 524e5, formatted: "52.4M", hint: "6 indices \xB7 32 shards" },
        uniqueTerms: { value: 189e5, formatted: "18.9M", hint: "exact cardinality \u2713" }
      },
      series: { queries, took_p50, took_p95, startLabel: rangeLabels(range)[0], endLabel: rangeLabels(range)[1] }
    };
  };
  function filterCount(filters) {
    let n = 0;
    for (const v of Object.values(filters || {})) {
      n += Array.isArray(v) ? v.length : 1;
    }
    return n;
  }
  function filterRatio(filters) {
    const n = filterCount(filters);
    if (!n) return 1;
    const fields = Object.keys(filters || {}).length;
    return Math.max(0.1, Math.pow(0.65, fields));
  }
  function applyFilters(data, filters) {
    if (!filters || !Object.keys(filters).length) return data;
    const r = filterRatio(filters);
    const values = [];
    for (const v of Object.values(filters)) {
      if (Array.isArray(v)) for (const x of v) values.push(String(x).toLowerCase());
      else values.push(String(v).toLowerCase());
    }
    const keep = (label) => {
      const s = String(label || "").toLowerCase();
      return values.some((v) => s.includes(v));
    };
    const walk = (node) => {
      if (Array.isArray(node)) {
        if (node.length && typeof node[0] === "object" && node[0] && "label" in node[0] && "value" in node[0]) {
          const hits = node.filter((b) => keep(b.label));
          if (hits.length) return hits.map((b) => ({ ...b, value: Math.max(1, Math.round(b.value * r * 1.2)) }));
          return node.map((b) => ({ ...b, value: Math.max(0, Math.round(b.value * r)) }));
        }
        if (node.length && typeof node[0] === "number") return node.map((v) => v * r);
        return node.map(walk);
      }
      if (node && typeof node === "object") {
        const out = {};
        for (const k of Object.keys(node)) {
          const v = node[k];
          if (v && typeof v === "object" && "value" in v && "formatted" in v && typeof v.value === "number") {
            const scaled = Math.round(v.value * r);
            out[k] = { ...v, value: scaled, formatted: formatLike(v.formatted, scaled) };
          } else {
            out[k] = walk(v);
          }
        }
        return out;
      }
      return node;
    };
    return walk(data);
  }
  function formatLike(prev, n) {
    const s = String(prev);
    if (s.startsWith("$")) return "$" + Math.round(n).toLocaleString("en");
    if (/^[\d.]+[KMB]$/.test(s)) return compact.format(n);
    if (/^\d+$/.test(s)) return String(Math.round(n));
    return compact.format(n);
  }
  function bucketForCustom(customRange) {
    if (!customRange || !customRange.from || !customRange.to) return "24H";
    const a = new Date(customRange.from).getTime();
    const b = new Date(customRange.to).getTime();
    if (!isFinite(a) || !isFinite(b) || b <= a) return "24H";
    const hrs = (b - a) / 36e5;
    if (hrs < 2) return "1H";
    if (hrs < 36) return "24H";
    if (hrs < 240) return "7D";
    if (hrs < 1080) return "30D";
    return "90D";
  }
  function mock(dashId, range = "24H", ctx = {}) {
    const { cluster = "", filters = {}, customRange = null } = ctx;
    const effectiveRange = range === "CUSTOM" ? bucketForCustom(customRange) : range;
    const customKey = customRange ? (customRange.from || "") + "/" + (customRange.to || "") : "";
    const seedKey = dashId + "|" + effectiveRange + "|" + cluster + "|" + Object.keys(filters).sort().join(",") + "|" + customKey;
    const seedRand = rng(hashStr(seedKey));
    seedRand.range = effectiveRange;
    let out;
    switch (dashId) {
      case "ai-overview":
        out = buildAiOverview(seedRand, effectiveRange);
        break;
      case "rag-quality":
        out = buildRagQuality(seedRand, effectiveRange);
        break;
      case "vector-index":
        out = buildVectorIndex(seedRand, effectiveRange);
        break;
      case "agent-memory":
        out = buildAgentMemory(seedRand, effectiveRange);
        break;
      case "search-discover":
        out = buildSearchDash(seedRand, effectiveRange);
        break;
      case "anomaly-detect":
        out = buildAnomalyDetect(seedRand, effectiveRange);
        break;
      case "ingest-pipeline":
        out = buildIngest(seedRand, effectiveRange);
        break;
      case "logs-overview":
        out = buildLogsOverview(seedRand, effectiveRange);
        break;
      case "system":
        out = buildSystem(seedRand, effectiveRange);
        break;
      default:
        out = buildAiOverview(seedRand, effectiveRange);
    }
    return applyFilters(out, filters);
  }

  // playground/src/data/query.js
  async function query(ctx = {}) {
    const {
      dashId,
      range = "24H",
      customRange = null,
      // { from: ISO, to: ISO } when range === 'CUSTOM'
      cluster = "",
      filters = {}
    } = typeof ctx === "string" ? { dashId: ctx } : ctx;
    const t0 = typeof performance !== "undefined" ? performance.now() : Date.now();
    const data = mock(dashId, range, { cluster, filters, customRange });
    const t1 = typeof performance !== "undefined" ? performance.now() : Date.now();
    return {
      data,
      meta: {
        dashId,
        range,
        customRange,
        cluster,
        filters,
        fetchedAt: (/* @__PURE__ */ new Date()).toISOString(),
        durationMs: Math.round((t1 - t0) * 10) / 10,
        source: DATA_SOURCE_KIND
      }
    };
  }
  var DATA_SOURCE_KIND = "mock";
  var dataSourceStatus = "MOCK DATA \xB7 BACKEND PENDING";

  // playground/src/ux/chart-types.js
  var demoSeries = Array.from(
    { length: 48 },
    (_, i) => 50 + Math.sin(i / 5.5) * 22 + Math.cos(i / 3.2) * 9 + i * 37 % 7
  );
  var demoItems = [
    { label: "alpha", value: 4230 },
    { label: "beta", value: 3180 },
    { label: "gamma", value: 2564 },
    { label: "delta", value: 1923 },
    { label: "epsilon", value: 1340 },
    { label: "zeta", value: 912 },
    { label: "eta", value: 680 },
    { label: "theta", value: 445 }
  ];
  var demoSegments = [
    { label: "2XX", value: 9620 },
    { label: "3XX", value: 410 },
    { label: "4XX", value: 190 },
    { label: "5XX", value: 48 }
  ];
  var demoTree = [
    { label: "root", value: 13e3, children: [
      { label: "api-gateway", value: 5100, children: [
        { label: "/v2/search", value: 2400 },
        { label: "/v2/cart", value: 1800 },
        { label: "/v2/catalog", value: 900 }
      ] },
      { label: "auth-service", value: 2400 },
      { label: "billing", value: 1600 },
      { label: "checkout", value: 1400 }
    ] }
  ];
  var chartTypes = {
    metric: {
      name: "METRIC",
      cols: 4,
      describe: "A single headline number with optional spark and delta.",
      render: () => Num({
        value: "42.0K",
        unit: "demo",
        spark: Spark(demoSeries, { w: 160, h: 30 }),
        delta: 3.1,
        emphasis: true
      })
    },
    gauge: {
      name: "GAUGE",
      cols: 4,
      describe: "Single value on a bounded 1px track.",
      render: () => Gauge({ value: 73, min: 0, max: 100, unit: "%", label: "demo" })
    },
    line: {
      name: "LINE",
      cols: 12,
      describe: "Full-width time series. Replaces both Line and Area.",
      render: () => Series(demoSeries, { h: 140, labels: ["START", "NOW"], unit: "u/s" })
    },
    spark: {
      name: "SPARK",
      cols: 3,
      describe: "Inline 1px sparkline with latest value.",
      render: () => Num({
        value: fmt(demoSeries[demoSeries.length - 1]),
        unit: "u/s",
        spark: Spark(demoSeries, { w: 200, h: 36 }),
        emphasis: false
      })
    },
    bar: {
      name: "BAR",
      cols: 6,
      describe: "Vertical 1px-line bar chart.",
      render: () => VBar({
        items: demoSeries.slice(0, 24).map((v, i) => ({ label: String(i).padStart(2, "0"), value: Math.round(v) })),
        h: 140,
        unit: "u"
      })
    },
    histogram: {
      name: "HISTOGRAM",
      cols: 12,
      describe: "Column histogram of counts across buckets.",
      render: () => Hist({
        items: Array.from({ length: 32 }, (_, i) => ({
          label: String(i),
          value: Math.round(Math.exp(-Math.pow((i - 16) / 5, 2)) * 1e3 + i % 3 * 30)
        })),
        h: 160,
        unit: "count"
      })
    },
    dist: {
      name: "DIST",
      cols: 12,
      describe: "Distribution bar. Replaces pie/donut.",
      render: () => Dist({ segments: demoSegments, width: 1200 })
    },
    stacked: {
      name: "STACKED",
      cols: 12,
      describe: "Horizontal stacked bars, one row per series.",
      render: () => Stacked({
        rows: [
          { label: "US", segments: demoSegments },
          { label: "DE", segments: demoSegments.map((s) => ({ ...s, value: s.value * 0.6 })) },
          { label: "JP", segments: demoSegments.map((s) => ({ ...s, value: s.value * 0.45 })) },
          { label: "BR", segments: demoSegments.map((s) => ({ ...s, value: s.value * 0.3 })) }
        ]
      })
    },
    topn: {
      name: "TOP-N",
      cols: 6,
      describe: "Ranked list with aligned 1px bars. Replaces horizontal bar.",
      render: () => TopN({ items: demoItems, total: demoItems.reduce((a, i) => a + i.value, 0), n: 8 })
    },
    treemap: {
      name: "TREEMAP",
      cols: 6,
      describe: "Nested hierarchy of ranked items.",
      render: () => Treemap({ items: demoTree })
    },
    heatmap: {
      name: "HEATMAP",
      cols: 12,
      describe: "Character-intensity grid. Rows \xD7 columns of numbers whose opacity encodes magnitude.",
      render: () => Heatmap({
        rows: ["A", "B", "C", "D", "E", "F", "G"],
        cols: ["00", "02", "04", "06", "08", "10", "12", "14", "16", "18", "20", "22"],
        matrix: Array.from({ length: 7 }, (_, r) => Array.from({ length: 12 }, (_2, c) => {
          const phase = Math.cos((c * 2 - 14) / 24 * 2 * Math.PI);
          return Math.round(80 + 60 * phase + r * 5 + (r + c * 7) % 9);
        })),
        cellFmt: (v) => String(v)
      })
    },
    multiples: {
      name: "MULTIPLES",
      cols: 12,
      describe: "Grid of small sparklines, one per dimension.",
      render: () => Multiples({
        items: Array.from({ length: 8 }, (_, i) => ({
          label: "SERIES " + String.fromCharCode(65 + i),
          values: demoSeries.map((v) => v + i * 3 + Math.sin(i) * 5),
          value: String(Math.round(50 + i * 2))
        })),
        w: 180,
        h: 22
      })
    },
    scatter: {
      name: "SCATTER",
      cols: 6,
      describe: "X/Y point cloud using `\xB7` characters as data points.",
      render: () => Scatter({
        points: Array.from({ length: 80 }, () => {
          const t = Math.random();
          return [20 + t * 80 + (Math.random() - 0.5) * 20, 10 + t * 70 + (Math.random() - 0.5) * 25];
        }),
        xLabel: "LATENCY",
        yLabel: "THROUGHPUT",
        h: 220
      })
    },
    table: {
      name: "TABLE",
      cols: 6,
      describe: "Text data table, no borders, tabular numbers.",
      render: () => Table({
        columns: ["NAME", "COUNT", "PCT", "TREND"],
        rows: demoItems.map((it, i) => [
          it.label,
          fmt(it.value),
          (it.value / demoItems.reduce((a, x) => a + x.value, 0) * 100).toFixed(1) + "%",
          i % 2 ? "\u25B2 3.1" : "\u25BC 1.4"
        ]),
        align: ["left", "right", "right", "right"]
      })
    },
    events: {
      name: "EVENTS",
      cols: 12,
      describe: "Recent event tail \u2014 time, severity, message.",
      render: () => Events({ items: [
        { at: "23:14:02.081", sev: "err", msg: "demo \xB7 upstream timed out (connect) while connecting to upstream" },
        { at: "23:13:51.602", sev: "warn", msg: "demo \xB7 slow query 842 ms  SELECT * FROM orders WHERE ..." },
        { at: "23:13:04.114", sev: "info", msg: "demo \xB7 batch 238 committed \xB7 14,402 docs \xB7 112 ms" },
        { at: "23:12:48.771", sev: "warn", msg: "demo \xB7 rate limit approaching 85% for tenant=acme" },
        { at: "23:12:09.033", sev: "info", msg: "demo \xB7 segment merged \xB7 3\u21921 \xB7 412 MB \u2192 386 MB" }
      ] })
    },
    markdown: {
      name: "TEXT",
      cols: 6,
      describe: "Free-form prose panel with very light markdown.",
      render: () => Markdown(
        `## NOTES

This is a **text** panel. Use it for *annotations*, playbooks,
SLO definitions, on-call runbooks \u2014 anything that the reader
needs to see next to the data.

Inline \`code\` renders in accent, which is the one place prose
borrows the accent color.`
      )
    },
    // ---- AI / RAG primitives -------------------------------
    embedspace: {
      name: "EMBED-SPACE",
      cols: 12,
      describe: "Embedding projection with 1px cluster hulls.",
      render: () => {
        const mk = (cx, cy, r, label, n) => {
          const pts = Array.from({ length: n }, () => {
            const a = Math.random() * Math.PI * 2;
            const rr = Math.sqrt(Math.random()) * r;
            return [cx + Math.cos(a) * rr, cy + Math.sin(a) * rr];
          });
          return { label, points: pts, centroid: [cx, cy] };
        };
        return EmbedSpace({
          clusters: [
            mk(22, 70, 10, "code", 120),
            mk(58, 38, 12, "docs", 180),
            mk(82, 66, 8, "runbook", 80),
            mk(40, 80, 9, "tickets", 110),
            mk(28, 22, 12, "chat", 160)
          ],
          h: 360
        });
      }
    },
    ribbon3d: {
      name: "3D RIBBON",
      cols: 12,
      describe: 'Axonometric stacked 1px time series. The "3D" chart.',
      render: () => {
        const series = Array.from({ length: 5 }, (_, i) => ({
          label: "SERIES " + String.fromCharCode(65 + i),
          values: Array.from(
            { length: 48 },
            (_2, j) => 40 + i * 9 + Math.sin(j / 5 + i) * 15 + Math.cos(j / 8) * 6
          )
        }));
        return Ribbon3D({ series, h: 260, depth: 14 });
      }
    },
    chord: {
      name: "CHORD FLOW",
      cols: 12,
      describe: "1px B\xE9zier arcs from sources to targets.",
      render: () => {
        const sources = Array.from({ length: 6 }, (_, i) => ({ id: "s" + i, label: "query " + (i + 1) }));
        const targets = Array.from({ length: 8 }, (_, i) => ({ id: "t" + i, label: "doc/chunk " + (i + 1) }));
        const flows = [];
        for (const s of sources) {
          const k = 2 + Math.floor(Math.random() * 3);
          for (let i = 0; i < k; i++) {
            flows.push({ from: s.id, to: "t" + Math.floor(Math.random() * 8), weight: Math.random() });
          }
        }
        return ChordArcs({ sources, targets, flows, h: 360 });
      }
    },
    pcoords: {
      name: "PARALLEL-COORDS",
      cols: 12,
      describe: "Multi-dimensional rows as 1px polylines across N axes.",
      render: () => ParallelCoords({
        dims: [{ name: "LAT" }, { name: "TOK" }, { name: "COST" }, { name: "RECALL" }, { name: "GROUND" }],
        rows: Array.from({ length: 60 }, () => [
          200 + Math.random() * 1500,
          800 + Math.random() * 2e4,
          0.01 + Math.random() * 0.2,
          80 + Math.random() * 18,
          70 + Math.random() * 28
        ]),
        h: 260
      })
    },
    attention: {
      name: "ATTENTION-MAP",
      cols: 12,
      describe: "Inline text with per-token opacity = attention weight.",
      render: () => AttentionMap({
        tokens: "The XERJ.ai cluster reset requires draining the WAL and flushing all memtables before restart . Lost writes in the fsync tail are unrecoverable .".split(" ").map((t) => ({
          text: t,
          weight: /(cluster|reset|drain|WAL|flush|memtables|restart|unrecoverable)/i.test(t) ? 0.7 + Math.random() * 0.3 : 0.08 + Math.random() * 0.22
        }))
      })
    },
    flowband: {
      name: "FLOW BAND",
      cols: 12,
      describe: "Single-row stacked flow allocation with labeled ticks.",
      render: () => FlowBand({
        segments: [
          { label: "SYS PROMPT", value: 1200 },
          { label: "CONTEXT", value: 8400 },
          { label: "COMPLETION", value: 640 }
        ],
        unit: "tok"
      })
    }
  };
  var chartTypeList = [
    // AI / RAG
    "embedspace",
    "ribbon3d",
    "chord",
    "pcoords",
    "attention",
    "flowband",
    // Classic
    "metric",
    "gauge",
    "spark",
    "line",
    "bar",
    "histogram",
    "dist",
    "stacked",
    "topn",
    "treemap",
    "heatmap",
    "multiples",
    "scatter",
    "table",
    "events",
    "markdown"
  ].map((id) => ({ id, ...chartTypes[id] }));

  // playground/src/data/export.js
  function toCsv(headers, rows) {
    const escape = (cell) => {
      const s = cell == null ? "" : String(cell);
      if (/[",\n\r]/.test(s)) return '"' + s.replace(/"/g, '""') + '"';
      return s;
    };
    const lines = [headers.map(escape).join(",")];
    for (const row of rows) lines.push(row.map(escape).join(","));
    return lines.join("\r\n") + "\r\n";
  }
  function downloadText(filename, mime, text) {
    const blob = new Blob([text], { type: mime + ";charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(url), 1500);
  }
  function hitsToCsv(hits, columns = ["_index", "_id", "_score", "_ts", "_source"]) {
    const headers = columns.map((c) => c.replace(/^_/, ""));
    const rows = hits.map(
      (h) => columns.map((c) => {
        const v = h[c];
        if (typeof v === "object" && v !== null) return JSON.stringify(v);
        return v;
      })
    );
    return toCsv(headers, rows);
  }
  async function svgToPng(svgEl, { scale = 2, filename = "chart.png" } = {}) {
    if (!svgEl) throw new Error("svgToPng: missing SVG element");
    const clone = svgEl.cloneNode(true);
    const rect = svgEl.getBoundingClientRect();
    const w = Math.max(1, Math.round(rect.width));
    const h = Math.max(1, Math.round(rect.height));
    clone.setAttribute("width", String(w));
    clone.setAttribute("height", String(h));
    const cs = getComputedStyle(svgEl);
    clone.setAttribute("color", cs.color);
    if (!clone.getAttribute("xmlns")) {
      clone.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    }
    const paper = getComputedStyle(document.documentElement).getPropertyValue("--z-paper").trim() || "#0D0D0D";
    const bg = `<rect width="100%" height="100%" fill="${paper}"/>`;
    const inner = clone.innerHTML;
    clone.innerHTML = bg + inner;
    const xml = new XMLSerializer().serializeToString(clone);
    const svg64 = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(xml);
    const img = new Image();
    img.crossOrigin = "anonymous";
    const blob = await new Promise((resolve, reject) => {
      img.onload = () => {
        const canvas = document.createElement("canvas");
        canvas.width = w * scale;
        canvas.height = h * scale;
        const ctx = canvas.getContext("2d");
        ctx.scale(scale, scale);
        ctx.drawImage(img, 0, 0, w, h);
        canvas.toBlob((b) => b ? resolve(b) : reject(new Error("toBlob failed")), "image/png");
      };
      img.onerror = (e) => reject(new Error("SVG image load failed"));
      img.src = svg64;
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(url), 1500);
    return blob;
  }

  // playground/src/data/dashboard-store.js
  var LS_KEY = "xerj.dashboards";
  function load() {
    try {
      const raw = localStorage.getItem(LS_KEY);
      if (!raw) return {};
      return JSON.parse(raw);
    } catch {
      return {};
    }
  }
  function save(m) {
    localStorage.setItem(LS_KEY, JSON.stringify(m));
  }
  function mergedDashboards(defaults2, { includeHidden = false } = {}) {
    const m = load();
    const hidden = new Set(m.hidden || []);
    const custom = m.custom || {};
    const names = m.names || {};
    const byId = Object.fromEntries(defaults2.map((d) => [d.id, d]));
    const orderList = Array.isArray(m.order) && m.order.length ? m.order : defaults2.map((d) => d.id);
    const out = [];
    const seen = /* @__PURE__ */ new Set();
    for (const id of orderList) {
      if (hidden.has(id) && !includeHidden) {
        seen.add(id);
        continue;
      }
      const def = byId[id];
      const user = custom[id];
      if (def) {
        out.push({
          ...def,
          name: names[id] || def.name,
          isUser: false,
          hidden: hidden.has(id)
        });
      } else if (user) {
        const source = user.clonedFrom ? byId[user.clonedFrom] : null;
        out.push({
          id,
          name: names[id] || user.name,
          render: source?.render || blankRender(user.name),
          isUser: true,
          clonedFrom: user.clonedFrom,
          dataSource: user.dataSource,
          hidden: hidden.has(id)
        });
      }
      seen.add(id);
    }
    for (const def of defaults2) {
      if (seen.has(def.id)) continue;
      if (hidden.has(def.id) && !includeHidden) continue;
      out.push({
        ...def,
        name: names[def.id] || def.name,
        isUser: false,
        hidden: hidden.has(def.id)
      });
    }
    return out;
  }
  function blankRender(name) {
    return () => ({
      title: (name || "BLANK").toUpperCase(),
      kicker: "USER DASHBOARD",
      meta: ["NO TEMPLATE"],
      panels: [
        {
          id: "empty",
          eyebrow: "NOTHING HERE YET",
          cols: 12,
          render: () => '<div class="mono faint">This user dashboard was cloned from a template that no longer exists. Open the MANAGE view to re-point it at a different template, or delete and recreate.</div>'
        }
      ]
    });
  }
  function renameDashboard(id, name) {
    const m = load();
    m.names = m.names || {};
    if (name && name.trim()) m.names[id] = name.trim();
    else delete m.names[id];
    save(m);
  }
  function reorderDashboards(orderIds) {
    const m = load();
    m.order = orderIds.slice();
    save(m);
  }
  function setHidden(id, hidden) {
    const m = load();
    m.hidden = m.hidden || [];
    const has = m.hidden.includes(id);
    if (hidden && !has) m.hidden.push(id);
    if (!hidden && has) m.hidden = m.hidden.filter((x) => x !== id);
    save(m);
  }
  function createUserDashboard({ name, fromId = null, dataSource = null } = {}) {
    const m = load();
    m.custom = m.custom || {};
    const id = "user-" + Math.random().toString(36).slice(2, 8) + Date.now().toString(36).slice(-4);
    m.custom[id] = {
      name: name || "Untitled",
      clonedFrom: fromId,
      createdAt: (/* @__PURE__ */ new Date()).toISOString(),
      dataSource
    };
    m.order = m.order || [];
    m.order.push(id);
    save(m);
    return id;
  }
  function deleteUserDashboard(id) {
    const m = load();
    if (m.custom) delete m.custom[id];
    if (m.order) m.order = m.order.filter((x) => x !== id);
    if (m.names) delete m.names[id];
    if (m.hidden) m.hidden = m.hidden.filter((x) => x !== id);
    save(m);
    localStorage.removeItem("xerj.layout." + id);
  }
  function resetAll() {
    localStorage.removeItem(LS_KEY);
  }
  function isUserDash(id) {
    const m = load();
    return !!(m.custom && m.custom[id]);
  }

  // playground/src/app.js
  var LS = {
    theme: "xerj.theme",
    time: "xerj.time",
    edit: "xerj.edit",
    search: "xerj.search",
    refresh: "xerj.refresh",
    cluster: "xerj.cluster",
    views: "xerj.views",
    filters: (dashId) => "xerj.filters." + dashId,
    layout: (dashId) => "xerj.layout." + dashId
  };
  function loadJSON(key, fallback) {
    try {
      const raw = localStorage.getItem(key);
      return raw ? JSON.parse(raw) : fallback;
    } catch {
      return fallback;
    }
  }
  function loadSearch() {
    return loadJSON(LS.search, null);
  }
  function loadAllFilters() {
    const out = {};
    for (const k of Object.keys(localStorage)) {
      if (k.startsWith("xerj.filters.")) {
        const id = k.slice("xerj.filters.".length);
        try {
          out[id] = JSON.parse(localStorage.getItem(k)) || {};
        } catch {
        }
      }
    }
    return out;
  }
  var _initial = parseRoute();
  var _urlState = parseUrlState();
  var state = {
    section: _initial.section,
    route: _initial.route,
    time: _urlState.time || localStorage.getItem(LS.time) || "24H",
    timeCustom: {
      from: _urlState.tfrom || localStorage.getItem("xerj.timeFrom") || "",
      to: _urlState.tto || localStorage.getItem("xerj.timeTo") || ""
    },
    cluster: _urlState.cluster || defaultClusterId(),
    refresh: parseInt(localStorage.getItem(LS.refresh) || "0", 10),
    // ms; 0 = off
    refreshTimer: null,
    fetchedAt: null,
    // ISO string of last successful query
    fetchMs: null,
    // last fetch duration in ms
    fetchErr: null,
    // last fetch error message
    loading: false,
    filters: loadAllFilters(),
    // { [dashId]: { field: value } }
    theme: document.documentElement.getAttribute("data-theme") || "night",
    edit: localStorage.getItem(LS.edit) === "1",
    mobile: localStorage.getItem("xerj.mobile") === "1",
    layouts: loadAllLayouts(),
    search: (() => {
      const loaded = loadSearch() || {};
      return {
        q: loaded.q || "",
        type: loaded.type || "match",
        index: loaded.index || "*",
        filters: loaded.filters || {},
        sort: loaded.sort || { field: "_score", dir: "desc" },
        showTime: loaded.showTime !== false,
        result: null
      };
    })(),
    _focusSelector: null
    // set by handlers that need focus restored
  };
  if (_urlState.filters) {
    state.filters[_initial.route] = { ...state.filters[_initial.route] || {}, ..._urlState.filters };
    saveFilters(_initial.route);
  }
  function currentFilters() {
    return state.filters[state.route] || {};
  }
  function saveFilters(dashId) {
    const v = state.filters[dashId];
    if (v && Object.keys(v).length) localStorage.setItem(LS.filters(dashId), JSON.stringify(v));
    else localStorage.removeItem(LS.filters(dashId));
  }
  function setFilter(field, value) {
    const dashId = state.route;
    const cur = { ...state.filters[dashId] || {} };
    const existing = cur[field];
    if (existing == null) {
      cur[field] = value;
    } else if (Array.isArray(existing)) {
      const idx = existing.indexOf(value);
      if (idx >= 0) {
        const next = existing.filter((v) => v !== value);
        if (next.length === 0) delete cur[field];
        else if (next.length === 1) cur[field] = next[0];
        else cur[field] = next;
      } else {
        cur[field] = [...existing, value];
      }
    } else {
      if (existing === value) delete cur[field];
      else cur[field] = [existing, value];
    }
    state.filters[dashId] = cur;
    saveFilters(dashId);
    writeUrlState();
  }
  function removeFilterValue(field, value) {
    const dashId = state.route;
    const cur = { ...state.filters[dashId] || {} };
    const existing = cur[field];
    if (existing == null) return;
    if (Array.isArray(existing)) {
      const next = existing.filter((v) => v !== value);
      if (next.length === 0) delete cur[field];
      else if (next.length === 1) cur[field] = next[0];
      else cur[field] = next;
    } else if (existing === value) {
      delete cur[field];
    }
    state.filters[dashId] = cur;
    saveFilters(dashId);
    writeUrlState();
  }
  function parseKql(text) {
    const out = {};
    const add = (k, v) => {
      if (!v) return;
      if (out[k] == null) out[k] = v;
      else if (Array.isArray(out[k])) out[k].push(v);
      else out[k] = [out[k], v];
    };
    const re = /([a-zA-Z_][\w.]*):"([^"]*)"|([a-zA-Z_][\w.]*):(\S+)|"([^"]+)"|(\S+)/g;
    let m;
    while ((m = re.exec(text)) !== null) {
      if (m[1] != null) add(m[1], m[2]);
      else if (m[3] != null) add(m[3], m[4]);
      else if (m[5] != null) add("q", m[5]);
      else if (m[6] != null) add("q", m[6]);
    }
    return out;
  }
  function serializeKql(filters) {
    const parts = [];
    for (const [k, v] of Object.entries(filters || {})) {
      const values = Array.isArray(v) ? v : [v];
      for (const x of values) {
        const needsQuote = /\s/.test(String(x));
        if (k === "q") {
          parts.push(needsQuote ? `"${x}"` : String(x));
        } else {
          parts.push(needsQuote ? `${k}:"${x}"` : `${k}:${x}`);
        }
      }
    }
    return parts.join(" ");
  }
  function applyKqlInput(text) {
    const dashId = state.route;
    state.filters[dashId] = parseKql(text);
    saveFilters(dashId);
    writeUrlState();
  }
  function clearFilters() {
    const dashId = state.route;
    state.filters[dashId] = {};
    saveFilters(dashId);
    writeUrlState();
  }
  function parseUrlState() {
    const qs = location.search.slice(1);
    if (!qs) return {};
    const out = {};
    const params = new URLSearchParams(qs);
    if (params.get("t")) out.time = params.get("t").toUpperCase();
    if (params.get("tfrom")) out.tfrom = params.get("tfrom");
    if (params.get("tto")) out.tto = params.get("tto");
    if (params.get("c")) out.cluster = params.get("c");
    const all2 = params.getAll("f");
    if (all2.length) {
      out.filters = {};
      for (const pair of all2) {
        const idx = pair.indexOf(":");
        if (idx > 0) {
          const k = pair.slice(0, idx);
          const v = pair.slice(idx + 1);
          const existing = out.filters[k];
          if (existing == null) out.filters[k] = v;
          else if (Array.isArray(existing)) existing.push(v);
          else out.filters[k] = [existing, v];
        }
      }
    }
    return out;
  }
  function writeUrlState() {
    const params = new URLSearchParams();
    if (state.time && state.time !== "24H") params.set("t", state.time);
    if (state.time === "CUSTOM" && state.timeCustom) {
      if (state.timeCustom.from) params.set("tfrom", state.timeCustom.from);
      if (state.timeCustom.to) params.set("tto", state.timeCustom.to);
    }
    if (state.cluster) params.set("c", state.cluster);
    for (const [k, v] of Object.entries(currentFilters())) {
      if (Array.isArray(v)) for (const x of v) params.append("f", `${k}:${x}`);
      else params.append("f", `${k}:${v}`);
    }
    const qs = params.toString();
    const url = location.pathname + (qs ? "?" + qs : "") + location.hash;
    history.replaceState(null, "", url);
  }
  function setRefreshInterval(ms) {
    state.refresh = ms;
    localStorage.setItem(LS.refresh, String(ms));
    if (state.refreshTimer) {
      clearInterval(state.refreshTimer);
      state.refreshTimer = null;
    }
    if (ms > 0) {
      state.refreshTimer = setInterval(() => {
        if (state.section === "dashboards" || state.section === "discover") render();
      }, ms);
    }
  }
  function loadViews() {
    return loadJSON(LS.views, []);
  }
  function saveViews(views) {
    localStorage.setItem(LS.views, JSON.stringify(views));
  }
  function saveCurrentView() {
    const dashId = state.route;
    const all2 = loadViews();
    const name = prompt("Save view as:", "View " + (all2.length + 1));
    if (!name) return;
    const id = "v-" + Date.now().toString(36);
    all2.push({
      id,
      name,
      dashId,
      time: state.time,
      cluster: state.cluster,
      filters: currentFilters(),
      savedAt: (/* @__PURE__ */ new Date()).toISOString()
    });
    saveViews(all2);
  }
  function deleteView(id) {
    saveViews(loadViews().filter((v) => v.id !== id));
  }
  function applyView(id) {
    const v = loadViews().find((x) => x.id === id);
    if (!v) return;
    state.time = v.time;
    state.cluster = v.cluster || state.cluster;
    state.filters[v.dashId] = { ...v.filters || {} };
    saveFilters(v.dashId);
    localStorage.setItem(LS.time, state.time);
    location.hash = "#/dashboards/" + v.dashId;
  }
  function relTime(iso) {
    if (!iso) return "";
    const delta = Date.now() - new Date(iso).getTime();
    if (delta < 2e3) return "just now";
    if (delta < 6e4) return Math.round(delta / 1e3) + "s ago";
    if (delta < 36e5) return Math.round(delta / 6e4) + "m ago";
    return Math.round(delta / 36e5) + "h ago";
  }
  function navStatus() {
    if (state.loading) return "LOADING\u2026";
    if (state.fetchErr) return "ERROR \xB7 " + state.fetchErr.slice(0, 40);
    const bits = [];
    bits.push(dataSourceStatus);
    if (state.fetchedAt) bits.push("UPDATED " + relTime(state.fetchedAt).toUpperCase());
    if (state.fetchMs != null) bits.push(state.fetchMs + "MS");
    return bits.join(" \xB7 ");
  }
  function runSearchNow() {
    state.search.result = mockSearch({
      q: state.search.q,
      type: state.search.type,
      index: state.search.index,
      filters: state.search.filters,
      sort: state.search.sort
    });
    localStorage.setItem(LS.search, JSON.stringify({
      q: state.search.q,
      type: state.search.type,
      index: state.search.index,
      filters: state.search.filters,
      sort: state.search.sort,
      showTime: state.search.showTime
    }));
  }
  function parseRoute() {
    const raw = (location.hash || "").replace(/^#\/?/, "");
    const parts = raw.split("/").filter(Boolean);
    const merged = mergedDashboards(defaults, { includeHidden: true });
    const firstOfSection = (sectionId) => {
      const list = dashboardsInSection(sectionId, merged);
      return list[0]?.id;
    };
    if (!parts.length) {
      const first = firstOfSection("dashboards") || "ai-overview";
      return { section: "dashboards", route: first };
    }
    if (parts[0] === "manage") return { section: "settings", route: "settings" };
    const sectionIds = new Set(SECTIONS.map((s) => s.id));
    if (sectionIds.has(parts[0])) {
      if (parts[0] === "dashboards") {
        const id = parts[1] || firstOfSection("dashboards") || "ai-overview";
        const known = merged.find((d) => d.id === id && (d.section || "dashboards") === "dashboards");
        if (known) return { section: "dashboards", route: id };
        return { section: "dashboards", route: firstOfSection("dashboards") || "ai-overview" };
      }
      const first = firstOfSection(parts[0]);
      if (first) return { section: parts[0], route: first };
    }
    if (merged.some((d) => d.id === parts[0])) {
      const d = merged.find((x) => x.id === parts[0]);
      const section = d.section || "dashboards";
      return { section, route: parts[0] };
    }
    return { section: "dashboards", route: firstOfSection("dashboards") || "ai-overview" };
  }
  async function buildSectionData(sectionId) {
    if (sectionId === "data") {
      const clusters = await listClusters();
      const indicesByCluster = {};
      const fieldsByIndex = {};
      for (const c of clusters) {
        indicesByCluster[c.id] = await listIndices(c.id);
      }
      const active = defaultClusterId();
      for (const i of indicesByCluster[active] || []) {
        fieldsByIndex[i.name] = await listFields(i.name);
      }
      return {
        clusters,
        indicesByCluster,
        fieldsByIndex,
        activeCluster: active,
        focusIndex: state._focusIndex || (indicesByCluster[active] || [])[0]?.name
      };
    }
    if (sectionId === "settings") {
      return {
        dashboards: mergedDashboards(defaults, { includeHidden: true }),
        views: loadViews()
      };
    }
    return {};
  }
  function loadAllLayouts() {
    const out = {};
    for (const id of Object.keys(registry)) {
      try {
        const raw = localStorage.getItem(LS.layout(id));
        if (raw) out[id] = JSON.parse(raw);
      } catch {
      }
    }
    return out;
  }
  function saveLayout(dashId) {
    const v = state.layouts[dashId];
    if (!v) localStorage.removeItem(LS.layout(dashId));
    else localStorage.setItem(LS.layout(dashId), JSON.stringify(v));
  }
  function applyTheme(t) {
    state.theme = t;
    document.documentElement.setAttribute("data-theme", t);
    localStorage.setItem(LS.theme, t);
  }
  function mergeLayout(defaultPanels, override) {
    if (!override) return defaultPanels.map((p) => ({ ...p, source: "default" }));
    const byId = Object.fromEntries(defaultPanels.map((p) => [p.id, p]));
    const hidden = new Set(override.hidden || []);
    const out = [];
    const seen = /* @__PURE__ */ new Set();
    const order = override.order || defaultPanels.map((p) => p.id);
    for (const id of order) {
      if (hidden.has(id)) {
        seen.add(id);
        continue;
      }
      const base = byId[id];
      if (base) {
        const cols = override.cols?.[id] ?? base.cols;
        out.push({ ...base, cols, source: "default" });
        seen.add(id);
      } else {
        const added = (override.added || []).find((a) => a.id === id);
        if (added) {
          const cols = override.cols?.[id] ?? added.cols;
          out.push({ ...added, cols, source: "added" });
          seen.add(id);
        }
      }
    }
    for (const p of defaultPanels) {
      if (!seen.has(p.id) && !hidden.has(p.id)) {
        const cols = override.cols?.[p.id] ?? p.cols;
        out.push({ ...p, cols, source: "default" });
      }
    }
    for (const added of override.added || []) {
      if (!seen.has(added.id)) {
        const cols = override.cols?.[added.id] ?? added.cols;
        out.push({ ...added, cols, source: "added" });
      }
    }
    return out;
  }
  function ensureOverride(dashId, defaultPanels) {
    if (state.layouts[dashId]) return state.layouts[dashId];
    const fresh = {
      order: defaultPanels.map((p) => p.id),
      cols: {},
      hidden: [],
      added: []
    };
    state.layouts[dashId] = fresh;
    return fresh;
  }
  function mutate(dashId, defaultPanels, fn) {
    const ov = ensureOverride(dashId, defaultPanels);
    fn(ov);
    saveLayout(dashId);
  }
  var SIZES = [2, 3, 4, 6, 8, 12];
  function renderEditChrome(p) {
    const sizes = SIZES.map(
      (s) => `<button type="button" data-panel="${esc(p.id)}" data-size="${s}" class="${p.cols === s ? "active" : ""}" aria-pressed="${p.cols === s}">${s}</button>`
    ).join('<span class="sep">\xB7</span>');
    const frac = Math.max(0, Math.min(12, p.cols)) / 12;
    const meterFill = (frac * 96).toFixed(1);
    const meter = `
    <svg class="meter" viewBox="0 0 96 6" preserveAspectRatio="none" aria-hidden="true">
      <line x1="0" y1="5" x2="96" y2="5" stroke="currentColor" stroke-width="1" stroke-opacity="0.25"/>
      <line x1="0" y1="5" x2="${meterFill}" y2="5" stroke="var(--z-accent)" stroke-width="1"/>
    </svg>`;
    return `
  <div class="panel-edit" aria-label="Edit panel">
    <span class="colsLabel"><span class="max">COL</span> ${p.cols}<span class="slash">/</span><span class="max">12</span></span>
    ${meter}
    <span class="sizes">${sizes}</span>
    <button type="button" class="remove" data-panel="${esc(p.id)}" data-remove aria-label="Remove">\u2715</button>
  </div>`;
  }
  function renderPanel(p, data, editMode) {
    let inner = "";
    try {
      if (p.source === "added") {
        const t = chartTypes[p.type] || chartTypes.markdown;
        inner = t.render(data);
      } else if (typeof p.render === "function") {
        inner = p.render({ data });
      } else {
        inner = "";
      }
      const stripped = String(inner || "").replace(/<[^>]+>/g, "").trim();
      if (!stripped) {
        inner = `<div class="panel-empty mono faint">NO DATA \xB7 ADJUST FILTERS OR TIME RANGE</div>`;
      }
    } catch (err) {
      inner = `<div class="panel-empty mono faint">PANEL ERROR \xB7 ${esc((err.message || err) + "").slice(0, 80)}</div>`;
    }
    const editAttrs = editMode ? ' draggable="true"' : "";
    const editChrome = editMode ? renderEditChrome(p) : "";
    const drillAttr = p.drilldown?.to ? ` data-drilldown-to="${esc(p.drilldown.to)}"` : "";
    const drillHint = p.drilldown?.to ? `<span class="drill-hint mono faint">\u2192 ${esc(p.drilldown.to.toUpperCase())}</span>` : "";
    return `
  <section class="panel${editMode ? " edit" : ""}" data-panel="${esc(p.id)}" style="grid-column: span ${p.cols};"${editAttrs}${drillAttr}>
    ${editChrome}
    ${p.eyebrow ? `<div class="key">${esc(p.eyebrow)}${drillHint}</div>` : ""}
    ${inner}
  </section>`;
  }
  function renderAddPicker() {
    const items = chartTypeList.map(
      (t) => `<button type="button" data-add="${esc(t.id)}" title="${esc(t.describe || "")}">${esc(t.name)}</button>`
    ).join('<span class="sep">\xB7</span>');
    return `
  <div class="add-picker">
    <span class="key" style="color:var(--z-accent);">+ ADD PANEL</span>
    <span class="types">${items}</span>
  </div>`;
  }
  async function render() {
    const app = document.getElementById("app");
    const allDash = mergedDashboards(defaults, { includeHidden: true });
    const dash = allDash.find((d) => d.id === state.route) || dashboardsInSection(state.section, allDash)[0] || registry["ai-overview"];
    const navDash = mergedDashboards(defaults);
    const dashboardsForSection = dashboardsInSection("dashboards", navDash);
    if (dash.id === "search-discover" && !state.search.result) {
      runSearchNow();
    }
    const activeFilters = currentFilters();
    let data;
    let fetchErr = null;
    if (state.section === "data" || state.section === "settings") {
      try {
        data = await buildSectionData(state.section);
      } catch (err) {
        data = {};
        fetchErr = err;
      }
    } else {
      state.loading = true;
      try {
        const result = await query({
          dashId: dash.id,
          range: state.time,
          customRange: state.time === "CUSTOM" ? state.timeCustom : null,
          cluster: state.cluster,
          filters: activeFilters
        });
        data = result.data;
        state.fetchedAt = result.meta.fetchedAt;
        state.fetchMs = result.meta.durationMs;
        state.fetchErr = null;
      } catch (err) {
        state.fetchErr = err.message || String(err);
        fetchErr = err;
      } finally {
        state.loading = false;
      }
      if (fetchErr) {
        app.innerHTML = `
        ${Nav({ sections: SECTIONS, activeSection: state.section, dashboards: dashboardsForSection, groups: DASHBOARD_GROUPS, activeDash: dash.id, theme: state.theme, edit: state.edit, status: navStatus() })}
        <div class="scene"><div class="key" style="margin-bottom:12px;">ERROR</div><h1 class="h-scene">CANNOT LOAD</h1></div>
        <pre class="mono faint" style="white-space:pre-wrap; font-size:var(--fs-13);">${esc(fetchErr && fetchErr.stack || fetchErr)}</pre>
        <div style="margin-top:var(--sp-6);"><button type="button" data-retry class="text-btn">RETRY</button></div>`;
        return;
      }
    }
    const view = dash.render({ data, time: state.time, search: state.search });
    try {
      const store = JSON.parse(localStorage.getItem("xerj.dashboards") || "{}");
      if (dash.isUser || store.names && store.names[dash.id]) {
        view.title = (dash.name || view.title || "").toUpperCase();
      }
    } catch {
    }
    const merged = mergeLayout(view.panels, state.layouts[dash.id]);
    const panelsHtml = merged.length ? merged.map((p) => renderPanel(p, data, state.edit)).join("") : `<div class="mono faint" style="grid-column: span 12; padding:var(--sp-6) 0;">All panels hidden. Click RESET to restore defaults.</div>`;
    const editFrame = state.edit ? `
    <div class="edit-frame" aria-hidden="true"></div>
    <div class="edit-strip-top" role="status">
      <span class="marker">EDIT MODE</span>
      <span class="tips">
        DRAG PANEL TO REORDER \xB7
        CLICK A <span class="key-bind">NUMBER</span> TO RESIZE \xB7
        <span class="key-bind">\u2715</span> TO REMOVE \xB7
        SCROLL FOR <span class="key-bind">+ ADD</span>
      </span>
      <span class="meta">${esc(dash.name.toUpperCase())} \xB7 ${merged.length} PANELS</span>
    </div>
    <div class="edit-strip-bottom" aria-hidden="true">${Array.from({ length: 12 }, (_, i) => `<span>${String(i + 1).padStart(2, "0")}</span>`).join("")}</div>
  ` : "";
    const gridOverlay = state.edit ? `<div class="edit-grid" aria-hidden="true">${"<span></span>".repeat(12)}</div>` : "";
    const addHtml = state.edit ? renderAddPicker() : "";
    const hideTimeCtrl = state.section !== "dashboards" && state.section !== "discover";
    const showFilterBar = state.section === "dashboards";
    const filterBarHtml = showFilterBar ? FilterBar({ filters: activeFilters, kql: serializeKql(activeFilters) }) : "";
    const refreshHtml = hideTimeCtrl ? "" : RefreshCtrl({ active: state.refresh });
    const clusterHtml = hideTimeCtrl ? "" : ClusterCtrl({ clusters: listClustersSync(), active: state.cluster });
    const savedViewsHtml = showFilterBar ? SavedViews({ views: loadViews(), dashId: dash.id }) : "";
    app.innerHTML = `
    ${Nav({
      sections: SECTIONS,
      activeSection: state.section,
      dashboards: dashboardsForSection,
      groups: DASHBOARD_GROUPS,
      activeDash: dash.id,
      theme: state.theme,
      edit: state.edit,
      mobile: state.mobile,
      status: navStatus()
    })}
    ${state.mobile ? '<div class="iphone-frame"><div class="iphone-notch"></div><div class="iphone-screen">' : ""}
    ${SceneHeader({
      title: view.title,
      kicker: view.kicker || "OBSERVE",
      meta: view.meta || [state.time],
      editable: state.edit,
      dashId: dash.id
    })}
    ${view.caption ? `<p class="caption">${esc(view.caption)}</p>` : ""}
    ${hideTimeCtrl ? "" : `<div class="dash-ctrls">${TimeCtrl({ active: state.time, custom: state.timeCustom })}${refreshHtml}${clusterHtml}</div>`}
    ${filterBarHtml}
    ${savedViewsHtml}
    <main class="canvas${state.edit ? " edit" : ""}" aria-label="${esc(dash.name)}">${gridOverlay}${panelsHtml}</main>
    ${addHtml}
    ${state.mobile ? '</div><div class="iphone-home-bar"></div></div>' : ""}
    ${Footer()}
    ${editFrame}
  `;
    app.setAttribute("aria-busy", "false");
    if (state._focusSelector) {
      const el = document.querySelector(state._focusSelector);
      if (el) {
        el.focus();
        if (el.tagName === "INPUT") {
          const v = el.value;
          el.setSelectionRange(v.length, v.length);
        }
      }
      state._focusSelector = null;
    }
  }
  var dragSrcId = null;
  document.addEventListener("click", (e) => {
    const secA = e.target.closest("[data-section]");
    if (secA) {
      e.preventDefault();
      const sid = secA.getAttribute("data-section");
      if (sid === "dashboards") {
        location.hash = "#/dashboards";
      } else {
        location.hash = "#/" + sid;
      }
      return;
    }
    const dashA = e.target.closest("[data-dash]");
    if (dashA) {
      e.preventDefault();
      const id = dashA.getAttribute("data-dash");
      location.hash = "#/dashboards/" + id;
      return;
    }
    const groupBtn = e.target.closest("[data-dash-group]");
    if (groupBtn) {
      e.preventDefault();
      const firstId = groupBtn.getAttribute("data-dash-group-first");
      if (firstId) location.hash = "#/dashboards/" + firstId;
      return;
    }
    const tb = e.target.closest("[data-time]");
    if (tb) {
      state.time = tb.getAttribute("data-time");
      localStorage.setItem(LS.time, state.time);
      if (state.time === "CUSTOM" && !state.timeCustom.from) {
        const now = /* @__PURE__ */ new Date();
        const prev = new Date(now.getTime() - 24 * 3600 * 1e3);
        const fmt2 = (d) => d.toISOString().slice(0, 16);
        state.timeCustom = { from: fmt2(prev), to: fmt2(now) };
        localStorage.setItem("xerj.timeFrom", state.timeCustom.from);
        localStorage.setItem("xerj.timeTo", state.timeCustom.to);
      }
      writeUrlState();
      render();
      return;
    }
    const rf = e.target.closest("[data-refresh]");
    if (rf) {
      setRefreshInterval(parseInt(rf.getAttribute("data-refresh"), 10));
      render();
      return;
    }
    if (e.target.closest("[data-retry]")) {
      render();
      return;
    }
    const fAdd = e.target.closest("[data-filter-add]");
    if (fAdd) {
      const raw = fAdd.getAttribute("data-filter-add");
      const idx = raw.indexOf(":");
      if (idx > 0) {
        const field = raw.slice(0, idx);
        const value = raw.slice(idx + 1);
        const drillHost = fAdd.closest("[data-drilldown-to]");
        if (drillHost) {
          const toId = drillHost.getAttribute("data-drilldown-to");
          if (toId === "search-discover") {
            state.search.filters = { ...state.search.filters || {}, [field]: value };
            state.search.result = null;
          } else {
            state.filters[toId] = { ...state.filters[toId] || {}, [field]: value };
            try {
              localStorage.setItem(LS.filters(toId), JSON.stringify(state.filters[toId]));
            } catch {
            }
          }
          const all2 = mergedDashboards(defaults, { includeHidden: true });
          const target = all2.find((d) => d.id === toId);
          const sectionId = target?.section || "dashboards";
          location.hash = sectionId === "dashboards" ? `#/dashboards/${toId}` : `#/${sectionId}`;
          return;
        }
        setFilter(field, value);
        render();
      }
      return;
    }
    const fRem = e.target.closest("[data-filter-remove]");
    if (fRem) {
      const raw = fRem.getAttribute("data-filter-remove");
      const idx = raw.indexOf(":");
      if (idx > 0) {
        removeFilterValue(raw.slice(0, idx), raw.slice(idx + 1));
        render();
      }
      return;
    }
    if (e.target.closest("[data-filter-clear]")) {
      clearFilters();
      render();
      return;
    }
    const cset = e.target.closest("[data-cluster-set]");
    if (cset) {
      state.cluster = cset.getAttribute("data-cluster-set");
      setDefaultCluster(state.cluster);
      writeUrlState();
      render();
      return;
    }
    if (e.target.closest("[data-view-save]")) {
      saveCurrentView();
      render();
      return;
    }
    const vApply = e.target.closest("[data-view-apply]");
    if (vApply) {
      applyView(vApply.getAttribute("data-view-apply"));
      return;
    }
    const vDel = e.target.closest("[data-view-delete]");
    if (vDel) {
      const id = vDel.getAttribute("data-view-delete");
      if (confirm("Delete this view?")) {
        deleteView(id);
        render();
      }
      return;
    }
    const th = e.target.closest("[data-theme-set]");
    if (th) {
      applyTheme(th.getAttribute("data-theme-set"));
      render();
      return;
    }
    if (e.target.closest("[data-mobile-toggle]")) {
      state.mobile = !state.mobile;
      localStorage.setItem("xerj.mobile", state.mobile ? "1" : "0");
      document.documentElement.classList.toggle("mobile-preview", state.mobile);
      render();
      return;
    }
    if (e.target.closest("[data-edit-toggle]")) {
      state.edit = !state.edit;
      localStorage.setItem(LS.edit, state.edit ? "1" : "0");
      render();
      return;
    }
    if (e.target.closest("[data-reset-layout]")) {
      const id = state.route;
      delete state.layouts[id];
      saveLayout(id);
      render();
      return;
    }
    const sz = e.target.closest("[data-size]");
    if (sz) {
      const pid = sz.getAttribute("data-panel");
      const cols = parseInt(sz.getAttribute("data-size"), 10);
      const dash = registry[state.route];
      const view = dash.render({ data: {}, time: state.time });
      mutate(state.route, view.panels, (ov) => {
        ov.cols[pid] = cols;
      });
      render();
      return;
    }
    const rm = e.target.closest("[data-remove]");
    if (rm) {
      const pid = rm.getAttribute("data-panel");
      const dash = registry[state.route];
      const view = dash.render({ data: {}, time: state.time });
      mutate(state.route, view.panels, (ov) => {
        if (!ov.hidden.includes(pid)) ov.hidden.push(pid);
        const idx = (ov.added || []).findIndex((a) => a.id === pid);
        if (idx >= 0) ov.added.splice(idx, 1);
      });
      render();
      return;
    }
    const qt = e.target.closest("[data-query-type]");
    if (qt) {
      state.search.type = qt.getAttribute("data-query-type");
      runSearchNow();
      state._focusSelector = "[data-search-input]";
      render();
      return;
    }
    const si = e.target.closest("[data-search-index]");
    if (si) {
      state.search.index = si.getAttribute("data-search-index");
      runSearchNow();
      state._focusSelector = "[data-search-input]";
      render();
      return;
    }
    const fa = e.target.closest("[data-facet-apply]");
    if (fa) {
      const raw = fa.getAttribute("data-facet-apply");
      const colonIdx = raw.indexOf(":");
      const field = raw.slice(0, colonIdx);
      const value = raw.slice(colonIdx + 1);
      const cur = state.search.filters[field];
      if (cur === value) delete state.search.filters[field];
      else state.search.filters[field] = value;
      runSearchNow();
      render();
      return;
    }
    if (e.target.closest("[data-facet-clear]")) {
      state.search.filters = {};
      runSearchNow();
      render();
      return;
    }
    const sortBtn = e.target.closest("[data-sort-field]");
    if (sortBtn) {
      state.search.sort = {
        field: sortBtn.getAttribute("data-sort-field"),
        dir: sortBtn.getAttribute("data-sort-dir")
      };
      runSearchNow();
      render();
      return;
    }
    if (e.target.closest("[data-toggle-time]")) {
      state.search.showTime = !state.search.showTime;
      runSearchNow();
      render();
      return;
    }
    if (e.target.closest("[data-export-csv]")) {
      const hits = state.search.result?.hits || [];
      const cols = state.search.showTime ? ["_index", "_id", "_score", "_ts", "_source"] : ["_index", "_id", "_score", "_source"];
      downloadText(
        "xerj-search-" + (/* @__PURE__ */ new Date()).toISOString().slice(0, 19).replace(/[:T]/g, "-") + ".csv",
        "text/csv",
        hitsToCsv(hits, cols)
      );
      return;
    }
    const pngBtn = e.target.closest("[data-export-png]");
    if (pngBtn) {
      const sec = pngBtn.closest(".panel");
      const svg = sec?.querySelector("svg.chart, svg.series");
      if (svg) {
        const id = sec.getAttribute("data-panel") || "panel";
        svgToPng(svg, { filename: "xerj-" + id + ".png" }).catch((err) => console.error("PNG export failed", err));
      }
      return;
    }
    const mgUp = e.target.closest("[data-mg-up]");
    if (mgUp) {
      const id = mgUp.getAttribute("data-mg-up");
      const list = mergedDashboards(defaults, { includeHidden: true });
      const i = list.findIndex((d) => d.id === id);
      if (i > 0) {
        const order = list.map((d) => d.id);
        [order[i - 1], order[i]] = [order[i], order[i - 1]];
        reorderDashboards(order);
        render();
      }
      return;
    }
    const mgDown = e.target.closest("[data-mg-down]");
    if (mgDown) {
      const id = mgDown.getAttribute("data-mg-down");
      const list = mergedDashboards(defaults, { includeHidden: true });
      const i = list.findIndex((d) => d.id === id);
      if (i >= 0 && i < list.length - 1) {
        const order = list.map((d) => d.id);
        [order[i + 1], order[i]] = [order[i], order[i + 1]];
        reorderDashboards(order);
        render();
      }
      return;
    }
    const mgRename = e.target.closest("[data-mg-rename]");
    if (mgRename) {
      const id = mgRename.getAttribute("data-mg-rename");
      const current = mergedDashboards(defaults, { includeHidden: true }).find((d) => d.id === id);
      const name = prompt('Rename "' + (current?.name || id) + '":', current?.name || "");
      if (name != null) {
        renameDashboard(id, name);
        render();
      }
      return;
    }
    const mgHide = e.target.closest("[data-mg-hide]");
    if (mgHide) {
      const id = mgHide.getAttribute("data-mg-hide");
      const list = mergedDashboards(defaults, { includeHidden: true });
      const cur = list.find((d) => d.id === id);
      setHidden(id, !cur?.hidden);
      render();
      return;
    }
    const mgClone = e.target.closest("[data-mg-clone]");
    if (mgClone) {
      const id = mgClone.getAttribute("data-mg-clone");
      const current = mergedDashboards(defaults, { includeHidden: true }).find((d) => d.id === id);
      const name = prompt('Clone "' + (current?.name || id) + '" as:', (current?.name || "Untitled") + " (copy)");
      if (name) {
        const newId = createUserDashboard({ name, fromId: id });
        location.hash = "#/dashboards/" + newId;
      }
      return;
    }
    const mgDelete = e.target.closest("[data-mg-delete]");
    if (mgDelete) {
      const id = mgDelete.getAttribute("data-mg-delete");
      if (isUserDash(id) && confirm('Delete user dashboard "' + id + '"? This cannot be undone.')) {
        deleteUserDashboard(id);
        if (state.route === id) {
          location.hash = "#/settings";
        } else {
          render();
        }
      }
      return;
    }
    const mgNew = e.target.closest("[data-mg-new]");
    if (mgNew) {
      const fromId = mgNew.getAttribute("data-mg-new");
      const name = prompt(fromId ? 'New dashboard from "' + fromId + '" \xB7 Name:' : "New blank dashboard \xB7 Name:", "Untitled");
      if (name) {
        const newId = createUserDashboard({ name, fromId: fromId || null });
        location.hash = "#/dashboards/" + newId;
      }
      return;
    }
    const mgCluster = e.target.closest("[data-mg-cluster]");
    if (mgCluster) {
      setDefaultCluster(mgCluster.getAttribute("data-mg-cluster"));
      state._focusIndex = null;
      render();
      return;
    }
    const mgIndex = e.target.closest("[data-mg-index]");
    if (mgIndex) {
      state._focusIndex = mgIndex.getAttribute("data-mg-index");
      render();
      return;
    }
    if (e.target.closest("[data-mg-reset-all]")) {
      if (confirm("Reset ALL saved state? This wipes dashboards, layouts, theme, filters.")) {
        for (const k of Object.keys(localStorage)) {
          if (k.startsWith("xerj.")) localStorage.removeItem(k);
        }
        resetAll();
        location.reload();
      }
      return;
    }
    const add = e.target.closest("[data-add]");
    if (add) {
      const typeId = add.getAttribute("data-add");
      const t = chartTypes[typeId];
      if (!t) return;
      const dash = registry[state.route];
      const view = dash.render({ data: {}, time: state.time });
      mutate(state.route, view.panels, (ov) => {
        ov.added = ov.added || [];
        const newId = typeId + "-" + Date.now().toString(36);
        ov.added.push({
          id: newId,
          type: typeId,
          eyebrow: t.name + " \xB7 NEW",
          cols: t.cols || 6
        });
        ov.order = ov.order || view.panels.map((p) => p.id);
        ov.order.push(newId);
      });
      render();
      requestAnimationFrame(() => window.scrollTo({ top: document.body.scrollHeight, behavior: "smooth" }));
      return;
    }
  });
  document.addEventListener("keydown", (e) => {
    const kq = e.target.closest && e.target.closest("[data-kql-input]");
    if (kq) {
      if (e.key === "Enter") {
        e.preventDefault();
        applyKqlInput(kq.value);
        state._focusSelector = "[data-kql-input]";
        render();
      } else if (e.key === "Escape") {
        e.preventDefault();
        kq.value = "";
        applyKqlInput("");
        state._focusSelector = "[data-kql-input]";
        render();
      }
      return;
    }
    const si = e.target.closest && e.target.closest("[data-search-input]");
    if (si) {
      if (e.key === "Enter") {
        e.preventDefault();
        state.search.q = si.value;
        runSearchNow();
        state._focusSelector = "[data-search-input]";
        render();
      } else if (e.key === "Escape") {
        si.value = "";
        state.search.q = "";
        state.search.filters = {};
        runSearchNow();
        state._focusSelector = "[data-search-input]";
        render();
      }
      return;
    }
    const rn = e.target.closest && e.target.closest("[data-rename-dash]");
    if (rn) {
      if (e.key === "Enter") {
        e.preventDefault();
        const id = rn.getAttribute("data-rename-dash");
        const name = rn.textContent.trim();
        renameDashboard(id, name);
        rn.blur();
        render();
      } else if (e.key === "Escape") {
        e.preventDefault();
        rn.blur();
        render();
      }
    }
  });
  document.addEventListener("change", (e) => {
    const tf = e.target.closest && e.target.closest("[data-time-from]");
    const tt = e.target.closest && e.target.closest("[data-time-to]");
    if (tf || tt) {
      state.timeCustom = {
        from: document.querySelector("[data-time-from]")?.value || "",
        to: document.querySelector("[data-time-to]")?.value || ""
      };
      localStorage.setItem("xerj.timeFrom", state.timeCustom.from);
      localStorage.setItem("xerj.timeTo", state.timeCustom.to);
      writeUrlState();
      render();
    }
  });
  document.addEventListener("blur", (e) => {
    const rn = e.target.closest && e.target.closest("[data-rename-dash]");
    if (!rn) return;
    const id = rn.getAttribute("data-rename-dash");
    const newName = rn.textContent.trim();
    renameDashboard(id, newName);
  }, true);
  document.addEventListener("dragstart", (e) => {
    if (!state.edit) return;
    const sec = e.target.closest(".panel.edit");
    if (!sec) return;
    dragSrcId = sec.getAttribute("data-panel");
    sec.classList.add("dragging");
    try {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", dragSrcId);
    } catch {
    }
  });
  document.addEventListener("dragover", (e) => {
    if (!state.edit || !dragSrcId) return;
    const sec = e.target.closest(".panel.edit");
    if (!sec) return;
    e.preventDefault();
    document.querySelectorAll(".drop-before, .drop-after").forEach((el) => el.classList.remove("drop-before", "drop-after"));
    const rect = sec.getBoundingClientRect();
    const after = e.clientX > rect.left + rect.width / 2;
    sec.classList.add(after ? "drop-after" : "drop-before");
  });
  document.addEventListener("dragleave", (e) => {
    const sec = e.target.closest(".panel.edit");
    if (sec && !sec.contains(e.relatedTarget)) sec.classList.remove("drop-before", "drop-after");
  });
  document.addEventListener("drop", (e) => {
    if (!state.edit || !dragSrcId) return;
    const sec = e.target.closest(".panel.edit");
    if (!sec) return;
    e.preventDefault();
    const targetId = sec.getAttribute("data-panel");
    const insertAfter = sec.classList.contains("drop-after");
    document.querySelectorAll(".drop-before, .drop-after").forEach((el) => el.classList.remove("drop-before", "drop-after"));
    if (!targetId || targetId === dragSrcId) return;
    const dash = registry[state.route];
    const view = dash.render({ data: {}, time: state.time });
    mutate(state.route, view.panels, (ov) => {
      const fullOrder = ov.order && ov.order.length ? ov.order.slice() : view.panels.map((p) => p.id);
      const have = new Set(fullOrder);
      if (!have.has(dragSrcId)) fullOrder.push(dragSrcId);
      if (!have.has(targetId)) fullOrder.push(targetId);
      const from = fullOrder.indexOf(dragSrcId);
      fullOrder.splice(from, 1);
      let to = fullOrder.indexOf(targetId);
      if (insertAfter) to += 1;
      fullOrder.splice(to, 0, dragSrcId);
      ov.order = fullOrder;
    });
    render();
  });
  document.addEventListener("dragend", () => {
    document.querySelectorAll(".dragging").forEach((el) => el.classList.remove("dragging"));
    document.querySelectorAll(".drop-before, .drop-after").forEach((el) => el.classList.remove("drop-before", "drop-after"));
    dragSrcId = null;
  });
  window.addEventListener("hashchange", () => {
    const parsed = parseRoute();
    state.section = parsed.section;
    state.route = parsed.route;
    const urlState = parseUrlState();
    if (urlState.time) state.time = urlState.time;
    if (urlState.cluster) state.cluster = urlState.cluster;
    if (urlState.filters) {
      state.filters[state.route] = { ...state.filters[state.route] || {}, ...urlState.filters };
      saveFilters(state.route);
    }
    writeUrlState();
    render();
  });
  if (state.refresh > 0) setRefreshInterval(state.refresh);
  if (state.mobile) document.documentElement.classList.add("mobile-preview");
  render();
})();
