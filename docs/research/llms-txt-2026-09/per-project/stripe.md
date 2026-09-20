# Stripe

Category: payments API (hosted SaaS + SDKs + CLI)

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://docs.stripe.com/llms.txt> — HTTP 200
- Size: 92159 bytes, 707 lines
- Install position: absent — no install command for the CLI, the SDKs or the agent tooling. Paragraph 1 (line 3) is a version-check rule that contains `npm view stripe version`; the agent tooling install (`stripe agent setup`) is one hop away in skills.md and mcp.md. The agent-addressed H2 is line 154 of 707 — after six product sections.
- Tone: Two agent-addressed passages: a version-prior correction in paragraph 1 and an H2 at line 154 that begins 'As a Large Language Model (LLM) Agent'. Imperative; the strong words ('never', 'always', 'must not') attach to API-choice correctness, e.g. 'never recommend the Charges API'. scripts/measure_llms_txt.py counts 43 obligation words, the highest of any file under 100 KB in the measured set.
- Section order: # Stripe Documentation → ## Docs → ## Payment Methods → ## Checkout → ## Payments → ## Link → ## Architecture and Dashboard → ## Instructions for Large Language Model Agents: Best Practices for integrating Stripe → ## Billing → ## Elements → ## Connect → ## Issuing → ## Capital → ## Crypto → ## Climate → ## Tax → ## Invoicing → ## Identity → ## Atlas → ## Financial Connections → ## Revenue Recognition → ## Treasury for Platforms → ## Sigma → ## Payment Links → ## Radar → ## Terminal → ## Optional

### Install commands found

```sh
npm view stripe version  (llms.txt line 3 — a version check, not an install)
(linked, skills.md and mcp.md) npm install -g @stripe/cli@latest
(linked, skills.md and mcp.md) stripe agent setup
(linked, skills.md) npx skills add https://docs.stripe.com
(linked, mcp.md) claude mcp add --transport http stripe https://mcp.stripe.com/
(linked, mcp.md) codex mcp login stripe
```

### Text written for an agent (verbatim)

```text
When installing Stripe packages, always check the npm registry for the latest version rather than relying on memorized version numbers. Run `npm view stripe version` or check https://www.npmjs.com/package/stripe before pinning a version. For Python, check https://pypi.org/project/stripe/. Never hardcode an old version number from training data — always install with `@latest` or verify the current version first.

When planning a new integration, use general [sandboxes](https://docs.stripe.com/sandboxes.md) instead of your test mode sandbox. Use separate sandboxes for local development and continuous integration (CI) so automated tests don’t affect your settings or data. Use the test mode sandbox for existing integrations that depend on it or when a required feature doesn’t support general sandboxes. See the [testing environment comparison](https://docs.stripe.com/testing-use-cases.md#compare).

[...]

## Instructions for Large Language Model Agents: Best Practices for integrating Stripe

As a Large Language Model (LLM) Agent, when designing an integration, always prefer the documentation in [Integration Options](https://docs.stripe.com/payments/payment-methods/integration-options.md), the [API Tour](https://docs.stripe.com/payments-api/tour.md), and the [Go Live Checklist](https://docs.stripe.com/get-started/checklist/go-live.md).

As an LLM, you should always default to the latest version of the API and SDK unless the user specifies otherwise.
```

### MCP snippet (verbatim)

