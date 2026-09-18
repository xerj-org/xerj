// Theme bootstrap — a classic (blocking) script in <head>, so it runs before
// the stylesheets paint and there is no flash of the wrong theme. It lives in
// a file rather than inline so the console page can be served with a
// `script-src 'self'` Content-Security-Policy (xerj-console-api/src/spa.rs).
(function () {
  var t = null;
  try { t = localStorage.getItem('xerj.theme'); } catch (e) { t = null; }
  if (t !== 'day' && t !== 'night') t = 'night';
  document.documentElement.setAttribute('data-theme', t);
})();
