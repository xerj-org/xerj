# How top projects use `llms.txt`, and what XERJ got wrong

**Read this before changing `landing/llms.txt`, `landing/llms-full.txt`, `AGENTS.md`, any
install text, or anything that asks an agent for feedback.** It exists so the same
mistakes are not made twice.

Research date: **2026-09-18**. Subject: 46 projects' `llms.txt`, `llms-full.txt` and other
agent-facing files, plus a probe of ~140 well-known GitHub projects, compared with
`https://xerj.org/llms.txt` as it stood that day (40,064 bytes, 234 lines).

---

## 1. Method, and how far to trust it

| Step | What ran | Status |
|---|---|---|
| Discover | Four independent agents, four angles: public llms.txt directories; direct probing of ~140 projects' docs domains and repo roots; installation-prompt patterns; feedback and contribution loops | complete — 176 candidates, 85 pattern notes |
| Dissect | One agent per project fetched the live files and extracted sizes, section order, install wording, agent-addressed text, MCP snippets, feedback asks — **verbatim** — and compared with XERJ's file | complete — 48 runs, 46 unique projects |
| Synthesize, critique, fact-check | One agent over everything, then a completeness critic, then a re-fetch fact-check of 25 claims | **not finished when this was committed** — see §10 |

What that means for a reader:

- Every per-project fact is **one agent pass over a live URL on 2026-09-18**. Quotes are
  verbatim as the agent returned them. They have **not** all been independently re-fetched.
