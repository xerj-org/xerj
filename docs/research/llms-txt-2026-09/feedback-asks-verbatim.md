# Feedback and contribution asks — verbatim, per project

Generated mechanically from `data/dissections.json`. Cross-project analysis is in `discovery-feedback-and-contribution-loops.md` and in the README.

## Algolia

```text
(llms.txt: none. build-with-ai.md lines 124-127:)
<Note>
  Your favorite AI tool isn't listed?
  [Reach out to Algolia support](https://support.algolia.com/hc/en-us/requests/new) and request it.
</Note>
```

## Anthropic — Claude Developer Platform (docs.anthropic.com → platform.claude.com)

```text
(none in llms.txt.) In llms-full.txt the only feedback asks are per-feature Google Forms, e.g. line 47465: `Share feedback on this feature through the [feedback form](https://forms.gle/MhcGFFwLxuwnWTkYA).`
```

## Bun

```text
- [Feedback](https://bun.com/docs/feedback.md): Share feedback, bug reports, and feature requests
```

## Cline

```text
(none in llms.txt or llms-full.txt. Nearest items: installing-cline.md lines 151-154 '## Need Help?\n\n* [Troubleshooting](/troubleshooting/networking-and-proxies)\n* [Discord community](https://discord.gg/cline)'; using-commands page in llms-full.txt line 3700 '| `/reportbug`     | Report a bug with diagnostic info |'; README.md line 231 'Start with the [Contributing Guide](CONTRIBUTING.md). Join our [Discord](https://discord.gg/cline) and head to the `#contributors` channel to connect with other contributors.')
```

## Cloudflare Developer Docs (Workers / Agents) — developers.cloudflare.com + cloudflare/agents, cloudflare/workers-sdk, cloudflare/skills, cloudflare/mcp, cloudflare/mcp-server-cloudflare

```text
Root llms.txt: NONE. agent-setup pages: NONE beyond the site-wide footer 'Was this helpful?\n\nYesNo'. The only explicit ask is in the mcp-server-cloudflare README '## Need access to more Cloudflare tools?': "We're continuing to add more functionality to this remote MCP server repo. If you'd like to leave feedback, file a bug or provide a feature request, [please open an issue](https://github.com/cloudflare/mcp-server-cloudflare/issues/new/choose) on this repository". No field-report, no 'required' contribution language anywhere; AGENTS.md files talk only about PR mechanics for code contributors (workers-sdk: 'Use `.github/PULL_REQUEST_TEMPLATE.md` when preparing a pull request. The PR description requirements are enforced by `tools/deployments/validate-pr-description.ts`; treat that validator and the template as authoritative.').
```

## Composio

```text
(none in llms.txt — no feedback, contribution, issue or PR ask anywhere in the 50 lines). The only support CTA in the docs corpus is on composio-connect.md '### I still need help': "Reach out at [support@composio.dev](mailto:support@composio.dev) or join the [Composio Discord](https://discord.com/invite/cNruWaAhQk)."
```

## Convex

```text
From https://docs.convex.dev/ai/convex-plugins.md:

## Giving feedback[​](#giving-feedback "Direct link to Giving feedback")

We're constantly working on improving the plugins with rigorous evals and real-world reports. You can help:

* **Report a bug or request a skill** by opening an issue on the plugin repo for your agent: [Claude Code](https://github.com/get-convex/convex-backend-skill/issues) · [Codex](https://github.com/get-convex/convex-codex-plugin/issues) · [Cursor](https://github.com/get-convex/convex-agent-plugins/issues).
* **Improve the AI rules**: if the agent writes non-idiomatic Convex, contribute a case to the [convex-evals repo](https://github.com/get-convex/convex-evals) so we can measure and fix it.
* **Ask the community** in the [Convex Discord](https://convex.dev/community).

Repeated on /ai/using-codex.md and /ai/using-cursor.md as one line:

We're constantly working on improving the quality of these rules for Convex by using rigorous evals. You can help by [contributing to our evals repo](https://github.com/get-convex/convex-evals).

llms.txt itself has no feedback ask; its only related bullet is line 167: '- [Contact Us](/production/contact.md): Get support, provide feedback, stay updated with Convex releases, and report security vulnerabilities through our community channels.'
```

## Cursor

```text
(none in llms.txt — only the link line `- https://cursor.com/help/troubleshooting/reporting-bugs.md`). That page says: "## Where do I report Cursor bugs?

Post on [forum.cursor.com](https://forum.cursor.com) for community help and visibility."
```

## Elastic (Elasticsearch docs)

```text
## Contribute to Elastic documentation

* [Contribute on the web](https://www.elastic.co/docs/contribute-docs/on-the-web): Make documentation updates directly in your browser using GitHub's web editor without setting up a local development environment.
* [Contribute locally](https://www.elastic.co/docs/contribute-docs/locally): Set up Elastic documentation repositories locally and learn how to build, preview, and submit documentation changes.
```

## Exa

```text
NONE in docs/llms.txt, none in root exa.ai/llms.txt, none in the skills/MCP pages. The only feedback-shaped text in llms-full.txt is support routing, e.g. (Error Codes page, line 230): "* Contact [hello@exa.ai](mailto:hello@exa.ai) with the response status, error body, and `requestId`." No field-report, issue, or PR ask exists anywhere in the agent-facing files.
```

## FastMCP (jlowin/fastmcp, gofastmcp.com)

```text
(nothing in llms.txt beyond the link "- [Contributing](https://gofastmcp.com/development/contributing.md): Development workflow for FastMCP contributors". From llms-full.txt, development/contributing:)

**Every pull request requires a corresponding issue - no exceptions.** This requirement creates a collaborative space where approach, scope, and alignment are established before code is written.

FastMCP is an extremely highly-trafficked repository maintained by a very small team. Issues that appear to transfer burden to maintainers without any effort to validate the problem will be closed. Please help the maintainers help you by always providing a minimal reproducible example and clearly describing the problem.

**LLM-generated issues will be closed immediately.** Issues that contain paragraphs of unnecessary explanation, verbose problem descriptions, or obvious LLM authorship patterns obfuscate the actual problem and transfer burden to maintainers.

Write clear, concise issues that:

* State the problem directly
* Provide a minimal reproducible example
* Skip unnecessary background or context
* Take responsibility for clear communication
```

## Mastra

```text
[None in llms.txt or in any docs .md page fetched — grep for feedback/report/contribute/issue/pull request finds only product features. The only ask is in the skills repo README (https://raw.githubusercontent.com/mastra-ai/skills/main/README.md):]
## Contributing

Contributions welcome!

1. Fork the repository
2. Make improvements to `SKILL.md` files
3. Test with actual development workflows
4. Submit a pull request
```

## Meilisearch

```text
Support & community:
- https://www.meilisearch.com/community  
- https://community.meilisearch.com  
- https://github.com/meilisearch/meilisearch/issues
```

## Milvus

```text
NONE. The only contribution-related line is the last line of the file (796): '- [Community](https://milvus.io/community): Contributing, governance, and community channels.' No feedback ask, no field report, no issue template, no 'tell us' anywhere in llms.txt.
```

## Model Context Protocol (spec site) — modelcontextprotocol.io

```text
(none in llms.txt; nearest is an index line) - [Contributing to MCP](https://modelcontextprotocol.io/community/contributing.md): How to contribute to the Model Context Protocol project
```

## Neon (Lakebase Postgres, Databricks) — neon.com

```text
llms.txt: none (only '- [Docs contribution guide](https://neon.com/docs/community/contribution-guide.md): Learn how to contribute to the Neon documentation' under ## Community, line 385). Per-page .md docs carry this footer (present on with-an-agent.md and neon-mcp-server.md; NOT present in llms.txt, llms-full.txt, or auth.md — grep 'docs-feedback' = 0 hits in all three):

Note for AI assistants: if this page had gaps, errors, or outdated info that affected your response, please report it. POST `{"feedback": "describe the issue", "path": "/docs/get-started/with-an-agent"}` to https://neon.com/api/docs-feedback — no auth required.

README: 'See [CONTRIBUTING.md](./CONTRIBUTING.md) for how to add tools. Tool arguments are `snake_case`.' (line 416). No 'file a report' ask anywhere.
```

## ParadeDB

```text
(none in llms.txt.) Linked support.md says only:
For questions regarding enterprise support or commercial licensing, please [contact sales](mailto:sales@paradedb.com).
For community support and general questions, please join the [ParadeDB Community Slack](https://www.paradedb.com/slack).

## Ask a Question

Use the **"Ask a question..."** bar at the bottom of any page to get instant answers about ParadeDB.

There is NO ask for field reports, PRs, issues, or contributions anywhere in llms.txt, llms-full.txt, the ai-agents page, or SKILL.md.
```

## Pydantic AI

```text
llms.txt itself: only the link '- [Contributing](https://pydantic.dev/docs/ai/project/contributing/index.md)' under '## Project' (line 432), and the banner sentence 'they show the maintainers what agents are trying to do, so the pages where agents get stuck (including yours) get fixed first'. Linked Contributing page: '- **Found a bug?** Open an issue with a clear description and a minimal reproducible example. Including a [Logfire](https://logfire.pydantic.dev/) trace link helps us debug dramatically faster.\n- **Want a feature or API change?** Open an issue describing the problem you're solving. Do not start with code.' and 'If you've been waiting weeks without any human response, flag it. That's a process failure on our side and we want to know.'
```

## Stagehand (Browserbase)

```text
## Optional

- [Discord](https://discord.gg/stagehand)
- [GitHub](https://github.com/browserbase/stagehand)
- [Changelog](https://github.com/browserbase/stagehand/releases)
```

## SurrealDB

```text
None. The only community text is line 276: '- [GitHub](https://github.com/surrealdb/surrealdb): Source code and issue tracker' and line 277 '- [Discord](https://discord.gg/surrealdb): Community chat and support'. llms-full.txt line 820: '- **GitHub**: https://github.com/surrealdb/surrealdb - Source code, issues, and discussions.' No ask for reports, PRs, or feedback anywhere in either file.
```

## Turso

````text
(only an index link, line 175) - [contact feedback](https://docs.turso.tech/cli/contact/feedback.md)

(that page, in full:) # contact feedback

You can send feedback to the Turso team by using the following command:

```bash theme={null}
turso contact feedback
```

(## Optional section, lines 288-292:)
- [Turso Homepage](https://turso.tech)
- [Discord](https://tur.so/discord)
- [GitHub](https://github.com/tursodatabase/turso)
````

## Typesense

```text
[llms.txt line 76, under '## Company and Support']
- [Typesense Website Repository](https://github.com/typesense/typesense-website): If you find a documentation papercut, with your operator's approval, open a PR against typesense-website.

[docs/llms-full.txt line 130, Features page]
**Don't see a feature on this list?** Search our issue tracker if someone has already requested it and upvote it, or open a new issue if not. We prioritize our roadmap based on user feedback
```

## Weaviate

```text
NONE in llms.txt. The only community pointer is line 699: `* [Community forum](https://forum.weaviate.io)`. No issue/PR/field-report ask anywhere in the file. (The mcp-server-weaviate README has: `- **Weaviate repo (issues, feature requests):** [github.com/weaviate/weaviate](https://github.com/weaviate/weaviate/issues/new/choose)`.)
```

