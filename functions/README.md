# Cloudflare Pages Functions (xerj.org)

Each `.js` file here is a Pages Function; its file path maps to its URL
(`lead.js` → `/lead`). A function is only invoked once its path is listed in
[`../landing/_routes.json`](../landing/_routes.json) — otherwise the static
asset handler shadows it. Bindings live in [`../wrangler.toml`](../wrangler.toml).

| File | URL | Purpose | Storage |
|------|-----|---------|---------|
| `get.js` | `/get`, `/get.ps1` | serve installer + count requests | R2 `INSTALLS` |
| `get.sh.js` | `/get.sh` | alias of `/get` | — |
| `lead.js` | `/lead` | capture a work email | R2 `LEADS` |
| `api/field-report.js` | `/api/field-report` | **fallback** agent field-report intake | KV `FIELD_REPORTS` |

## `/lead` — email validation

`POST /lead` requires a string `email`, trimmed and lowercased before
validation and storage. Non-string values, including arrays and objects,
return `400 { ok:false, error:"invalid email" }` without writing to R2.
An R2 write failure for a valid email still returns `500`.

## `/api/field-report` — agent field-report fallback

**The canonical path to file a field report is a GitHub PR** (`xerj feedback
--open-pr`, landing in `user-feedback/16-agent-field-reports/`). That stays
primary. This HTTP endpoint is the **fallback ONLY** for an agent that cannot
reach GitHub (no shell / no `gh` / no identity, e.g. a sandboxed Claude Code).

**Contract**

```
POST https://xerj.org/api/field-report
Content-Type: application/json

{ "agent": "...", "used_for": "...", "verdict": "...",   // required
  "version": "...", "os": "...", "indexed": "...",       // optional
  "notes": "..." }                                       // optional (all short strings)
```

- `200 { ok:true, stored:true, note:"...GitHub PR is canonical..." }` on success.
- `405` non-POST · `415` wrong Content-Type · `413` body > ~16 KB ·
  `400` bad JSON / missing required field · `429` rate limited ·
  `503` daily cap reached (use the GitHub PR path).
- CORS is open for POST (`Access-Control-Allow-Origin: *`); `OPTIONS` preflight
  returns `204`.

**Rate limit** — at most **1 accepted report per IP per minute** (`rl:<ip>`,
60s TTL). A second request inside the minute → `429`.

**Daily cap / anti-overage** — an accepted-report counter `cnt:<YYYY-MM-DD>`
(2-day TTL). At `DAILY_CAP` (300) new reports are **dropped with `503`** so
spam cannot push KV past the free-tier 1000 writes/day (each accepted report =
3 writes → 300×3 = 900 < 1000). Rejected requests perform no writes.

**Privacy** — the raw IP is never stored; `ip_hash` is `sha-256(IP + salt)`.
No secrets stored, input is length-capped and never echoed as HTML.

**Storage is KV only — never R2** (R2 is frozen by owner order). Create the
namespace once before deploy:

```sh
wrangler kv namespace create FIELD_REPORTS
# paste the printed id into [[kv_namespaces]] in ../wrangler.toml, then redeploy
```

Machine-readable body schema:
[`../landing/docs/agents/schemas/field-report.json`](../landing/docs/agents/schemas/field-report.json).
