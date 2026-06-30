# Kibana feedback corpus

A structured, reproducible corpus of real user feedback about Kibana, pulled
from primary public sources and classified into product-relevant categories.
This is the input we use to decide what XERJ.ai should and shouldn't
build.

> **15,282 artifacts · 5 sources · 17 categories · last refreshed 2026-04-14**

## What's inside

```
kibana/
  README.md                  ← you are here
  INDEX.md                   ← master stats table (numbers, sources, sentiment)
  SCHEMA.md                  ← artifact JSON schema (frontmatter, fields)
  TAXONOMY.md                ← 15 Kibana-specific categories + classifier rules
  sources/
    hackernews/raw.jsonl     ← 1,504 HN comments + stories about Kibana
    hackernews/enriched.jsonl
    reddit/raw.jsonl         ← 962 Reddit posts (r/elasticsearch, r/devops, r/sysadmin, …)
    reddit/enriched.jsonl
    stackoverflow/raw.jsonl  ← 2,836 SO questions tagged kibana / kibana-N / lens-kibana / …
    stackoverflow/enriched.jsonl
    github/raw.jsonl         ← 9,230 issues across elastic/kibana, elastic/elasticsearch,
    github/enriched.jsonl       elastic/eui, opensearch-project/OpenSearch-Dashboards (auth via GH_TOKEN)
    discourse/raw.jsonl      ← 750 posts from discuss.elastic.co (Elastic forum)
    discourse/enriched.jsonl
  categories/
    01-dashboard-authoring/  ← README.md + pointers.jsonl
    02-discover-and-kql/
    03-visualization-quality/
    04-performance-and-load/
    05-index-patterns-data-views/
    06-sharing-and-export/
    07-alerting-and-watcher/
    08-spaces-and-rbac/
    09-upgrades-and-migration/
    10-docs-and-ux/
    11-ml-and-anomaly/
    12-maps-and-geo/
    13-observability-apm/
    14-siem-and-security/
    15-plugin-and-ecosystem/
    99-noise/                ← CI failures from elastic/kibana, filtered out
    99-uncategorized/        ← classifier escape hatch (currently 1 item)
  themes/
    top-pains.md             ← top 12 negative items per category, ranked, quoted
    top-asks.md              ← top 50 GitHub feature requests by reactions
    cross-source-clusters.md ← regex patterns matching the same complaint
                                across HN + Reddit + SO + GitHub
  pipeline/
    collect.mjs              ← pulls from the 5 APIs into sources/ (per-repo flush, 422-aware)
    classify.mjs             ← keyword → category, sentiment, tags (deterministic)
    stats.mjs                ← writes INDEX.md and per-category READMEs
    themes.mjs               ← writes themes/*.md from negative-sentiment items
    design-inputs.mjs        ← writes themes/design-inputs.md + the JS module
                                 web/src/data/feedback-citations.js that the
                                 dashboards import for their citation panels
```

## How to refresh

Zero dependencies. Pure Node 22 ESM, only built-in `fetch`.

```bash
cd user-feedback/kibana

# pull from all 5 sources. With GH_TOKEN set, the GitHub pull is the slow
# step (~8 min for 4 repos × 100 pages). Without auth, GH skips after page 5.
GH_TOKEN=$GH_TOKEN node pipeline/collect.mjs

# or one source at a time
node pipeline/collect.mjs --source github --pages 100

# classify everything (deterministic, idempotent)
node pipeline/classify.mjs

# write INDEX.md + per-category READMEs
node pipeline/stats.mjs

# write themes/*.md
node pipeline/themes.mjs

# write design-inputs.md + dashboard-citations.json + the web/ JS module
node pipeline/design-inputs.mjs
```

A full refresh including GitHub auth takes ~10 minutes wall-clock. JSONL files
are append-only and deduped on `id`, so re-runs are safe and only fetch new
items. The collector flushes per-repo for GitHub so killing the run mid-way
preserves whatever already finished.

## Headline numbers (2026-04-14)

| | items | negative | positive | neutral |
|---|---:|---:|---:|---:|
| **TOTAL** | **15,282** | **5,801** (38.0%) | **1,721** (11.3%) | **7,760** (50.8%) |

**Top 5 categories by volume**

1. `10-docs-and-ux` — 3,610 items (general UI/nav + catch-all)
2. `04-performance-and-load` — 2,387 items, **78% negative** ← single biggest pain signal
3. `01-dashboard-authoring` — 2,367 items (Lens, TSVB, Canvas, Vega, dashboards)
4. `02-discover-and-kql` — 1,740 items (Discover, KQL, search bar)
5. `15-plugin-and-ecosystem` — 717 items (plugin breakage, EUI internal rewrites)

**Top 5 categories by negative ratio** (signal of pain, not just volume)

1. `04-performance-and-load` — 1,867 / 2,387 negative (**78%**)
2. `09-upgrades-and-migration` — 321 / 602 negative (53%)
3. `07-alerting-and-watcher` — 171 / 349 negative (49%)
4. `15-plugin-and-ecosystem` — 336 / 717 negative (47%)
5. `02-discover-and-kql` — 736 / 1,740 negative (42%)

