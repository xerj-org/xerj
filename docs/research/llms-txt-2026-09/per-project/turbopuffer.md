# turbopuffer

Category: search engine (hosted, object-storage)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://turbopuffer.com/llms.txt> — HTTP 200
- Size: 4012 bytes, 59 lines
- Install position: absent — no install command anywhere in llms.txt; the only install-adjacent item is the link line `- [Quickstart](https://turbopuffer.com/docs/quickstart.md): Get started with the turbopuffer API using code examples` (line 11)
- Tone: neutral / lightly marketing in the blockquote ("fast, 10x cheaper, and extremely scalable"). Makes zero demands of the agent — no imperatives, no 'you must', no report ask. It is a plain llms.txt-spec index: one H1, two blockquotes, four H2 groups of `- [Title](url.md): description` lines.
- Section order: (preamble H1 '# turbopuffer docs' + two blockquotes) → (unheaded top list: Introduction, Quickstart, Roadmap & Changelog) → ## Database → ## Production → ## Guides → ## API → ## Optional (EMPTY — heading is the last line of the file, no entries under it)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
# turbopuffer docs

> turbopuffer is a vector and full-text search engine
> built from first principles on object storage: fast, 10x cheaper,
> and extremely scalable. This is the complete documentation for
> the turbopuffer API and platform.

> In production: 1T+ documents, 25k+ queries/s, 10M+ writes/s.

