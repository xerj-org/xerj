# Kibana feedback · master index

Generated 2026-04-14 · 
**15282** real artifacts across **5** primary sources.

## Sources

| Source | Items |
|--------|------:|
| `github` | 9230 |
| `stackoverflow` | 2836 |
| `hackernews` | 1504 |
| `reddit` | 962 |
| `discourse` | 750 |
| **TOTAL** | **15282** |

## Sentiment (engine-classified, ±)

| Class | Count | Share |
|-------|------:|------:|
| negative | 5801 | 38.0% |
| neutral  | 7760 | 50.8% |
| positive | 1721 | 11.3% |

## Categories (primary)

| Category | Items | Negative | Positive | Top sources |
|----------|------:|---------:|---------:|-------------|
| [`10-docs-and-ux`](categories/10-docs-and-ux/README.md) | 3610 | 717 | 554 | gith=2087 hack=634 stac=561 |
| [`04-performance-and-load`](categories/04-performance-and-load/README.md) | 2387 | 1867 | 103 | gith=1349 disc=337 stac=333 |
| [`01-dashboard-authoring`](categories/01-dashboard-authoring/README.md) | 2367 | 907 | 342 | gith=1187 stac=676 redd=247 |
| [`02-discover-and-kql`](categories/02-discover-and-kql/README.md) | 1740 | 736 | 206 | gith=979 stac=522 hack=101 |
| [`99-noise`](categories/99-noise/README.md) | 891 | 0 | 0 | gith=891 |
| [`15-plugin-and-ecosystem`](categories/15-plugin-and-ecosystem/README.md) | 717 | 336 | 50 | gith=553 stac=81 hack=58 |
| [`09-upgrades-and-migration`](categories/09-upgrades-and-migration/README.md) | 602 | 321 | 43 | gith=444 stac=65 disc=52 |
| [`03-visualization-quality`](categories/03-visualization-quality/README.md) | 542 | 151 | 76 | gith=444 hack=44 stac=39 |
| [`05-index-patterns-data-views`](categories/05-index-patterns-data-views/README.md) | 451 | 172 | 50 | gith=205 stac=198 disc=24 |
| [`99-uncategorized`](categories/99-uncategorized/README.md) | 356 | 60 | 30 | gith=337 disc=19 |
| [`07-alerting-and-watcher`](categories/07-alerting-and-watcher/README.md) | 349 | 171 | 51 | gith=191 stac=52 hack=47 |
| [`12-maps-and-geo`](categories/12-maps-and-geo/README.md) | 335 | 104 | 65 | stac=125 gith=124 redd=36 |
| [`06-sharing-and-export`](categories/06-sharing-and-export/README.md) | 291 | 72 | 40 | gith=158 stac=71 hack=27 |
| [`13-observability-apm`](categories/13-observability-apm/README.md) | 251 | 77 | 41 | gith=115 hack=65 stac=36 |
| [`08-spaces-and-rbac`](categories/08-spaces-and-rbac/README.md) | 213 | 63 | 28 | gith=90 stac=61 redd=34 |
| [`14-siem-and-security`](categories/14-siem-and-security/README.md) | 126 | 29 | 33 | gith=53 hack=31 redd=26 |
| [`11-ml-and-anomaly`](categories/11-ml-and-anomaly/README.md) | 54 | 18 | 9 | gith=23 hack=16 redd=11 |

## Top tags

| Tag | Count |
|-----|------:|
| `discover` | 945 |
| `ci-noise` | 891 |
| `alerting` | 349 |
| `saved-objects` | 269 |
| `license` | 231 |
| `apm` | 193 |
| `anomaly` | 158 |
| `fleet` | 131 |
| `lens` | 121 |
| `canvas` | 86 |
| `dark-mode` | 72 |
| `tsvb` | 65 |
| `siem` | 63 |
| `slow-dashboard` | 59 |
| `ml` | 45 |
| `kql` | 44 |
| `oom` | 35 |
| `time-picker` | 33 |
| `data-views` | 27 |
| `filter-bar` | 14 |
| `csv-export` | 10 |
| `pdf-export` | 9 |
| `security-app` | 6 |
| `upgrade-break` | 4 |

## Pipeline

```
node pipeline/collect.mjs   # pull from APIs, write sources/<src>/raw.jsonl
node pipeline/classify.mjs  # rewrite enriched.jsonl + category pointers
node pipeline/stats.mjs     # rewrite this file + per-category READMEs
```

See `SCHEMA.md` and `TAXONOMY.md` for definitions.