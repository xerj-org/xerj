// Figures on article pages (content/visuals/*.html). Progressive enhancement:
// every figure reads without this file. With it, stacked panels become ARIA tabs
// (arrow keys, Home, End), and the YAML line checker, the search lab, the
// step-through and the picker run. No network, no storage.
(function () {
  'use strict';
  document.documentElement.classList.add('vz-js');

  function each(sel, fn, root) {
    var list = (root || document).querySelectorAll(sel);
    for (var i = 0; i < list.length; i++) fn(list[i], i);
  }

  each('[data-vz-tabs]', function (box) {
    var tabs = [].slice.call(box.querySelectorAll('[role="tab"]'));
    if (!tabs.length) return;
    function select(tab, focus) {
      tabs.forEach(function (t) {
        var on = t === tab;
        t.setAttribute('aria-selected', on ? 'true' : 'false');
        t.tabIndex = on ? 0 : -1;
        var panel = document.getElementById(t.getAttribute('aria-controls'));
        if (panel) panel.hidden = !on;
      });
      if (focus) tab.focus();
    }
    tabs.forEach(function (t, i) {
      t.addEventListener('click', function () { select(t, false); });
      t.addEventListener('keydown', function (e) {
        var n = null;
        if (e.key === 'ArrowRight') n = tabs[(i + 1) % tabs.length];
        else if (e.key === 'ArrowLeft') n = tabs[(i - 1 + tabs.length) % tabs.length];
        else if (e.key === 'Home') n = tabs[0];
        else if (e.key === 'End') n = tabs[tabs.length - 1];
        if (n) { e.preventDefault(); select(n, true); }
      });
    });
    var start = tabs.filter(function (t) { return t.getAttribute('aria-selected') === 'true'; })[0];
    select(start || tabs[0], false);
  });

  // Line-shape tier of the family sniffer, as engine/crates/xerj-autoindex/src/sniff.rs
  // states it: a non-blank line is YAML-like when it matches `key: value` (optionally
  // after "- ") or starts with "- ", except "- [ ]" and "- [x]" task items. Under 3
  // non-blank lines the ratio is 0. The sniffer also needs the text to parse as a
  // YAML stream before it picks YAML; this checker cannot run that parse. \w here
  // is ASCII, where the Rust regex is Unicode-aware.
  var KEY = /^\s*(- )?[\w.@\/-]+:(\s|$)/;
  function yamlLike(line) {
    var t = line.replace(/^\s+/, '');
    var box = t.indexOf('- [ ] ') === 0 || t.indexOf('- [x] ') === 0 || t.indexOf('- [X] ') === 0;
    return KEY.test(line) || (t.indexOf('- ') === 0 && !box);
  }
  each('[data-vz-yaml]', function (box) {
    var input = box.querySelector('textarea');
    var fill = box.querySelector('.vz-meter-fill');
    var out = box.querySelector('.vz-out');
    var list = box.querySelector('.vz-lines');
    if (!input || !out) return;
    function run() {
      var lines = input.value.split(/\r?\n/).filter(function (l) { return l.trim() !== ''; });
      var hits = lines.filter(yamlLike).length;
      var ratio = lines.length < 3 ? 0 : hits / lines.length;
      var pct = Math.round(ratio * 100);
      if (fill) {
        fill.style.width = pct + '%';
        fill.classList.toggle('is-over', ratio >= 0.6);
      }
      var msg = hits + ' of ' + lines.length + ' non-blank lines read as YAML (' + pct + '%). ';
      if (lines.length < 3) msg += 'Under 3 non-blank lines the sniffer does not apply this test.';
      else if (ratio >= 0.6) msg += 'At or over 60%: if the text also parses as YAML, the sniffer gives it the yaml family, and a note there can be dropped with 0 documents. Read autoindex-catalog after the run.';
      else msg += 'Under 60%: the sniffer keeps reading this file as text.';
      out.textContent = msg;
      if (list) {
        list.textContent = '';
        lines.slice(0, 40).forEach(function (l) {
          var li = document.createElement('li');
          var y = yamlLike(l);
          if (y) li.className = 'is-y';
          var tag = document.createElement('b');
          tag.textContent = y ? 'YAML' : 'text';
          li.appendChild(tag);
          li.appendChild(document.createTextNode(l));
          list.appendChild(li);
        });
      }
    }
    input.addEventListener('input', run);
    run();
  });
  // Search lab. BM25 here is the formula XERJ uses (engine/crates/xerj-fts/src/bm25.rs):
  // k1 = 1.2, b = 0.75, idf = ln(1 + (N - n + 0.5) / (n + 0.5)). The vector side is a
  // toy: 5 hand-set numbers per word (network, server, memory, error, time), summed per
  // text and compared by cosine. It is not a real embedding model.
  var W = {connection:[1,0,0,.3,0],connect:[1,0,0,0,0],refused:[.6,0,0,1,0],port:[1,.4,0,0,0],'9200':[.6,.8,0,0,0],
    reach:[1,.3,0,0,0],unreachable:[1,.3,0,.6,0],server:[.4,1,0,0,0],search:[0,1,0,0,0],index:[0,1,.2,0,0],
    restart:[.2,.6,0,.4,0],stuck:[0,.2,0,1,0],error:[0,0,0,1,0],agent:[0,.2,.6,0,0],memory:[0,0,1,0,0],
    forgets:[0,0,1,.3,0],forget:[0,0,1,.3,0],sessions:[0,0,.8,0,.4],remember:[0,0,1,0,0],remembers:[0,0,1,0,0],
    chats:[0,0,.7,0,.3],yesterday:[0,0,.3,0,1],discussed:[0,0,.6,0,.3],assistant:[0,.1,.6,0,0],api:[.3,.8,0,0,0],
    down:[.6,.3,0,.8,0],offline:[.8,.3,0,.6,0],timeout:[.8,0,0,.8,.2],crash:[0,.3,0,1,0],recall:[0,0,1,0,0],
    coding:[0,.2,.3,0,0],across:[0,0,0,0,.3],bot:[0,.2,.6,0,0],database:[0,1,.3,0,0],engine:[0,1,0,0,0],
    history:[0,0,.7,0,.6],last:[0,0,0,0,.8],week:[0,0,0,0,1],network:[1,0,0,0,0],fails:[0,0,0,1,0],
    broken:[0,.2,0,1,0],hangs:[0,.2,0,1,.3],frozen:[0,.2,0,1,0]};
  function toks(s) { return s.toLowerCase().match(/[a-z0-9]+/g) || []; }
  function bm25(docs, q) {
    var D = docs.map(toks), N = D.length, avg = 0, out = [];
    D.forEach(function (d) { avg += d.length / N; });
    var terms = toks(q).filter(function (t, i, a) { return a.indexOf(t) === i; });
    D.forEach(function (d, i) {
      var s = 0;
      terms.forEach(function (t) {
        var n = D.filter(function (x) { return x.indexOf(t) >= 0; }).length;
        var tf = d.filter(function (x) { return x === t; }).length;
        if (!n || !tf) return;
        s += Math.log(1 + (N - n + 0.5) / (n + 0.5)) * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * d.length / avg));
      });
      if (s > 0) out.push({ i: i, s: s });
    });
    return out.sort(function (a, b) { return b.s - a.s; });
  }
  function emb(s) {
    var v = [0, 0, 0, 0, 0];
    toks(s).forEach(function (t) { if (W[t]) for (var j = 0; j < 5; j++) v[j] += W[t][j]; });
    return v;
  }
  function cos(a, b) {
    var d = 0, x = 0, y = 0;
    for (var j = 0; j < 5; j++) { d += a[j] * b[j]; x += a[j] * a[j]; y += b[j] * b[j]; }
    return x && y ? d / Math.sqrt(x * y) : 0;
  }
  function knn(docs, q, k) {
    var qv = emb(q);
    if (!qv.some(Boolean)) return [];
    return docs.map(function (d, i) { return { i: i, s: cos(qv, emb(d)) }; })
      .sort(function (a, b) { return b.s - a.s; }).slice(0, k);
  }
  function rrf(lists, ws, k) {
    var acc = {};
    lists.forEach(function (l, li) {
      l.forEach(function (h, r) { acc[h.i] = (acc[h.i] || 0) + ws[li] / (k + r + 1); });
    });
    return Object.keys(acc).map(function (i) { return { i: +i, s: acc[i] }; })
      .filter(function (h) { return h.s > 0; }).sort(function (a, b) { return b.s - a.s; });
  }
  function fill(ol, hits, docs, digits, empty) {
    if (!ol) return;
    ol.textContent = '';
    if (!hits.length) { var e = document.createElement('li'); e.className = 'is-none'; e.textContent = empty; ol.appendChild(e); return; }
    hits.slice(0, 3).forEach(function (h) {
      var li = document.createElement('li'), b = document.createElement('b');
      b.textContent = h.s.toFixed(digits);
      li.appendChild(b);
      li.appendChild(document.createTextNode(docs[h.i]));
      ol.appendChild(li);
    });
  }
  each('[data-vz-search]', function (box) {
    var docs = [].map.call(box.querySelectorAll('.vz-docs li'), function (li) { return li.textContent.trim(); });
    var input = box.querySelector('input[type="text"]');
    var out = box.querySelector('.vz-out');
    var num = function (n) { var el = box.querySelector('input[name="' + n + '"]'); return el ? +el.value : 1; };
    if (!input) return;
    function run() {
      var q = input.value, b = bm25(docs, q), v = box.hasAttribute('data-vz-bm25-only') ? [] : knn(docs, q, 3);
      fill(box.querySelector('[data-out="bm25"]'), b, docs, 4, 'No hit. No word of the query is in any line.');
      fill(box.querySelector('[data-out="vec"]'), v, docs, 3, 'No hit. The toy knows none of these words.');
      var k = num('k'), wb = num('wb'), wv = num('wv');
      fill(box.querySelector('[data-out="rrf"]'), rrf([b.slice(0, 3), v], [wb, wv], k), docs, 4, 'Nothing to fuse.');
      each('output[data-for]', function (o) { o.textContent = num(o.getAttribute('data-for')); }, box);
      if (out) {
        var msg = b.length ? 'BM25 found ' + b.length + ' line' + (b.length > 1 ? 's' : '') + (b.length > 1 ? ' that share' : ' that shares') + ' a word with the query.' : 'BM25 found nothing: no line shares a word with the query.';
        if (!box.hasAttribute('data-vz-bm25-only')) msg += v.length ? ' The vector side always returns its 3 nearest lines, even weak ones.' : '';
        out.textContent = msg;
      }
    }
    each('button[data-q]', function (btn) {
      btn.addEventListener('click', function () { input.value = btn.getAttribute('data-q'); run(); });
    }, box);
    each('input', function (el) { el.addEventListener('input', run); }, box);
    run();
  });

  // Step-through: one step at a time with Back and Next. Without JS every step shows.
  each('[data-vz-steps]', function (box) {
    var steps = [].slice.call(box.querySelectorAll('.vz-step'));
    var nav = box.querySelector('.vz-stepnav');
    if (!steps.length || !nav) return;
    var back = nav.querySelector('[data-step="-1"]'), next = nav.querySelector('[data-step="1"]');
    var label = nav.querySelector('output'), at = 0;
    function show() {
      steps.forEach(function (s, i) { s.hidden = i !== at; });
      if (label) label.textContent = 'Step ' + (at + 1) + ' of ' + steps.length;
      back.disabled = at === 0;
      next.disabled = at === steps.length - 1;
    }
    back.addEventListener('click', function () { if (at > 0) { at--; show(); } });
    next.addEventListener('click', function () { if (at < steps.length - 1) { at++; show(); } });
    show();
  });

  // Picker: each answer adds points to the options named in its data-fit.
  each('[data-vz-pick]', function (box) {
    var cards = [].slice.call(box.querySelectorAll('[data-opt]'));
    var out = box.querySelector('.vz-out');
    function run() {
      var pts = {};
      cards.forEach(function (c) { pts[c.getAttribute('data-opt')] = 0; });
      each('input:checked', function (el) {
        (el.getAttribute('data-fit') || '').split(' ').forEach(function (o) { if (o in pts) pts[o]++; });
      }, box);
      var best = Math.max.apply(null, cards.map(function (c) { return pts[c.getAttribute('data-opt')]; }));
      var names = [];
      cards.forEach(function (c) {
        var on = best > 0 && pts[c.getAttribute('data-opt')] === best;
        c.classList.toggle('is-hi', on);
        if (on) names.push(c.querySelector('.vz-t').textContent);
      });
      if (out) out.textContent = best > 0 ? 'Closest fit for your answers: ' + names.join(' or ') + '.' : 'Pick an answer above.';
    }
    each('input', function (el) { el.addEventListener('change', run); }, box);
    run();
  });
})();