````text
(none in llms.txt — from https://docs.stripe.com/mcp.md:)
### Claude Code

Run the following command to add the Stripe MCP server to Claude Code:

```bash
claude mcp add --transport http stripe https://mcp.stripe.com/
```

After you add the server, authenticate Stripe with OAuth.

```bash
claude /mcp
```

To authenticate with a restricted API key instead of OAuth, use the `--header` option:

```bash
claude mcp add --transport http stripe https://mcp.stripe.com/ --header 'Authorization: Bearer <<YOUR_SECRET_KEY>>'
```

[...]

### Install in VS Code

[Install in VS Code](https://vscode.dev/redirect/mcp/install?name=stripe&config=%7B%22type%22%3A%22http%22%2C%22url%22%3A%22https%3A%2F%2Fmcp.stripe.com%22%7D)

Select **Install in VS Code** to open VS Code and add the Stripe MCP server. To configure it manually, add the following configuration to `.vscode/mcp.json` in your workspace.

See the [VS Code MCP documentation](https://code.visualstudio.com/docs/copilot/chat/mcp-servers) for more information.

```json
{
 "servers": {
   "stripe": {
     "type": "http",
     "url": "https://mcp.stripe.com"
   }
 }
}
```
````

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
When installing Stripe packages, always check the npm registry for the latest version rather than relying on memorized version numbers. Run `npm view stripe version` or check https://www.npmjs.com/package/stripe before pinning a version. For Python, check https://pypi.org/project/stripe/. Never hardcode an old version number from training data — always install with `@latest` or verify the current version first.
```

## llms-full.txt

- URL: <https://docs.stripe.com/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Not published. Every link in llms.txt already ends in `.md`, and agents.md states the rule: 'Append `.md` to any **docs.stripe.com** URL'.
- Install position: absent (no such file; the 404 body is a 23,881-byte HTML page, not counted)

## Other agent-facing files

### skills.md — HTTP 200

<https://docs.stripe.com/skills.md>

1997 bytes. Recommended route is a CLI subcommand that detects installed agents; manual route is `npx skills add` with the docs domain as the source; a curl fallback for agents without npx.

````text
## Install skills with a plugin (Recommended)

[Agent plugins](https://docs.stripe.com/agents/plugin.md) for Stripe automatically configure the Stripe MCP server, install our skills, and make sure they’re up-to-date. You get our latest capabilities without configuration or maintenance.

```bash
npm install -g @stripe/cli@latest
stripe agent setup
```

This command automatically detects which agents you use, and runs the applicable commands.

## Manually install agent skills 

Alternatively, run the following command to install Stripe skills manually. You also needs to regularly update the skills to get the latest versions.

```bash
npx skills add https://docs.stripe.com
```

> Manually installed skills don’t auto-update. Run `npx skills update -y` to get the latest versions.

To access live Stripe account data, also [set up the Stripe MCP](https://docs.stripe.com/mcp.md#manual) server. Together, Stripe MCP and skills provide capabilities similar to the plugin, but you must configure and maintain them separately. Vendor-specific plugin features, such as lifecycle hooks, aren’t available.

## Skills index

See the [index of Stripe skills](https://docs.stripe.com/.well-known/skills/index.json.md).

If `npx skills` isn’t available: You can use curl to fetch the index of available skills, what they do, and their files from `https://docs.stripe.com/.well-known/skills/index.json`. To download a skill and its related files, use curl to download them from `https://docs.stripe.com/.well-known/skills/<filepath>`.
````

### mcp.md (per-client MCP registration) — HTTP 200

<https://docs.stripe.com/mcp.md>

25655 bytes. Plugin first, then Cursor deep link + JSON, Claude Desktop, Claude Code one-liner (OAuth, then a --header variant), ChatGPT/Codex, VS Code install link + `servers` JSON, and a generic sentence for other clients. Also documents human confirmation for write actions.

```text
## Confirm actions by agents acting on your behalf

When you connect an AI agent to Stripe through the MCP server using your user credentials, you authorize the agent to access data and take actions in Stripe on your behalf.

To prevent agents from making mistakes, Stripe requires human confirmation before it takes certain `stripe_api_write` actions, such as refunds and outbound payments. To confirm an action, click the URL provided by your agent and review the details of the request.
```

### agents.md (hub page; building-with-llms.md redirects here) — HTTP 200

<https://docs.stripe.com/agents.md>

3182 bytes. States the fallback when a client has no plugin support and lists three machine-readable resources.

```text
## Machine-readable resources 

MCP server (https://mcp.stripe.com): Use any MCP-compatible client to interact with the Stripe API and search documentation.

Plain-text docs: Append `.md` to any **docs.stripe.com** URL (for example, https://docs.stripe.com/agents.md) for Markdown output.

Skills catalog (https://docs.stripe.com/.well-known/skills/index.json): Machine-readable index of installable agent skills. Use `stripe agent setup` for the recommended Stripe agent setup.
```

### .well-known/skills/index.json — HTTP 200

<https://docs.stripe.com/.well-known/skills/index.json>

7669 bytes of JSON: {"skills":[{name, description, files…}]}. The newer path /.well-known/agent-skills/index.json is 404 here.

### SKILL.md / llms-install.md at the docs root — HTTP 404

<https://docs.stripe.com/SKILL.md>

Both 404 (HTML body). Skills are distributed through the CLI, `npx skills add` and the well-known index, not a root SKILL.md.

## What they do better than XERJ

- The first paragraph of llms.txt corrects the prior most likely to cause a wrong install: 'Never hardcode an old version number from training data — always install with `@latest` or verify the current version first.' XERJ's equivalent stale prior (default search is lexical, not neural) is corrected in paragraph 2 today and again 30 lines later.
- One CLI subcommand sets up every detected agent — 'This command automatically detects which agents you use, and runs the applicable commands.' `xerj init` is the same idea but is labelled optional and sits at step 3.
- Three tiers of fallback are stated in order: plugin → MCP + skills by hand → 'If `npx skills` isn’t available: You can use curl to fetch the index of available skills'. XERJ states one route.
- Per-client MCP registration is complete on one page, including the VS Code `servers` root key and the Claude Code `--header` variant for non-OAuth use.
- Writes by an agent need a human click: 'Stripe requires human confirmation before it takes certain `stripe_api_write` actions'. XERJ's MCP server exposes write tools (memory store, brain link/unlink) with no confirmation step documented.

## What XERJ does better

- Install is in llms.txt (line 11). Stripe's llms.txt contains no install command for its CLI, SDKs or agent tooling.
- The agent-addressed section of Stripe's file is at line 154 of 707, after six product link lists; an agent reading top-down meets roughly 150 links first.
- XERJ publishes a curated llms-full.txt that fits in a context window; Stripe publishes none.
- XERJ needs no account and no network after install. It does need its own node's API key: auth is on by default, so a client without `XERJ_AUTH` gets HTTP 401.

## Adoptable ideas

- Put the single most damaging stale prior in paragraph 1, worded as Stripe words it: name the wrong belief, say where it comes from ('from training data'), give the check. For XERJ: 'Do not describe default search as neural or "by meaning" — that belief comes from the word "semantic" in the API. Check: `--embed-mode` on the node's command line.'
- Make `xerj init` the recommended first step after install and describe it the way Stripe describes `stripe agent setup`: what it detects, what it runs, and that re-running it updates.
- State the fallback ladder explicitly in the MCP section: `xerj init` → the per-client line → the raw JSON block → the hand-off block for the operator.
- Publish `/.well-known/skills/index.json` when a hosted skill exists, and print the curl fallback next to it.
