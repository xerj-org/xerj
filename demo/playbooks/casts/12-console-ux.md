# Embedded Console UX

_Use case doc: Xerj Console (xerj-ux)_

The console SPA ships inside the binary and is served at /_xerj-console/.

### ✅ console SPA shell

```bash
curl -s "http://localhost:9200/_xerj-console/"
```

```json
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Xerj Console · typography-first observability for Xerj</title>
<meta name="color-scheme" content="dark light">
<script>
  // Theme bootstrap — runs before CSS to prevent a flash of wrong theme.
  (function () {
    var t = localStorage.getItem('xerj.theme');
    if (t !== 'day' && t !== 'night') t = 'night';
    document.documentElement.setAttribute('data-theme', t);
  })();
</script>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@400;700;800;900&family=JetBrains+Mono:wght@400;500;700&family=Inter:wght@400;500;600&display=swap" rel="stylesheet">
<link rel="stylesheet" href="assets/tokens.css">
<link rel="stylesheet" href="assets/base.css">
<link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Ctext y='13' font-family='monospace' font-size='14' font-weight='900' fill='%23EEBB00'%3Ez%3C/text%3E%3C/svg%3E">
</head>
<body>
<div id="app" aria-busy="true"></div>
<!--
  Auth guard.  Hits /_xerj-console/api/v1/me before app.js runs.  On 401 we
  redirect to /_xerj-console/login (or /_xerj-console/setup when this is a fresh
  install — th
… (2291 more bytes)
```

_HTTP 200_

### ✅ console app bundle

```bash
curl -s "http://localhost:9200/_xerj-console/src/app.js"
```

```json
// ============================================================
// XERJ.ai — Bootstrap + router + layout engine
//
// One render loop. Edit mode layers drag/resize/remove/add on
// top of the same render — no separate editor routes.
// ============================================================

import { registry, defaults, SECTIONS, DASHBOARD_GROUPS, dashboardsInSection } from './dashboards/registry.js';
import { Nav, SceneHeader, TimeCtrl, RefreshCtrl, FilterBar, ClusterCtrl, SavedViews, Footer, MobileCtrl } from './ux/chrome.js';
import { query, dataSourceStatus } from './data/query.js';
import { chartTypes, chartTypeList } from './ux/chart-types.js';
import { esc } from './ux/text.js';
import { mockSearch } from './data/mock.js';
import { hitsToCsv, downloadText, svgToPng } from './data/export.js';
import {
  mergedDashboards, renameDashboard, reorderDashboards, setHidden,
  createUserDashboard, deleteUserDashboard, isUserDash, resetAll as resetDashboards,
} from './data/dashboard-store.js';
import {
  listClusters, listIndices, listFields, listClustersSync,
  defaultClusterId, setDefaultCluster,
} from './data/data-sources.js';

// ---------- state -----------------------------------------
const LS = {
  theme:   'xerj.theme',
  time:    'xerj.time',
  edit:    'xerj.edit',
  search:  'xerj.search',
  refresh: 'xerj.refresh',
  cluster: 'xerj.cluster',
  views:   'xerj.vi
… (47867 more bytes)
```

_HTTP 200_

### ✅ first-launch setup page

```bash
curl -s "http://localhost:9200/_xerj-console/setup"
```

```json
<!doctype html>
<html lang="en" data-theme="night">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Xerj Console · setup</title>
<meta name="color-scheme" content="dark light">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@800;900&family=JetBrains+Mono:wght@400;500&family=Inter:wght@400;500;600&display=swap" rel="stylesheet">
<link rel="stylesheet" href="assets/tokens.css">
<link rel="stylesheet" href="assets/base.css">
<link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Ctext y='13' font-family='monospace' font-size='14' font-weight='900' fill='%23EEBB00'%3Ez%3C/text%3E%3C/svg%3E">
<style>
  body {
    min-height: 100vh; display: flex; align-items: center; justify-content: center;
    padding: 2rem;
  }
  .auth-card {
    background: var(--bg-surface, #15171a);
    border: 1px solid var(--border, #2a2d33);
    border-radius: 8px; padding: 2rem;
    max-width: 480px; width: 100%;
    color: var(--fg, #e6e8eb);
  }
  .auth-card h1 {
    font-family: 'Big Shoulders Display', sans-serif;
    font-weight: 900; font-size: 2.25rem; letter-spacing: -0.02em;
    margin: 0 0 .5rem; color: #EEBB00;
  }
  .auth-card 
… (6914 more bytes)
```

_HTTP 200_

