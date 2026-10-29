# `scripts/seo/` — how the xerj.org website is built and kept honest

`landing/` is the website. Cloudflare Pages publishes that directory **verbatim**
(`wrangler.toml`: `pages_build_output_dir = "landing"`), so there is **no build
step at deploy time**: whatever HTML is committed is what the world gets. That
one fact drives everything below. The generators in this directory run on your
machine, write their output into `landing/`, and you commit the output. CI's job
is only to prove that the committed output is what the generators would produce
right now.

Everything here is **Python 3 standard library only**. No npm, no lockfile, no
pip install, no supply-chain surface. `python3 scripts/seo/<tool>.py` from the
repository root is always the invocation.

---

The dependency-free playground state regressions run the committed browser
bundle with controlled browser state: `node --test scripts/landing/test_*.cjs`
(Node 22, also run by the SEO job). To regenerate the playground bundle, run
`esbuild playground/src/app.js --bundle --outfile=playground/app.bundle.js`
from `landing/`. Custom dashboards inherit their template's data and render
functions while keeping their own layout overrides. Clones can themselves be
cloned; missing or cyclic template references produce the blank fallback.

The shared marketing-page copy controls in `landing/interact.js` display
`COPIED` only after the clipboard API or legacy copy command succeeds. A denied
copy displays `COPY FAILED`; visitors can retry or select the command manually.
The temporary feedback restores the original button label. Run the interaction
regressions with `node --test scripts/landing/test_interact.cjs` (Node 18+; no
packages are required).

Dependency-free landing interaction tests run with
`node --test scripts/landing/test_*.cjs` (Node 22) in the SEO job. Playground
PNG export snapshots computed SVG paint styles before rasterization, since a
detached SVG image cannot inherit the page stylesheet or its custom properties.
For the real-browser pixel check, serve the repository root with
`python3 -m http.server 8857 --bind 127.0.0.1`, open
`http://127.0.0.1:8857/scripts/landing/png_export_fixture.html`, and click the
export button. The live query stroke and exported PNG must retain the yellow
crosshair (nonzero yellow pixels and a crosshair pixel distinct from the white
background). The click downloads one PNG for inspection. This is a static
fixture; it does not run XERJ or contact external services.
Regenerate its committed bundle from `landing/` using
`esbuild playground/src/app.js --bundle --outfile=playground/app.bundle.js`.

Playground saved views restore the saved time range, cluster and filters;
custom views also save their FROM/TO endpoints. Legacy saved views without
endpoints retain the current custom endpoints.

## 1. Architecture

```
content/answers/<slug>.md   ─┐                      ┌─> landing/answers/<slug>.html   (page)
content/compare/<slug>.md   ─┤                      ├─> landing/answers/<slug>.md     (Markdown twin)
                             ├─ build_articles.py ──┤─> landing/answers/index.html    (section hub)
scripts/seo/pagedata.py     ─┤                      ├─> landing/answers/index.json    (hub search index)
scripts/seo/urlmap.py       ─┘                      └─> a spliced block in landing/llms.txt

landing/**/*.html  ── fix_heads.py ──> the <!-- seo:begin --> … <!-- seo:end --> block in every <head>, and the GA4 snippet at the top of <head>
landing/**/*.html  ── fix_links.py ──> every internal <a href>, plus the site-map footer block
landing/**/*.html  ── set_nav.py ───> the primary nav in every page
landing/**/*.html  ── gen_sitemap.py ─> landing/sitemap.xml   (deploy only — gitignored)
(brand tokens)     ── mk_og_card.py ──> landing/og/xerj-card.png
```

### What is generated (never hand-edit)

| Path | Written by | How you change it |
|---|---|---|
| `landing/answers/*.html`, `landing/compare/*.html` | `build_articles.py` | edit `content/<section>/<slug>.md` |
| `landing/answers/*.md`, `landing/compare/*.md` (Markdown twins) | `build_articles.py` | edit the same source |
| `landing/answers/index.html`, `landing/compare/index.html` | `build_articles.py` | it is derived from the article set |
| `landing/answers/index.json`, `landing/compare/index.json` | `build_articles.py` | ditto |
| the block between `<!-- BEGIN GENERATED ARTICLE TREE -->` and `<!-- END … -->` in `landing/llms.txt` | `build_articles.py` | ditto — the rest of `llms.txt` is hand-written |
| the block between `<!-- seo:begin … -->` and `<!-- seo:end -->` in every page's `<head>` | `fix_heads.py` | edit `scripts/seo/pagedata.py` |
| the block between `<!-- seo:footer-begin … -->` and `<!-- seo:footer-end -->` | `fix_links.py` | edit `FOOTER_MAP` in `fix_links.py` |
| every internal `<a href>` in `landing/` | `fix_links.py` | write the href you mean; the pass rewrites it to the served form |
| `landing/sitemap.xml` | `gen_sitemap.py` at deploy time | nothing: it is gitignored and `deploy-pages.yml` writes it before the upload |
| `landing/og/xerj-card.png` | `mk_og_card.py` | edit the rasteriser; the brand tokens come from `landing/style.css` |
| the primary nav (`<nav class="nav">`) in every page, and in the `build_articles.py` templates | `set_nav.py` | edit `LEFT` / `RIGHT` in `set_nav.py`, then run it; `--check` verifies |
| the `<a class="nav-stars">` GitHub stars badge in every primary nav (its fallback count) | `stars_badge.py` | run it to refresh the count from the GitHub API; `--check` verifies; `landing/theme.js` refreshes the number in the browser |

Generated article pages and twins carry a first-line marker:

```html
<!-- generated by build_articles.py — edit content/answers/<slug>.md instead -->
```

`build_articles.py --write` **refuses** to overwrite a file in those output
paths that does not carry the marker, and `--check` **fails** on a marked file
that has no matching source. Both directions are guarded.

### What is hand-written

Everything else under `landing/`: the marketing pages, `landing/docs/**`,
`landing/llms.txt` and `landing/llms-full.txt` outside the spliced block,
`landing/robots.txt`, `landing/_headers`, `landing/_routes.json`,
`landing/style.css`, `landing/brandbook/**`. Hand-written pages still have their
`<head>` block and their internal links owned by the generators — you edit the
body copy, `pagedata.py` owns the title/description/JSON-LD, and `fix_links.py`
owns the hrefs and the footer.

