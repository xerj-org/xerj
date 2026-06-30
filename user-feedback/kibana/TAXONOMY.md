# Kibana feedback taxonomy · 15 categories

Each category has a slug, a short definition, the pain axis it covers, and
the keyword rules the classifier uses. Pain axis is one of:
`AUTHORING`, `QUERYING`, `RENDERING`, `SCALING`, `OPS`, `UX`, `TRUST`.

Add a category only when at least 20 real artifacts already land in it
elsewhere — we don't speculate.

---

## 01-dashboard-authoring        · AUTHORING
Building and editing visualizations and dashboards: Lens, TSVB, Vega, Canvas,
drag-and-drop pains, config dialogs, missing chart types, broken filters.
**Keywords:** `lens`, `tsvb`, `canvas`, `vega`, `visualization editor`,
`visualize`, `dashboard builder`, `drag and drop`, `chart type`.

## 02-discover-and-kql           · QUERYING
Discover, search bar, KQL / Lucene syntax, field selection, scroll/paging,
saved searches, deep pagination, "no results" confusion.
**Keywords:** `discover`, `kql`, `lucene`, `search bar`, `saved search`,
`scroll api`, `document table`.

## 03-visualization-quality      · RENDERING
Gaps in the chart catalog, missing chart types, broken stacking, axis bugs,
legend truncation, can't compare series, no diff-views.
**Keywords:** `pie chart`, `stacked bar`, `axis`, `legend`, `tooltip`,
`bug in chart`, `can't plot`.

## 04-performance-and-load       · SCALING
Slow dashboards, spinner hell, timeouts, first paint, too-many-requests,
large index pattern pain, browser memory blow-up, tab lockups.
**Keywords:** `slow`, `loading forever`, `spinner`, `timeout`, `timed out`,
`hang`, `freeze`, `out of memory`, `performance`.

## 05-index-patterns-data-views  · UX
Index patterns / data views renames, field refresh, wildcard pattern pain,
runtime fields, scripted fields, mapping explosion surfaced through UI.
**Keywords:** `index pattern`, `data view`, `field refresh`, `runtime field`,
`scripted field`, `mapping`.

## 06-sharing-and-export         · UX
PDF reports, CSV export, embedded iframes, URL state explosions, short URLs,
reporting queue stuck.
**Keywords:** `pdf`, `csv export`, `reporting`, `embed`, `iframe`, `share`,
`short url`, `reporting queue`.

## 07-alerting-and-watcher       · OPS
Watcher → Alerting → Rules migration, rule authoring, connector config,
noisy alerts, silent alerts, delivery failures.
**Keywords:** `watcher`, `alerting`, `rule`, `connector`, `slack`,
`opsgenie`, `pagerduty`, `notification`.

## 08-spaces-and-rbac            · OPS
Spaces, role mapping, feature privileges, API key management, confusing
permissions, "I can see it but can't edit it".
**Keywords:** `spaces`, `rbac`, `role mapping`, `feature privilege`,
`api key`, `permission`, `403`.

## 09-upgrades-and-migration     · TRUST
Major-version upgrades breaking dashboards, saved objects migration errors,
no skip-version path, upgrade assistant failures.
**Keywords:** `upgrade`, `migration`, `saved object`, `broken dashboard`,
`version upgrade`, `8.0`, `7.17`.

## 10-docs-and-ux                · UX
Navigation changes, inconsistent terminology, dark mode issues, modal soup,
filter-bar pain, time picker confusion.
**Keywords:** `navigation`, `confusing`, `dark mode`, `filter bar`,
`time picker`, `ui`, `docs`, `documentation`.

## 11-ml-and-anomaly             · UX
ML features — anomaly detection, data frame analytics, paywall complaints,
accuracy problems, job setup.
**Keywords:** `anomaly detection`, `machine learning`, `ml job`,
`data frame`, `transform`, `paywall`.

## 12-maps-and-geo               · RENDERING
Maps, ems tiles, geo_point rendering, offline tiles, choropleth, heatmap.
**Keywords:** `maps`, `ems`, `geo_point`, `choropleth`, `heatmap`,
`offline tiles`, `geojson`.

## 13-observability-apm          · AUTHORING
APM UI, traces, services inventory, logs-metrics-traces correlation,
slow service map, integration breakage.
**Keywords:** `apm`, `service map`, `transaction`, `span`, `traces`,
`observability`, `uptime`.

## 14-siem-and-security          · OPS
SIEM app, detections engine, timelines, cases, noisy alerts, rule tuning,
endpoint integration.
**Keywords:** `siem`, `detection`, `case`, `timeline`, `security`,
`endpoint`, `fleet`.

## 15-plugin-and-ecosystem       · TRUST
Plugin breakage on upgrade, plugin API instability, missing / removed
features, community plugins dying, Elastic internal rewrites.
**Keywords:** `plugin`, `extension`, `api change`, `removed`, `deprecated`,
`breaking change`.

---

## Rules for assignment

1. Each artifact gets **one primary** category (the single best fit).
2. Up to **three secondary** categories for cross-cutting concerns.
3. If a classifier score is below threshold, the artifact lands in
   `99-uncategorized/` to be triaged by hand.
4. Re-running the classifier reassigns — pointer files are deterministic.
5. Never silently drop artifacts. Every row in a source file must appear in
   at least one category (possibly `99-uncategorized`).
