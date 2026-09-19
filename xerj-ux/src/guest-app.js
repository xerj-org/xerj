// ============================================================
// XERJ Console — guest shell
//
// What a person sees after opening a share link and typing the passcode: a
// read-only view of exactly the shared indices — what is in them (CORPUS) and
// a reader for any record with the records linked to it (READER) — under a
// banner that says so and says when it ends.
//
// This is a SEPARATE entry point from the operator console (app.js), on
// purpose. It imports only what a viewer needs:
//
//   data/guest.js · data/transport-guest.js · data/reader-api.js ·
//   data/search-body.js · data/schema-roles.js · data/catalog.js ·
//   ux/safe-dom.js · ux/reader-render.js · ux/corpus-render.js ·
//   ux/reader-view.js
//
// — no dashboard registry, no dashboard store, no prefs sync, no data-source
// manager, no mock data, and nothing that knows the console API exists.
// test/guest-graph.test.mjs walks this import graph and fails if that ever
// stops being true. At runtime `installGuestGuard` refuses any request that
// is not one of the share's four operations before it leaves the page.
//
// No markup strings: all DOM is built by ux/safe-dom.js.
// ============================================================

import {
  readShare, clearShare, isExpired, guestBanner, installGuestGuard, watchExpiry, SHARE_KEY,
  markEnded, readEnded, clearEnded,
} from './data/guest.js';
import { makeGuestTransport } from './data/transport-guest.js';
import { makeReaderApi, FATAL_KINDS } from './data/reader-api.js';
import { h, mount } from './ux/safe-dom.js';
import { renderCorpus } from './ux/corpus-render.js';
import { ReaderView, parseReaderRoute } from './ux/reader-view.js';

const ENDED_TEXT = {
  expired:      ['THIS SHARE HAS EXPIRED', 'Ask the person who sent it for a new link.'],
  unauthorized: ['THIS SHARE IS NO LONGER VALID', 'It was revoked or has expired. Ask the person who sent it for a new link.'],
  invalid:      ['THIS SHARE LINK IS NOT VALID', 'It has expired or was not opened from a share page. Open the link you were sent again.'],
  left:         ['YOU LEFT THE SHARED VIEW', 'The access key was removed from this browser tab. Open the share link again to come back.'],
};

/** The screen after a guest session ends. Deliberately says nothing about
 *  the engine, the index, or why a request was refused. */
export function endedScreen(reason) {
  const [title, body] = ENDED_TEXT[reason] || ENDED_TEXT.invalid;
  return h('div', { class: 'guest-ended', role: 'status', 'data-guest-ended': reason in ENDED_TEXT ? reason : 'invalid' },
    h('div', { class: 'key' }, 'XERJ · SHARED VIEW'),
    h('h1', { class: 'h-scene' }, title),
    h('p', null, body),
    // This tab keeps showing this screen on reload (data/guest.js#markEnded).
    // The way out for the engine's own operator: forget that, go to sign-in.
    h('p', { class: 'faint' }, h('button', { class: 'text-btn', type: 'button', 'data-guest-operator': '1' }, 'OPERATOR SIGN-IN')));
}

/** Wire the ended screen's one button: forget the marker, open the login. */
function wireEnded(app, win) {
  const btn = app.querySelector('[data-guest-operator]');
  if (btn) btn.addEventListener('click', () => { clearEnded(win.sessionStorage); win.location.href = '/_xerj-console/login'; });
}

/** Did every shared index answer? Only then is the corpus view's data kept. */
export function corpusComplete(summaries) {
  return Array.isArray(summaries) && summaries.length > 0 && summaries.every((x) => x && !x.error);
}

