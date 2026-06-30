# Design inputs · top 50 actionable Kibana pains

Generated 2026-04-14 from `themes/top-asks.md` + `themes/top-pains.md`. Each entry maps a real, scored, attributable user pain to the XERJ.ai response.

Use this as the brief for the next dashboard sprint. If you ship a feature that is **not** in this list, you are speculating.

---

## 01-dashboard-authoring

XERJ.ai response: `web/src/dashboards/* + edit-mode card grid · panels are JS objects, no rewrite-induced regressions`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 1 | github | 178 | Export chart to image | [↗](https://github.com/elastic/kibana/issues/1366) |
| 2 | stacko | 68 | How to do &quot;where not exists&quot; type filtering in Kibana/ELK? | [↗](https://stackoverflow.com/questions/27537521/how-to-do-where-not-exists-type-filtering-in-kibana-elk) |
| 3 | github | 46 | [Reporting] Exporting raw data from table-based visualizations | [↗](https://github.com/elastic/kibana/issues/30982) |
| 4 | github | 43 | Add bucket_selector aggregation to visualizations | [↗](https://github.com/elastic/kibana/issues/17544) |
| 5 | github | 42 | Nested field support in visualizations | [↗](https://github.com/opensearch-project/OpenSearch-Dashboards/issues/657) |
| 6 | github | 41 | Multi-select (OR) dashboard filtering | [↗](https://github.com/elastic/kibana/issues/3693) |
| 7 | github | 39 | Customizing data table visualization column width | [↗](https://github.com/elastic/kibana/issues/2516) |
| 8 | github | 38 | Panel that shows the latest value of a field | [↗](https://github.com/elastic/kibana/issues/678) |
| 9 | github | 38 | Row and column mode in data table vis | [↗](https://github.com/elastic/kibana/issues/3620) |
| 10 | github | 33 | Change visualisation chart type | [↗](https://github.com/elastic/kibana/issues/1607) |
| 11 | github | 22 | Trends | [↗](https://github.com/elastic/kibana/issues/2647) |
| 12 | github | 12 | [migration v6.5] Another Kibana instance appears to be migrating the index | [↗](https://github.com/elastic/kibana/issues/25464) |

## 02-discover-and-kql

XERJ.ai response: `web/src/dashboards/search-discover.js · live query box, 8 query types, plan tree`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 13 | github | 143 | [Dashboard] Allow Authors to Limit Interactivity | [↗](https://github.com/elastic/kibana/issues/9575) |
| 14 | github | 64 | Custom drilldown links for a dashboard panel | [↗](https://github.com/elastic/kibana/issues/12560) |
| 15 | github | 57 | Ability to change the index pattern on a visualization | [↗](https://github.com/elastic/kibana/issues/17542) |
| 16 | github | 44 | Possibility to hide time field in Discover view | [↗](https://github.com/elastic/kibana/issues/3319) |

## 03-visualization-quality

XERJ.ai response: `web/src/ux/charts*.js · 23 primitives, all 1px, no chart furniture`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 17 | github | 71 | Resizable legends | [↗](https://github.com/elastic/kibana/issues/3189) |

## 04-performance-and-load

XERJ.ai response: `engine/xerj-engine + xerj-storage · single binary, no GC, mmap, doc-values (G2)`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 18 | reddit | 162 | OpenSearch (Elasticsearch open source fork) joins the Linux Foundation | [↗](https://www.reddit.com/r/devops/comments/1fi05mp/opensearch_elasticsearch_open_source_fork_joins/) |
| 19 | reddit | 88 | Docker for Windows doesn’t work for large projects, even for development, so disappointing | [↗](https://www.reddit.com/r/docker/comments/ldzvxy/docker_for_windows_doesnt_work_for_large_projects/) |
| 20 | github | 40 | add command line option to execute the optimize task standalone | [↗](https://github.com/elastic/kibana/issues/6057) |
| 21 | github | 31 | Conditional XY bar metric colors | [↗](https://github.com/elastic/kibana/issues/4482) |
| 22 | github | 9 | An alternative to relative imports for local source code | [↗](https://github.com/elastic/kibana/issues/40446) |
| 23 | github | 8 | Introduce view and edit modes for Dashboards | [↗](https://github.com/elastic/kibana/issues/9431) |

## 05-index-patterns-data-views

XERJ.ai response: `engine/xerj-api · /v1/indices schema is code, not a clickable wizard`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 24 | github | 57 | Remove index pattern mapping cache | [↗](https://github.com/elastic/kibana/issues/6498) |
| 25 | github | 45 | Per-user profiles, settings in Kibana | [↗](https://github.com/elastic/kibana/issues/17888) |
| 26 | github | 39 | Timelion query language support for scripted fields | [↗](https://github.com/elastic/kibana/issues/9022) |
| 27 | github | 18 | Exclude system indices from matching index templates | [↗](https://github.com/elastic/elasticsearch/issues/42508) |

## 06-sharing-and-export

XERJ.ai response: `TODO · CSV/PNG export from any panel · static URL is enough`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 28 | github | 372 | Export Documents as CSV | [↗](https://github.com/elastic/kibana/issues/1992) |
| 29 | github | 21 | Timepicker as part of embedded dashboard | [↗](https://github.com/elastic/kibana/issues/2739) |

## 07-alerting-and-watcher

XERJ.ai response: `TODO · rules as code, NL→rule, no separate Watcher app`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 30 | stacko | 71 | worker_connections are not enough | [↗](https://stackoverflow.com/questions/28265717/worker-connections-are-not-enough) |
| 31 | github | 10 | Kibana sometimes sends HTTP requests to Elasticsearch without credentials | [↗](https://github.com/elastic/kibana/issues/9583) |

## 08-spaces-and-rbac

XERJ.ai response: `TODO · single XERJ.ai token, indices ARE the boundary`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 32 | github | 18 | RBAC - Phase 3 - Feature Controls | [↗](https://github.com/elastic/kibana/issues/20277) |

## 09-upgrades-and-migration

XERJ.ai response: `web/src/dashboards/* schema · upgrades can't break dashboards because dashboards are code`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 33 | reddit | 109 | Is there a 'boring' alternative to ElasticSearch? | [↗](https://www.reddit.com/r/linuxadmin/comments/ikx0y9/is_there_a_boring_alternative_to_elasticsearch/) |
| 34 | discou | 62 | Replace the kibana logo? | [↗](https://discuss.elastic.co/t/replace-the-kibana-logo/27547/13) |
| 35 | github | 45 | Ability to export the index pattern along with saved objects | [↗](https://github.com/elastic/kibana/issues/4288) |
| 36 | github | 45 | Expose object import/export as an API | [↗](https://github.com/elastic/kibana/issues/4759) |
| 37 | github | 38 | Saved object authorization - Phase 1 | [↗](https://github.com/elastic/kibana/issues/4453) |
| 38 | github | 15 | RBAC, OLS and Spaces | [↗](https://github.com/elastic/kibana/issues/18473) |

## 10-docs-and-ux

XERJ.ai response: `web/UX_BOOK.md · text is the UI, fewer modals`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 39 | github | 162 | Allow sorting on multiple fields | [↗](https://github.com/elastic/kibana/issues/696) |
| 40 | github | 113 | Pipeline aggregations | [↗](https://github.com/elastic/kibana/issues/4584) |
| 41 | reddit | 89 | Elasticsearch, Kibana, and Fluentd as an alternative to Splunk | [↗](https://www.reddit.com/r/devops/comments/d07ya1/elasticsearch_kibana_and_fluentd_as_an/) |
| 42 | github | 71 | Support Bucket Script Aggregation | [↗](https://github.com/elastic/kibana/issues/4707) |
| 43 | github | 65 | Allow users to specify display names for fields | [↗](https://github.com/elastic/kibana/issues/1896) |
| 44 | stacko | 49 | Where is the kibana error log? Is there a kibana error log? | [↗](https://stackoverflow.com/questions/30855522/where-is-the-kibana-error-log-is-there-a-kibana-error-log) |
| 45 | github | 47 | Anonymous access | [↗](https://github.com/elastic/kibana/issues/18331) |
| 46 | github | 12 | Aliasing imports with `as` | [↗](https://github.com/elastic/kibana/issues/11283) |

## 12-maps-and-geo

XERJ.ai response: `TODO · ASCII region grid, no offline-tile pain`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 47 | github | 16 | Proposal to rename Kibana "Index Patterns" | [↗](https://github.com/elastic/kibana/issues/44955) |

## 15-plugin-and-ecosystem

XERJ.ai response: `web/src/dashboards/registry.js · drop-in JS module, no plugin API to break`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 48 | github | 18 | Kibana Globalization | [↗](https://github.com/elastic/kibana/issues/6515) |

## 99-uncategorized

XERJ.ai response: `—`

| # | Source | Score | Title | Link |
|---|--------|------:|-------|------|
| 49 | github | 50 | Pivot table | [↗](https://github.com/elastic/kibana/issues/5049) |
| 50 | github | 41 | Add a configuration setting for default "Rows Per Page" setting in Management | [↗](https://github.com/elastic/kibana/issues/56406) |
