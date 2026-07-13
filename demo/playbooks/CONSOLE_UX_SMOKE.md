# XERJ Console — UX Smoke Test / Issue Reproduction

**Date:** 2026-07-13
**Build:** xerj v1.0.0-rc.3 (`engine/target/release/xerj`)
**Scope:** Validation only — reproduce 4 reported Console SPA issues with live evidence. No product code was modified.
**Server:** `XERJ_DISABLE_QUERY_CACHE=1 xerj --insecure --data-dir /home/claude/xerj-ux-smoke` → ES-compat :9200, Console at `http://localhost:9200/_xerj-console/`
**Seed data:** `logs-prod` index, 5 docs (so Discover/dashboards render against real data; dashboards themselves render from `data/mock.js`).

Screenshots: `demo/screenshots/ux-smoke/`. Raw capture (network log, DOM dumps, backend JSON): produced under `scratchpad-ux/` during the run (not committed).

---

## Auth method used

First-launch auth is passkey/WebAuthn enrollment (no bypass exists even under `--insecure`; the console still redirects unauthenticated users to `/_xerj-console/login`). I completed enrollment programmatically with **Puppeteer + a CDP virtual authenticator**:

```
WebAuthn.enable
WebAuthn.addVirtualAuthenticator {protocol:'ctap2', transport:'internal',
  hasResidentKey:true, hasUserVerification:true, isUserVerified:true,
  automaticPresenceSimulation:true}
```

Then drove `setup.html`: consumed the boot setup token (`.../setup#token=…`), filled email/display, clicked *Enrol passkey*. `navigator.credentials.create()` was serviced by the virtual authenticator, `/auth/passkey/finish` set the session cookie, and the page redirected to the console. Verified: `GET /_xerj-console/api/v1/me` → `200 {role: "owner", email: "owner@xerj.local"}`. No dev/insecure bypass was needed or used.

Driver script: `scratchpad-ux/smoke.js`.

---

## Issue 1 — Dashboards are a fixed, code-defined set (no true new-from-scratch)

**Status: REPRODUCED (with a correction to the count).**

The dashboard list is defined entirely in JS at build time. There are **8 hardcoded dashboards** under the *Dashboards* section, not 15:
`ai-overview, rag-quality, vector-index, agent-memory, logs-overview, anomaly-detect, ingest-pipeline, system` — plus **5 fixed single-view sections** (`discover, alerts, data, users, settings`) = **13 registered views total**. The sub-nav buckets the 8 into groups AI / Logs / Infra (screenshot 02 nav dump matches exactly: `AI · OVERVIEW / RAG · QUALITY / VECTOR · INDEX / AGENT · MEMORY / LOGS / INFRA`).

"New dashboard" and "Clone" exist (Settings → MANAGE view, handlers `data-mg-new` / `data-mg-clone` in `app.js`), but:
- A net-new "blank" dashboard has no panel builder — it renders the `blankRender()` placeholder ("This user dashboard was cloned from a template that no longer exists…"). Real content only comes by cloning a code template, whose `render` is inherited wholesale.
- Naming is a `window.prompt()`.
- Every create path funnels through `createUserDashboard()` which writes **only to localStorage** (see Issue 2).

**Evidence:** `02-issue1-dashboard-list.png`; `scratchpad-ux/nav.txt`.
**Cause:** `xerj-ux/src/dashboards/registry.js` (the `all` array is the entire universe of dashboards); create/clone UI at `xerj-ux/src/app.js:1117-1150`; blank placeholder at `xerj-ux/src/data/dashboard-store.js` `blankRender()`.
**Severity: Medium** — product limitation / expectation gap vs. Kibana, not a data-loss bug on its own.

---

## Issue 2 — Custom dashboards are NOT persisted at the backend (KEY ISSUE)

**Status: REPRODUCED — and worse than reported.**

I created a custom dashboard in-session via the exact store function the MANAGE UI calls:
`createUserDashboard({ name: 'SMOKE Custom Dash', fromId: 'ai-overview' })` → id `user-blrz2tho2f`. After reload it renders in the nav under the **OTHER** group and shows the cloned AI-Overview panels (screenshot 03, title "SMOKE CUSTOM DASH").

**The backend never learns about it.** Full network capture over the entire session:

1. **Zero non-GET requests to the real CRUD API** `/_xerj-console/api/v1/dashboards`. Count captured = **0** (`dashboards-crud-writes.json`). Grep of the whole frontend confirms no source file references that endpoint: `grep -rn "api/v1/dashboards" xerj-ux/src` → *NONE*.
2. `GET /_xerj-console/api/v1/dashboards` (authed, in-page) **after** creating the dashboard →
   `200 {"data":{"dashboards":[],"total":0}}`. The dedicated `.xerj_dashboards` index (which has a full create/list/get/replace/patch/delete API in `engine/crates/xerj-console-api/src/dashboards.rs`) is **empty**.
3. The only durable write path the SPA uses is `xerj-console-sync.js`, which mirrors the `xerj.dashboards` localStorage blob into the generic `/prefs` doc. In this run **even that did not carry the dashboard**: the single `PUT /prefs` observed was
   `{"theme":"dark","time":"24H","cluster":"LOCAL","mobile":"false","edit":"1"}` — **no `dashboards` key** — and `GET /prefs` returned the same four keys. Root cause of that secondary miss: `startPush()` snapshots current localStorage as the "already-pushed" baseline at boot, and the push is diff-triggered — a dashboard that already exists in localStorage at load time is treated as clean and is never sent.