export function bootGuest(win = window) {
  const doc = win.document;
  const app = doc.getElementById('app');
  const share0 = readShare(win.sessionStorage);
  if (!share0) {
    // No usable record: either one that fails validation (cleared here), or a
    // reload after a share ended in this tab — say again how it ended.
    const hadRecord = (() => { try { return win.sessionStorage.getItem(SHARE_KEY) != null; } catch { return false; } })();
    const reason = hadRecord ? 'invalid' : (readEnded(win.sessionStorage) || 'invalid');
    clearShare(win.sessionStorage);
    markEnded(win.sessionStorage, reason);
    mount(app, endedScreen(reason));
    wireEnded(app, win);
    app.setAttribute('aria-busy', 'false');
    return null;
  }
  clearEnded(win.sessionStorage); // a live share: whatever ended here before is history

  // The live session. `end()` nulls it, after which every transport call
  // refuses locally ('expired') — nothing keeps using a key we dropped.
  const session = { share: share0 };
  installGuestGuard(share0, win);
  const live = () => session.share;
  const transport = makeGuestTransport(share0);
  // Re-check the clock and the ended flag on every call, not just at boot.
  for (const op of ['search', 'mapping', 'ego']) {
    const inner = transport[op];
    transport[op] = (...args) => {
      if (!live() || isExpired(live())) { end('expired'); return Promise.reject(Object.assign(new Error('expired'), { kind: 'expired' })); }
      return inner(...args);
    };
  }
  const api = makeReaderApi(transport);

  let ended = false;
  let cancelExpiry = () => {};
  let bannerTimer = null;
  let corpusState = { status: 'loading' };

  const view = new ReaderView({
    api, guest: true,
    indices: () => (live() ? [...live().indices] : []),
    onFatal: (kind) => end(kind === 'expired' ? 'expired' : 'unauthorized'),
  });

  function end(reason) {
    if (ended) return;
    ended = true;
    session.share = null;
    cancelExpiry();
    if (bannerTimer) win.clearInterval(bannerTimer);
    view.detach();
    clearShare(win.sessionStorage);
    markEnded(win.sessionStorage, reason);
    win.removeEventListener('hashchange', route);
    doc.removeEventListener('visibilitychange', onVisible);
    win.removeEventListener('storage', onStorage);
    mount(app, endedScreen(reason));
    wireEnded(app, win);
  }

  // ----- chrome ------------------------------------------------------
  mount(app, [
    h('header', { class: 'guest-bar', role: 'status', 'data-guest-banner': '1' },
      h('span', { class: 'guest-bar__text mono', 'data-guest-banner-text': '1' }),
      h('button', { class: 'text-btn guest-bar__leave', 'data-guest-leave': '1' }, 'LEAVE')),
    h('nav', { class: 'guest-nav', 'aria-label': 'Shared view' },
      h('a', { class: 'guest-nav__item', href: '#/corpus', 'data-guest-nav': 'corpus' }, 'CORPUS'),
      h('a', { class: 'guest-nav__item', href: '#/reader', 'data-guest-nav': 'reader' }, 'READER')),
    h('div', { class: 'guest-main', 'data-guest-main': '1' }),
    h('footer', { class: 'guest-foot mono faint' },
      'Read-only view of a shared XERJ index. Nothing else on this engine is visible from here, and nothing you do here changes it.'),
  ]);
  app.setAttribute('aria-busy', 'false');
  doc.title = 'Shared view · XERJ';
  const main = app.querySelector('[data-guest-main]');
  const bannerText = app.querySelector('[data-guest-banner-text]');
  const paintBanner = () => { if (live()) mount(bannerText, guestBanner(live())); };
  paintBanner();
  bannerTimer = win.setInterval(paintBanner, 30_000);
  app.querySelector('[data-guest-leave]').addEventListener('click', () => end('left'));

  // ----- routes ------------------------------------------------------
  let corpusSeq = 0;
  async function showCorpus() {
    view.detach();
    mount(main, h('div', { class: 'guest-scene' },
      h('div', { class: 'key' }, 'WHAT WAS SHARED WITH YOU'),
      h('h1', { class: 'h-scene' }, 'SHARED CORPUS'),
      h('div', { 'data-guest-corpus': '1' })));
    const slot = main.querySelector('[data-guest-corpus]');
    mount(slot, renderCorpus(corpusState, { guest: true, brain: null }));
    // Only a COMPLETE answer is kept. A summary that failed (the engine was
    // restarting, the network blinked) is shown — and asked for again every
    // time this view is entered, exactly as the Reader re-asks after a failed
    // search. The first version cached whatever came back, so one failed load
    // read "engine unreachable" for the life of the tab (PR #945 review).
    if (corpusState.status === 'ok' && corpusComplete(corpusState.summaries)) return;
    const s = live();
    if (!s) return;
    const seq = ++corpusSeq;
    const summaries = await Promise.all(s.indices.map((i) => api.indexSummary(i)));
    if (ended || seq !== corpusSeq) return;
    const fatal = summaries.find((x) => FATAL_KINDS.has(x.kind));
    if (fatal) { end(fatal.kind === 'expired' ? 'expired' : 'unauthorized'); return; }
    corpusState = { status: 'ok', summaries };
    if (slot.isConnected) mount(slot, renderCorpus(corpusState, { guest: true, brain: null }));
  }

  function showReader(hash) {
    mount(main, h('div', { class: 'guest-scene' },
      h('div', { class: 'key' }, 'ONE RECORD, AND WHAT LINKS TO IT'),
      h('h1', { class: 'h-scene' }, 'READER'),
      h('div', { 'data-guest-reader': '1' })));
    view.attach(main.querySelector('[data-guest-reader]'));
    view.setRoute(parseReaderRoute(hash));
  }

  function route() {
    if (ended) return;
    if (!live() || isExpired(live())) { end('expired'); return; }
    const hash = win.location.hash || '';
    const isReader = /^#\/reader(\?|$)/.test(hash);
    for (const a of app.querySelectorAll('[data-guest-nav]')) {
      a.classList.toggle('active', a.getAttribute('data-guest-nav') === (isReader ? 'reader' : 'corpus'));
    }
    if (isReader) showReader(hash); else showCorpus();
  }

  function onVisible() {
    // Timers are throttled in background tabs; re-check the clock on return.
    if (doc.visibilityState === 'visible' && live() && isExpired(live())) end('expired');
  }
  function onStorage(e) {
    // sessionStorage is per-tab, but be exact anyway: if our record goes
    // away or changes under us, this session is over.
    if (e.storageArea === win.sessionStorage && (e.key === SHARE_KEY || e.key === null)) end('left');
  }

  cancelExpiry = watchExpiry(share0, () => end('expired'));
  win.addEventListener('hashchange', route);
  doc.addEventListener('visibilitychange', onVisible);
  win.addEventListener('storage', onStorage);
  route();
  return { end, view };
}
