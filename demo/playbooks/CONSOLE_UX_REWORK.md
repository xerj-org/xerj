# XERJ Console — UX Rework: Integration + Visual-Regression Verification

**Date:** 2026-07-13
**Build:** xerj v1.0.0-rc.3, merged binary (`engine/target/release/xerj`, rebuilt 08:51 — re-bundles `xerj-ux/` via `xerj-console-api/build.rs`).
**Scope:** Integrate the two console-rework branches, rebuild, boot fresh, and prove each of the 4 validated problems from `CONSOLE_UX_SMOKE.md` is FIXED — under the same visual design language / brandbook.
**Server:** `XERJ_DISABLE_QUERY_CACHE=1 xerj --insecure --data-dir /home/claude/xerj-ux-rework` → ES-compat :9200, Console at `http://localhost:9200/_xerj-console/`.
**Auth:** first-launch passkey enrol via Puppeteer + CDP virtual authenticator (same harness as the smoke run). Driver: `scratchpad-ux/smoke-after.js`. `GET /api/v1/me` → `200 {role: owner}`.

AFTER screenshots: `demo/screenshots/ux-rework/`. BEFORE baseline: `demo/screenshots/ux-smoke/`. Raw capture (network log, DOM measurements, backend JSON, on-disk proofs): `scratchpad-ux/after-*.json`.

---

## Integration

Two file-disjoint branches, both forked from `66bd53a` (just before the smoke commit), merged to `main` conflict-free:

| Branch | Touches | Merge commit |
|---|---|---|
| `feat/console-dashboards-backend` | `engine/crates/xerj-console-api/` only (dashboards CRUD gaps + panel schema + first-launch **seeding** `seed.rs`) | `9a5e06a` |
| `worktree-wf_d2791a67-0d6-4` | `xerj-ux/` only (durable CRUD wiring, panel builder, free-form grid, chrome fix) | `040646d` |

The smoke playbook + baseline screenshots survived both 3-way merges (neither branch ever contained them). Full scoped rebuild: `cargo build --release -j 16 -p xerj-server` (exit 0). The new frontend modules are confirmed bundled in the binary (`dashboard-api.js`, `panel-query.js`, `panel-render.js` all served `200`; unique tokens `renderUserPanelChrome` / `data-builder-save` / `panelResult` present in the binary).

**What wires the fix:** `index.html` boot now runs, before app.js first paint: `GET /me` → `sync.pullAll()` → **`dashboard-store.hydrate()` (GET /dashboards)**. `dashboard-store.js` is an async facade over the new `dashboard-api.js` CRUD client; `createUserDashboard()` **POSTs** a real doc and every panel mutation **PATCHes** it. The backend seeds 13 built-in dashboards as durable `default`/`managed` rows on first launch (`.xerj_dashboards` had 14 docs on a fresh boot before any user action).

---

## Issue 1 — new-from-scratch dashboard now builds REAL panels (was: blank placeholder)

**BEFORE:** a net-new/blank dashboard rendered the `blankRender()` placeholder ("cloned from a template that no longer exists…"); content only came from cloning a code template. No panel builder.

**AFTER — FIXED.** `createUserDashboard({name:'AFTER · NET-NEW DASH'})` creates a **declarative** dashboard. An empty net-new dashboard shows `EMPTY DASHBOARD · CLICK + ADD PANEL BELOW TO BUILD YOUR FIRST PANEL` and a real inline builder (`02`): VISUALIZATION tiles (metric/spark/gauge/line/bar/histogram/dist/topn/treemap/table/events), an INDEX picker, query-kind/field/metric controls, and a **LIVE PREVIEW**. I built a panel (viz=topn, index=`logs-prod`, kind=terms, field=`level`) via the builder UI; it renders a live data-driven TOP-N (`info 18 / warn 8 / error 4`, i.e. 60/26.7/13.3%) through the shared chart catalog — **not** a placeholder.

- Measured (`after-issue1.json`): `userPanelCount:1`, `isBlankPlaceholder:false`, `unconfigured:false`, real chart present.
- **Evidence:** `02-issue1-builder-open.png`, `03-issue1-netnew-real-panel.png`.
- **Cause fixed:** `xerj-ux/src/app.js` (`renderBuilder`/`commitBuilder`/`renderDeclarativePanel` path), `xerj-ux/src/data/panel-query.js`, `xerj-ux/src/ux/panel-render.js`.

## Issue 2 — custom dashboards are now DURABLE at the backend (KEY — was: localStorage-only, lost on cache clear)

**BEFORE:** zero non-GET requests to `/_xerj-console/api/v1/dashboards`; the CRUD backend had no caller; `GET /dashboards` returned `{dashboards:[],total:0}`; clearing localStorage lost the dashboard.

**AFTER — FIXED.** Full durability chain proven four independent ways:

1. **Writes hit the real CRUD API** (baseline was **0**). Network captured **1 POST + 3 PATCH** to `/api/v1/dashboards[/…]` (`after-dashboards-crud-writes.json`): create → add-panel → resize → move. Final doc `version:4`.
2. **`GET /api/v1/dashboards` (authed) returns it** with the panel: `total` includes id `d04bb8e5-…`, `panels.length:1` (`after-backend-check.json`).
3. **Survives a full `localStorage.clear()` + reload** (`08`): the dashboard is still in the nav (USER → AFTER · NET-NEW DASH) and its TOP-N panel re-renders the live data — restored by boot `hydrate()` from the backend, since LS was empty (`after-issue2-clearls.json`: `navHasDash:true`, `userPanelRendered:true`).
4. **Survives a full server restart** (browser-independent, on-disk): the doc is in the engine's `.xerj_dashboards` index — `curl :9200/.xerj_dashboards/_search {ids:[…]}` returns `name:"AFTER · NET-NEW DASH", visibility:private, version:4, panels:1, panel0.query.index:logs-prod, geometry:{x:1,y:1,w:8,h:3}` **after the process was killed and rebooted on the same data-dir**.

