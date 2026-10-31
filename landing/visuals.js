// Figures on article pages (content/visuals/*.html). Progressive enhancement:
// every figure reads without this file. With it, stacked panels become ARIA tabs
// (arrow keys, Home, End) and the YAML line checker runs. No network, no storage.
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
})();