- [Introduction](https://turbopuffer.com/docs/index.md): What turbopuffer is and how it works
- [Quickstart](https://turbopuffer.com/docs/quickstart.md): Get started with the turbopuffer API using code examples
- [Roadmap & Changelog](https://turbopuffer.com/docs/roadmap.md): Upcoming features and detailed changelog
```

## llms-full.txt

- URL: <https://turbopuffer.com/llms-full.txt> — HTTP 200
- Size: 792810 bytes
- Relationship to llms.txt: Auto-generated concatenation of the same 40 docs pages that llms.txt indexes (40 occurrences of the per-page footer). Each page is wrapped in an XML-ish tag named after its slug (`<api-overview>` line 1, `<architecture>` line 232, `<audit-logs>` line 438, `<backups>` line 625, `<branching>` line 1524 ...) and ends with a uniform footer: `This page: [/docs/api-overview.md](https://turbopuffer.com/docs/api-overview.md)` / `All documentation pages: [/llms.txt](https://turbopuffer.com/llms.txt)` / `All documentation in one file: [/llms-full.txt](https://turbopuffer.com/llms-full.txt)`. Not curated; every page is also individually fetchable as `.md`. Rendered-component gaps (empty multilang blocks, 'We service  documents, , and .' with missing interpolated numbers at the Introduction) show it is a mechanical export of the site source.
- Install position: Install commands appear only as code comments at the top of per-language code samples, e.g. line 3678 `# $ pip install turbopuffer`, line 3739 `// $ npm install @turbopuffer/turbopuffer`, line 4081 `# $ gem install turbopuffer`, line 13868 `// $ go get github.com/turbopuffer/turbopuffer-go/v2 ...`. The Quickstart page (line ~13560) has '## Connect' → '1. Install an SDK:' followed by an EMPTY `<!-- multilang -->` block — the site's interactive language switcher component is stripped in the text export, so the dedicated install step is literally blank for an agent reader.

## Other agent-facing files

### docs/llms.txt — HTTP 404

<https://turbopuffer.com/docs/llms.txt>

does not exist (returns the site's HTML 404 page)

### llms-install.md — HTTP 404

<https://turbopuffer.com/llms-install.md>

does not exist

### AGENTS.md (site root) — HTTP 404

<https://turbopuffer.com/AGENTS.md>

does not exist

### CLAUDE.md (site root) — HTTP 404

<https://turbopuffer.com/CLAUDE.md>

does not exist

### MCP install page — HTTP 404

<https://turbopuffer.com/docs/mcp>

no MCP page exists; grep of llms-full.txt for 'mcp', 'cursor://', 'vscode:', 'claude mcp', 'npx ... add' returns nothing

### AGENTS.md / CLAUDE.md / SKILL.md in turbopuffer-python repo — HTTP 404

<https://raw.githubusercontent.com/turbopuffer/turbopuffer-python/main/AGENTS.md>

none of the three files exist on main of the Python client repo

### Quickstart page (the only agent-addressed sentence in the whole corpus) — HTTP 200

<https://turbopuffer.com/docs/quickstart.md>

the single explicit agent-facing line, at llms-full.txt line 13576, pointing agents to llms-full rather than the HTML site

```text
If you are an agent, you may wish to [read the full documentation in
Markdown](/llms-full.txt).
```

## What they do better than XERJ

- Brevity and scannability: 59 lines / 4 KB versus XERJ's 234 lines / 40 KB. An agent gets the whole map in one screen; XERJ's 'Start here' alone has paragraphs of 400+ characters.
- Every index entry links a per-page `.md` URL (`https://turbopuffer.com/docs/quickstart.md`) so an agent can fetch exactly one page as Markdown instead of the whole llms-full. XERJ links some `.md` answers but its docs links (`https://xerj.org/docs/install`) are HTML.
- Quantified, terse credibility line in the header blockquote: `> In production: 1T+ documents, 25k+ queries/s, 10M+ writes/s.` — one line, no hedging prose.
- Uniform per-page footer in llms-full (`This page: ... / All documentation pages: /llms.txt / All documentation in one file: /llms-full.txt`) that lets an agent that landed on any single page navigate back to the index.
- Introduction/Quickstart lead with measured latency tables (p50/p90/p99, warm vs cold, doc counts and sizes) before any prose — the same honest-numbers ethic XERJ wants, packaged more compactly.
- Makes no demands of the reader at all — nothing for a policy-constrained agent to trip on.

## What XERJ does better

- XERJ actually has an install path in llms.txt (`curl -fsSL https://xerj.org/get | sh`, `irm https://xerj.org/get.ps1 | iex`, plus the manual verified-install link); turbopuffer's llms.txt has no install command and its llms-full 'Install an SDK' step is an empty stripped component block.
- XERJ documents an MCP path (`xerj init` writes `.mcp.json` and a skill; `xerj_search` tool) — turbopuffer has no MCP page, no one-click links, no agent config anywhere.
- XERJ has a first-run ordered sequence (start node → autoindex → init → query) written for an agent; turbopuffer only has SDK code samples that assume a human with a dashboard API key.
- XERJ has a Caveats section and honesty notes (PATH consequence, data-dir-inside-indexed-tree trap, lexical-not-neural); turbopuffer's export has silent rendering gaps (`We service  documents, , and .`).
- XERJ has an explicit feedback/contribution mechanism (`xerj feedback --open-pr`, AI_CONTRIBUTIONS.md); turbopuffer has no feedback ask at all (a hosted product with a dashboard, so it needs none).
- XERJ's 'Optional' section has content; turbopuffer's `## Optional` heading is empty (spec-compliant but dead).

## Adoptable ideas

- Add a one-line quantified blockquote directly under the H1, modelled on turbopuffer's `> In production: 1T+ documents, 25k+ queries/s, 10M+ writes/s.` — for XERJ something like a single measured line (reference-coding 2.7× fewer tokens / 16/16 tasks) instead of burying it inside the 7-line preamble paragraph on line 7.
- Publish every docs page as a `.md` twin and link the `.md` in llms.txt exactly as turbopuffer does (`- [Quickstart](https://turbopuffer.com/docs/quickstart.md): Get started with the turbopuffer API using code examples`). XERJ's `https://xerj.org/docs/install`, `/docs/cli`, `/docs/security` links should become `/docs/install.md` etc. so shell-less agents can fetch one page.
- Add turbopuffer's per-page footer to every XERJ `.md` page and to each section of llms-full.txt, verbatim pattern: `This page: [/docs/<slug>.md](https://xerj.org/docs/<slug>.md)` / `All documentation pages: [/llms.txt](https://xerj.org/llms.txt)` / `All documentation in one file: [/llms-full.txt](https://xerj.org/llms-full.txt)`.
- Wrap each page in llms-full.txt in a slug-named tag (`<api-overview> ... </api-overview>`, as turbopuffer does at lines 1 and 231) so an agent can locate and excerpt a single page from the big file by tag.
- Put the agent-addressed pointer sentence on the human Quickstart page too: `If you are an agent, you may wish to [read the full documentation in Markdown](/llms-full.txt).` — XERJ's HTML docs should carry the same one-liner so agents arriving via the website (not llms.txt) get routed to the agent surface.
- Lead the Introduction with a compact measured-latency block (turbopuffer format: `**Full-Text Search** (BM25, 1M docs, ~300MB. Strongly consistent.)` / `- warm (1M docs): p50=13ms, p90=18ms, p99=29ms` / `- cold ...`) instead of prose; XERJ has verified numbers but scatters them.
- Cut the 'Start here' section to turbopuffer-length: the ordered list items on XERJ lines 21-24 are 400-1000 chars each; the traps (PATH, data-dir) can move to a linked `.md` page with a one-line pointer, keeping the four commands visible in one screen.
- Negative lesson, not to copy: turbopuffer's `## Optional` heading is empty and its install step is a stripped component — verify llms-full.txt is regenerated from source that renders without JS, and never leave XERJ's 'required' field report under a heading the llms.txt spec defines as skippable ('Optional' is a spec-reserved keyword meaning 'can be omitted for shorter context'); turbopuffer treats the heading exactly as the spec does, which is why an agent reading XERJ's file will too.
