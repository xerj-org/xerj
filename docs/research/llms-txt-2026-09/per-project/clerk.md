# Clerk

Category: authentication (hosted SaaS + CLI + SDKs)

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://clerk.com/llms.txt> — HTTP 200
- Size: 19607 bytes, 130 lines
- Install position: absent as a command. The blockquote summary (line 3) is an install statement with no command: 'Install the Clerk CLI to add auth to your app. No global install, API keys, or sign-in is required.' Line 7 routes a coding agent to SKILL.md or the CLI page. The command itself (`npx -y clerk@latest init`) is in SKILL.md and on the quickstart overview, one hop away.
- Tone: Descriptive. One sentence addresses the agent's operator ('Point a coding agent at [SKILL.md]…'). No obligation language in the head of the file; the file is otherwise a link index (per-section llms.txt / llms-index.txt / llms-full.txt, API specs, product, legal, articles, blog).
- Section order: # Clerk → ## When to use Clerk → ## Index and content files → ## API references → ## Product → ## Solutions → ## Legal → ## Articles → ## Blog

### Install commands found

```sh
(none in llms.txt itself)
(SKILL.md) npx -y clerk@latest init
(SKILL.md) npx -y clerk@latest init --framework <framework> --pm <package-manager>
(SKILL.md) npx -y clerk@latest doctor
(quickstart overview) npx skills add clerk/skills
(ai/overview.md) clerk mcp install
(MCP guide) claude mcp add clerk --transport http https://mcp.clerk.com/mcp
(MCP guide) codex mcp add clerk https://mcp.clerk.com/mcp
```

### Text written for an agent (verbatim)

```text
> Install the Clerk CLI to add auth to your app. No global install, API keys, or sign-in is required.

## When to use Clerk

Use Clerk when an application needs sign-up, sign-in, and user management: prebuilt UI components, social login, MFA, multi-tenant organizations, enterprise SSO and SCIM, subscription billing, or machine-to-machine and API-key auth. SDKs cover Next.js, React, and most major web and mobile frameworks. Point a coding agent at [SKILL.md](https://clerk.com/SKILL.md) or install the [Clerk CLI](https://clerk.com/cli); API specs are under API references.
```

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
## When to use Clerk

