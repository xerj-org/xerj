# Vespa

Category: search engine

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.vespa.ai/llms.txt> — HTTP 200
- Size: 68937 bytes, 998 lines
- Install position: No install instructions in the file body. Install only appears as link titles: line 55 '[Build / install Vespa]' under '## Self Managed', line 62 '[Docker containers]', line 260 '[Install Vespa on Kubernetes]', line 943 '[vespa skills install]' under '## Clients'. No quick-start page is linked under a recognisable 'Getting started' heading; the sections are alphabetical by filename, so the CLI/quick-start material sits at lines ~895-955 near the end.
- Tone: neutral / descriptive. The preamble is a product summary in the third person ('Vespa is a powerful and scalable engine...'); it makes no demands of the agent, gives no imperatives, and ends with 'This overview provides a glimpse into the capabilities of the Vespa Search engine. For more in-depth information, please refer to each of the documentation links below.' Every link entry uses the identical pattern '- [Title](https://docs.vespa.ai/...html.md): [](/...html.md "View as Markdown")' — the description slot is filled by an empty-text markdown link, which is a generator artefact, not a description.
- Section order: # Vespa documentation (prose preamble: blockquote summary, **Key Features**, **Architecture**, **Application Package**, **APIs and Interfaces**) → ## About Documentation → ## Access Logging → ## Self Managed → ## Applications → ## Api → ## Approximate Nn Hnsw → ## Archive → ## Enclave → ## Attributes → ## Automated Deployments → ... 191 H2 sections total, alphabetised by source-page filename (e.g. '## Bm25', '## Cloudflare Workers', '## Contributing' at line 309, '## Llm Help' at line 576, '## Migrating From Elastic Search' at 604, '## Securing Your Vespa Installation' at 803, '## Clients' at 903 containing the vespa-cli subcommand pages incl. 'vespa skills install' at line 943) ... → ## Release Notes → ## Vespaignore → ## Visiting → ## Wand → ## Web Services → ## Whats More → ## Whitepaper → ## Working With Chunks → ## Xgboost → ## Zones

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
**Application Package**

A Vespa application is defined by an **application package**, which contains all the necessary configuration, schemas, components, and machine-learned models. This self-contained package allows for atomic deployments and ensures consistency between code and configuration.

Key files in an application package include:

* **`services.xml`**: Defines the services and clusters that make up the application, including their topology and resource allocation.
* **`schemas/*.sd`**: Defines the document types, their fields, and how they should be indexed and searched. Rank profiles are also defined within schemas.

**APIs and Interfaces**

Vespa provides a comprehensive set of APIs for interacting with the system:

* **Document API (`/document/v1/`)**: A REST API for performing CRUD operations on documents.
* **Query API (`/search/`)**: A powerful API for querying data using YQL, with extensive options for ranking, grouping, and presentation.
* **Configuration and Deployment APIs**: REST APIs for deploying application packages and managing system configuration.

(NOTE for coordinator: this is the best the file offers — it is a mental-model primer, not an install/first-use passage. The file contains no passage that drives install or first use.)
```

## llms-full.txt

- URL: <https://docs.vespa.ai/llms-full.txt> — HTTP 200
- Size: 4522666 bytes
- Relationship to llms.txt: Same generator, same skeleton: identical 43-line prose preamble (the 'Stateless Container Clusters' paragraph occurs exactly once in each), then the same 191 alphabetical '## <Section>' headings in the same order, but under each heading the full body of every .html.md page is inlined (each page rendered as '### Title' + the '[](...html.md "View as Markdown")' link + '#### Title' + body, with heading levels shifted down). So llms-full = llms.txt link index with every link dereferenced and concatenated; nothing curated, nothing added. The docs page /en/learn/llm-help.html.md says: 'We also provide [llms-full.txt](../../llms-full.txt) which contains the _full_ content of all documentation pages in markdown format. This file is relatively large (almost 0.5M words as of Oct 2025), so use accordingly.' Only 1 H1 in the whole file.
- Install position: Install content is scattered by alphabetical page order, not front-loaded: first `docker run` at line 763 (inside '## Self Managed' build-install page), quick-start container run at line 1502 (docker-containers page), '$ brew install vespa-cli' at line 15702 (Kubernetes/operations page context: 'Install [Vespa CLI](../clients/vespa-cli.html):'). No MCP config snippet, no 'npx skills', no cursor:// or vscode: links anywhere in 4.5MB (grep for 'mcp' and 'npx skills' returned only the llm-help page's services.xml McpRequestHandler snippet).

## Other agent-facing files

### marketing llms.txt (vespa.ai) — HTTP 200

<https://vespa.ai/llms.txt>

WordPress/Yoast plugin artefact, 29 lines / 2733 bytes: a sitemap-style list of 5 marketing pages, 5 press-release posts, 5 category links and the sitemap. No install, no agent text. Its own '## Optional' section contains only '- [Sitemap index](https://vespa.ai/sitemap_index.xml)'.

```text
> Vespa is the AI Search Platform for fast, accurate AI search, AI agents, personalization, recommendations, and retrieval\.