**Top 10 GitHub feature requests** (by reactions, real elastic/kibana issues)

| Reactions | Comments | Title |
|----------:|---------:|-------|
| 469 | 401 | Nested field support |
| 372 | 571 | Export Documents as CSV |
| 363 | 227 | Option to show percentage |
| 178 | 88 | Export chart to image |
| 162 | 156 | Resizable discover table columns |
| 146 | 268 | Extract log event context |
| 121 | 84 | Change linked saved search in editor |
| 113 | 144 | Pipeline aggregations |
| 88 | 83 | Per-visualization local time picker |
| 77 | 86 | Y scale to fit each split chart |

These tell us what to ship in XERJ.ai v1: **CSV export, image export,
nested fields, resizable columns, per-panel time, local timezones, pipeline
aggs**. Every one of those has been requested for years by hundreds of users.

## Sources and methodology

| Source | API | Auth | Pulled |
|--------|-----|------|-------:|
| GitHub Issues | api.github.com `/repos/{owner}/{repo}/issues` (4 repos) | `GH_TOKEN`, 5K/hr | 9,230 |
| Stack Overflow | api.stackexchange.com `/2.3/questions` (13 tags) | none | 2,836 |
| Hacker News | hn.algolia.com `/api/v1/search_by_date` (25 queries) | none | 1,504 |
| Reddit | `reddit.com/.json` per-subreddit search (12 queries) | none | 962 |
| Discourse | `discuss.elastic.co/search.json` (20 queries) | none | 750 |

GitHub is the deepest source because the **`elastic/kibana`** repo (1,141),
**`elastic/elasticsearch`** (1,030), **`elastic/eui`** (3,832 — internal
component library, where most plugin-breakage lives), and
**`opensearch-project/OpenSearch-Dashboards`** (3,044 — the open-source fork's
issue tracker) collectively give us 9,230 structured, scored, attributable
items. Every GitHub Issues endpoint hits the same 10K-result cap (page 100);
the collector breaks early on `422 Unprocessable Entity` and moves to the
next repo.

**To scale further still** beyond 15K:
- Web-scrape Gartner Peer Insights / G2 / TrustRadius reviews (rate-limited,
  needs WebFetch — there's a `webreviews/` slot already prepared)
- Pull Stack Exchange answers (currently we only pull questions)
- Use the GitHub `/repos/{owner}/{repo}/discussions` and
  `/repos/{owner}/{repo}/issues/comments` endpoints to add comment threads
- Add YouTube comments via the YouTube Data API
- Add `elastic/beats`, `elastic/integrations`, `elastic/logstash` repos

The collector is structured so each new source is one new function in
`pipeline/collect.mjs` and one new entry in `SOURCES`.

## Schema

Every artifact is one JSONL line with this shape (full schema in `SCHEMA.md`):

```json
{
  "id": "hn-42173842",
  "source": "hackernews",
  "source_url": "https://news.ycombinator.com/item?id=42173842",
  "posted_at": "2024-11-14T22:14:00Z",
  "captured_at": "2026-04-14T18:22:41Z",
  "author": "someuser",
  "score": 42,
  "kind": "comment",
  "title": "...",
  "body": "...",
  "product": ["kibana"],
  "versions": ["8.13"],
  "lang": "en",
  "sentiment": "negative",
  "category": "09-upgrades-and-migration",
  "secondary_categories": ["01-dashboard-authoring"],
  "tags": ["upgrade-break", "saved-objects"]
}
```

## How this informs XERJ.ai

Every category has a `README.md` listing its top 20 items by score. Every
category has a `pointers.jsonl` mapping ids back to the full bodies in
`sources/`. The `themes/top-pains.md` file is a single-page synthesis of
the highest-impact negative artifacts — that's what to read first.

When a XERJ.ai feature is being designed, the first question to ask
is: **which category does it serve, and what's in that category's top 20?**
If the answer is "nothing", the feature is speculative. If the answer is "12
of these 20", it's worth shipping.

## Honest limits

- Sentiment is a naive lexicon classifier — it confuses sarcasm and
  technical "the bug is" framing. Treat the absolute counts with care; the
  *relative* ranking across categories is what's reliable.
- The 99-noise bucket catches `Failing test:` issues from CI but is not
  exhaustive. ~146 items.
- Some categories (`12-maps-and-geo`, `06-sharing-and-export`) are
  under-represented because their language is generic — many real complaints
  about maps just say "the map".
- The classifier is keyword rules, not an LLM. Ten minutes of `node
  pipeline/classify.mjs` swap-out turns it into an LLM call against XERJ.ai's
  own embed proxy. Schema doesn't change.
- We pull only English-language artifacts. Non-English Reddit/SO posts are
  dropped by the `kibana` substring guard.
- Public data only. No private forum exports, no email, no support tickets.

## Privacy

Only public, attribution-bearing data. Usernames are kept verbatim because
they're the attribution. No PII beyond what the user themselves chose to
publish. The `webreviews/` directory (when populated) holds short summaries
of Gartner / G2 reviews that link back to the source instead of duplicating
the full text.
