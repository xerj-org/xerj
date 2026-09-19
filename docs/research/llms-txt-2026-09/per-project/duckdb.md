# DuckDB

Category: analytical database

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://duckdb.org/llms.txt> — HTTP 200
- Size: 3371 bytes, 37 lines
- Install position: absent — no install command anywhere in the file; the nearest thing is the '## Clients' link list (CLI / Go / Node.js / Python / R / Rust overview pages), each of which carries its own install instructions behind the link
- Tone: neutral / advisory. Second-person-less; six declarative 'Things to remember' bullets that state facts and one behavioural recommendation ('When loading large chunks of data, use ... to speed up the loading process'). Makes no demands of the agent, asks nothing back, has no 'required' language.
- Section order: # DuckDB (blockquote summary) → (untitled bullet list) 'Things to remember when using DuckDB:' → ## Clients → ## Extensions → ## Examples → ## Optional

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
Things to remember when using DuckDB:

- DuckDB uses a PostgreSQL-compatible SQL language. All DuckDB clients use the same SQL language.
- DuckDB can load data from popular formats, including CSV, JSON and Parquet. DuckDB can also directly run queries on CSV, JSON and Parquet files.
- DuckDB supports persistent storage but can also run in in-memory mode. The in-memory mode is useful when analyzing small data sets or doing data transformation steps.
- When loading large chunks of data, use the [`SET preserve_insertion_order = false;` configuration setting](https://duckdb.org/docs/lts/sql/dialect/order_preservation) to speed up the loading process and reduce the memory load. When using DuckDB in combination with dataframe libraries such as pandas, turn this mode back on after loading by issuing `SET preserve_insertion_order = true;`.
- DuckDB supports data lake formats such as [Delta Lake](https://duckdb.org/docs/lts/core_extensions/delta), [Iceberg](https://duckdb.org/docs/lts/core_extensions/iceberg/overview) and [DuckLake](https://duckdb.org/docs/lts/core_extensions/ducklake).
- If a workload requires concurrent write access by multiple DuckDB clients, consider using [DuckLake](https://duckdb.org/docs/lts/core_extensions/ducklake).
```

## llms-full.txt

- URL: <https://duckdb.org/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: does not exist. Also 404: https://duckdb.org/docs/llms.txt, https://raw.githubusercontent.com/duckdb/duckdb/main/llms.txt, https://duckdb.org/llms-install.md. DuckDB ships only the single 37-line hand-written llms.txt on the docs site and links out to the LTS docs (all links use the /docs/lts/ prefix, i.e. a pinned stable line rather than /docs/stable/).
- Install position: absent (file does not exist; the 404 body is duckdb.org's HTML 404 page, 268,753 bytes — not counted)

## Other agent-facing files

### AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/duckdb/duckdb/main/AGENTS.md>

Contributor/coding-agent guide for the DuckDB C++ repo (17,911 bytes, 493 lines): build commands, test runner + sqllogictest format, formatting, architecture pipeline, Vector API, coding guidelines, navigation tips, generated files. Aimed at agents modifying DuckDB itself, not at agents installing/using DuckDB. No install command for end users, no MCP, no feedback/field-report ask; only mention of contribution is the pointer '`CONTRIBUTING.md` - Contribution guidelines'. Section order: Overview / Build Commands / Testing / Code Formatting / Extensive Testing / Architecture / Extensions / Key Development Patterns / Vector API / Coding Guidelines / Navigation Tips / Documentation / Important Files.

```text
This file provides guidance to coding agents when working with code in this repository.

[...]

It is recommended to use `make reldebug` and `build/reldebug/test/unittest` unless a good reason exists to use the debug build - the debug build is much slower than the reldebug build.

[...]

Avoid adding comments specific to how a change was made to the code that relates to a specific issue. For example, a comment like "add +1 to fix an off-by-one error" is not relevant to understanding the code. Such comments related to specific issues that were addressed belong in a PR description or commit message, not in the code itself.
```

### CLAUDE.md — HTTP 200

<https://raw.githubusercontent.com/duckdb/duckdb/main/CLAUDE.md>

10-byte include pointer so Claude Code reads the same AGENTS.md; one source of truth for all agent harnesses.

```text
@AGENTS.md
```

### llms-install.md — HTTP 404

<https://duckdb.org/llms-install.md>

does not exist

### SKILL.md — HTTP 404

<https://raw.githubusercontent.com/duckdb/duckdb/main/SKILL.md>

does not exist

### MCP install page — HTTP 404

<https://duckdb.org/docs/stable/guides/mcp>

does not exist; DuckDB publishes no MCP server or one-click agent install on its docs site

### Installation page (human docs, for reference) — HTTP 200

<https://duckdb.org/docs/installation>

The human install matrix (JS-rendered picker). The llms.txt does NOT link to it; install is reached only via the per-client overview links (e.g. https://duckdb.org/docs/lts/clients/python/overview). Note: https://duckdb.org/docs/lts/clients/overview answers 200 but is a meta-refresh redirect stub to /docs/current/clients/overview.html — an agent fetching llms.txt links without following JS/meta redirects gets an 11-line 'Redirecting…' page.

```text
<title>Redirecting&hellip;</title>
<link rel="canonical" href="https://duckdb.org/docs/current/clients/overview.html">
```

## What they do better than XERJ

- Brevity and a single job: 37 lines / 3.4 KB, readable in one hop with zero scrolling; the whole file is 'facts an agent gets wrong' plus six links. XERJ's llms.txt is 234 lines / 40 KB and mixes index, install, caveats, protocol and obligations in one file.
- The 'Things to remember when using DuckDB:' bullet form is a clean, reusable pattern: each bullet is one fact + one consequence + one link, with a concrete behavioural rule ('use `SET preserve_insertion_order = false;` ... turn this mode back on after loading'). XERJ's equivalent (Caveats) is buried at line 39 behind a 40-KB header and reads as prose paragraphs.
- No demands, no obligations, no 'required' language: the tone is purely advisory, so nothing in it can contradict itself the way XERJ's 'required, not optional ... the template is under Optional' does.
- CLAUDE.md = '@AGENTS.md' — a single source of truth for every agent harness with no drift; XERJ maintains separate llms.txt / llms-full.txt / AGENTS.md / skill text.
- All links pin to /docs/lts/ (a stable line), so what an agent reads today will not change under it between releases.
- AGENTS.md explicitly tells the agent which build to use and why ('the debug build is much slower than the reldebug build') and how to bound runaway queries (`SET max_execution_time=5000;`) — operational 'do not waste time' guidance rather than feature marketing.

## What XERJ does better

- Install actually exists: XERJ's llms.txt has copy-pasteable install commands at line 11-12 (`curl -fsSL https://xerj.org/get | sh`, `irm https://xerj.org/get.ps1 | iex`), the PATH consequence, and a numbered first-run sequence. DuckDB's llms.txt has no install command at all — an agent must follow a client link (one of which is a meta-refresh stub) to find `pip install duckdb`.
- A shell-less path is at least named ('Agent quickstart: the HTTP-only path for an agent with no shell'); DuckDB has no agent-specific install or MCP path and does not address non-shell personas anywhere.
- Honesty caveats are inline and specific (lexical-vs-neural embedding trap, data-dir-inside-indexed-tree trap, exit-code semantics, 'never invent a number' for progress). DuckDB's llms.txt has no failure-mode or honesty guidance.
- A machine-executable feedback loop exists (`xerj feedback --open-pr ...`, `--dry-run`, CLA-exempt one-file PR, 'a negative report is a good report'). DuckDB asks for nothing back and provides no channel.
- llms-full.txt exists (1,235 lines) as the expanded reference; DuckDB has none (404).
- Tool-shape guidance for MCP is present ('The MCP tool `xerj_search` accepts a plain string as `query`'); DuckDB has no MCP surface.

## Adoptable ideas

- Adopt DuckDB's opening form verbatim as the shape of XERJ's Caveats and move it to the top, before install: a heading line 'Things to remember when using XERJ:' followed by 5-7 one-line bullets each of the form <fact> + <what to do> + <one link>, modelled on 'When loading large chunks of data, use the [`SET preserve_insertion_order = false;` configuration setting](https://duckdb.org/docs/lts/sql/dialect/order_preservation) to speed up the loading process and reduce the memory load.' — e.g. 'XERJ's default embedder is lexical, not neural; `semantic` and memory recall are sub-word overlap + BM25 unless the node was started with `--embed-mode neural` (link).' This directly targets the 9-of-12 'misread lexical as neural' failure and the fact that the first thing an agent reads is currently a 40-KB header.
- Shrink llms.txt to DuckDB scale (target <= 60 lines / <= 6 KB) and push everything else into llms-full.txt. DuckDB's whole file is 3,371 bytes; XERJ's line 1 already says 'the detail lives behind the links, not here' but the file is 40,064 bytes. Keep in llms.txt only: the summary blockquote, 'Things to remember', Start here (install + 4 numbered steps), and link lists.
- Resolve the 'required / Optional' contradiction by renaming the section. DuckDB uses '## Optional' for genuinely optional third-party resources ('Awesome DuckDB', 'tldr pages entry for `duckdb`'). XERJ's line 7 says 'this is required, not optional ... and the template is under Optional' and then line 223 is '## Optional' — rename that heading to e.g. '## Your side of the deal (required)' or move the field-report bullet up into 'Start here' step 4 (where it already lives) and leave '## Optional' for third-party lists only. (Inference: the llms.txt spec reserves 'Optional' for skippable content, so agents that follow the spec may drop that section from context entirely — which would explain the low PR rate.)
- Copy the CLAUDE.md-as-pointer trick: DuckDB's CLAUDE.md is exactly '@AGENTS.md' (10 bytes). XERJ's repo AGENTS.md and the skill written by `xerj init` could be unified the same way so the harness-specific files never drift from the canonical text.
- Pin agent-facing links to a stable docs line the way DuckDB pins every llms.txt link to https://duckdb.org/docs/lts/... — XERJ links mix xerj.org/docs/... and /answers/... with no version pin; add /docs/lts/ or /docs/<rc-line>/ aliases so a cached llms.txt keeps resolving.
- Add one 'bound your runtime' bullet in the DuckDB AGENTS.md style ('Use the `max_execution_time` setting (milliseconds, `0` = no limit) to abort a query that runs too long') — XERJ's analogue is `--max-minutes` / `--approve` on autoindex, currently only reachable via the 'Estimate a run' link; surface it as a top-level 'Things to remember' bullet since agents run on a person's laptop.
- Negative finding worth recording for the coordinator: a 41k-star project ships NO install command, NO MCP, NO feedback ask, and NO llms-full.txt in its llms.txt, and its docs links partly resolve to a JS/meta-refresh 'Redirecting…' stub (https://duckdb.org/docs/lts/clients/overview, 557 bytes). XERJ's install/feedback surface is already ahead of this reference; the lesson from DuckDB is form (short, factual, top-loaded, non-demanding), not content.