Generated by Yoast SEO v28.4, this is an llms.txt file, meant for consumption by LLMs.
```

### Getting help from LLMs (docs page, the only human-written agent guidance) — HTTP 200

<https://docs.vespa.ai/en/learn/llm-help.html.md>

Explains the .html -> .html.md trick, how to use llms.txt as an entrypoint (`curl -O https://docs.vespa.ai/llms.txt`), warns llms-full is ~0.5M words, states there is no official MCP server, and shows how a user can enable an MCP endpoint inside their own Vespa app via services.xml.

````text
### Public Vespa MCP server

We don't provide any official [MCP](https://modelcontextprotocol.io/) server at this time, but will update this page as soon as we do.

### Personal MCP server

Users can enable MCP server capablities in their own Vespa apps. This can be done by adding `McpRequestHandler` to `services.xml` with one or more `McpSpecProvider` components.

#### Example MCP config

Add this to `services.xml`

```
<component id="com.yahoo.search.mcp.McpSearchSpecProvider" bundle="container-search-and-docproc"/>
<handler id="ai.vespa.mcp.McpRequestHandler" bundle="container-disc">
    <binding>http://*/mcp/*</binding>
</handler>
```
````

### vespa skills install (CLI reference page) — HTTP 200

<https://docs.vespa.ai/en/reference/clients/vespa-cli/vespa_skills_install.html.md>

The real agent-onboarding mechanism: a CLI subcommand that downloads agent skills from github.com/vespa-engine/skills and drops them into the directory each harness auto-discovers (Claude Code, Codex, Cursor, Antigravity), global or per-project, interactive prompt if flags omitted.

````text
Skills are downloaded from https://github.com/vespa-engine/skills and copied into the directory the chosen harness(es) discover automatically. Run 'vespa skills list' to see available skills.

If no skill names are given, all available skills are installed. If –harness or –local/–global are not given and the terminal is interactive, you will be prompted to choose.

```
vespa skills install [skill]... [flags]
```

### Examples

```
$ vespa skills install
$ vespa skills install schema-authoring app-package
$ vespa skills install --harness claude,codex --local
```
````

### vespa-engine/skills README.md — HTTP 200

<https://raw.githubusercontent.com/vespa-engine/skills/main/README.md>

Skills repo (7 skills: app-package, elasticsearch-migration, feed-operations, pyvespa, query-builder, schema-authoring, vespa-cli). Two install paths (vespa CLI and `npx skills add vespa-engine/skills`), an 'Example Prompts' section with four ready-to-paste user prompts, a `generate.py --check` CI gate that regenerates AGENTS.md/cursor plugin/README table from SKILL.md, an evals suite, and a 500-line SKILL.md ceiling rule.

````text
## Installation

### Vespa CLI (recommended)