Use Clerk when an application needs sign-up, sign-in, and user management: prebuilt UI components, social login, MFA, multi-tenant organizations, enterprise SSO and SCIM, subscription billing, or machine-to-machine and API-key auth. SDKs cover Next.js, React, and most major web and mobile frameworks. Point a coding agent at [SKILL.md](https://clerk.com/SKILL.md) or install the [Clerk CLI](https://clerk.com/cli); API specs are under API references.
```

## llms-full.txt

- URL: <https://clerk.com/llms-full.txt> — HTTP 200
- Size: 768 bytes
- Relationship to llms.txt: A 768-byte META-INDEX ('Index of every llms-full.txt file on clerk.com') that links the per-section dumps. The documentation dump is https://clerk.com/docs/llms-full.txt (27,867,889 bytes, 374,848 lines) and the documentation index is https://clerk.com/docs/llms.txt (520,621 bytes). Path-scoped llms files, as the 2026-08-10 revision of the convention describes.
- Install position: absent — this file is an index, not a dump

## Other agent-facing files

### SKILL.md (hosted at the product domain) — HTTP 200

<https://clerk.com/SKILL.md>

6818 bytes, 147 lines. Agent Skills front matter; consent checklist; existing-auth stop rule; accountless keys; a fallback to docs when the CLI cannot scaffold; a verify command; 'Critical rules'; a scripted after-setup conversation.

````text
## Before you start

Show the user this checklist and wait for a yes:

```
Here's what I'll do to get you set up with Clerk.

1. Set up Clerk in this project, or scaffold a new app if this directory is empty
2. Start your app with Clerk installed.

Shall I proceed?
```

[...]

## Step 4: Verify

```bash
npx -y clerk@latest doctor
```

Then start the app, confirm the auth controls render, and fix anything the CLI reports.

[...]

## Critical rules

- Use Node.js 20.9.0 or later.
- Next.js 15+: `auth()` is async. Always `await auth()`
- `ClerkProvider` goes inside `<body>`, not wrapping `<html>`
- Never expose `CLERK_SECRET_KEY` in client code
- Use the current framework package, such as `@clerk/nextjs`, `@clerk/react`, `@clerk/expo`, `@clerk/react-router`, or `@clerk/tanstack-react-start`; never legacy Core 2 names such as `@clerk/clerk-react` or `@clerk/clerk-expo`.
- Do not read or print existing environment variable files; ask the user for any missing non-sensitive configuration
````

### getting-started/quickstart/overview.md (where the paste prompt lives) — HTTP 200

<https://clerk.com/docs/getting-started/quickstart/overview.md>

3317 bytes. Three equivalent on-ramps printed as three bullets: a paste prompt, a CLI command, a skills command.

```text
- Agent prompt: `Add Clerk auth to my app: clerk.com/SKILL.md`
- CLI: `npx clerk@latest init`
- Agent Skills: `npx skills add clerk/skills`
```

### guides/ai/overview.md — HTTP 200

<https://clerk.com/docs/guides/ai/overview.md>

3621 bytes. Skills, CLI ('It auto-detects agent vs. human mode'), and a one-command MCP registration for every detected client.

````text
## Use Clerk's MCP server

Clerk provides its own MCP server that helps AI coding agents provide accurate SDK snippets and implementation patterns. This is useful when building authentication features with Clerk in AI tools. If you use the Clerk CLI, one command registers it in the AI clients detected on your machine:

```bash
clerk mcp install
```

Learn more, including manual per-client setup, in the [Clerk MCP server guide](https://clerk.com/docs/guides/ai/mcp/clerk-mcp-server.md).
````

### guides/ai/mcp/clerk-mcp-server.md (per-client MCP page) — HTTP 200

<https://clerk.com/docs/guides/ai/mcp/clerk-mcp-server.md>

8883 bytes. A reachability check first, then per-client blocks; each block ends with how to verify in THAT client.

````text
claude mcp add clerk --transport http https://mcp.clerk.com/mcp
   ```
2. Run `/mcp` once you've opened a Claude Code session to verify the connection.
````

### .well-known/agent-skills/index.json (newer discovery path) — HTTP 200

<https://clerk.com/.well-known/agent-skills/index.json>

9908 bytes, 20 skills, `$schema` https://schemas.agentskills.io/discovery/0.2.0/schema.json, a `revision` and per-skill `type`, `description`, `url`. The older /.well-known/skills/index.json is ALSO served (10875 bytes).

### agents.md — HTTP 404

<https://clerk.com/agents.md>

A real 404 that still helps: a 311-byte Markdown body, '# Page Not Found … `/agents.md` does not exist on clerk.com. Try one of these instead'.

```text
# Page Not Found

`/agents.md` does not exist on clerk.com.

Try one of these indexes to locate the correct URL:

- [Clerk site index](https://clerk.com/llms.txt): marketing pages, blog, changelog, and guides
- [Clerk docs index](https://clerk.com/docs/llms.txt): table of contents for every documentation page
```

## What they do better than XERJ

- A seven-word paste prompt that names a hosted file: 'Add Clerk auth to my app: clerk.com/SKILL.md'. XERJ's hero prompt is one 34-word sentence that points at llms.txt, a 40 KB file.
- Consent is scripted and short: 'Show the user this checklist and wait for a yes' followed by a two-item checklist and 'Shall I proceed?'.
- A stop rule for the dangerous case: 'If auth exists, stop and get approval for a migration plan' and 'Do not modify or remove existing auth without approval.' XERJ has no equivalent for 'a node is already running with someone's data'.
- No account is needed for a first success: 'supported frameworks default to accountless setup'. (XERJ needs no account at all — but its llms.txt does not say so in the first screen.)
- A verify command with a name that says what it does (`npx -y clerk@latest doctor`), and per-client MCP verify steps ('Run `/mcp` once you've opened a Claude Code session to verify the connection.').
- Its 404 for a missing Markdown path is itself Markdown and suggests where to go. xerj.org's 404 for `/docs/install.md` is an 11 KB HTML page.
- Both skills-discovery paths are served, old and new.

## What XERJ does better

- The install command is printed in llms.txt; Clerk's llms.txt describes installing but prints no command.
- XERJ publishes an integrity-checked install path; Clerk's path is `npx -y clerk@latest`, unpinned, with no integrity statement.
- XERJ's llms-full.txt is a curated reference an agent can load; Clerk's documentation dump is 27.9 MB.
- XERJ states limits and failure modes in the entry file.

## Adoptable ideas

- Publish https://xerj.org/SKILL.md and change the hero prompt to the Clerk shape: '<goal>: xerj.org/SKILL.md'. Keep the current prompt's goal wording.
- Copy the consent block's form: a fenced, numbered 'Here's what I'll do' list ending 'Shall I proceed?'.
- Add a stop rule for the case XERJ's install can damage: a node already listening on the port, or a data directory that already holds indices — 'stop and ask; do not start a second node or reuse the directory without approval'.
- Give XERJ a `doctor`-shaped verify (proposed — not implemented): one command that checks binary on PATH, node reachable, key accepted, at least one `ax-*` index with documents, MCP entry present.
- Make xerj.org's 404 for `*.md` and `*.txt` a short Markdown body that links llms.txt.
- Say 'no account, no API key, no network after install' in the blockquote summary, as Clerk says 'No global install, API keys, or sign-in is required.'