- A maintainer spot-check re-fetched 14 claims with `curl`: **13 confirmed**, 1 inconclusive
  (the check's own search pattern was wrong, not the claim). Five file sizes re-measured
  matched the dissections to the byte (Ollama 1,674 B / 29 lines; DuckDB 3,371 / 37;
  turbopuffer 4,012 / 59; AI SDK 2,217 / 46; XERJ 40,064 / 234).
- Every defect listed against **XERJ's own file** in §3 was verified by hand against
  `landing/llms.txt` and the live site on the same day. Those are facts, not agent opinion.
- Agents marked their own inferences `INFER` / `INFERENCE`. Those markers are preserved.
- **The comparisons are anchored.** Every dissecting agent was briefed with XERJ's known
  gaps (the shell-less failure, the lexical-vs-neural misreading, the "required … under
  Optional" contradiction) and asked to compare. So a criticism appearing in all 46
  dissections — the shell-less gap does — is the brief echoing, **not 46 independent
  confirmations**. What is independent is what each agent *found in the other project's
  files*: the quotes, sizes, snippets and URLs. Weigh those, not the repetition.
- The web changes. Re-measure before quoting any number here in public.

Quotations from third-party documentation are short excerpts, each with its source URL,
reproduced for comparison and commentary.

---

## 2. Headline numbers (computed from `data/dissections.json`, not from anyone's summary)

| | |
|---|---|
| `llms.txt` size across 46 projects | min 1,674 B · p25 16,049 · **median 25,717** · p75 44,802 · max 175,811 |
| XERJ | 40,064 B, 234 lines — **rank 35 of 47**. Above the median, below the 75th percentile. **Not an outlier.** |
| Projects whose `llms.txt` contains **no install command at all** | **27 of 46** — most are auto-generated documentation indexes |
| Projects with text addressed to an agent in `llms.txt` | 22 of 46 |
| Projects with one-click install links | 20 of 46 |
| Projects with an MCP config snippet | 21 of 46 |
| Projects with any feedback or contribution ask | 23 of 46 — **none of them an obligation** |
| Projects publishing `llms-full.txt` | 37 of 46 — median 1.96 MB, max 59.4 MB. XERJ's is 83,712 B and is one of the few that fits in a context window |
| GitHub repos (of ~140 probed) serving `llms.txt`, `llms-install.md` or `SKILL.md` at the repo root | **0** |
| Projects, of everything probed, that ask an agent to file a report or open a PR after use | **0 — XERJ is the only one** |

Full table: [`comparison-table.md`](./comparison-table.md).

---

## 3. The mistakes in XERJ's `llms.txt` — each verified against the file

Line numbers refer to `landing/llms.txt` at commit `4d8dadbf`.

**M1 — A demand, contradicted by its own placement.** Line 7 says the field report is
"**required, not optional**". The template it points to sits under a heading named
`## Optional` (line 223), and the body says "the template is under Optional". In the
llms.txt convention `## Optional` means *skip this if context is short*; an agent that
prunes it drops the obligation entirely. Vercel uses `## Optional` only for machine
catalogues an agent may ignore, which is what the convention intends.

The same framing is in `AGENTS.md`, whose second heading reads "If you used XERJ, you owe
it a field report". Fix both together.

**M2 — The ask is a pull request.** A PR needs a shell, git, `gh`, a GitHub identity and
write permission. The agent is usually working inside *someone else's* repository whose
own rules forbid exactly that: by GitHub code search, 1,356 `AGENTS.md` and 1,792
`CLAUDE.md` files mention `gh issue create`, and the dominant instruction among those
sampled is a guardrail against it (Sonarr: "Before executing any
command that writes to GitHub … these guardrails apply. No exceptions."). Those rules and
the harness's own permission prompt outrank a sentence in our `llms.txt`. Result, measured:
7 field-report files in the repo (counted 2026-09-18) and, as one discovery agent
reported it, 4 external field-report PRs since 2026-08-18.

**M3 — A shell is assumed from line 1.** `curl -fsSL https://xerj.org/get | sh` is the
first instruction; the no-shell route is one link among twenty. The 22-run first-time-agent
study (2026-08-12) had already shown shell-less personas fail at *every* model tier. Every
strong file in this study leads with capability branching — AI SDK: "If you can fetch URLs,
search the docs first … If you are working inside a local coding project with filesystem
access, install the AI SDK skill first".

**M4 — MCP is undiscoverable.** Zero occurrences of `claude mcp add`, `mcpServers`, or any
client config JSON. `xerj init` is labelled "(optional, one command)" as step 3, and
`xerj mcp` appears only in the last bullet of `## Optional`. 21 of 46 projects print the
snippet inline in `llms.txt`, and a `claude mcp add` line was recorded somewhere in the
agent-facing files of 36 of 46 projects; many print it per client (Claude Code, Cursor, VS Code, Codex, Windsurf,
Claude Desktop) with the file path and the key-name differences called out (Cloudflare:
"Visual Studio Code uses `servers` as the root key. Configurations copied from Cursor or
Claude Desktop use `mcpServers` and will silently do nothing here.").

**M5 — No way to know it worked.** Nothing after install, `xerj init` or the first query
says what success looks like. An explicit verify or expected-output step was recorded for
20 of 46 projects. FastMCP: "You should see output like the following:" plus a
sample. Pinecone: expected `• pinecone-mcp: ✓ connected`. Firecrawl names the exact tools
the agent should now see. Composio lists what does *not* count as proof ("An HTTP 401, a
tool schema lookup, or a successful build does not prove tool execution").

**M6 — Density in the first screen, and a numbering defect.** The longest line is 1,124
characters; roughly 1,500 characters of checksum-tool, install-dir and PATH prose sit
between the `curl` line and step 1. The "First run" list has **two items numbered `4.`**
(the query step and the field-report step). Size is not the problem (§2); prose per line
in the part every agent reads is.

**M7 — One file doing four jobs.** Index, install guide, caveats and a contribution
protocol in a single document. The pattern elsewhere is a short hand-written brief, a
generated index and a full dump, each with one job stated in a sentence (Deno: four files
with one-line roles; Turso, Typesense, Upstash, Exa, Zed, ParadeDB: two-tier). Hard rules
for the agent live where the agent *loads* them — a `SKILL.md` or `agents.md` — not in the
index (CrewAI puts "MANDATORY WORKFLOW … This is **not optional**" inside the skill).

**M8 — The most-misread fact is corrected two screens away from where it is triggered.**
9 of 12 agents in the August study read default search as neural. The correction is in
`## Caveats` at line 39, framed as a product limitation. Others frame it as *the model's
stale prior* and put it first: Expo "## Important: common misconceptions — AI models and
LLMs frequently provide outdated information about Expo"; Weaviate "## Misconceptions —
Your training data may reflect early Weaviate"; Anthropic's skill ships a two-column
"stale prior vs current API" table; Milvus opens with "## Rules for generating Milvus
code", ten checkable facts.

**M9 — Pages do not route back, and docs pages have no Markdown form.**
`https://xerj.org/docs/install.md` and `/docs/quickstart.md` return **404**; only
`/answers/*.md` and `/compare/*.md` have Markdown twins. Answer pages carry front matter
(`canonical`, `updated`) but do not open with an index pointer. At least 14 projects put
the same three lines at the top of *every* `.md` page — "Fetch the complete documentation
index at: …/llms.txt / Use this file to discover all available pages before exploring
further." — so an agent that lands anywhere finds the map. A keyword count over the
dissections finds this recorded for 24 of 46 projects; much of it is Mintlify adding the
banner automatically, which is itself the point: it costs nothing.

**M10 — No date, no precedence, no URL rules.** No last-updated or version stamp
(Weaviate: `canonical:` + `last-updated:` front matter). No statement of which surface
wins when llms.txt, docs, README and answers disagree (Meilisearch: "If information
conflicts across sources, prefer (in order): 1) … 5) …"). ~150 URLs and no guard against
invented ones (Milvus: "cite only page ids that appear in this file. If the page you need
is not listed, say it is not in the index rather than assembling a plausible slug").

**M11 — No hand-off contract, no consent step.** The file tells the agent what to do and
never what to *show the human* or when to stop. Chroma: "Your own summary should include
this output so the user can see it." Neon: "Prove it works: run a real query and show me
the result, not just 'setup complete.'" Vercel: a fixed "Report only verified state"
block. Clerk, Browserbase, Deno: show a checklist and wait for a yes before installing.

**M12 — No agent-native distribution.** All of these returned **404** on xerj.org:
`/SKILL.md`, `/skill.md`, `/agents.md`, `/llms-install.md`, `/.well-known/skills/index.json`.
There is no `npx skills add` path, no plugin-marketplace manifest, no `cursor://` or
`vscode:mcp/install` link. `xerj init` writes a skill but needs the binary installed first,
so it cannot be the on-ramp. `npx skills add <owner>/<repo>` appeared in 36 probed files;
a hosted `SKILL.md` plus a seven-word paste prompt ("Read https://browserbase.com/SKILL.md
to set up Browserbase") is the single most common installation prompt found.

---

## 4. Why the field reports do not arrive

Stated once, plainly, because it is the question this research was commissioned to answer.

1. **No one else asks.** Across every `llms.txt`, `llms-full.txt`, `agents.md` and
   `SKILL.md` probed, no project asks an agent to report back, and none frames feedback
   as a debt. Tone in the strongest files is procedural and makes no demands.
2. **The channel is wrong for the actor.** Projects that *do* receive agent feedback at
   volume use a channel with no git, no CLA and no GitHub identity:
   - a tool inside the product — Claude Code `SendFeedback` (triggered by a *failure*, not
     by session end; consent is a keystroke), Codex `/feedback`, Amp `amp report-bug`,
     PostHog `hogli devex:feedback` ("Local agents must use it too … the command is a
     no-op [in sandboxes]");
   - a plain HTTP POST — Expo: `curl -X POST https://api.expo.dev/v2/feedback/docs-send …`
     with the model and harness named in the payload; Neon prints a no-auth POST in a
     "Note for AI assistants" footer on every page;
   - an MCP tool — `submit_feedback` (LanceDB, Ollama, Tavily); Firecrawl's
     `firecrawl_search_feedback` **refunds a credit** per report;
   - nothing the agent has to do at all — Pydantic AI reads `?intent=&stack=&harness=`
     query parameters on docs fetches; Upstash asks only for a `User-Agent` header;
     ClickHouse's CLI telemetry records which agent invoked it.
3. **The agent's own rules forbid it.** See M2. "Required" in our file loses to the
   operator's `AGENTS.md`, to the permission prompt, and to the dominant project stance
   on the receiving side too (Zed, LLVM, MicroPython: no autonomous-agent PRs).
4. **A one-keystroke rating captures more sessions than a 25-line report ever will**
   (Claude Code's "How is Claude doing this session?").

What XERJ should keep: `xerj feedback` auto-filling version, OS and what was indexed is
the same lineage as `next info` and Homebrew's `gist-logs` — a good design. What should
change is the *transport* and the *framing*: an HTTP endpoint and an MCP tool that a
shell-less agent can call, triggered when something fails or at the end of a first task,
asked for once, in one sentence, with a reason that serves the reader (Vexa: "We can see
that these pages get fetched; we cannot see the question. If you tell us, the next reader
gets a page that answers it"). Keep the PR route for humans who want it.

All 32 notes on this, verbatim with URLs:
[`discovery-feedback-and-contribution-loops.md`](./discovery-feedback-and-contribution-loops.md).
Per-project asks: [`feedback-asks-verbatim.md`](./feedback-asks-verbatim.md).

---

## 5. What XERJ already does better — do not regress these

The same strengths were named independently across nearly all 48 dissections.

- **Install is in the file.** 27 of 46 projects have no install command in `llms.txt`.
  Ours is on line 11.
- **A verified-install path without `curl | sh`**, with fail-closed SHA-256 checking.
  In the August study this alone flipped a policy-constrained agent from blocked to
  success. Most projects print a bare `curl … | sh` with no integrity statement.
- **Honest caveats with numbers**, where others print marketing ("fastest", "most
  popular", unsourced download counts).
- **The hero prompt is found on hop 1** from raw HTML (6/6 funnel runs). Never regress it.
- **`llms-full.txt` is curated and fits in a context window** (84 KB). The median elsewhere
  is 1.96 MB of concatenated docs; several exceed 5 MB.
- **Tool schemas in three shapes**, generated from a real `tools/list`.
- **Question-shaped answer pages**, which is how agents actually arrive.
- **The PATH warning** anticipates a real first-run failure. Keep the fact; move it to the
  step where it bites and make it a command, not a paragraph (ClickHouse's skill:
  "run `which clickhousectl`, install if missing").
- **A feedback CLI exists at all.** Most projects have nothing.

---

## 6. Rules for any future change

A change to the agent-facing files should be able to answer yes to each of these.

1. **Three entry surfaces, each complete:** shell; MCP-only; HTTP-only. An agent with no
   terminal reaches a working query without following more than one link.
2. **No obligation language.** Nothing is "required", "owed" or "your side of the deal".
   `## Optional` contains only things that are safe to skip.
3. **Every install or setup step ends with a verify line and its expected output.**
4. **The lexical-by-default correction is adjacent to the first mention of semantic
   search**, framed as a prior the model is likely to hold.
5. **MCP registration is printed, per client,** with the file path and root key
   (`mcpServers` vs `servers`), plus `claude mcp add …` as a one-liner.
6. **Feedback has a channel a sandboxed, shell-less agent can use** — HTTP or an MCP
   tool — and is asked for once, with the reason stated.
7. **First-screen prose is short.** No line over ~300 characters above the first command.
   Numbered lists are numbered correctly.
8. **The file states what it is, when it was last updated, and which source wins on
   conflict.** Docs links resolve as Markdown.
9. **Tell the agent what to show the human** when it finishes, and when to ask first.
10. **Only one open PR edits `landing/llms.txt` at a time.** Several feature branches
    appending lines concurrently is how the file became four documents in one.
11. **Re-run the first-time-agent harness** (`demo/playbooks/AGENT_FTX_HARNESS.md`) on the
    shell-less personas before and after. The August study is the baseline.
12. **Measure before asserting.** See §8.

---

## 7. Installation-prompt patterns found

Sixteen patterns, each with verbatim examples and URLs, in
[`discovery-install-prompt-patterns.md`](./discovery-install-prompt-patterns.md); every
agent-addressed passage per project in
[`install-prompt-catalogue.md`](./install-prompt-catalogue.md). In order of how often
they appeared and how little they cost to adopt:

| # | Pattern | Example |
|---|---|---|
| 1 | Hosted `SKILL.md` + a one-line paste prompt | "Read https://browserbase.com/SKILL.md to set up Browserbase" · Firecrawl "Read and follow https://www.firecrawl.dev/agent-onboarding/SKILL.md" · Tavily "Read and execute tavily.com/agent-setup/SKILL.md" |
| 2 | `claude mcp add` one-liner, then per-client JSON | `claude mcp add --transport http <name> <url>` · `claude mcp add <name> -- <command>` |
| 3 | Capability branching at the top of `llms.txt` | AI SDK, Composio (with a failure ladder: "If skills are unsupported or installation fails, use the documentation linked below and report that fallback") |
| 4 | Verify-before-claiming-done written into the prompt | Neon, Tavily ("say 'installed; restart/rescan required' rather than claiming they are already active"), E2B, Exa (HTTP status as the success test) |
| 5 | Consent checklist before install | Clerk, Browserbase, Deno |
| 6 | `npx skills add <owner>/<repo>` (Agent Skills spec; 36 probed files name it) | Vercel, Upstash, Cloudflare, Supabase, Weaviate, ParadeDB … |
| 7 | Plugin-marketplace two-liner | `/plugin marketplace add <org>/skills` + `/plugin install <x>@<marketplace>` |
| 8 | One-click deep links | `cursor://anysphere.cursor-deeplink/mcp/install?…` · `vscode:mcp/install?…` · deep links that pre-fill the *prompt* (`cursor://…/prompt?text=…`, `codex://new?prompt=…`) |
| 9 | Fetch-and-execute `prompt.md` with an execution contract | Cloudflare: "Complete all of the following steps yourself … Do not ask the user to run any of these commands." · Vercel: "Pause only for user authentication, approval, or UI actions you cannot perform." |
| 10 | The CLI installs itself into detected agents | `stripe agent setup`, `neon init`, `npx ctx7 setup`, `vespa skills install --harness …`, `bun init` writing `CLAUDE.md` as a side effect of the normal first command |
| 11 | Self-registration line for the project's `AGENTS.md` | Typesense gives the exact sentence to append |
| 12 | "Example prompts" a human types after install | Cloudflare, Vespa, Mastra, Stagehand, Convex ("build me a todo app with Convex") |
| 13 | `.well-known/skills/index.json` | 29 probed docs domains |
| 14 | `llms-install.md` (Cline marketplace convention) | 690 files on GitHub by code search, almost all MCP servers; **0 of the top projects probed publish one** — adopt only if listing in that marketplace |
| 15 | Secret-handling rules inside the prompt | Exa, Neon, Weaviate, Firecrawl ("Never put an API key in an agent chat") |
| 16 | README badge to `llms.txt` | 123 READMEs; XERJ already has it; low yield |

Context: the llms.txt convention was revised on 2026-08-10 — path-scoped files, `.md`
twins, and discovery through `rel="alternate" type="text/markdown"`. `llms-full.txt` is
not in the spec; it is a de-facto convention and means different things on different sites.

---

## 8. Corrections to things said during this research

Recorded because the process mistakes matter as much as the file's.

- **"At 40 KB ours is likely an outlier."** Said before measuring; wrong. Rank 35 of 47.
  The real outliers are first-screen density and the number of jobs the file does.
- **"`llms-install.md` is a convention top projects follow."** Two angles disagreed. Both
  are right in scope: widely used by MCP servers listed in Cline's marketplace, absent
  from the infrastructure projects studied. Not a priority.
- **Probing trap:** GitBook, Fern and Mintlify sites answer **HTTP 200** with a
  "Page Not Found" Markdown body for `/AGENTS.md`, `/llms-install.md`, `/SKILL.md`.
  A status code is not evidence a file exists. Read the body.
- **Two earlier documentation defects of the same family** (from the August study): the
  MCP server was described but shipped to no one, and `mcp-tools.json` was stale at six
  tools. Agent-facing docs must be generated from, or checked against, what is released.
- **Workflow design:** a single synthesis agent was handed all 52 results at once
  (~317k tokens) and ran for hours; an account switch paused it twice. Synthesis over
  this much material should be map-reduce, and results should be written to disk as they
  arrive rather than held for a final step.

---

## 9. Priority order for the rewrite

1. Remove obligation language; move the feedback ask to the end of the first task; add an
   HTTP endpoint and an MCP `feedback` tool. *(M1, M2, §4)*
2. Open `llms.txt` with three capability branches and a complete MCP-only path; print
   per-client MCP registration. *(M3, M4)*
3. Add verify lines with expected output after install, `xerj init` and the first query.
   *(M5)*
4. Put a short "what you probably assume / what is true" block at the top, with
   lexical-by-default first. *(M8)*
5. Publish `https://xerj.org/SKILL.md` and the seven-word paste prompt; add
   `.well-known/skills/index.json`. *(M12)*
6. Give every `.md` page the three-line index banner; publish `.md` twins for `/docs/*`.
   *(M9)*
7. Split the brief from the index; add last-updated, precedence and URL rules; fix the
   duplicate `4.`. *(M6, M7, M10)*
8. Add the hand-off contract and a consent step to the hero prompt. *(M11)*

---

## 10. Not finished when this was committed

- The workflow's synthesis agent — producing a full proposed `llms.txt`, an
  `llms-install.md` and a library of paste-ready prompts — was still running. Its output,
  and the critic and fact-check passes that follow it, are to be added here as
  `proposals/` when they complete. Nothing in this directory depends on them.
- The rewrite itself is deliberately **not** in this change: several open feature PRs
  each append a line to `landing/llms.txt`, and rule 10 above applies.

---

## 11. Files

| File | What it is |
|---|---|
| [`comparison-table.md`](./comparison-table.md) | All 46 projects: sizes, install position, agent text, one-click links, MCP snippet, feedback ask |
| [`install-prompt-catalogue.md`](./install-prompt-catalogue.md) | Every agent-addressed passage, MCP snippet and best install passage, verbatim, per project |
| [`feedback-asks-verbatim.md`](./feedback-asks-verbatim.md) | Every feedback or contribution ask, verbatim, per project |
| [`discovery-install-prompt-patterns.md`](./discovery-install-prompt-patterns.md) | 16 installation-prompt patterns with examples and URLs |
| [`discovery-feedback-and-contribution-loops.md`](./discovery-feedback-and-contribution-loops.md) | 32 notes on how projects get feedback from agents, and what happened when they tried |
| [`discovery-directories.md`](./discovery-directories.md) | 18 patterns from the llms.txt directories |
| [`discovery-direct-probing.md`](./discovery-direct-probing.md) | 19 findings from probing ~140 projects directly, including who publishes nothing |
| [`per-project/`](./per-project) | One page per project: everything extracted, what they do better, what XERJ does better, adoptable ideas |
| [`data/dissections.json`](./data/dissections.json), [`data/discovery.json`](./data/discovery.json) | The raw structured results, lossless |