The [Vespa CLI](https://docs.vespa.ai/en/vespa-cli.html) can install skills directly for Claude Code, Codex, Cursor and Antigravity CLI - no manual cloning required:

```bash
vespa skills install
```

### npx skills

[`npx skills`](https://github.com/vercel-labs/skills) installs skills into any of 70+ supported agent harnesses (Claude Code, Cursor, Codex, and just about every other agent harness):

```bash
npx skills add vespa-engine/skills
```

## Example Prompts

**Schema authoring:**
> "Create a Vespa schema for a product catalog with title, description, price, category, and a 384-dim embedding for semantic search."

**Query building:**
> "Write a hybrid search query that combines BM25 text matching with nearest-neighbor vector search, using reciprocal rank fusion."
````

### vespa-engine/skills AGENTS.md (auto-generated) — HTTP 200

<https://raw.githubusercontent.com/vespa-engine/skills/main/AGENTS.md>

Generated skill index for agents: path + one-line description + reference-doc list per skill, plus a three-step 'How to Use' that tells the agent to read the SKILL.md, load docs/ lazily, and combine skills.

```text
## How to Use

1. Read the relevant `SKILL.md` for the task at hand.
2. Load files from the `docs/` folder when you need deeper reference detail.
3. Combine skills as needed — e.g., use **schema-authoring** to define the schema,
   **app-package** to set up services.xml, **query-builder** for rank profiles, and
   **feed-operations** to populate data.
```

### vespa-engine/skills vespa-cli/SKILL.md — HTTP 200

<https://raw.githubusercontent.com/vespa-engine/skills/main/vespa-cli/SKILL.md>

The one file in the Vespa universe that gives an agent install commands with a verification step: brew on macOS, a curl|tar for Linux, then `vespa version`. YAML frontmatter with name/description for harness discovery.

````text
### Installation

On macOS via Homebrew:

```bash
brew install vespa-cli
```

On Linux or other platforms, download the binary from the GitHub releases page:

```bash
# Example for Linux amd64
curl -fsSL https://github.com/vespa-engine/vespa/releases/latest/download/vespa-cli_linux_amd64.tar.gz | tar xz
sudo mv vespa /usr/local/bin/
```

Verify the installation:

```bash
vespa version
```
````

### Contributing to Vespa (docs page linked from llms.txt '## Contributing') — HTTP 200

<https://docs.vespa.ai/en/learn/contributing.html.md>

Human contributor guide: GitHub flow, OWNERS-file reviewers, semver, issues, Slack. No agent-specific ask, no field-report concept, no 'required' language.

```text
We track issues in [GitHub issues](https://github.com/vespa-engine/vespa/issues). It is fine to submit issues also for feature requests and ideas, whether you intend to work on them or not.
```

### vespa-engine/vespa AGENTS.md — HTTP 404

<https://raw.githubusercontent.com/vespa-engine/vespa/master/AGENTS.md>

does not exist

### vespa-engine/vespa CLAUDE.md — HTTP 404

<https://raw.githubusercontent.com/vespa-engine/vespa/master/CLAUDE.md>

does not exist

### docs.vespa.ai/llms-install.md — HTTP 404

<https://docs.vespa.ai/llms-install.md>

does not exist (server answered 200 with the 404.html body, 276237 bytes; treated as 404)

### docs.vespa.ai/AGENTS.md and /CLAUDE.md — HTTP 404

<https://docs.vespa.ai/AGENTS.md>

do not exist (soft-404: 200 with 404.html body)

## What they do better than XERJ

- Every docs page is available as Markdown by appending .md to the URL ('Every page of the documentation is available in Markdown format, by changing the URL from `.html` to `.html.md`'), and llms.txt links the .md variant directly — so an agent with only a fetch tool (no shell) gets clean text on every hop. XERJ links .md for answers/ but /docs/install and /docs/quickstart are linked as HTML.
- A 43-line prose mental-model primer before the link index (Key Features, Architecture, Application Package, APIs) — orients an agent on what the system IS before it clicks anything. XERJ's preamble goes straight to demands and install.
- Agent onboarding is a product feature shipped in the main CLI: `vespa skills install` (with --harness claude,codex,cursor,antigravity and --local/--global), plus `vespa skills list` / `vespa skills update`. Skills land in the directory the harness auto-discovers. XERJ has `xerj init` writing .mcp.json + a 10-line skill, but only for Claude Code/Cursor and it is buried as 'optional' step 3.
- A separate, Apache-2.0, machine-checked skills repo (vespa-engine/skills): SKILL.md is the single source of truth, `python generate.py --check` fails CI if AGENTS.md / cursor plugin.json / README table drift, a 500-line SKILL.md cap, an evals suite (`uv run vespaskills eval`), and a `.claude-plugin/marketplace.json` for the Claude Code plugin marketplace.
- Second, zero-friction install channel through a third-party universal installer: `npx skills add vespa-engine/skills` ('installs skills into any of 70+ supported agent harnesses'). XERJ has no npx/one-click path.
- README 'Example Prompts' section: four copy-paste user prompts per skill area (schema, app package, hybrid query, feed) that show a human what to ask their agent — a concrete adoption trigger XERJ's llms.txt lacks.
- Honest, explicit MCP status statement in the human-written page: 'We don't provide any official MCP server at this time, but will update this page as soon as we do.' — sets expectations instead of leaving the agent to discover absence.
- Size warning attached to llms-full: 'This file is relatively large (almost 0.5M words as of Oct 2025), so use accordingly.'

## What XERJ does better

- XERJ's llms.txt actually contains install commands for three OSes at line 11-12 ('curl -fsSL https://xerj.org/get | sh', 'irm https://xerj.org/get.ps1 | iex'), a numbered first-run sequence, the PATH gotcha, and the data-dir-outside-index gotcha. Vespa's docs llms.txt has zero install commands and zero imperatives — it is a plain auto-generated docs index with a prose preface.
- XERJ's llms.txt is a curated 234-line file with '## Start here', '## Caveats', '## Core agent operations' and per-link descriptions written for an agent; Vespa's is 998 lines, alphabetised by filename (so 'Approximate Nn Hnsw' precedes 'Attributes' and the quick start is nowhere obvious), and every description slot is an empty `[](... "View as Markdown")` generator artefact.
- XERJ ships an MCP server inside the binary (`xerj mcp`), publishes tool schemas (mcp-tools.json, anthropic-tools.json) and gives the HTTP fallback (`POST http://localhost:9200/ax-*/_search` with an ES query-DSL object); Vespa has no official MCP server and no tool schema.
- XERJ has a feedback mechanism at all (`xerj feedback --open-pr --agent ... --verdict ...`, CLA-exempt one-file PRs, 'a negative report is a good report'). Vespa asks nothing of the agent and has no agent-contribution protocol — its Contributing page is a human GitHub-flow guide.
- XERJ has a Caveats section and honesty scaffolding (lexical embedder, measured numbers) up front; Vespa's preamble is marketing-adjacent ('powerful and scalable', 'highly efficient') with no caveats.
- XERJ's llms-full is 84 KB (curated); Vespa's is 4.5 MB / 95,612 lines — an unusable single fetch for most agents, which their own docs concede.

## Adoptable ideas

- Ship agent onboarding as a first-class CLI subcommand family mirroring `vespa skills install` / `vespa skills list` / `vespa skills update` (https://docs.vespa.ai/en/reference/clients/vespa-cli/vespa_skills_install.html.md). XERJ already has `xerj init`; promote it from '(optional, one command)' step 3 to the primary step right after install, add `--harness claude,codex,cursor,antigravity` and `--local/--global` flags, and copy the verbatim behaviour statement: 'copied into the directory the chosen harness(es) discover automatically'. This directly addresses the shell-less/MCP-only persona failure: the skill/MCP registration happens once on the operator's machine.
- Publish a standalone `xerj-org/skills` repo, Apache-2.0, one folder per skill with SKILL.md frontmatter (`name`, `description`) plus `docs/`, and make it installable via the universal installer: `npx skills add xerj-org/skills` (https://github.com/vercel-labs/skills, quoted from the Vespa README: 'installs skills into any of 70+ supported agent harnesses'). Add `.claude-plugin/marketplace.json` so it appears in the Claude Code plugin marketplace. This is the missing one-click/no-shell channel.
- Copy the CI drift gate: 'Run `python generate.py --check` before submitting a PR' / '`python generate.py --check  # CI mode — exits 1 if any generated file is out of date`' (https://raw.githubusercontent.com/vespa-engine/skills/main/README.md). Generate AGENTS.md, mcp-tools.json descriptions, and the llms.txt 'Core agent operations' section from one source so the 9-of-12 'lexical misread as neural' class of drift gets caught mechanically.
- Add an 'Example Prompts' block to llms.txt and README, in Vespa's format (bold area label + blockquoted prompt), e.g. from the Vespa README: '**Query building:**\n> "Write a hybrid search query that combines BM25 text matching with nearest-neighbor vector search, using reciprocal rank fusion."' — XERJ equivalents would be one prompt each for autoindex, search, def, brain, feedback. These are what a human pastes into an agent, i.e. the adoption trigger for humans, not agents.
- Make every docs URL in llms.txt resolve to Markdown (Vespa: 'Every page of the documentation is available in Markdown format, by changing the URL from `.html` to `.html.md`', https://docs.vespa.ai/en/learn/llm-help.html.md). XERJ links https://xerj.org/docs/install and /docs/quickstart as HTML; serve /docs/install.md and /docs/quickstart.md and link those, so a fetch-only persona never lands on HTML.
- Adopt Vespa's explicit-status sentence pattern for the MCP server and the field report: 'We don't provide any official MCP server at this time, but will update this page as soon as we do.' XERJ should state the inverse plainly at the top: which release ships `xerj mcp`, and — for the feedback problem — rename '## Optional' (the llms.txt spec's own reserved heading name for skippable content) to something like '## Your side of the deal (required)' and move the field-report entry into '## Start here' step 4 only; the spec-reserved 'Optional' heading is telling agents the report can be skipped, which contradicts 'required, not optional' on line 7.
- Copy the size-honesty line for llms-full: 'This file is relatively large (almost 0.5M words as of Oct 2025), so use accordingly.' — XERJ's llms.txt should state llms-full's size (84 KB / ~1,235 lines) and when to prefer it over per-page fetches, so a token-budgeted agent can choose.
- Add a SKILL.md-level 'Verify the installation' step with the expected command, as in vespa-cli/SKILL.md ('Verify the installation:\n\n```bash\nvespa version\n```'). XERJ's Start-here jumps from install to starting a node; insert `xerj --version` (or the full-path equivalent given the PATH caveat) as an explicit verification checkpoint that the policy-constrained persona can report on.