**Durability proof (same authenticated browser):** cleared `localStorage` and reloaded. `xerj.dashboards` came back **`null`** (`after-clear-ls.json`) — the custom dashboard is **gone**, because nothing durable held it. Screenshot 04 shows the console fell back to the default AI-Overview.

**Fresh browser context:** new context → redirected to `/_xerj-console/login`, `localStorage.xerj.dashboards = null` (screenshot 05, `fresh-ctx.json`). No server-side record to restore from.

**Net:** a custom dashboard survives a *plain* reload only because of localStorage; it has **no backend durability at all** — not in the `.xerj_dashboards` CRUD index (never written), and not even reliably in `/prefs`. Clear the browser or switch machines and it is lost.

**Evidence:** `03-issue2-custom-in-nav.png`, `04-issue2-after-localstorage-clear.png`, `05-issue2-fresh-context.png`; `scratchpad-ux/{network.json, dashboards-crud-writes.json, backend-check.json, after-clear-ls.json, fresh-ctx.json, created.json}`.
**Cause:** `xerj-ux/src/data/dashboard-store.js` (all mutations `localStorage`-only); `xerj-ux/src/xerj-console-sync.js` (mirrors to `/prefs`, never to `/dashboards`; diff-baseline bug drops the blob); the real CRUD API `engine/crates/xerj-console-api/src/dashboards.rs` has **no frontend caller**.
**Severity: High** — silent user-data loss; the built, tested backend feature is entirely unwired.

---

## Issue 3 — Edit-mode chrome overlaps panel content and the sub-nav

**Status: REPRODUCED.**

Toggling EDIT overlays: a full-canvas dashed `edit-frame`, a top status strip ("EDIT MODE · DRAG PANEL TO REORDER · CLICK A NUMBER TO RESIZE · ✕ TO REMOVE · SCROLL FOR + ADD"), a 12-span `edit-grid` overlay, and per-panel `panel-edit` chrome (COL x/12 label + meter + `2·3·4·6·8·12` size buttons + ✕ remove), plus `draggable="true"` on every panel.

Measured overlap (`overlap.json`): the `panel-edit` chrome vertically overlaps the panel's own eyebrow/`.key` in the tested panels (`chromeOverlapsKey: true` for tokens, cost/spend, savings, cacheHit, queriesSeries). Visually (screenshot 06) the `COL 2/12 … ✕` bars sit on top of the panel titles ("TOKENS · IN + OUT", "SPEND · USD") on the narrow 2-col tiles, and the top EDIT-MODE strip overlaps the AI/RAG/VECTOR/… dashboard sub-nav row. `hasGridOverlay: true`, `gridSpans: 12`.

**Evidence:** `06-issue3-edit-mode-full.png` (clear collision on the small tiles + sub-nav), `07-issue3-panel-overlap-zoom.png`; `scratchpad-ux/overlap.json`.
**Cause:** `xerj-ux/src/app.js` — `renderEditChrome()` (`~:557`), per-panel chrome injected as first child of `.panel` at `:599-608`, grid/frame/strips at `:700-722`.
**Severity: Medium** — cosmetic/usability; the chrome is legible on 6- and 12-col panels but collides on the dense 2-col row and against the sub-nav.

---

## Issue 4 — Panel customization is shallow (fixed spans, no free resize)

**Status: REPRODUCED.**

Customization enumerated live (`customization.json`):
- **Resize = 6 discrete column spans only:** `["2","3","4","6","8","12"]` (the `SIZES` array). No free/continuous resize, no row-height control. `hasFreeResizeHandle: false`.
- **Move = drag-to-reorder only** (`draggablePanels: 13`) — reflow within the 12-col grid; no free x/y placement.
- **Remove:** ✕ per panel (`removeButtons: 13`). **Add:** a fixed `add-picker` of chart types.
- **No per-panel reconfiguration:** default panels render from their code template (`p.render`); there is no query/field/agg/viz editor. Added panels are limited to the built-in `chartTypes` with mock data.

This is far shallower than Kibana's per-panel query + viz editor and free grid placement.

**Evidence:** `06-issue3-edit-mode-full.png` (size buttons `2·3·4·6·8·12` visible); `scratchpad-ux/customization.json`.
**Cause:** `xerj-ux/src/app.js` — `const SIZES = [2,3,4,6,8,12]` (`:558`), `mutate(... ov.cols[pid] = cols)` (`:972`); layout override stores only a `cols` map (`:541`), no geometry/config.
**Severity: Medium** — product/expectation gap vs. Kibana.

---

## Summary

| # | Issue | Reproduced | Severity |
|---|-------|-----------|----------|
| 1 | Hardcoded dashboard set, no true from-scratch builder | Yes (8 dashboards + 5 sections, not 15) | Medium |
| 2 | Custom dashboards not persisted at backend | **Yes — no `/dashboards` write ever; lost on LS-clear** | **High** |
| 3 | Edit-grid / chrome overlaps content + sub-nav | Yes | Medium |
| 4 | Shallow panel customization (fixed spans) | Yes | Medium |

**Headline:** Issue 2 is real and the most serious — the frontend has an entire durable dashboards CRUD backend (`.xerj_dashboards` index + `dashboards.rs`) that **no code path calls**; custom dashboards live only in localStorage (and not even reliably in the `/prefs` mirror), so they vanish on cache-clear or a new device.