- **Evidence:** `08-issue2-after-localstorage-clear.png`; `after-{backend-check,issue2-clearls,dashboards-crud-writes,network}.json`.
- **Cause fixed:** new `xerj-ux/src/data/dashboard-api.js` (the missing CRUD caller); `dashboard-store.js` rewritten as async write-through facade; `index.html` boot calls `hydrate()`; backend `dashboards.rs` create/replace/patch/delete + `seed.rs` first-launch seeding.

## Issue 3 — edit-mode chrome no longer overlaps panel titles or the sub-nav (re-measured)

**BEFORE:** the `panel-edit` chrome vertically overlapped the panel `.key` (title) on dense 2-col tiles (`chromeOverlapsKey:true` for tokens/spend/…); the top EDIT-MODE strip overlapped the dashboard sub-nav.

**AFTER — FIXED.** Re-measured on the same dense `ai-overview` (13 panels) in edit mode (`after-issue3.json`): **`anyChromeOverlapsKey:false`** across all 13 panels, and **`stripOverlapsNav:false`** (edit strip at top≈628 sits far below the sub-nav at bottom≈94). `.panel.edit` now reserves a 24px `padding-top` strip; chrome is `position:absolute` inside it (grip top-left, size toolbar + ✕ top-right); the EDIT-MODE strip is `sticky` in normal flow below the sub-nav.

- **Evidence (contrast):** BEFORE `ux-smoke/06-issue3-edit-mode-full.png` (strip on sub-nav, "COL 2/12 ✕" on titles) vs AFTER `ux-rework/06-issue3-edit-mode-full.png` + `07-issue3-panel-overlap-zoom.png`.
- **Cause fixed:** `xerj-ux/src/app.js` (`renderEditChrome`, `editStripFlow` in normal flow) + `xerj-ux/assets/base.css` (`.panel.edit{padding-top:24px}`, `.panel-edit{position:absolute}`, `.edit-strip-top{position:sticky}`).

## Issue 4 — free-form resize + move (independent w/h/x/y), not 6 discrete spans

**BEFORE:** resize = 6 discrete column spans only (`["2","3","4","6","8","12"]`), no height control, `hasFreeResizeHandle:false`; move = reorder only.

**AFTER — FIXED.** User (declarative) panels carry free-form geometry `{x,y,w,h}` with real drag affordances: `data-resize="e|s|se"` handles + a `data-grip` move handle (`after-issue4.json`: all present). Simulated a real SE-corner pointer drag and a grip drag:

| step | geometry |
|---|---|
| initial | `{x:0, y:0, w:6, h:2}` |
| after SE resize | `{x:0, y:0, w:8, h:3}` — **width AND height changed independently** |
| after grip move | `{x:1, y:1, w:8, h:3}` — **x AND y changed** |

`widthChanged:true`, `heightChanged:true`, `positionChanged:true`. The final geometry `{x:1,y:1,w:8,h:3}` is the geometry persisted in the backend doc (Issue 2). Discrete presets remain as a convenience but are no longer the only path.

- **Evidence:** `04-issue4-free-resize.png` (panel grown to `8×3`), `05-issue4-free-move.png`.
- **Cause fixed:** `xerj-ux/src/app.js` (`renderUserPanelChrome`, `panelGeom`/`compact`, `startGeomDrag`/`moveGeomDrag`/`endGeomDrag`, `userPanelsMutate`), `base.css` `.panel-resize.*` / `.panel-grip`.

---

## ES-compat gate (merged binary)

Full ES-YAML suite against the merged binary (fresh `/tmp/xerj-gate` data-dir, `es-yaml-runner --dir tests/es-compat-yaml/yaml`):

```
1360 passed · 0 failed · 3 skipped · 1363 total
```

**1360 / 0 / 3 — gate held.** No regressions from the backend changes.

## Brandbook / visual regression

No redesign — the rework works like Kibana under the same skin. The design language is intact across every AFTER screenshot vs the baseline: Big Shoulders Display headlines, JetBrains Mono labels, the yellow `#EEBB00` accent, the dark canvas, the dashed edit frame, typography-first panels (tokens: `xerj-ux/assets/tokens.css` + `base.css`; brandbook: `landing/brandbook/`). New surfaces (builder, free-form chrome, resize handles) reuse the existing token/label vocabulary. Compare `ux-smoke/01-console-landing.png` ↔ `ux-rework/01-console-landing.png`.

---

## Summary

| # | Issue | Before | After | Status |
|---|-------|--------|-------|--------|
| 1 | New-from-scratch builder | blank placeholder | live data-driven builder + panels | **FIXED** |
| 2 | Backend durability (KEY) | localStorage-only, 0 CRUD writes, lost on clear | POST/PATCH, GET shows it, survives LS-clear **and** server restart | **FIXED** |
| 3 | Edit chrome overlap | overlaps titles + sub-nav | `anyChromeOverlapsKey:false`, `stripOverlapsNav:false` | **FIXED** |
| 4 | Shallow customization | 6 discrete spans, reorder-only | free-form w/h resize + x/y move (`6×2`→`8×3`→`@1,1`) | **FIXED** |

**Gate:** 1360/0/3. **Brandbook:** no regression. All four validated problems are fixed at genuine Kibana quality under the existing XERJ skin.
