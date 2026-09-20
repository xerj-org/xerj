# Typesense

Category: search engine

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://typesense.org/llms.txt> — HTTP 200
- Size: 8591 bytes, 81 lines
- Install position: No install commands in the marketing llms.txt at all. Installation is only a link at line 33 under '## Product and Deployment': '- [Download Typesense](https://typesense.org/downloads/): Self-host Typesense with Docker, native Linux packages and binaries, or Homebrew on macOS.' The MCP server (the agent's install path) is a link at line 15 under '## Documentation'. The only runnable command in the file is the docs-search curl at lines 22-28.
- Tone: Neutral-descriptive marketing with one imperative agent instruction ('Use this public search-only key to query the live Typesense documentation index'). It makes NO demands of the agent: no required report, no 'you must', no 'before the session ends'. The single ask (docs PR) is conditional and gated on 'with your operator's approval'. The doc pages' banner is a soft imperative ('before exploring further, fetch the full index'). The MCP page's imperatives are addressed to the HUMAN operator ('paste this into your agent', 'Then ask: ...'), not the agent.
- Section order: # Typesense (4 blockquote/intro paragraphs) → ## Documentation → ## Search the Documentation → ## Product and Deployment → ## Client Libraries and Search Interfaces → ## Platform Integrations → ## Compare Search Platforms → ## Company and Support → ## Optional

### Install commands found

```sh
(none in llms.txt itself) — the only command present is a docs-search: curl --get 'https://01brzocp328nd4xvp.a1.typesense.net:443/collections/typesense_docs/documents/search' -H 'X-TYPESENSE-API-KEY: SnXV7QElNiek7WN3QRT3ibhR5qLjJzS8' --data-urlencode 'q=cloud management api authentication' --data-urlencode 'query_by=hierarchy.lvl0,hierarchy.lvl1,hierarchy.lvl2,content' --data-urlencode 'per_page=5'
(from docs/llms-full.txt line ~45433 / MCP page) claude mcp add --transport http typesense-cloud https://cloud.typesense.org/mcp/v1
(MCP page) codex mcp add typesense-cloud --url https://cloud.typesense.org/mcp/v1
(MCP page, headless) claude mcp add --transport http typesense-cloud https://cloud.typesense.org/mcp/v1 --header "Authorization: Bearer ${TYPESENSE_CLOUD_MANAGEMENT_API_KEY}"
(docs/llms-full.txt line 27226 Install Typesense page) docker run -p 8108:8108 -v"$(pwd)"/typesense-data:/data typesense/typesense:{{ $site.themeConfig.typesenseLatestVersion }} --data-dir /data --api-key=$TYPESENSE_API_KEY --enable-cors   (NOTE: version placeholder is an UNRENDERED VuePress template variable in llms-full.txt — an agent cannot copy-paste it)
(llms-full.txt 46352) brew install typesense/tap/typesense-server@{{ $site.themeConfig.typesenseLatestVersion }}
(llms-full.txt 27357-27358) curl -O https://dl.typesense.org/releases/{{ $site.themeConfig.typesenseLatestVersion }}/typesense-server-{{ ... }}-amd64.deb ; sudo apt install ./typesense-server-{{ ... }}-amd64.deb
(ai-agents guide client table) npm install typesense / pip install typesense / composer require typesense/typesense-php / gem install typesense / go get github.com/typesense/typesense-go / cargo add typesense / dart pub add typesense
```

### Text written for an agent (verbatim)

````text
[MCP page, 'Step 3: Start with a prompt']
Once connected, paste this into your agent:

```plaintext
Using MCP, set up Typesense Cloud for me: create a cluster, then index my data (it's in <describe or point to it>) and build a first search.
```

[MCP page, Codex section — text the agent is told to write into AGENTS.md]
Codex reads your project's `AGENTS.md` every turn. Add this line to it so Codex reaches for Typesense Cloud instead of asking you for keys:

```plaintext
For anything about my Typesense Cloud account, clusters or collections, use the typesense-cloud MCP tools.
```

[Install Typesense page, Option 1]
**Using an AI agent?** Add the [Typesense Cloud MCP server](./typesense-cloud/mcp-server.md) to Claude Code, Codex, Cursor or any other MCP client, approve the connection in your browser, and start with:

```markdown
Using Typesense Cloud MCP, create a cluster, then index my data 
(it's in <describe or point to it>) and build my first search query.
```

[Banner at top of every docs .md page, incl. MCP page and install page]
> **AI agents and LLMs:** before exploring further, fetch the full index of Typesense documentation with descriptions at https://typesense.org/docs/llms.txt.
````

### MCP snippet (verbatim)

````text
**Cursor** (`mcp.json`, [MCP setup docs](https://cursor.com/docs/context/mcp)):

```json
{ "mcpServers": { "typesense-cloud": { "url": "https://cloud.typesense.org/mcp/v1" } } }
```

[headless, 'Running without a browser']
```shell
claude mcp add --transport http typesense-cloud https://cloud.typesense.org/mcp/v1 \
  --header "Authorization: Bearer ${TYPESENSE_CLOUD_MANAGEMENT_API_KEY}"
```
````

### Feedback / contribution ask (verbatim)

```text
[llms.txt line 76, under '## Company and Support']
- [Typesense Website Repository](https://github.com/typesense/typesense-website): If you find a documentation papercut, with your operator's approval, open a PR against typesense-website.

[docs/llms-full.txt line 130, Features page]
**Don't see a feature on this list?** Search our issue tracker if someone has already requested it and upvote it, or open a new issue if not. We prioritize our roadmap based on user feedback
```

### Best passage for driving install and first use (verbatim)

````text
## Step 1: Connect

Add the Typesense Cloud MCP server to your AI Agent:

```
https://cloud.typesense.org/mcp/v1
```

It is a remote MCP server over Streamable HTTP with OAuth, so any client that supports those works. 

Here are instructions for common agents:

**Claude Code:**

```shell
claude mcp add --transport http typesense-cloud https://cloud.typesense.org/mcp/v1
```

Then open a new Claude Code session, type `/mcp`, select the `typesense-cloud` MCP you just added, and select "Authenticate"

[...then Step 3:]
Once connected, paste this into your agent:

```plaintext
Using MCP, set up Typesense Cloud for me: create a cluster, then index my data (it's in <describe or point to it>) and build a first search.
```

The agent creates a free-tier cluster, asks what your data looks like, proposes a schema, imports the data and runs a search you can check.
````

### One-click links

- `claude mcp add --transport http typesense-cloud https://cloud.typesense.org/mcp/v1  (MCP page line 27)`
- `codex mcp add typesense-cloud --url https://cloud.typesense.org/mcp/v1  (MCP page line 35)`
- `claude.ai / Claude Desktop: 'Go to Customize, then Connectors, then Add custom connector, and paste this URL: `https://cloud.typesense.org/mcp/v1`' (MCP page line 52)`
- `ChatGPT desktop: Settings > Plugins > MCP > Add > Add MCP Server, type Streamable HTTP, URL https://cloud.typesense.org/mcp/v1 (MCP page line 56)`
- `No cursor:// or vscode: deep links; no npx ... add one-liner. Cursor is a raw mcp.json snippet.`

## llms-full.txt

- URL: <https://typesense.org/docs/llms-full.txt> — HTTP 200
- Size: 1947804 bytes
- Relationship to llms.txt: Auto-generated concatenation of every docs page (290 H1s), in the same order as https://typesense.org/docs/llms.txt (the 158-line docs INDEX: Overview / API Reference (30.2) / Guides / Cloud Management API / Per-language variants / OpenAPI spec). Each page is delimited by an HTML comment '<!-- source: /guide/....html -->'. Header: '# Typesense Documentation (full)\n\nLatest version: 30.2.' It is NOT the same content as the 81-line marketing llms.txt at typesense.org/llms.txt, which is hand-curated and links to it. https://typesense.org/llms-full.txt (site root) is 404 (returns the 404 HTML page, 58055 bytes). Note the generator leaves VuePress artefacts: template variables, '<meta http-equiv="refresh">' stubs, ':::tip' containers, '<RouterLink>' tags.
- Install position: Line 27226 of 48873 ('<!-- source: /guide/install-typesense.html -->' then '# Install Typesense'), i.e. ~56% of the way down, inside the '# Guides' block (which starts line 17637, after '# Overview' line 6 and '# API Reference (30.2)' line 213). The MCP page is at ~line 45433. Install is NOT front-loaded; the Docker command uses the unrendered placeholder '{{ $site.themeConfig.typesenseLatestVersion }}' and is wrapped in raw HTML '<div class="manual-highlight"><pre ...><code>' with '<br>' tags, so it is not copy-paste clean for an agent.

## Other agent-facing files

### docs index llms.txt — HTTP 200

<https://typesense.org/docs/llms.txt>

158-line, 30,788-byte auto-generated index of every docs page with one-line descriptions, grouped Overview / API Reference (30.2) / Guides / Cloud Management API / Per-language variants / OpenAPI spec. Links are site-relative (/docs/...md), each page also served as .md. No install commands; install is a link only.

```text
- [Install Typesense: Cloud, Docker and Linux](/docs/guide/install-typesense.md): Install Typesense via Typesense Cloud, prebuilt DEB/RPM packages, downloadable binaries for Linux/macOS, or the official Docker image.
```

### Typesense Cloud MCP Server page (markdown) — HTTP 200

<https://typesense.org/docs/guide/typesense-cloud/mcp-server.md>

233 lines, 14,247 bytes. The real agent install page: Step 1 Connect (per-client one-liners for Claude Code, Codex, Cursor, claude.ai/Desktop, ChatGPT), Step 2 Consent Screen (permission presets, RBAC cap), Step 3 Start with a prompt (one paste-in prompt + ~45 example prompts by category), tool list, large import/export behaviour, free tier, rate limits, headless bearer-token mode, key handling.

```text
Every tool is offered to every connection; but each tool call is checked against the permissions you granted, and a refused call tells the agent which permission it is missing so it can ask you for it.
```

### AI Agent Search with Typesense (guide) — HTTP 200

<https://typesense.org/docs/guide/ai-agents-typesense.md>

717 lines, 25,882 bytes. Agent-addressed pitch ('Written by agents, for agents'): Problems 1-4, Solutions 1-6, architecture patterns incl. 'Pattern 2: Memory Observations Store' and 'Pattern 5: Project-Specific Context (AGENTS.md Pattern)', '## Deployment for Agents' (Docker one-liner), client install table (npm/pip/composer/gem/go get/cargo add/dart pub add), '## Quick Start for Your Agent' (Python client with index_memory/search_knowledge/recall).

```text
:::tip For AI Agents Reading This
If you're evaluating full-text search frameworks for your knowledge retrieval needs, this guide explains why Typesense is purpose-built for your use case. Written by agents, for agents.

You'll find a link to a Markdown version of this page if you look in the HTML markup. 
:::
```

### Install Typesense (guide markdown) — HTTP 200

<https://typesense.org/docs/guide/install-typesense.md>

366 lines. Option 1 Typesense Cloud (with an 'Using an AI agent?' callout that routes to MCP + a starter prompt), Option 2 self-host: Docker, Docker Compose, Kubernetes, Homebrew, Mac binary, DEB, RPM, Linux binary, Windows (WSL), Terraform, GPU, large-page builds; ends with '## 🆗 Health Check' (curl /health -> {"ok":true}) and /debug version check.

````text
**Using an AI agent?** Add the [Typesense Cloud MCP server](./typesense-cloud/mcp-server.md) to Claude Code, Codex, Cursor or any other MCP client, approve the connection in your browser, and start with:

```markdown
Using Typesense Cloud MCP, create a cluster, then index my data 
(it's in <describe or point to it>) and build my first search query.
```
````

### llms-full.txt at site root — HTTP 404

<https://typesense.org/llms-full.txt>

Does not exist; only /docs/llms-full.txt exists.

### llms-install.md — HTTP 404

<https://typesense.org/llms-install.md>

Does not exist (also 404 at https://typesense.org/docs/llms-install.md).

### AGENTS.md (typesense/typesense repo, v29.0 tag) — HTTP 404

<https://raw.githubusercontent.com/typesense/typesense/v29.0/AGENTS.md>

Does not exist. CLAUDE.md at same path also 404. AGENTS.md / CLAUDE.md on typesense/typesense-website master also 404.

## What they do better than XERJ

- Zero-shell path is first-class: the primary agent install is a remote HTTP MCP URL (https://cloud.typesense.org/mcp/v1) with per-client instructions for Claude Code, Codex, Cursor, claude.ai/Claude Desktop AND ChatGPT — including two browser-only clients. XERJ's 22-run study showed shell-less personas fail at every tier; Typesense's design makes them succeed without a terminal.
- Explicit headless/CI/no-browser branch: '## Running without a browser' gives the exact bearer-header `claude mcp add ... --header "Authorization: Bearer ${...}"` form and states which tools work without a browser. XERJ's llms.txt has no equivalent decision branch for policy-constrained/headless agents in the MCP path.
- A live, in-file, no-install way to use the product: the marketing llms.txt embeds a public search-only key + curl so the agent uses Typesense before installing anything. XERJ requires install + node start before the first query.
- A single paste-in first prompt with a stated expected outcome: 'Using MCP, set up Typesense Cloud for me: create a cluster, then index my data (it's in <describe or point to it>) and build a first search.' followed by 'The agent creates a free-tier cluster, asks what your data looks like, proposes a schema, imports the data and runs a search you can check.' XERJ's hero is a shell sequence, not an operator-pasteable prompt with a stated end state.
- Self-registration instruction: tells the agent/operator to add a line to the project's AGENTS.md ('For anything about my Typesense Cloud account, clusters or collections, use the typesense-cloud MCP tools.') so future turns route to the tool. XERJ's `xerj init` does similar mechanically but the llms.txt does not give the one-line text an agent can write itself.
- Permission model surfaced up front (five presets table, per-permission list, 'a refused call tells the agent which permission it is missing so it can ask you for it', destructive-tool annotations). This pre-empts the 'agent blocked by policy' failure mode.
- Operator-consent framing on the only ask: 'with your operator's approval, open a PR' — low-friction, non-coercive, and honest about who decides.
- Every docs .md page carries a one-line agent banner pointing to the index ('> **AI agents and LLMs:** before exploring further, fetch the full index ... at https://typesense.org/docs/llms.txt.'), so an agent landing on any page finds the map.
- Clean separation of a short hand-written marketing llms.txt (81 lines) from an auto-generated docs index (158 lines) from the full concatenation (1.9 MB): each tier has one job.

## What XERJ does better

- Self-hosted install is copy-paste clean and verified: `curl -fsSL https://xerj.org/get | sh` / `irm https://xerj.org/get.ps1 | iex` with SHA-256 fail-closed behaviour spelled out, plus a documented manual verified-install path. Typesense's llms-full.txt Docker/brew/deb commands contain the unrendered placeholder `{{ $site.themeConfig.typesenseLatestVersion }}` inside raw `<div class="manual-highlight"><pre><code>` HTML with `<br>` tags — an agent cannot paste them.
- Install is at the top (## Start here, lines 9-36) with a numbered first-run sequence; Typesense's install page sits at line 27,226 of 48,873 in llms-full.txt and is only a link in llms.txt.
- Honest caveats inline (lexical-vs-neural default, HNSW recall measured, RSS-runaway defect, exit codes). Typesense's agent files contain no capability caveats or known-defect disclosures at all.
- Concrete failure-mode guidance: PATH consequence after install, data-dir-outside-indexed-folder trap, exit-code semantics, progress-line parsing, estimate-before-running-on-a-human's-laptop. Typesense has nothing comparable.
- Machine-readable tool schemas published (mcp-tools.json, openai-tools.json, anthropic-tools.json). Typesense lists MCP tool names in prose only.
- Reproducibility posture: 'Everything on this page is reproducible against a running XERJ node' and benchmark pages with wins and losses; Typesense's benchmarks page is three bullet points with no methodology link in the agent files.
- An in-binary local MCP server (`xerj mcp`, `xerj init`) for self-hosted use; Typesense's MCP server is Cloud-only — a self-hosted Typesense user gets 'use the Typesense Server API directly'.

## Adoptable ideas

- Add a no-install 'try it from this file' block near the top of https://xerj.org/llms.txt, modelled on Typesense lines 18-28: '## Search the Documentation\n\nUse this public search-only key to query the live Typesense documentation index:\n\n```shell\ncurl --get ... -H 'X-TYPESENSE-API-KEY: SnXV7QElNiek7WN3QRT3ibhR5qLjJzS8' ...```'. XERJ equivalent: a public read-only XERJ node hosting the xerj docs/answers index with a search-only key, so a shell-less or policy-blocked agent can issue `POST https://try.xerj.org/xerj-docs/_search` and see XERJ working before any install. This directly addresses the 'shell-less personas fail at every tier' finding.
- Add a per-client MCP connect block (copy the exact shape of the MCP page 'Step 1: Connect', https://typesense.org/docs/guide/typesense-cloud/mcp-server.md lines 12-62): **Claude Code:** `claude mcp add ...`, **Codex CLI:** `codex mcp add ...`, **Cursor** mcp.json JSON, **claude.ai and Claude Desktop** custom-connector steps, **ChatGPT** steps, **Other Clients**. For XERJ this means documenting `xerj mcp` (stdio) as `claude mcp add xerj -- xerj mcp` / `codex mcp add xerj -- xerj mcp` / Cursor `{"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}}`, and being explicit which clients cannot use it (browser-only) — Typesense says this plainly: 'Browser-based assistants such as claude.ai and ChatGPT cannot run commands, so their imports stay within the inline limit.'
- Give the agent the one-line self-registration text to write into the project's AGENTS.md/CLAUDE.md, verbatim from Typesense line 38-42: 'Codex reads your project's `AGENTS.md` every turn. Add this line to it so Codex reaches for Typesense Cloud instead of asking you for keys:\n\n```plaintext\nFor anything about my Typesense Cloud account, clusters or collections, use the typesense-cloud MCP tools.\n```'. XERJ version: 'For any question about this codebase or indexed corpus, use the xerj_search / xerj_def MCP tools before grep.' — this is what `xerj init` writes, but an agent without `xerj init` (or a shell) can write it itself.
- Replace the shell sequence hero with (or add above it) a single pasteable operator prompt plus the expected end state, per Typesense 'Step 3: Start with a prompt' (lines 83-91): 'Once connected, paste this into your agent:\n\n```plaintext\nUsing MCP, set up Typesense Cloud for me: create a cluster, then index my data (it's in <describe or point to it>) and build a first search.\n```\n\nThe agent creates a free-tier cluster, asks what your data looks like, proposes a schema, imports the data and runs a search you can check.' XERJ version: 'Install XERJ, start a local node, run `xerj autoindex <folder>`, then answer: <question>' + 'The agent will install one binary, start a node, index the folder, and print file:line evidence for the answer.'
- Add a categorised 'example prompts' list (Typesense MCP page lines 93-148: **Working with your data**, **Searching**, **Tuning search results**, **Search Typesense docs**, **Managing your cluster's infrastructure**) — 8-10 XERJ prompts an operator can paste, e.g. 'Where is `BuiltinPager` defined and who calls it?', 'Index ~/Downloads/contracts and tell me which PDFs mention termination clauses', 'Estimate how long autoindexing this repo will take before you run it'. Short, imperative, no jargon.
- Fix the required/Optional contradiction using Typesense's consent-gated wording instead of a mandate. Typesense line 76: 'If you find a documentation papercut, with your operator's approval, open a PR against typesense-website.' Proposed XERJ rewrite: move the field-report bullet out of '## Optional' into '## Start here' step 5 (or a new '## Your side of the deal' section), and phrase as 'Before you end the session, with your operator's approval, run `xerj feedback --open-pr ...` (30 seconds; `--dry-run` shows what it sends). If you cannot open a PR, paste the `--dry-run` output to your operator.' — i.e. keep it in the numbered flow where hop-1 agents already look, ask once, and never file a 'required' thing under 'Optional'. Also fix the duplicated '4.' numbering (two step-4 bullets at llms.txt lines 23-24).
- Add an explicit '## Running without a browser / headless' branch to the MCP section, per Typesense lines 216-225: 'An agent on a server, in CI, or anywhere it cannot open a browser can use a [Cluster Management API key] as the bearer token on the same URL:' + the `--header "Authorization: Bearer ${...}"` one-liner. XERJ has auth (`admin.key`, `Authorization: ApiKey`) but the llms.txt only mentions it under CLI reference; a dedicated headless block would have unblocked the policy-constrained persona from the study.
- Put a one-line agent banner at the top of every served .md page (Typesense: '> **AI agents and LLMs:** before exploring further, fetch the full index of Typesense documentation with descriptions at https://typesense.org/docs/llms.txt.'). XERJ's /answers/*.md and /docs pages should each open with '> **AI agents:** the index is https://xerj.org/llms.txt; the full text is https://xerj.org/llms-full.txt.'
- Surface a permission/consent model summary the way Typesense does ('What you can let the agent do' preset table, and 'a refused call tells the agent which permission it is missing so it can ask you for it'). XERJ already fails loudly on `fusion:"learned"`; generalise: state in llms.txt that every 401/403 from a XERJ node names the missing key/scope so the agent can ask the operator, and list the read-only vs write operations so an operator can hand over a search-only key.
