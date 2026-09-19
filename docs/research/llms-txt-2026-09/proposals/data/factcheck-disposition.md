## Disposition — what happened to everything that was not a clean "found"

| Id | Result | What was done |
|---|---|---|
| C028 | NOT CONFIRMED — the sentence "copy the above MCP information into their expected format (json, yaml, etc)" is not on `supabase.com/docs/guides/ai-tools/mcp.md` (200, text). The 2026-09-18 draft quoted it in §3.6. | **Removed from the report.** The Supabase one-liner on the same page (C027) is confirmed and kept. The claim stays in this list so the removal is visible. |
| B30 | found (loose) — Lighthouse: the sentence is on the page, inside HTML markup. | Kept, and labelled "loose match" where it is used (report §3.16). |
| B46 | found (loose) — RedMonk: the body uses HTML entities for the apostrophes. | Kept, labelled "loose match", and labelled as RedMonk's characterisation rather than curl's statement (report §5.3). |
| B03–B07 | confirmed as **404** — the five `install.md` examples listed by the install.md README. | Used as evidence in report §4. |
| C071 | confirmed as a **soft 404** — `docs.sentry.io/SKILL.md` answers HTTP 200 with a "Page Not Found" body. | Recorded as not existing (per-project/sentry.md). |
| C137, C138 | confirmed as **soft 404s** — `vercel.com/SKILL.md` and `vercel.com/llms-full.txt` answer HTTP 200 with a 2.6 MB HTML application shell. | Cited as soft-404 examples (report §0); Vercel's real `llms-full.txt` is under `/docs/` (per-project/vercel.md). |
| C085 | `docs.expo.dev/llms-full.txt` answers 200 but its effective URL and body are `llms.txt`. | Recorded as "not published separately" (per-project/expo.md). |
| D01–D13 | confirmed as **404** on xerj.org. | These are the surfaces the ship checklist (report §9) says nothing may link to yet. |

Corrections made to claims while checking them (the claim text now reflects the source that actually holds the quote):

- **Cloudflare (C009).** The paste prompt is in the clipboard script of `https://developers.cloudflare.com/agent-setup/` (HTML). It is not in `/agent-setup/index.md`, which is where the draft's URL pointed.
- **no-agents.md (B26).** `https://codeberg.org/rossabaker/no-agents.md` is a repository whose *name* ends in `.md`; the page is HTML by design. The checker's soft-404 rule misfired on it, so the claim reads the raw README, and the rule now exempts code-host file viewers (`github.com/<o>/<r>/blob/…`, a Codeberg repository root).
- **SurrealDB (B50, B51).** The draft attributed "**surrealdb** should be listed as connected." to SurrealDB without a URL. It is on `/docs/agents/claude-code.md`, and it is confirmed *absent* from `surrealdb.com/llms.txt`.
- **Sources found for quotes the draft left without a URL:** MicroPython (B61, the wiki's ContributorGuidelines), QuantEcon (B62, qeps issue 12 through the GitHub API), ClickHouse's `AGENT` env var (B57), Neon's unknown-agent rule (B56), Sonarr (B53, pinned commit), Vexa (B54, pinned commit), GreptimeDB (B55), Anthropic's skills overview (B52).
- **Deleted, because no source could be found:** "Meilisearch's prompt-sequence tutorial" (the string occurs in neither data file nor on any fetched page).
- **Deleted, because they depended on a corpus that was never committed:** the draft's counts "`claude mcp add` appears in 34 probed files", "`npx skills add` … 36 probed files", "19 probed files", "29 probed domains", "appears in 7 probed files", "104 of the 131 files". The report now uses the counts in `llms-txt-measurements.md`, which anyone can re-run.

What this file does **not** check: that a quote means what the report says it means (that is a reader's job), anything behind a login, and the numbers other projects publish about themselves — those are labelled "published by X, not run by us" wherever they appear.
