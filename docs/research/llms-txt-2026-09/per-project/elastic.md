# Elastic (Elasticsearch docs)

Category: search engine

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://www.elastic.co/docs/llms.txt> — HTTP 200
- Size: 17198 bytes, 171 lines
- Install position: absent — no install command anywhere. The closest thing is a bare link under '## Deploy and manage' (line 72): '* [Deploy](https://www.elastic.co/docs/deploy-manage/deploy)' with no description, and line 96 '* [Uninstall](...)'. Under '## Elastic fundamentals' (line 25) the first bullet is '[Evaluate Elastic during a trial]' — a sales-funnel framing, not an install.
- Tone: neutral / lightly marketing in the intro blockquote ('Build powerful search and RAG applications...', 'reduce your time to value'). Purely descriptive link index otherwise. Makes zero demands of the agent; never addresses the reader as an agent at all. The word 'agent' appears only as product names (Elastic Agent, Elastic Agent Builder).
- Section order: # Elastic Documentation (blockquote intro + 3 solution links + 8-item doc-structure overview) → ## Elastic fundamentals → ## Solutions and use cases → ## Manage data → ## Explore and analyze data with Kibana → ## Deploy and manage → ## Manage your Cloud account and preferences → ## Troubleshooting → ## Elastic release notes → ## Reference → ## Extend and contribute → ## Contribute to Elastic documentation

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

```text
## Contribute to Elastic documentation

* [Contribute on the web](https://www.elastic.co/docs/contribute-docs/on-the-web): Make documentation updates directly in your browser using GitHub's web editor without setting up a local development environment.
* [Contribute locally](https://www.elastic.co/docs/contribute-docs/locally): Set up Elastic documentation repositories locally and learn how to build, preview, and submit documentation changes.
```

### Best passage for driving install and first use (verbatim)

```text
* [**Elastic fundamentals**](https://www.elastic.co/docs/get-started): Understand the basics about the deployment options, platform, and solutions, and features of the documentation.
* [**Solutions and use cases**](https://www.elastic.co/docs/solutions): Learn use cases, evaluate, and implement Elastic's solutions: Observability, Search, and Security.
* [**Manage data**](https://www.elastic.co/docs/manage-data): Learn about data store primitives, ingestion and enrichment, managing the data lifecycle, and migrating data.

(NOTE: this is the closest thing to an onboarding passage; there is no passage that would get a first-time agent to install and use the product. The file is a plain auto-generated docs index with nothing about installation.)
```

## llms-full.txt

- URL: <https://www.elastic.co/docs/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Does not exist. Also 404: https://www.elastic.co/llms.txt, https://www.elastic.co/llms-full.txt, https://www.elastic.co/docs/llms-install.md, https://raw.githubusercontent.com/elastic/elasticsearch/main/llms.txt, .../llms-install.md, .../SKILL.md. Elastic ships only the single docs-root llms.txt link index.
- Install position: absent (file does not exist)

## Other agent-facing files

### AGENTS.md (elastic/elasticsearch repo) — HTTP 200

<https://raw.githubusercontent.com/elastic/elasticsearch/main/AGENTS.md>

Contributor guide for coding agents working INSIDE the Elasticsearch source repo (184 lines, 16621 bytes): toolchain, Gradle commands, project structure, test cheatsheet, formatting, license headers, plus a dedicated '## Best Practices for Automation Agents' section and a '## Methods with Required Javadoc Reading' list. It is about contributing code, not installing or using Elasticsearch. Explicitly bans AI attribution trailers.

```text
## Best Practices for Automation Agents
- Never edit unrelated files; keep diffs tightly scoped to the task at hand.
- Prefer Gradle tasks over ad-hoc scripts.
- When scripting CLI sequences, leverage `gradlew` task.
- Unrecognized changes: assume other agent; keep going; focus your changes. If it causes issues, stop + ask user.
- Do not add "Co-Authored-By" or any AI attribution trailers to commit messages, by any means—including `--trailer`, `-m`, or any other git flag. commit messages should adhere to the 50/72 rule: use a maximum of 50 columns for the commit summary. Your harness may introduce a hook that automatically adds attributions trailers to relevant git commands. Use `bash -lc` or a similar approach in this case to conform to the rule.
```

