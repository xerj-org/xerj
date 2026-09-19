# Comparison table — 56 projects, 52 of which publish an llms.txt (46 measured 2026-09-18, 10 added 2026-09-19)

Generated mechanically from `data/dissections.json` and `data/dissections-2026-09-19.json` by `scripts/build_tables.py` (`--check` fails if this file is stale). Sorted by llms.txt size. "Install" = whether installation commands appear in llms.txt itself.

**Distribution of llms.txt size (bytes):** min 1,674 · p25 16,049 · median 24,814 · p75 45,599 · max 175,811. **XERJ: 40,064 bytes, 234 lines — rank 39 of 53.**

**Counts:** install commands absent from llms.txt in 30/52 · agent-addressed text in 28 · one-click links in 20 · MCP snippet in 24 · a feedback/contribution ask in 24 · llms-full.txt published by 39.

The columns record what a dissecting pass wrote down for each project, so "MCP snippet: yes" can mean a snippet quoted from a linked page. What is literally inside each llms.txt — code fences, install commands, `claude mcp add` — is measured by script over a larger set in [`proposals/llms-txt-measurements.md`](proposals/llms-txt-measurements.md).

4 of the 10 projects added on 2026-09-19 publish **no llms.txt at all**. They are XERJ's nearest functional peers (local code-search and MCP-server projects); their README is the agent-install surface. They are listed last and are excluded from the distribution and the counts above.

