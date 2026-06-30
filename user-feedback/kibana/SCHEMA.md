# Kibana feedback artifact schema

Every artifact is a single normalized JSON record appended to the source's
`raw.jsonl` file under `sources/<source>/`. A record looks like this:

```json
{
  "id": "hn-42173842",
  "source": "hackernews",
  "source_url": "https://news.ycombinator.com/item?id=42173842",
  "parent_url": "https://news.ycombinator.com/item?id=42173000",
  "posted_at": "2024-11-14T22:14:00Z",
  "captured_at": "2026-04-14T18:22:41Z",
  "author": "someuser",
  "score": 42,
  "kind": "comment",
  "title": "Kibana is a pain in the ass — but what's the alternative?",
  "body": "We've run Kibana at $dayjob for four years and every upgrade breaks our dashboards. The KQL parser rewrote twice…",
  "product": ["kibana", "elasticsearch"],
  "versions": ["8.13", "8.14"],
  "lang": "en",
  "sentiment": "negative",
  "category": "09-upgrades-and-migration",
  "secondary_categories": ["01-dashboard-authoring"],
  "tags": ["upgrade-break", "kql", "dashboards", "production"],
  "quote": "every upgrade breaks our dashboards"
}
```

## Required fields

| Field         | Type     | Notes                                                 |
|---------------|----------|-------------------------------------------------------|
| `id`          | string   | `{source_prefix}-{source_id}`, globally unique        |
| `source`      | enum     | `hackernews` `reddit` `stackoverflow` `github` `webreviews` |
| `source_url`  | string   | Canonical URL the user would click                    |
| `posted_at`   | ISO 8601 | When the comment / issue was written                  |
| `captured_at` | ISO 8601 | When the collector fetched it                         |
| `kind`        | enum     | `story` `comment` `issue` `question` `answer` `review`|
| `title`       | string   | May be empty for comments                             |
| `body`        | string   | Main text content (plain, no HTML)                    |
| `product`     | string[] | Lowercase product labels mentioned                    |

## Optional / enriched fields (populated by classifier)

| Field                  | Type     | Notes                                                 |
|------------------------|----------|-------------------------------------------------------|
| `author`               | string   | Username / handle                                     |
| `score`                | number   | Upvotes / reactions / HN points                       |
| `versions`             | string[] | Extracted product versions (`7.17`, `8.13`, etc.)     |
| `lang`                 | string   | Language code — default `en`                          |
| `sentiment`            | enum     | `positive` `neutral` `negative`                       |
| `category`             | string   | Primary category slug — see `TAXONOMY.md`             |
| `secondary_categories` | string[] | Other categories this artifact also touches          |
| `tags`                 | string[] | Free-form tags (kql, lens, spaces, dark-mode, ...)    |
| `quote`                | string   | 1-line representative quote for later surfacing       |
| `parent_url`           | string   | For comments — the parent story/thread                |

## Storage layout

```
sources/
  hackernews/        raw.jsonl
  reddit/            raw.jsonl
  stackoverflow/     raw.jsonl
  github/            raw.jsonl
  webreviews/        raw.jsonl
```

One line per artifact. JSONL is chosen over per-file markdown so 5000+ items
stay grep-able, rsync-friendly, and cheap to update. Per-artifact markdown is
generated on demand by `pipeline/export-markdown.mjs` (not required to use
the data).

## Classification output

`categories/<slug>/pointers.jsonl` holds one line per artifact assigned to
that category, in the form:

```json
{ "id": "hn-42173842", "source": "hackernews", "score": 42, "tags": ["upgrade-break","kql"] }
```

Pointers are a projection of `raw.jsonl` — to read the full body, look the id
up in the matching source file. Each classifier re-run rewrites pointers from
scratch so there's no drift.

## Identity and dedupe

Artifacts are keyed by `source:id`. A given run re-fetches items and
**replaces** an existing entry with the same id so re-runs are idempotent.
Cross-source duplicates (same URL on HN + Reddit) are kept separately and
linked via `source_url` clustering in `themes/cross-source-clusters.md`.

## Privacy

Only public data. Usernames and profile handles are captured verbatim
because they are part of the attribution. No email, IP, or private-message
content is captured. Web review snippets under `webreviews/` are summaries,
not full review text — they cite the source URL instead.