### MCP server README (elastic/mcp-server-elasticsearch) — HTTP 200

<https://raw.githubusercontent.com/elastic/mcp-server-elasticsearch/main/README.md>

Install/config guide for the (deprecated) Docker-distributed Elasticsearch MCP server (272 lines, 10660 bytes). Contains a docker run command, a Claude Desktop mcpServers JSON snippet (stdio), an HTTP mode + mcp-proxy variant, a '### Verify the connection' checklist, a health/monitoring section, and a 5-tool list (list_indices, get_mappings, search, esql, get_shards). Opens with a deprecation CAUTION pointing to Elastic Agent Builder's MCP endpoint (9.2.0+). No one-click links, no 'claude mcp add', no npx.

````text
#### Configure Claude Desktop

Add this configuration to your Claude Desktop configuration file:

```json
{
  "mcpServers": {
    "elasticsearch-mcp-server": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-e", "ES_URL",
        "-e", "ES_API_KEY",
        "docker.elastic.co/mcp/elasticsearch",
        "stdio"
      ],
      "env": {
        "ES_URL": "<elasticsearch-cluster-url>",
        "ES_API_KEY": "<elasticsearch-API-key>"
      }
    }
  }
}
```
````

### CLAUDE.md (elastic/elasticsearch repo) — HTTP 200

<https://raw.githubusercontent.com/elastic/elasticsearch/main/CLAUDE.md>

Exists but is 10 bytes / 0 newline — effectively a pointer stub (almost certainly '@AGENTS.md' or similar include). INFERRED, not verified content beyond size.

### llms-install.md / SKILL.md / repo-root llms.txt — HTTP 404

<https://raw.githubusercontent.com/elastic/elasticsearch/main/llms-install.md>

None of these exist (llms-install.md 404 at both docs root and repo root; SKILL.md 404; repo-root llms.txt 404).

## What they do better than XERJ

- Size discipline: 171 lines / 17 KB versus XERJ's 234 lines / 40 KB. Every line is one link + one sentence; an agent can scan the whole thing in one read with no prose to wade through.
- Consistent one-sentence link descriptors written for search ('Overview of Elastic deployment options including Elastic Cloud, self-managed, and serverless, to help choose the infrastructure model that fits your needs.') — the description contains the keywords an agent would query for, which matches XERJ's own 'search it for your topic' framing better than some of XERJ's longer bullets do.
- A stable, product-agnostic section taxonomy (fundamentals / solutions / manage data / explore / deploy / troubleshooting / release notes / reference / contribute) that maps 1:1 onto the docs site nav, so a hop from llms.txt lands on the identically named page.
- AGENTS.md has a hard, tool-level rule for the co-author trailer problem ('by any means—including `--trailer`, `-m`, or any other git flag ... Use `bash -lc`') — more explicit than XERJ's CLA co-author gate wording, and worth mirroring in AI_CONTRIBUTIONS.md.
- AGENTS.md 'Methods with Required Javadoc Reading' pattern: a short must-read-before-acting list. XERJ's '## Caveats (read before you rely on a capability)' is the same idea, but Elastic's is a bare list of names rather than paragraphs.
- MCP README has an explicit '### Verify the connection' + 'If the connection fails, verify:' checklist and a '### Available tools' list with one-line tool semantics — a shell-less agent can read that and know what it can call.

## What XERJ does better

