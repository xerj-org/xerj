// Day/Night theme for xerj.org. Loaded blocking in <head>, before style.css,
// so a remembered choice applies before the first paint instead of flashing
// the default "day". The choice lives in localStorage and is read on every
// page; the DAY/NIGHT buttons (button[data-theme]) write it. See #1973.
(function () {
  var KEY = 'xerj.theme';
  var root = document.documentElement;
  var saved = null;
  try { saved = localStorage.getItem(KEY); } catch (_) {}
  if (saved === 'day' || saved === 'night') root.setAttribute('data-theme', saved);

  function buttons() { return document.querySelectorAll('button[data-theme]'); }
  function apply(theme) {
    root.setAttribute('data-theme', theme);
    var all = buttons();
    for (var i = 0; i < all.length; i++) {
      all[i].classList.toggle('active', all[i].getAttribute('data-theme') === theme);
    }
  }
  function onClick() {
    var theme = this.getAttribute('data-theme');
    apply(theme);
    try { localStorage.setItem(KEY, theme); } catch (_) {}
  }
  document.addEventListener('DOMContentLoaded', function () {
    apply(root.getAttribute('data-theme') || 'day');
    var all = buttons();
    for (var i = 0; i < all.length; i++) all[i].addEventListener('click', onClick);
  });
})();

// GitHub stars badge in the nav (a.nav-stars, written by scripts/seo/stars_badge.py).
// The page ships a build-time count; this refreshes it from the public API at
// most once per 10 minutes per browser (the unauthenticated limit is 60/hour).
// Any failure keeps the shipped number. Rounding matches stars_badge.compact().
(function () {
  var KEY = 'xerj.ghstars', TTL = 600000;
  function fmt(n) { return n < 1000 ? String(n) : (Math.floor(n / 100 + 0.5) / 10).toFixed(1).replace(/\.0$/, '') + 'k'; }
  function show(n) {
    if (!(n > 0) || n !== Math.floor(n)) return;
    var all = document.querySelectorAll('[data-gh-stars]');
    for (var i = 0; i < all.length; i++) {
      all[i].textContent = '\u2605 ' + fmt(n);
      var a = all[i].closest('a');
      if (a) a.setAttribute('aria-label', 'XERJ on GitHub, ' + n.toLocaleString('en-US') + ' stars');
    }
  }
  document.addEventListener('DOMContentLoaded', function () {
    if (!document.querySelector('[data-gh-stars]')) return;
    var c = null;
    try { c = JSON.parse(localStorage.getItem(KEY) || 'null'); } catch (_) {}
    if (c && c.n) show(c.n);
    if (c && Date.now() - c.t < TTL) return;
    if (!window.fetch) return;
    fetch('https://api.github.com/repos/xerj-org/xerj', { headers: { Accept: 'application/vnd.github+json' } })
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (d) {
        var n = d && d.stargazers_count;
        if (!(n > 0)) return;
        show(n);
        try { localStorage.setItem(KEY, JSON.stringify({ n: n, t: Date.now() })); } catch (_) {}
      })
      .catch(function () {});
  });
})();