| Project | llms.txt bytes | lines | llms-full bytes | Install in llms.txt | Agent text | One-click | MCP snippet | Feedback ask |
|---|---:|---:|---:|---|---|---:|---|---|
| [Ollama](per-project/ollama.md) | 1,674 | 29 | 236,111 | absent | yes | 0 | — | — |
| [Vercel AI SDK](per-project/vercel-ai-sdk.md) | 2,217 | 46 | 6,062,390 | Lines 22-30, section '## Local Coding Agents | yes | 0 | — | — |
| [Supabase](per-project/supabase.md) | 2,709 | 43 | 7,022,257 | absent | — | 0 | — | — |
| [DuckDB](per-project/duckdb.md) | 3,371 | 37 | 0 | absent | — | 0 | — | — |
| [turbopuffer](per-project/turbopuffer.md) | 4,012 | 59 | 792,810 | absent | — | 0 | — | — |
| [Composio](per-project/composio.md) | 4,445 | 50 | 1,160,265 | Line 11, inside '## Before you implement' (s | yes | 1 | yes | yes |
| [Vercel](per-project/vercel.md) | 4,726 | 64 | 9,509,006 | Line 25, inline in a link description — not  | yes | 0 | — | — |
| [Deno](per-project/deno.md) | 5,055 | 60 | 2,497,222 | absent | — | 0 | — | — |
| [Algolia](per-project/algolia.md) | 7,866 | 188 | 0 | Lines 7-10, the FIRST section after the desc | yes | 6 | yes | yes |
| [Typesense](per-project/typesense.md) | 8,591 | 81 | 1,947,804 | No install commands in the marketing llms.tx | yes | 5 | yes | yes |
| [Firecrawl](per-project/firecrawl.md) | 8,815 | 128 | 1,914,650 | absent | — | 0 | — | — |
| [LlamaIndex](per-project/llamaindex.md) | 10,908 | 89 | 0 | absent | — | 0 | — | — |
| [Langfuse](per-project/langfuse.md) | 15,971 | 137 | 0 | absent | yes | 0 | yes | — |
| [Cloudflare Developer Docs](per-project/cloudflare-developer-docs.md) | 16,049 | 138 | 59,403,277 | absent | yes | 1 | yes | yes |
| [Elastic](per-project/elastic.md) | 17,198 | 171 | 0 | absent | — | 0 | — | yes |
| [Meilisearch](per-project/meilisearch.md) | 17,702 | 256 | 8,670 | absent | — | 0 | — | yes |
| [Tavily](per-project/tavily.md) | 18,171 | 212 | 856,222 | Line 16, second H2 ('## Quick Start: CLI (fa | yes | 2 | yes | — |
| [ParadeDB](per-project/paradedb.md) | 18,336 | 131 | 680,006 | line 5 of 131 (3rd bullet) — a link only, no | yes | 3 | yes | yes |
| [Cline](per-project/cline.md) | 18,871 | 116 | 675,700 | line 6 of 116 (2nd bullet): '- [Installing C | yes | 2 | yes | yes |
| [Sentry](per-project/sentry.md) | 19,243 | 189 | 0 | Line 11, first bullet of '## Instructions fo | yes | 0 | yes | — |
| [Clerk](per-project/clerk.md) | 19,607 | 130 | 768 | absent | yes | 0 | — | — |
| [Cursor](per-project/cursor.md) | 21,678 | 455 | 142,881 | absent | — | 1 | yes | yes |
| [Chroma](per-project/chroma.md) | 22,134 | 183 | 814,626 | absent | — | 0 | — | — |
| [LangChain / LangGraph / LangSmith](per-project/langchain-langgraph-langsmith.md) | 22,273 | 197 | 6,915,750 | absent | — | 0 | — | — |
| [Windsurf](per-project/windsurf.md) | 24,156 | 132 | 636,246 | absent | — | 0 | — | — |
| [Exa](per-project/exa.md) | 24,309 | 168 | 766,571 | No install command in the file itself. The n | yes | 10 | yes | yes |
| [Modal](per-project/modal.md) | 25,320 | 372 | 2,321,860 | absent | — | 0 | — | — |
| [Stagehand](per-project/stagehand.md) | 26,114 | 189 | 898,822 | absent | — | 0 | — | yes |
| [SurrealDB](per-project/surrealdb.md) | 27,938 | 294 | 51,217 | First runnable command is line 65 (end of '# | yes | 1 | yes | yes |
| [Zed](per-project/zed.md) | 29,076 | 238 | 0 | absent | — | 0 | — | — |
| [Weaviate](per-project/weaviate.md) | 29,583 | 732 | 0 | NOT a top-level section. SDK install is one  | yes | 0 | yes | yes |
| [Pydantic AI](per-project/pydantic-ai.md) | 30,549 | 435 | 5,782,671 | Line 26, second bullet of the first section  | yes | 3 | yes | yes |
| [Turso](per-project/turso.md) | 33,136 | 292 | 1,083,272 | No install instructions in the file itself.  | — | 6 | yes | yes |
| [Bun](per-project/bun.md) | 33,721 | 328 | 2,136,096 | line 6, the 2nd link in '## Docs' (right aft | — | 0 | — | yes |
| [Browserbase](per-project/browserbase.md) | 34,956 | 226 | 1,033,754 | line 7 of 226, third link in the flat index: | yes | 3 | yes | — |
| [CrewAI](per-project/crewai.md) | 38,837 | 233 | 1,964,675 | absent | — | 0 | — | — |
| [Neon](per-project/neon.md) | 39,737 | 410 | 6,564,134 | Two places. (a) Line 9, first bullet of '##  | yes | 5 | yes | yes |
| [Pinecone](per-project/pinecone.md) | 39,873 | 278 | 3,814,905 | absent | yes | 0 | — | — |
| [LanceDB](per-project/lancedb.md) | 44,802 | 240 | 1,668,626 | absent | — | 0 | — | — |
| [Convex](per-project/convex.md) | 45,599 | 498 | 2,554,656 | absent | yes | 3 | yes | yes |
| [Claude Code docs](per-project/claude-code-docs.md) | 46,963 | 361 | 9,512,179 | absent | yes | 0 | — | — |
| [Model Context Protocol](per-project/model-context-protocol.md) | 48,383 | 354 | 2,487,841 | absent | — | 0 | — | yes |
| [Mastra](per-project/mastra.md) | 50,105 | 751 | 0 | absent | yes | 5 | yes | yes |
| [Expo](per-project/expo.md) | 55,257 | 807 | 0 | Line 22, a bare command under '## Performanc | yes | 0 | — | yes |
| [FastMCP](per-project/fastmcp.md) | 64,883 | 574 | 2,885,378 | line 6 of 574, the 2nd link in the file: "-  | — | 5 | yes | yes |
| [Anthropic](per-project/anthropic.md) | 68,036 | 699 | 35,109,013 | absent | yes | 1 | yes | yes |
| [Vespa](per-project/vespa.md) | 68,937 | 998 | 4,522,666 | absent | — | 0 | — | — |
| [Milvus](per-project/milvus.md) | 81,998 | 796 | 0 | Line 58 '## Get Started' (4th H2, after URL  | yes | 4 | yes | yes |
| [Stripe](per-project/stripe.md) | 92,159 | 707 | 0 | absent | yes | 0 | yes | — |
| [ClickHouse](per-project/clickhouse.md) | 103,444 | 500 | 34,655,192 | Lines 5-12, the FIRST H2 (## Get started for | — | 0 | — | — |
| [Upstash](per-project/upstash.md) | 163,576 | 1,472 | 3,456,603 | Lines 3-7 (the <Tip> before any heading) poi | yes | 10 | yes | — |
| [Qdrant](per-project/qdrant.md) | 175,811 | 696 | 0 | absent | — | 0 | — | — |
| **XERJ (for reference)** | **40,064** | **234** | **83,712** | line 11, inside 'Start here' | yes | 0 | — | yes |
| [GitHub MCP server](per-project/github-mcp-server.md) | none published | — | — | README only (4 install lines recorded) | — | 1 | README | — |
| [Serena](per-project/serena.md) | none published | — | — | README only (4 install lines recorded) | — | 0 | README | — |
| [claude-context](per-project/claude-context.md) | none published | — | — | README only (3 install lines recorded) | — | 0 | README | — |
| [probe](per-project/probe.md) | none published | — | — | README only (7 install lines recorded) | — | 0 | README | — |