- XERJ's llms.txt is an actual onboarding surface: install command on line 11 ('curl -fsSL https://xerj.org/get | sh'), a numbered first-run path, `xerj init` MCP wiring, and a field-report ask. Elastic's llms.txt has no install command, no agent prompt, no MCP mention, no one-click link — it is a plain auto-generated docs index with nothing about installation.
- XERJ addresses the reader as an agent in the first line ('This file is an index for you, the AI agent'); Elastic never acknowledges an agent reader in llms.txt.
- XERJ ships llms-full.txt (1,235 lines) — Elastic has none at any of four candidate URLs.
- XERJ has a '## Caveats' section with honesty content (lexical-vs-neural, resource-aware runs); Elastic's file has no caveats, no honest-claims material, only marketing intro copy.
- XERJ has an HTTP-only agent path documented ('[Agent quickstart]: the HTTP-only path for an agent with no shell'); Elastic's MCP path requires Docker + AWS Marketplace and is deprecated.
- XERJ's MCP server is a first-party binary feature (`xerj init` writes .mcp.json); Elastic's is deprecated and superseded by a cloud product, so an agent reading Elastic's material has no supported local MCP route.
- XERJ's contribution ask is a one-command CLI (`xerj feedback --open-pr ...`); Elastic's contribution ask is 9 links to docs-writing style guides, none of which is a one-step action.

## Adoptable ideas

- Elastic has NO answer to the field-report problem — nothing here explains the low PR rate except by contrast: Elastic's feedback ask is 9 passive links and gets no agent PRs by design. The XERJ-specific fix is structural, not borrowed: move the field-report bullet out of the section literally titled '## Optional' (llms.txt line 223) and into '## Start here' step 4 where it already says 'required, not optional' — an agent that reads section headings as authority (which is how Elastic's file is built) sees the 'Optional' heading and drops the step.
- Adopt Elastic's one-line-per-link discipline for the '## Answers' block (llms.txt lines 79-174, the bulk of the 40 KB): cap each descriptor at one sentence like Elastic's '* [Deployment options](...): Overview of Elastic deployment options including Elastic Cloud, self-managed, and serverless, to help choose the infrastructure model that fits your needs.' — target ~20 KB so the index reads in one hop.
- Add a 'Verify the connection' + 'If it fails, verify:' checklist for the MCP/HTTP-only path, modelled verbatim on https://raw.githubusercontent.com/elastic/mcp-server-elasticsearch/main/README.md: '1. Start your MCP client (for example, Claude Desktop or Cursor). 2. Check that the Elasticsearch MCP Server appears in your available MCP servers. 3. Test a simple query through your agent interface to confirm it can access your Elasticsearch indices.' — this directly targets the shell-less persona failure from the 22-run study.
- Add an '### Available tools' one-liner list for the XERJ MCP server, in Elastic's format ('- `list_indices`: List all available Elasticsearch indices / - `search`: Perform an Elasticsearch search using query DSL'), so a no-shell agent knows the tool surface without fetching a schema.
- Copy the AGENTS.md trailer-ban wording into .github/AI_CONTRIBUTIONS.md: 'Do not add "Co-Authored-By" or any AI attribution trailers to commit messages, by any means—including `--trailer`, `-m`, or any other git flag. ... Your harness may introduce a hook that automatically adds attributions trailers to relevant git commands. Use `bash -lc` or a similar approach in this case to conform to the rule.' — it names the harness-hook failure mode that XERJ's CLA co-author gate (#269/#308) has already hit.
- Adopt the 'Methods with Required Javadoc Reading' pattern ('If you encounter any of the following methods, you must go and read their javadoc before taking any other actions:') as a compact 'Read before you rely on' list at the top of XERJ's Caveats — e.g. a bare list: lexical-not-neural, autoindex is CLI-only, estimate before running — since 9 of 12 agents misread lexical as neural despite the current paragraph form.
- Borrow Elastic's 'Unrecognized changes: assume other agent; keep going; focus your changes. If it causes issues, stop + ask user.' for XERJ's autoindex/init guidance about pre-existing .mcp.json / skill files (XERJ already writes .bak backups; the rule tells the agent what to do when it finds someone else's edits).
- Positioning evidence for docs/WHY_XERJ.md and the ES scorecard: Elastic's own MCP server README opens '> This MCP server is deprecated and will only receive critical security updates going forward. It has been superseded by the Elastic Agent Builder MCP endpoint, which is available in Elastic 9.2.0+ and Elasticsearch Serverless projects.' — i.e. the supported ES agent path is now a hosted product, while XERJ's is a local binary. Quote it verbatim with the URL rather than characterising it.