---

## Public agent skill discovery

`landing/.well-known/agent-skills/xerj/SKILL.md` is the hand-written public
documentation-routing skill. It is independent of the repository-specific
`tools/xerj-code/SKILL.md`; internal skills are not automatically published.
`python3 scripts/seo/build_skills.py --write` generates the discovery index
from the public skill's frontmatter and the SHA-256 of its exact bytes.
The generator accepts a plain `name` and a JSON-quoted, single-line
`description` (a YAML subset). Run `--check` to detect missing or stale output;
CI also runs `python3 scripts/seo/test_build_skills.py`.

The index follows the draft
[Agent Skills Discovery v0.2.0](https://github.com/cloudflare/agent-skills-discovery-rfc).
Its digest detects a mismatch between the index and fetched artifact; it is
not a signature or a reason to trust or execute instructions. Both resources
are static assets with explicit media types, CORS and cache revalidation in
`landing/_headers`. Their responses request `no-transform` to preserve bytes.
After deployment, verify both GET responses and compute the digest from the
fetched skill bytes. A Python static-file server does not validate Pages'
header handling; the published Cloudflare responses must be checked separately.

## 2. The load-bearing URL fact

**Cloudflare Pages serves this site extensionless.** `html_handling` is at its
default (`auto-trailing-slash`), verified against the live site on 2026-08-20:

```
landing/product.html      ->  https://xerj.org/product          (200)
landing/docs/index.html   ->  https://xerj.org/docs/            (200)
landing/index.html        ->  https://xerj.org/                 (200)

https://xerj.org/product.html    -> 308 -> https://xerj.org/product
https://xerj.org/docs/index.html -> 308 -> https://xerj.org/docs/
https://xerj.org/docs            -> 308 -> https://xerj.org/docs/
```

Exactly one spelling of each URL returns 200; every other spelling permanently
redirects to it.

`scripts/seo/urlmap.py` is the **single source of truth** for that mapping.
`deployed_url(rel)` computes the one 200 URL for a file. Three consumers must
produce **byte-identical strings**:

* `<link rel="canonical">` — `fix_heads.py`
* `<meta property="og:url">` — `fix_heads.py`
* `<loc>` in `sitemap.xml` — `gen_sitemap.py`

`seo_lint.py` re-derives the value independently from `urlmap` and fails the
build if any of the three disagrees. **Do not reimplement this mapping
anywhere.**

Practical rules:

* **Never hand-write a `.html` internal link.** `/docs/foo.html` costs a 308 for
  every user and crawler and splits the internal link graph across two spellings
  of one document. Write `/docs/foo`; if you write the `.html` form,
  `fix_links.py --write` will rewrite it and `--check` will fail until you do.
* Assets are *not* pages. `urlmap._ASSET_SUFFIXES` (`.css .js .json .txt .xml
  .png .svg .pdf .sh .ps1 …`) and `_ASSET_EXACT` (`/get`, `/get.ps1`,
  `/openapi.json`) are left byte-for-byte alone.
* Genuine exceptions live in `urlmap.HREF_FIXUPS`, not in the page. Today that is
  `/get/index.html -> /docs/install` (`landing/get` is a *file*, the installer
  script, so `/get/` has always been a 404) and `/benchmarks -> /benchmarks/`.
* The docs ⌘K search index (`<script type="application/json" id="docs-index">`)
  stores navigation targets in a `slug` field that the UI interpolates straight
  into an `<a href>`. `fix_links.py` rewrites those through the same function —
  do not bypass it.

### 2.1 The second load-bearing Pages fact: `landing/404.html`

Pages also derives its **not-found behaviour** from the published tree, and it
is a fork with only two branches:

* **no top-level `404.html`** — Pages "assumes that you are deploying a
  single-page application" and answers every unmatched path with the root
  `index.html` and an **HTTP 200**;
* **`404.html` present** — Pages walks up from the requested path to the
  nearest `404.html` and serves it with a real **HTTP 404**.

This site was in the first branch until 2026-08-22. Measured on the live
origin that day:

```
GET /                                200  21841 B  ETag "93477f59916732f83dae4e1ce69bc7f9"
GET /answers/what-is-xerj            200  21841 B  ETag "93477f59916732f83dae4e1ce69bc7f9"
GET /this-page-has-never-existed…    200  21841 B  ETag "93477f59916732f83dae4e1ce69bc7f9"
```

Three URLs, one of which has never existed on any branch, one body, one ETag.
That is a **soft 404**: every typo, every dead inbound link and every URL an AI
crawler guesses returns a plausible page instead of an error, each one a
byte-identical duplicate of the homepage, and a page that is removed can never
be signalled as removed. It also silently invalidated the earlier audit of this
site, which only ever requested files that exist.

The cure is the file itself. `landing/404.html` is an ordinary hand-written
page — registered in `pagedata.py`, `<head>` owned by `fix_heads.py`, footer
owned by `fix_links.py` — with two differences:

* it is in `urlmap.NOINDEX`, so it carries `<meta name="robots" content="noindex">`
  (it is reachable at `/404` as a normal document, and an indexed error page is
  worse than no error page);
* it is therefore in `SITEMAP_EXCLUDE`. A sitemap lists pages worth crawling.

**Never add a catch-all to `_redirects`.** `/* /index.html 200` restores the
soft 404 exactly, and a catch-all with a 3xx status is the redirect-shaped
variant Google also files as a soft 404. Named prefixes (`/docs/*`) are fine.
`seo_lint.py` **rule 67** fails the build on a missing `404.html` and on a
catch-all source; `_redirects` carries the same warning in a comment.

`verify_live.py`'s not-found probe is how you confirm it after a deploy — see
§7.

---

## 3. How to add an article

### 3.1 Frontmatter schema

The dialect is a deliberately tiny strict YAML subset implemented in
`scripts/seo/article_data.py` — that file is authoritative; this table is a
summary of it. Unknown fields, duplicate fields, tabs, `null`, empty values and
`{...}` mappings are all hard errors with a `file:line:` location.

Syntax: top-level `field: value` at column 0; list items are `  - item` (exactly
two spaces) or an inline `[a, b]`; nested map keys under a list item are
`    key: value` (exactly four spaces).

**Required** (`article_data.REQUIRED_FIELDS`):

| Field | Type | Rules |
|---|---|---|
| `title` | string | the `<title>`; aim ≤ 60 chars |
| `h1` | string | the on-page `<h1>`. The body must **not** contain an H1 |
| `description` | string | meta description + `og:description`; target 110–165 chars (`seo_lint` rule 28 warns outside it) |
| `slug` | string | `^[a-z0-9]+(-[a-z0-9]+)*$` **and must equal the source filename stem** |
| `cluster` | string | topic cluster, shown in the page kicker |
| `question` | string | the search question the page answers |
| `intent` | string | e.g. `informational`, `commercial` |
| `published` | string | ISO `YYYY-MM-DD` |
| `updated` | string | ISO `YYYY-MM-DD`. Required since #974: it is the page's `dateModified` and must be bumped on every real edit of the source (a new article normally starts with the same value as `published`). Before it was required, sources without it dated themselves from the content file's git committer date, which a squash merge rewrites — main then went red on `build_articles --check` the day the PR merged |
| `author` | string | shown in the source note |
| `reviewer` | string | shown in the source note |
| `schema_type` | string | `TechArticle` or `Article` — nothing else |
| `links_out` | list of string | see 3.2; every item must resolve or the build fails |
| `faq` | list of `{q, a}` | **4 to 8 items**; every `q` must end with `?`. There is **no** rule on how a `q` opens — see 3.2b |

**Optional** (`article_data.TOP_FIELDS` minus the above):

| Field | Type | Rules |
|---|---|---|
| `evidence` | list of `{claim, source}` | `source` is an `https://` URL, a site path starting `/`, or a repo-relative path (optionally `path/to/file.rs:120`) that **must exist on disk**. Repo sources render as provenance text with no href |
| `noindex` | bool | `true` adds `robots: noindex` **and** removes the page from `sitemap.xml` (`urlmap.is_noindex` reads it) |
| `agent_prompt` | string | one logical line, must contain `https://xerj.org/llms.txt`, must not contain a Markdown fence |
| `commands` | list of `{cmd, note}` | `cmd` must shell-parse, and must start with `xerj` + a real subcommand (`autoindex mcp serve verify help version`) or a flag, or with an allowed shell primitive (`bash cd command curl env export git sh until wget`). No `<placeholders>` |
| `answer` | string | **GEO.** 40–60 words, answer-first, restating the question; rendered in a "Short answer" card under the H1 and in the `.md` twin |
| `key_fact` / `key_fact_source` / `key_fact_date` | strings, all three or none | **GEO.** One sentence with a number; source is an `https://` URL, a site path, a repo path that exists (rendered as a GitHub link), or the literal `capture` (the run this page documents); ISO date it was measured or checked |
| `intent_group` | string | **GEO.** The agent-intent cluster (task column of the query research CSV); groups `llms.txt` and drives "same intent" related links |
| `queries` | list of string | **GEO.** Literal queries from the research CSV the page targets (published in `index.json`); omit when none fits |
| `structured_data` | list | opt-in JSON-LD: `faq` → `FAQPage`, `howto` → `HowTo` from `commands:` (≥ 2; command cards get `id="step-N"`), `software`. See trap 6 and lint rules 46/46b. No page sets it in this port |
| `og_image` / `og_image_alt` | string | both or neither; must be `og/<category>-<slug>.png` and exist under `landing/` |
| `last_checked` | string | ISO date every third-party fact was re-read (best-of pages) |
| `items` | list of `{name, url, license, install}` | best-of pages: emitted as `ItemList` of `SoftwareApplication`; `url` must be `https://` |

### 3.2 `links_out` resolution

| You write | It means |
|---|---|
| `what-is-xerj` | a **bare slug** — an article in the *same* section (`/answers/what-is-xerj`) |
| `compare/xerj-vs-vector-database` | a known section prefix (`answers/ compare/ docs/ use-cases/ case-studies/ industries/`) gets a leading `/` |
| `/docs/agents/endpoints` | an absolute site path |
| `https://xerj.org/product` | allowed; any other host is rejected |

Every target is resolved against the real tree. A dangling target is a build
error with the source line, not a 404 in production. An ambiguous bare slug
(same slug in both sections) is also an error — qualify it.

### 3.3 Body rules

* The **first non-empty block must be the TL;DR paragraph**, starting `**TL;DR**`.
  `ste_check.py` STE102 wants it in the 30–60 word AEO extraction window.
* Only ATX `##` and `###` headings. No `#` (the H1 comes from frontmatter), no
  `####`, and no level skips (`##` → `####` is an error).
* **Raw HTML is rejected** outside fenced code blocks. Tables, lists, blockquotes,
  fences, inline code, links and emphasis are supported; anything else is not.
* Never link a Markdown twin with an ordinary `<a href="…md">` — see trap 5.

### 3.4 Minimal working example

```markdown
---
title: "How do I add a new answer article?"
h1: "How do I add a new answer article?"
description: "A throwaway article used to verify the walkthrough in scripts/seo/README.md."
slug: "readme-walkthrough-example"
cluster: "Site maintenance"
question: "How do I add a new answer article?"
intent: "informational"
published: "2026-08-21"
updated: "2026-08-21"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
agent_prompt: "Act as a coding agent. Read https://xerj.org/llms.txt, start XERJ, and index the folder you must search before you send a query."
commands:
  - cmd: "xerj --insecure --data-dir ./.xerj-data"
    note: "Start a local development node."
links_out:
  - "what-is-xerj"
  - "/docs/agents/endpoints"
faq:
  - q: "Where does the source file go?"
    a: "Put the source file in content/answers or in content/compare."
  - q: "Which command writes the HTML?"
    a: "Run python3 scripts/seo/build_articles.py --write from the repository root."
  - q: "Can I edit the generated HTML?"
    a: "No. Edit the Markdown source and run the generator again."
  - q: "Does the file name matter?"
    a: "Yes. The file name must be the same as the slug field."
---

**TL;DR** — Write one Markdown file in `content/answers`, run
`build_articles.py --write`, then run the gates that CI runs. The generator
writes the HTML page, the Markdown twin, the section hub and the section index
for you. You never edit the generated files.

## Source location

The source file goes in `content/answers` or in `content/compare`. The file name
must be the same as the `slug` field.
```

### 3.4b Write the FAQ question the way a person types it

There used to be a validator here that rejected any `faq.q` not opening with
`what|why|how|when|where|which|who|does|do|did|is|are|can|should|will`. **It was
removed deliberately.** These articles are targeted at the literal strings people
send to an assistant, and those strings routinely open with "I…", "My…", "The…",
"Claude Code…" or "Best way…":

```yaml
faq:
  - q: "The indexer died overnight. Do I have to start over?"
  - q: "My codebase indexer says indexed but I don't see my code. How do I check it actually finished?"
  - q: "Best way to search a local docs folder?"
```

The validator forced each of those to be rotated into a grammatically tidy
question, which destroyed the exact match that is the entire mechanism. Copy the
user's phrasing verbatim. The only structural rules left are the 4-to-8 count and
the trailing `?`.

### 3.5 The commands

```sh
# 1. the content gate first — it is cheap and it fails with a file:line
python3 scripts/seo/factcheck.py content/answers/<slug>.md --fail-on error

# optional, advisory only, never blocking: the STE house-style reader
python3 scripts/seo/ste_check.py content/answers/<slug>.md

# 2. generate
python3 scripts/seo/build_articles.py --write
python3 scripts/seo/gen_sitemap.py --write

# 3. prove the tree is self-consistent
python3 scripts/seo/build_articles.py --check
python3 scripts/seo/fix_heads.py  --check
python3 scripts/seo/fix_links.py  --check
python3 scripts/seo/mk_og_card.py --check
python3 scripts/seo/seo_lint.py

# 4. commit, then re-run step 2+3 once — see trap 2
```

`build_articles.py --only <slug>` builds one article and its hub, which is handy
while iterating. Run the full `--write` before committing: `--only` skips the
section `index.json` files and the `llms.txt` splice.

### 3.6 Verified walkthrough (real output)

The article above was actually created, built, gated and deleted while writing
this file. Verbatim:

```
$ python3 scripts/seo/ste_check.py content/answers/readme-walkthrough-example.md --fail-on ERROR
0 finding(s): 0 ERROR, 0 WARN, 0 INFO

$ python3 scripts/seo/factcheck.py content/answers/readme-walkthrough-example.md --fail-on error
1 file(s), 0 ERROR, 0 WARN

$ python3 scripts/seo/build_articles.py --write
wrote 63 changed generated file(s) (121 outputs; 58 selected article(s))

$ git status --short | grep '^??'
?? content/answers/readme-walkthrough-example.md
?? landing/answers/readme-walkthrough-example.html
?? landing/answers/readme-walkthrough-example.md

$ python3 scripts/seo/gen_sitemap.py --write
WARNING: <lastmod> fell back to filesystem mtime for 1 file(s):
  mtime-fallback /workspace/.../landing/answers/readme-walkthrough-example.html
wrote /workspace/.../landing/sitemap.xml (140 URLs, 15645 bytes)

$ python3 scripts/seo/build_articles.py --check
ok   build_articles.py output is current (121 files; 58 selected article(s))

$ python3 scripts/seo/fix_heads.py --check
WARNING: mtime fallback used for 1 file(s): .../content/answers/readme-walkthrough-example.md
ok   every <head> matches the generator

$ python3 scripts/seo/fix_links.py --check
ok   internal links and site-map footer are current

$ python3 scripts/seo/seo_lint.py --quiet
seo_lint: 142 pages · 0 gate violation(s) · 18 warning(s)

$ python3 scripts/seo/factcheck.py --fail-on error
58 file(s), 0 ERROR, 68 WARN  (--explain <rule-id> for the evidence and a rewrite)

$ python3 scripts/seo/ste_check.py content/answers/*.md content/compare/*.md --fail-on ERROR
106 finding(s): 0 ERROR, 0 WARN, 106 INFO
```

Both transcripts above are historical and left verbatim. The `ste_check.py`
lines in them are no longer a gate — the script is advisory now (§7), and
`--fail-on ERROR` is no longer the way to invoke it.

Two things that transcript teaches:

* **One new article rewrites its whole section.** 63 files changed for one added
  article, because the article sidebar (`article_nav`) lists every article in the
  section. A one-article diff touching 60 files is normal, not a bug.
* **An uncommitted source has no git date**, so `<lastmod>`/`dateModified` fall
  back to the filesystem mtime and both tools say so loudly on stderr. That is
  trap 2, visible.

Teardown, also verbatim — note that deleting only the source is deliberately an
error, because an orphaned generated page would otherwise stay live:

```
$ rm content/answers/readme-walkthrough-example.md
$ python3 scripts/seo/build_articles.py --check
build_articles: .../landing/answers/readme-walkthrough-example.html: generated output has no matching Markdown source
(exit 1)

$ rm landing/answers/readme-walkthrough-example.html landing/answers/readme-walkthrough-example.md
$ python3 scripts/seo/build_articles.py --write
wrote 61 changed generated file(s) (119 outputs; 57 selected article(s))
$ python3 scripts/seo/gen_sitemap.py --write
wrote /workspace/.../landing/sitemap.xml (139 URLs, 15527 bytes)
$ git status --porcelain
$                      # empty: the tree round-tripped exactly
```

**To delete an article: remove the source *and* its two generated outputs, then
re-run `build_articles.py --write` and `gen_sitemap.py --write`.**

---

## 4. How to add or edit an ordinary (non-article) landing page

Ordinary pages are hand-written HTML. Their body copy is yours; their `<head>`
is not.

1. Create the file under `landing/` (`landing/foo.html` → `/foo`;
   `landing/foo/index.html` → `/foo/`).
2. **Register it in `scripts/seo/pagedata.py`.** `PAGES` is keyed by the
   landing-relative path and is the only thing that decides a page's title,
   description, JSON-LD type and breadcrumb label. A page missing from `PAGES`
   raises `KeyError: <rel> has no entry in scripts/seo/pagedata.py or content/`
   and every generator stops.

   | Key | Required? | Meaning |
   |---|---|---|
   | `label` | yes | the page's own breadcrumb name (last `ListItem`) |
   | `kind` | yes | `software` \| `collection` \| `techarticle` \| `article` \| `webpage` → `SoftwareApplication` / `CollectionPage` / `TechArticle` / `Article` / `WebPage`, and the matching `og:type` |
   | `description` | recommended | meta + `og:` + `twitter:` description, 110–165 chars. Omit only if the file's existing one is already unique and correctly sized |
   | `title` | optional | overrides the file's `<title>`; omit to keep it |

3. If the page sits under a new directory level, add that level to
   `pagedata.SECTION_HUBS` so the breadcrumb has a real link. Every URL in
   `SECTION_HUBS` must be a page that exists — `seo_lint` enforces it. If there
   is no hub page, leave the level out rather than pointing at a 404.
4. Special cases, all in `urlmap.py`: `NOINDEX` (adds `robots: noindex`),
   `SITEMAP_EXCLUDE` (kept separate from `NOINDEX` on purpose — a page can be
   sitemap-excluded without being noindex, and the lint distinguishes them), and
   `FOOTER_SKIP` for pages that do not load `/style.css` and would render the
   injected footer unstyled.
5. Re-run, in this order:

   ```sh
   python3 scripts/seo/fix_heads.py  --write
   python3 scripts/seo/fix_links.py  --write
   python3 scripts/seo/gen_sitemap.py --write
   python3 scripts/seo/seo_lint.py
   bash .github/scripts/landing-constants-guard.sh
   ```

   Add the page to `fix_links.FOOTER_MAP` if it should be reachable from the
   site-map footer. Google's one stated internal-link rule is that every page you
   care about has a link from at least one other page; the footer is how this
   site satisfies it, and `seo_lint`/`audit.py` will report an orphan.

A page whose copy quotes the ES-YAML conformance figure, the canonical-operation
count, the release version or a kNN exactness claim is also subject to
`landing-constants-guard.sh` — see §7.

---

## 5. The sitemap

`sitemap.xml` is a **deploy artifact, not a committed file**:
`.github/workflows/deploy-pages.yml` runs the generator on a full checkout and
uploads the result with the rest of `landing/`. It is in `.gitignore`, so a
local run never shows up as a change.

```sh
python3 scripts/seo/gen_sitemap.py --write     # write landing/sitemap.xml locally
python3 scripts/seo/gen_sitemap.py --check     # CI gate: the derivation still validates
```

Why it is not committed: every landing edit that forgot to re-run the generator
turned `main` red from `seo.yml` (which reads the file), and every such pull
request carried a two-line `<lastmod>` conflict. `seo_lint.py`, `audit.py` and
`verify_live.py` build the document from the tree when the file is absent, so
they check what the deploy would ship rather than a copy in git.

The file is a **derivation**, not a document:

* one `<url>` per HTML file under `landing/`, minus `urlmap.SITEMAP_EXCLUDE` and
  minus any article whose frontmatter sets `noindex: true`;
* `<loc>` = `urlmap.deployed_url(file)` — absolute, https, extensionless,
  byte-identical to that page's canonical and `og:url`;
* `<lastmod>` = the page's content date (`DateSource.page_lastmod`): article
  `updated:` front matter, else JSON-LD `dateModified`, else a `pagedata.py`
  `updated`, else — on a full clone — the last commit that changed more than
  shared chrome (`<head>`, the primary nav, the generated footer), else mtime.
  `git log -1` of the HTML file is not the date: a sitewide head commit would
  stamp every URL today;
* **no `<priority>`, no `<changefreq>`.** Google ignores both; Bing stated in
  July 2025 that both are ignored and influence nothing. Emitting them is noise;
* **no non-HTML assets.** A `.txt` or `.json` URL produces no search result.
  `llms.txt` is advertised in `robots.txt`, which is where it belongs;
* one flat sitemap, no index file — 185 URLs against a 50,000-URL limit.

**Never hand-edit it.** It is not committed at all — `Deploy site
(Cloudflare Pages)` runs `gen_sitemap.py --write` on a full checkout and
uploads the result with the rest of `landing/`, and `.gitignore` keeps a local
`--write` out of `git status`. Two reasons that matters:

1. The served file is derived from the tree it ships with, so it cannot fall
   behind a landing edit the way a committed copy did.
2. `<lastmod>` is the reason. Google uses `lastmod` only "if it's consistently
   and verifiably accurate", and the judgement is **sitewide**: one page whose
   `lastmod` drifts from reality teaches the crawler to ignore the field for
   every page. The date is the content date above, which is checkable from the
   source front matter, the page's own `dateModified`, or `git log` of a
   content change. The predecessor of this file was hand-maintained — 24
   entries for an 80-page site, 18 of which were URLs that 308-redirected
   rather than pages, four of which were non-HTML assets, and all 24 carrying
   a `lastmod` that disagreed with git. That is the failure this script exists
   to prevent.

When none of the content dates apply and git cannot answer (uncommitted file,
shallow clone, no git), the tool falls back to the filesystem mtime and prints
`WARNING: <lastmod> fell back to filesystem mtime` on stderr. That is why the
deploy checks out with `fetch-depth: 0`: a shallow clone has no commit to date
the page from, and the warning would list every page it restamped to today.

### 5.1 Tell IndexNow after a production deploy

IndexNow lets Bing, Yandex, Seznam, Naver and the other participating engines
recrawl a changed URL at once instead of on their next sitemap read. It is a
manual step, run from a checkout after the production deploy is live. No
workflow runs it.

```bash
python3 scripts/seo/indexnow_ping.py                        # dry run: lists what it would send
python3 scripts/seo/indexnow_ping.py --send                 # every <loc> in landing/sitemap.xml
python3 scripts/seo/indexnow_ping.py --send https://xerj.org/product
python3 scripts/seo/indexnow_ping.py --host hub.xerj.org --send   # reads the live hub sitemap
```

The key is public by design: the engine fetches `https://<host>/<key>.txt` and
compares it with the key in the request. The file is
`landing/e94e889e7bee019e40a7617324e7a20a.txt`, and the hub generator writes
the same file into `tools/corpus-hub-site/out/`. Do not rename or delete it;
a new key needs a new file live on the host first. Send only URLs that
changed, and do not resend an unchanged sitemap on a schedule.

---

## 6. The tools

| Tool | What it does | Modes | A failure means |
|---|---|---|---|
| `urlmap.py` | the URL authority: file → deployed URL, href rewriting, `NOINDEX`/`SITEMAP_EXCLUDE`, git-derived dates | run bare to print the whole map | n/a — it is a library |
| `article_data.py` | strict frontmatter parser + schema validator; shared by the generator, `pagedata` and `urlmap` so article metadata cannot drift | library | `file:line: <message>` — fix the frontmatter |
| `pagedata.py` | hand-written per-page title/description/JSON-LD-kind/breadcrumb table for non-article pages; falls through to article frontmatter | library | `KeyError` — register the page |
| `seo_head.py` | the one implementation of the `<head>` block, shared by `fix_heads.py` and the article generator | library | n/a |
| `build_articles.py` | renders `content/**/*.md` into pages, Markdown twins, hubs, `index.json` and the `llms.txt` article tree | `--write` / `--check`, `--only SLUG`, `--json`, `--allow-ste-errors` | committed HTML is not what the sources produce, or a source is invalid. Run `--write` and commit |
| `gen_sitemap.py` | derives `landing/sitemap.xml` (a deploy artifact, gitignored) from the filesystem and git | `--write` (deploy) / `--check` (validate only) | the derivation no longer produces a valid sitemap |
| `fix_heads.py` | owns canonical, the full Open Graph + Twitter set, JSON-LD and `<title>`/description in every page's `<head>` | `--write` / `--check` | a `<head>` was hand-edited or a page is unregistered. Run `--write` |
| `fix_links.py` | rewrites internal `.html` hrefs to the served extensionless form and regenerates the site-map footer | `--write` / `--check` | someone hand-wrote a `.html` link or edited the footer block. Run `--write` |
| `mk_og_card.py` | deterministically rasterises `landing/og/xerj-card.png` (stdlib scanline rasteriser + zlib PNG writer — no Pillow) | `--write` / `--check` | the committed PNG is not the current render. Run `--write` |
| `seo_lint.py` | the merge gate for the technical-SEO invariants: canonical/OG/JSON-LD sets, sitemap coverage both ways, URL shape, Markdown-twin contract, robots, CSP, fonts, link graph | default gate; `--warnings-as-errors`, `--quiet` | a numbered rule was broken. The message names the rule; the rule is defined and explained at its definition in the file |
| `factcheck.py` | content gate: blocks claims the binary cannot back (S3 backup, RBAC, HA, unqualified "semantic search", retracted benchmark magnitudes) and enforces number provenance. Rules live in `claims_rules.py` | `--fail-on error\|warn`, `--self-test`, `--fixture-check`, `--explain RULE-ID`, `--list-rules`, `--stats`, `--check-matrix` | an article makes an unsupportable claim. **Fix the claim** — see trap 8 |
| `ste_check.py` | ASD-STE100 (Simplified Technical English) house-style reader, plus the article structure checks (H1, TL;DR, FAQ). **Advisory, not a gate** — see §7 | `--fail-on ERROR\|WARN\|INFO`, `--self-test`, `--show`, `--stats`, `--only`, `--skip` | Treat every tier as advice. Never reword a user-phrased H1 or FAQ question to satisfy it |
| `claims_rules.py` | the fact-check rule corpus: each rule carries its evidence citation and a compliant rewrite | data | n/a |
| `audit.py` | the full 12-family technical-SEO audit (thinness, duplication, click depth, page weight, image CLS, …). Also supplies the HTML parser that `seo_lint.py` uses, so the auditor and the gate cannot disagree | `--fail-on`, `--max-findings`, `--json`, `--report` | in CI it is **report-only** (`if: always()`), never blocking |
| `indexnow_ping.py` | **manual, never CI**: sends changed URLs to IndexNow (Bing, Yandex, Seznam, Naver and others). The key file is `landing/<key>.txt`; the hub generator writes the same key into its `out/` | dry run by default; `--send`, `--host xerj.org\|hub.xerj.org`, `[URL ...]` | HTTP 403: the key file is not live on that host yet. 422: a URL is not on the host |
| `verify_live.py` | **post-deploy, never CI**: fetches every `<loc>` from a live origin (the sitemap comes from a local `landing/sitemap.xml` if you just generated it, otherwise from the tree) and checks a literal 200, a body that is not the homepage, and a canonical equal to the requested URL — plus one probe that a never-existing path answers 404 | `[base]`, `--jobs`, `--limit`, `--timeout`, `--json`, `--skip-not-found-probe` | the deploy does not match the tree. On a PR everything fails, which is why it is not a gate — see §7 |

`scripts/seo/testdata/factcheck/*.md` is the fact-check regression suite: 21
fixtures, 14 `bad_` and 7 `good_`, every rule required to own at least one.
`factcheck.py --fixture-check` runs them and prints a confusion matrix.

---

## 7. The gates

`.github/workflows/seo.yml` runs on `landing/**`, `content/**`,
`scripts/seo/**` and the workflow file itself:

| Step | Catches |
|---|---|
| `build_articles.py --check` | committed article HTML/twins/hubs/`index.json`/`llms.txt` block differ from what the sources render; an orphaned generated page; an invalid source; STE ERRORs in any article |
| `gen_sitemap.py --check` | the derivation no longer produces a valid `sitemap.xml` (served URL form, Markdown twins, dead entries). The file itself is built by the deploy, not committed |
| `fix_heads.py --check` | a hand-edited `<head>`; a missing canonical/OG/JSON-LD; a page with no `pagedata.py` entry |
| `fix_links.py --check` | a hand-written `.html` internal link; an edited footer block; a link to a page that does not exist |
| `mk_og_card.py --check` | the committed OG card is not the current render |
| `seo_lint.py` | the numbered invariants: canonical ≠ `og:url` ≠ sitemap `<loc>`; a `noindex` page advertised in the sitemap; a page on disk missing from the sitemap and vice versa; a missing Markdown twin or a twin linked as an ordinary `<a>`; a widened `script-src`; `SECTION_HUBS` pointing at a 404; fonts without preconnect/`display=swap`; a missing `landing/404.html` or a `_redirects` catch-all (rule 67) |
| `factcheck.py --self-test` + `--fail-on error` | an unsupportable product claim, or a number with no provenance |
| `audit.py --fail-on critical` | **report only**, `if: always()` — it never fails the build |

`ste_check.py` is **not** in this table and is **not** in `seo.yml`. It was a gate
and was taken off the blocking path: ASD-STE100's grammar rules fight the
plain-language phrasing this content depends on, and a gate that rewrites the
strings we are trying to match costs more than it catches. Run it by hand as an
authoring aid; `--self-test` still passes and should stay passing.

`--fixture-check` is **not** in `seo.yml` today; run it locally whenever you
touch `claims_rules.py` or `scripts/seo/testdata/`.

### The check that must NOT be a gate

`verify_agent_discovery.py` checks the agent-discovery contract after deployment:

```sh
python3 scripts/seo/verify_agent_discovery.py
python3 scripts/seo/verify_agent_discovery.py https://staging.example.com --json
# Opt in only on a deployment expected to publish the public xerj skill:
python3 scripts/seo/verify_agent_discovery.py --skills --json
```

The default checks five requests: homepage GET and HEAD, then the three fixed
documentation targets from its discovery headers. It requires the relations
on the final response, not just in 103 Early Hints; a target must return 200
with the expected media type, and OpenAPI JSON must have a version and paths.
`--skills` additionally checks the v0.2.0 index, the public `xerj` entry, CORS,
and the exact-byte SHA-256 of its single-file artifact. It checks consistency,
not authenticity, and does not validate other skills or activate instructions.

Only fixed paths on the chosen origin are fetched. Redirects are reported,
never followed; responses are capped at 1 MiB. Requests are direct HTTP(S),
without proxy configuration, credentials or cookies. `--timeout` limits each
socket operation (not total run time). Exit 0 means pass, 1 means a delivery
or contract failure, and 2 means invalid CLI arguments. Wait for deployment
propagation before treating a failure as a code defect. The tool reads no
XERJ node or user corpus and changes no remote state. CI runs only its
loopback fixtures: `python3 scripts/seo/test_verify_agent_discovery.py`.

`verify_live.py` needs a deployment, so on a pull request every URL it asks for
404s. Wiring it into `seo.yml` would make the merge gate fail on correct work,
which is how gates get ignored. Run it **after** a deploy:

```sh
python3 scripts/seo/verify_live.py                    # https://xerj.org
python3 scripts/seo/verify_live.py https://staging.example.com --jobs 4
```

It reads the sitemap this tree produces — the generated one, since
`landing/sitemap.xml` is not committed — never `<base>/sitemap.xml`: the
question is whether this tree reached production, and a stale deploy must not
be allowed to answer for itself. `seo_lint.py`'s header reserves
rules 15, 22 and 63 for exactly these three checks; this is where they live.

### The gate that is easy to miss

**`.github/scripts/landing-constants-guard.sh` is run by `ci.yml`, not by
`seo.yml`.** It predates the article generator and it was missed during the work
that built this toolchain — it then caught a real defect. Run it:

```sh
bash .github/scripts/landing-constants-guard.sh
```

It exists because `landing/` has no build step, so every published constant is
copy-pasted across dozens of files. A 2026-08-12 first-contact agent study found
four different ES-YAML conformance figures live at once, a page headlining "seven
canonical operations" whose own card said "six", version footers frozen three
releases back, and three machine-readable schema files nothing linked to. It
checks four things:

1. **ES-YAML conformance figures** agree with the canonical `CONFORMANCE`
   constant in the script (today `1444/1454`).
2. **Version stamps** in the `XERJ.AI ·` footer brand line, and the
   `Current release:` line in `llms*.txt`, match the newest `v1.0.0-rc.*` git
   tag. It needs `fetch-depth: 0` for the tags.
3. The site does not claim **both six and seven** canonical agent operations.
4. Every `landing/docs/agents/schemas/*.json` has an **inbound link** — an
   artifact a crawler cannot discover is equivalent to an absent one.

A genuinely historical figure is legal, but it must say so: put
`<!-- snapshot:YYYY-MM-DD -->` immediately before it and state the current value
nearby. The marker is what separates "deliberately historical" from "nobody
updated this".

---

## 8. Traps

Every one of these cost real time. They are listed in the order they bite.

### 1. `.gitignore` has a blanket `*.md` — new Markdown needs an explicit re-include

The repo ignores `*.md` by default and re-includes specific paths with `!` rules.
This has silently swallowed files three times before this work (the pull-request
template, `tools/**/SKILL.md`, and the whole fact-check fixture corpus), and it
tried to do it again here. The article sources, the published Markdown twins and
`scripts/seo/testdata/` each needed their own line:

```gitignore
!content/**/*.md
!landing/**/*.md
!scripts/seo/testdata/**/*.md
!scripts/seo/README.md
```

Forget one and the file never reaches the deploy — `git add` refuses it, no error
is printed, and every `<link rel="alternate">` 404s in production while
everything passes locally. **Whenever you add Markdown anywhere in this repo,
run `git check-ignore -v <path>` and confirm the last matching pattern is a `!`
rule.** `git status --porcelain` will not tell you; ignored files are invisible
to it.

### 2. `dateModified` comes from git, so the first commit is never the last one

Since #974 an article's `dateModified` comes from its **required `updated:`
frontmatter field** — author-controlled data, immune to the loop below. The
sitemap's `<lastmod>` uses that same field for a generated article (then the
page's JSON-LD `dateModified`, then a pinned `pagedata.py` date). It does
**not** use `git log -1` of the generated HTML: a sitewide head commit, and a
shallow checkout whose files all share today's mtime, stamped every URL with
one day. `git log` is only the fallback for a hand-written page that publishes
no date, and only for a commit that changed more than shared chrome. A
generated article does not move on regeneration: its date is `updated:`.

The sitemap is not committed, so no build round is needed for it — `--check`
validates the derivation and the deploy writes the file.

The stderr warning `WARNING: <lastmod> fell back to filesystem mtime` means a
URL had no in-file date and no usable git history. It is expected for a
brand-new untracked page, and is a bug in CI for a page that is already
published.

### 3. CI needs `fetch-depth: 0`

`actions/checkout` defaults to a shallow clone with no tags. Sitemap
`<lastmod>` does not require that full history for a page that carries
`updated:` or `dateModified`: those dates are in the checkout. A page with
neither is dated from a chrome-stripped git commit, which a shallow clone does
not have — it falls back to the filesystem mtime, which is the deploy machine's
checkout time. The deploy therefore fetches full history before it generates
the sitemap.
`fix_heads.py` can still move a date that is computed from `git log` when the
page has no pinned date and no JSON-LD date to keep, and
`landing-constants-guard.sh` fails differently on the same cause: it
resolves "newest released tag" through `git tag --list`, which a shallow checkout
does not fetch. Both `seo.yml` and the `landing-constants-guard` job in `ci.yml`
already set `fetch-depth: 0`. Any new workflow that touches this toolchain must
too.

### 4. The article footer carries no version string — on purpose

`landing-constants-guard.sh` matches `XERJ.AI[^<]*V1.0.0-RC.?[0-9]+` and flags
only stamps that *disagree* with the newest tag. **A footer with no version is
never examined; the guard is satisfied by absence, not by agreement.** Do not
"fix" the article footer by adding a version back. Doing so re-creates exactly
the drift the guard exists to catch, and it would be the only version string on
pages whose entire point is that they do not depend on one.

### 5. Markdown twins are `<link rel="alternate">`, never `<a>`

Every article page has a twin at `landing/<section>/<slug>.md`, served at
`https://xerj.org/<section>/<slug>.md`. It is what an AI agent fetches instead of
parsing the HTML. Two rules, both machine-enforced:

* **The twin is excluded from `sitemap.xml`** (the sitemap only walks `*.html`).
* **The twin is referenced only by
  `<link rel="alternate" type="text/markdown" href="…">` in `<head>`** — exactly
  one such link per page, pointing at exactly the right path.

An ordinary `<a href="….md">` makes the twin a competing document for the same
content: a duplicate that Google must pick between, on a page that already told
it which URL is canonical. `build_articles.assert_generated_contract` refuses to
emit one and `seo_lint.check_markdown_twins` fails the build on one.

### 6. Never emit `FAQPage`, `HowTo` or `aggregateRating` JSON-LD

Articles carry a `faq:` block and it renders as a normal FAQ section — but **no
`FAQPage` structured data**. Google restricted FAQ rich results to government and
health sites and they end entirely on 7 May 2026. `HowTo` and its documentation
were removed outright. `aggregateRating`/`Review` would be invented — there is no
review corpus — which is a manual-action risk under Google's structured-data spam
policies. The emitted set is: `Organization` + `WebSite` sitewide, one page-body
type (`SoftwareApplication` / `CollectionPage` / `TechArticle` / `Article` /
`WebPage`), and `BreadcrumbList` below the root. That is the default.

**One opt-in exception (2026-10-19).** An article source may list
`structured_data: [faq, howto, software]`. `seo_head.extra_structured_data` then
adds `FAQPage` (from `faq:`), `HowTo` (from `commands:`, at least two) and a
`SoftwareApplication` for the product. Since 2026-10-23 every article in
`content/answers` and `content/compare` sets `structured_data: [faq]`, so
those pages emit `FAQPage`. HowTo stays off. A best-of page
can still emit an `ItemList` of the tools in its visible summary table, from
`items:`, without that opt-in. Google shows no rich result for FAQ or HowTo,
but answer engines and agents read the blocks as plain structured data when a
page does opt in. `seo_lint` rule 46 lets the two types through only on a page
that opted in, and rule 46b (`jsonld.faq_not_visible`) fails the page if a
`FAQPage` question or answer is not visible in the page text. No
`aggregateRating` / `Review`, ever.

### 7. JSON-LD needs no CSP change

`<script type="application/ld+json">` is a **data block**, not a script. WHATWG
HTML's "prepare the script element" algorithm returns at step 13 — before the CSP
inline check at step 21 — so `script-src` never applies to it. Adding structured
data to a page can never require touching `landing/_headers`, and `seo_lint.py`
rule 55 fails the build if anyone widens `script-src` on account of it. (The
`'unsafe-inline'` already in the policy predates this work; it is something to
remove later, not to build on.)

The GA4 tag is the exception that is a script. `seo_head.ga4_snippet()` emits
Google's standard async `gtag.js` snippet once, immediately after `<head>`, on
every page (`fix_heads.py` for pages already on disk, `build_articles.py` for
generated articles). Rule 55's allowlist includes `https://*.googletagmanager.com`
for that tag and still fails any other `script-src` host. `landing/_headers`
also allows `https://*.google-analytics.com` and `https://*.analytics.google.com`
on `connect-src`, and the Analytics and Tag Manager hosts on `img-src`, so the
tag's collect calls are not blocked. That set is Google's GA4 policy without
Google Signals. It does not allow `google.com`, DoubleClick, or ads hosts.

### 8. When `factcheck.py` fires, fix the claim — never the wording

The rules match claims the product cannot back. Rewording to slip past the regex
leaves the false claim on a public page and removes the only thing that would
have caught it. `--explain <RULE-ID>` prints the rule's evidence and a compliant
rewrite; use that. During consolidation this gate caught a real ERROR in an
article that had already passed every other check. `factcheck.py` was initially
left out of CI — do not remove it again. (`ste_check.py` is a different case: it
is deliberately advisory. See the note under the gate table in §7.)

### 9. Do not hand-edit generated HTML

Worth repeating because it is the failure this whole directory exists to prevent.
A hand edit to `landing/answers/*.html` survives locally, survives review, and
fails `build_articles.py --check` in CI with a message about the *generator*, not
about your edit. Edit `content/`, run `--write`, commit the result.

---

## 9. Quick reference

```sh
# everything seo.yml runs, in order
python3 scripts/seo/build_articles.py --check
python3 scripts/seo/gen_sitemap.py    --check
python3 scripts/seo/fix_heads.py      --check
python3 scripts/seo/fix_links.py      --check
python3 scripts/seo/mk_og_card.py     --check
python3 scripts/seo/seo_lint.py
python3 scripts/seo/factcheck.py --self-test && python3 scripts/seo/factcheck.py --fail-on error

# plus the things seo.yml does NOT run
python3 scripts/seo/factcheck.py --fixture-check
bash .github/scripts/landing-constants-guard.sh

# advisory, never blocking
python3 scripts/seo/ste_check.py --self-test
python3 scripts/seo/ste_check.py content/answers/*.md content/compare/*.md

# after a deploy only — never in CI
python3 scripts/seo/verify_live.py https://xerj.org

# regenerate everything
python3 scripts/seo/build_articles.py --write
python3 scripts/seo/fix_heads.py      --write
python3 scripts/seo/fix_links.py      --write
python3 scripts/seo/gen_sitemap.py    --write
python3 scripts/seo/mk_og_card.py     --write
```
