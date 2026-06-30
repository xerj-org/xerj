# Xerj Console Backend API · Design v1

**Status:** design — not implemented. Review and confirm scope before any code lands.
**Version target:** ship MVP alongside xerj `v1.0.x` (post-rc.1).
**Mount point:** `/_xerj-console/api/v1/...` (separate from ES-compat `/_search` and native `/v1/`).
**Packaging:** for v1.0.x, Xerj Console ships compiled into the xerj binary (`crates/xerj-api` + a new `crates/xerj-console-api`), bundled with `xerj-local` as the default-and-only data source. Post-v1.0.x, Xerj Console extracts as a stand-alone product that connects to multiple data sources (xerj remote, Elasticsearch, OpenSearch, Prometheus, Postgres, …) — Kibana-style multi-source observability with xerj as one option among many. The interface boundary is "every endpoint under `/_xerj-console/api/v1`"; the same crate must extract cleanly — no engine-internal types in any handler signature, no shared globals beyond the `AppState` already used by the rest of the API. The pluggable data-source layer (§5.4) is what enables that extraction; we bake the abstraction in on day one.

This document defines what Xerj Console — the production observability UI bundled inside the xerj server binary — needs from the engine to stop relying on browser localStorage. It answers the subscription question (yes, SSE), the auth question (magic link → passkey, no passwords ever, then SSO/SAML/OIDC for enterprise, API tokens after passkey only), the cluster question (Xerj Console surfaces real RAFT state when the node runs as part of a cluster), the data-source question (built-in xerj is one connection among many; the API never assumes a single backend), and draws a clear MVP / Follow-on line.

---

## 1 · Why a separate API surface

Today the playground (which currently doubles as Xerj Console) keeps everything client-side:

| Feature | Today | Why that's a problem |
|---|---|---|
| Dashboard renames / clones / hides / reorder | `localStorage["xerj.dashboards"]` | Every browser is its own cluster of opinions. SE on call A and SE on call B don't share a setup. Lose a cookie ⇒ lose work. |
| Per-dashboard panel layouts | `localStorage["xerj.layout.<id>"]` | Same — no sync, no audit. |
| Saved views (named time + filter snapshots) | `localStorage["xerj.views"]` | A view created during an incident isn't reachable from a teammate's browser. |
| Search box / time / theme / default cluster | `localStorage["xerj.search"]`, `xerj.time`, `xerj.theme`, `xerj.cluster`, … | Per-browser only. |
| Users / API keys / sessions | none — UI is mock | No identity, no multi-tenancy, no auditable access. |
| Alert rules + recent fires | none — UI is mock | Buyers ask "how do we author alerts" and we have no answer. |

We move all of these to engine-backed REST + SSE so Xerj Console behaves like a real product: durable across restarts, shared across browsers, auditable, scriptable.

---

## 2 · Conventions

### 2.1 URL space

```
/_xerj-console/api/v1/<resource>                       — REST collection
/_xerj-console/api/v1/<resource>/<id>                  — REST item
/_xerj-console/api/v1/<resource>/_stream               — SSE collection-level subscription
/_xerj-console/api/v1/<resource>/<id>/_stream          — SSE item-level subscription
/_xerj-console/api/v1/<resource>/<id>/<sub>            — sub-resources (e.g. dashboard layout)
```

Why `_xerj-console/api/v1` and not `/_xy` or `/v1`?
- `_xerj-console/` is already reserved for the bundled UI assets. Putting the API under `_xerj-console/api/v1` keeps the reverse-proxy story trivial — one prefix, one CORS rule.
- `v1` is explicit so we can ship `v2` without breaking older clients.
- Native xerj endpoints under `/v1/...` (indices, schema, pipelines) stay separate — they're for engine ops, not for Xerj Console UX state.

### 2.2 Response shape

```json
{ "data": { ... },          // resource body or array
  "meta": {
    "etag": "W/\"42\"",      // when resource versioning matters
    "page": { "next": "..." }, // for paginated lists
    "request_id": "..."      // echo of x-request-id for grepping
  } }
```

Errors echo xerj's existing `XerjError` shape (HTTP-aligned status + structured body).

### 2.3 Auth

All `_xerj-console/api/v1` routes (except the bootstrap and login endpoints documented in §3) require an authenticated session — passkey-backed by default, optionally upgraded to SSO/SAML/OIDC for enterprise. Bearer API tokens are accepted, but a token can only be minted by a session that has already enrolled a passkey. There is no password authentication anywhere, ever. The full flow — first-launch CLI magic link, passkey enrollment, login, admin-issued invites, API tokens, SSO — lives in **§3 · Authentication & Identity** below. Every write writes the calling principal into the audit log (`who`, `when`, `before`, `after`).

### 2.4 Multi-tenancy

Each persisted resource carries `owner` (user ID) and `org_id` (tenant ID). The default org for v1 is `"default"`. Read scope is filtered by `(owner == me) OR (org_id == my_org AND visibility == "shared")` — same pattern Kibana ended up at, simpler shape.

### 2.5 Storage

Each resource family lives in a hidden system index (`.xerj_*`) so it's never accidentally listed by `_cat/indices` or matched by user `*` patterns. The indices are created on first server boot if missing.

| Index | Mapping highlights | Retention |
|---|---|---|
| `.xerj_dashboards` | id, owner, org_id, visibility, name, panels (json), updated_at, version | none — durable |
| `.xerj_views` | id, owner, dashboard_id, time, filters, updated_at | none |
| `.xerj_prefs` | user_id (`_id`), theme, cluster, time, mobile, layout_overrides | none |
| `.xerj_alert_rules` | id, owner, query, schedule, threshold, connectors[], updated_at | none |
| `.xerj_alert_fires` | rule_id, fired_at, level, summary, sample_docs[] | 90 d hot, then archived |
| `.xerj_audit` | who, when, action, resource, before, after | 365 d |
| `.xerj_users` | user_id (`_id`), email, display_name, role, status (`pending` / `active` / `disabled`), created_at, last_seen_at | none |
| `.xerj_passkeys` | id (`_id` = credential_id), user_id, public_key (cose-encoded), sign_count, transports[], aaguid, name, created_at, last_used_at | none |
| `.xerj_magic_links` | id (`_id` = token_hash), purpose (`bootstrap` / `invite` / `recovery`), user_id (nullable for bootstrap), email, role, created_by, created_at, expires_at, used_at, used_from_ip | rotated — used or expired entries reaped after 30 d |
| `.xerj_sessions` | id (`_id` = session_id), user_id, created_at, expires_at, ip, ua, idp (`passkey` / `oidc` / `saml`), revoked_at | rotated — expired/revoked sessions reaped after 7 d |
| `.xerj_api_tokens` | id (`_id` = token_hash), user_id, name, scopes[], created_at, last_used_at, revoked_at | none |
| `.xerj_idp_config` | id (`_id` = "oidc" / "saml"), config (json), updated_at, updated_by | none — single doc per protocol |
| `.xerj_cluster_state` | node_id (`_id`), role (`leader` / `follower` / `learner`), term, commit_index, last_applied, peers[], updated_at | rotated — current snapshot only |
| `.xerj_connections` | id (`_id`), name, kind, url, auth (json, secret encrypted at rest), default, managed, created_at, created_by, etag | none — durable |

These indices use xerj's existing storage path — no new persistence layer to build. Schema evolution uses the same `_evolve` endpoint as user indices.

### 2.6 Concurrency

Mutating endpoints respect `If-Match: <etag>` for optimistic locking. Server returns `409 Conflict` with the current resource on mismatch so the client can re-fetch and re-display. Without `If-Match` the request is "last write wins" — fine for prefs, dangerous for shared dashboards, so the UI sends `If-Match` for shared resources.

### 2.7 Subscriptions — Server-Sent Events (SSE)

**Yes, we want them.** Use cases are real: live dashboard auto-refresh, alert fires push, ingest-rate gauge, "another user just renamed the dashboard you're editing" toast.

**SSE over WebSocket** because:
- One-way push (server → browser) covers every Xerj Console use case; we never need the browser to push to the server through the same connection (writes go through normal POST/PUT).
- Plain HTTP — reverse proxies, WAFs, and corporate egress filters don't break it.
- Built-in reconnect via `Last-Event-ID` header so the server can replay missed events.
- Simple text wire format, no framing protocol.

**Endpoint pattern:**

```
GET /_xerj-console/api/v1/<resource>[/<id>]/_stream
Accept: text/event-stream

→ 200 OK
   Content-Type: text/event-stream

   event: snapshot
   id: 0
   data: {"data": {...}}

   event: update
   id: 1
   data: {"op":"updated","resource":"dashboards","id":"...","etag":"...","fields":{...}}

   event: heartbeat
   id: 2
   data: {"ts":"2026-04-26T22:30:00Z"}
```

The first frame is always a snapshot of the current state so the client doesn't need a separate fetch + subscribe round-trip. Heartbeats every 15 s keep intermediaries from idling out the connection. The `Last-Event-ID` header on reconnect tells the server "resume from event N + 1"; the server replays from a per-stream ring buffer (default 256 events) or sends a fresh `snapshot` frame if the requested ID is too old.

---

## 3 · Authentication & Identity

### 3.1 Goals (and non-goals)

* Default flow is **dead simple**: install xerj, run it, click the magic link the CLI prints, enroll a passkey, you're in.
* **No passwords**, anywhere, ever. Not as a fallback, not for "service accounts", not in any form. The data model has no `password_hash` column.
* **Enterprise upgrade** is OIDC / SAML / OAuth2 ("SSO"). Once the org admin wires SSO, new users no longer need a magic-link round-trip — the IdP becomes the issuer of identity.
* **API tokens** exist for scripting and CI, but a user can only mint one *after* they have enrolled at least one passkey. This stops the "I emailed myself a token from a brand-new account" bypass.
* **Inviting users** is a passkey-holder-only action. The invitation is itself a magic link, scoped to "set up your passkey", nothing else. An invite never grants access — it grants the right to enroll a credential that grants access.
* Non-goal: TOTP / SMS / hardware-OTP fallbacks. Passkeys cover the device-bound and platform-bound cases already; adding more factors is more code, more compliance scope, and more user pain.

### 3.2 First-launch flow (the magic-link bootstrap)

When `xerj` starts and detects `.xerj_users` is empty (no `active` users exist), it enters **bootstrap mode**:

1. The server generates a single-use, 30-minute magic link token, stores its `sha256` in `.xerj_magic_links` with `purpose: "bootstrap"` and `role: "admin"`, and prints to stderr:

   ```
   ┌─────────────────────────────────────────────────────────────┐
   │ Xerj Console is unconfigured. Open this link to claim admin:       │
   │                                                              │
   │   http://localhost:9200/_xerj-console/setup#token=...              │
   │                                                              │
   │ This link is valid for 30 minutes and works once.            │
   └─────────────────────────────────────────────────────────────┘
   ```
2. The user opens the link. The Xerj Console SPA reads the fragment, calls `POST /_xerj-console/api/v1/auth/magic/redeem`, gets back a short-lived **enrollment session** (`X-Xerj Console-Enroll-Session: ...`).
3. The SPA walks the user through `POST /auth/passkey/begin` → browser WebAuthn prompt → `POST /auth/passkey/finish`. The server creates the `.xerj_users` doc with `role: "admin"`, status `active`, persists the credential to `.xerj_passkeys`, and returns a normal session cookie.
4. The magic-link doc is marked `used_at` so the same token cannot be reused.
5. From this point forward bootstrap mode is off; the next restart skips the printout.

If the user closes the browser before finishing enrollment, the link is still valid until expiry — they can re-open it. If it expires, an admin can mint a fresh one with `xerj admin magic-link --role admin` (CLI calls `POST /_xerj-console/api/v1/auth/magic/issue` against the local socket using the binary's own root credential, see §3.7).

### 3.3 Login flow (returning user)

```
POST /_xerj-console/api/v1/auth/login/begin                  { email }
   → { challenge, allow_credentials: [{id, transports[]}, ...] }
POST /_xerj-console/api/v1/auth/login/finish                 { credential_assertion }
   → 204 + Set-Cookie: xerj_session=...
```

* `allow_credentials` is empty if the email is unknown — the same response shape as a known email with zero passkeys, so the endpoint is not an account-enumeration oracle.
* On success the server creates a `.xerj_sessions` doc, writes a `Secure; HttpOnly; SameSite=Lax` cookie, and returns 204. The cookie is the only thing the SPA needs from then on.
* Sessions live 12 h by default (configurable, see §6 open questions). Idle expiry: 30 min of no requests.

### 3.4 Admin invites (existing user invites a teammate)

```
POST /_xerj-console/api/v1/auth/magic/issue                  { email, role, ttl_minutes? }
   → { id, link, expires_at }
```

* Caller must hold an `admin` (or `owner`) role and an active passkey-backed session.
* `role` is one of `admin` / `editor` / `viewer` (see §3.6).
* The server creates `.xerj_users` with `status: "pending"` and `.xerj_magic_links` with `purpose: "invite"`. The invitee's first action via the link is to enroll a passkey (same `passkey/begin` + `passkey/finish` dance as bootstrap), which flips their status to `active`.
* A pending user with no enrolled passkey **cannot log in** — there is no other credential to authenticate with. If the invite expires, the admin re-issues; the user record stays pending.
* The link can be sent over email by the admin (the response includes the URL; the API itself does not have an SMTP integration in v1 — it's the admin's pasteboard or an out-of-band mailer).

### 3.5 Adding more passkeys to your own account

A user with one passkey wants a second one (laptop + phone, or a roaming security key):

```
POST /_xerj-console/api/v1/auth/passkey/begin                 (authenticated)
   → { challenge, exclude_credentials: [...] }
POST /_xerj-console/api/v1/auth/passkey/finish                { attestation, name? }
   → { id, name, created_at }
GET  /_xerj-console/api/v1/auth/passkeys                      list my passkeys
DELETE /_xerj-console/api/v1/auth/passkeys/:id                revoke one (cannot revoke last one unless an SSO IdP is configured)
```

`name` is a human label (`"MacBook"`, `"YubiKey 5C"`); set on enroll, mutable later via `PATCH`.

### 3.6 Roles

Three built-in roles for v1 — keep it small, enforce server-side, every endpoint declares its required role inline.

| Role | Read | Write own | Write shared | Manage users / invites | Manage SSO / cluster config |
|---|---|---|---|---|---|
| `viewer` | ✅ | ❌ | ❌ | ❌ | ❌ |
| `editor` | ✅ | ✅ | ✅ | ❌ | ❌ |
| `admin` | ✅ | ✅ | ✅ | ✅ | ❌ |
| `owner` | ✅ | ✅ | ✅ | ✅ | ✅ |

`owner` is implied by being the **first** user to claim bootstrap; subsequent admins are minted with `admin`. Owner cannot be deleted — only transferred via `PATCH /_xerj-console/api/v1/users/:id { "role": "owner" }` from the current owner (the call atomically demotes the previous owner to `admin`).

### 3.7 API tokens

```
POST   /_xerj-console/api/v1/auth/api-tokens                   { name, scopes[] }
   → { id, secret, name, scopes[] }   ← `secret` returned ONCE, never re-fetchable
GET    /_xerj-console/api/v1/auth/api-tokens                   list my tokens (no secrets)
DELETE /_xerj-console/api/v1/auth/api-tokens/:id               revoke
```

Gating:
* The caller must be authenticated by passkey **and** must have at least one passkey credential currently enrolled. If they later revoke their last passkey, all their API tokens are also revoked (cascading delete in `.xerj_api_tokens`).
* `scopes` is a subset of the caller's role — a `viewer` can only mint read-scoped tokens.
* Tokens are sent as `Authorization: Bearer xy_<id>_<secret>`. The server hashes the secret at write time; the row stores `sha256(secret)`, never the secret itself.

CLI binding: `xerj` reads a `XERJ_TOKEN` env var or `~/.xerj/credentials` for non-interactive contexts (CI, ops scripts, the runbook in `demo/`).

### 3.8 SSO (OIDC / SAML / OAuth2)

Off in v1.0; on once a buyer asks. The contract is fixed now to keep storage stable:

```
GET  /_xerj-console/api/v1/auth/idp                            list configured providers
PUT  /_xerj-console/api/v1/auth/idp/:protocol                  upsert (owner only) — protocol ∈ {oidc, saml}
DELETE /_xerj-console/api/v1/auth/idp/:protocol                disable
GET  /_xerj-console/api/v1/auth/idp/:protocol/login            redirect to IdP
GET  /_xerj-console/api/v1/auth/idp/:protocol/callback         IdP → us
```

* On first SSO login, the server **provisions a `.xerj_users` doc on demand** — no admin invite needed — using the IdP's email claim and a role mapping configured in `.xerj_idp_config` (`role_claim`, `role_mapping`, `default_role`).
* SSO-provisioned users still **must enroll a passkey** before they can mint API tokens, even though their session was minted by the IdP. (Carve-out: the owner can flip a config bit `allow_sso_only_tokens: true` if they prefer to centralize all credentials at the IdP. Off by default.)
* Sessions from SSO carry `idp: "oidc"` or `idp: "saml"` in `.xerj_sessions` so the audit log distinguishes them.

### 3.9 SSE auth (the Last-Event-ID problem)

`EventSource` cannot set custom headers. The session cookie covers the SPA cleanly because Xerj Console ships in the same binary as xerj (same origin, no CORS dance). For non-browser clients hitting an SSE endpoint with an API token, we accept `?token=<bearer>` on `_stream` URLs — it's a known web-platform compromise; we mitigate by:

* Logging only the token's `id`, never the secret (already true elsewhere).
* Making the `?token=` form opt-in per-token via `scopes: ["sse"]` so a regular API token can't accidentally be replayed in a URL where it would land in proxy logs.

### 3.10 Audit & rate-limiting hooks

* Every auth-relevant write (`magic/issue`, `magic/redeem`, `passkey/finish`, `login/finish`, `api-tokens` POST/DELETE, `idp` PUT/DELETE, role change) writes to `.xerj_audit` with `who`, `when`, `before`, `after`, `ip`, `ua`.
* `login/begin`, `login/finish`, `magic/redeem` are rate-limited per source IP (sliding window; 10 / minute / IP, 100 / hour / IP). On limit exceeded the server returns 429 with no detail in the body — do not leak whether the email or token was valid.

---

## 4 · Cluster awareness / RAFT

Xerj Console must not just *display* the local node — when xerj is running as part of a RAFT cluster it must surface real cluster state (leader, term, peers, replication health) and accept admin-grade actions through the API.

### 4.1 Detection

At startup, the engine knows whether it was launched standalone (`--data-dir`) or as a cluster member (`--cluster ...` / a `[cluster]` section in `xerj.toml`). Xerj Console reads `GET /_xerj-console/api/v1/cluster/info` to learn this once at boot and then opens an SSE channel for live updates.

```
GET /_xerj-console/api/v1/cluster/info
→ { "mode": "standalone" | "raft",
    "node_id": "n1",
    "self_url": "http://...",
    "version": "1.0.0-rc.1",
    "started_at": "..." }
```

When `mode == "standalone"`, all `cluster/raft*` endpoints below return 404. Xerj Console renders the cluster page with a single-node card and hides the topology widget.

### 4.2 RAFT state endpoints (only present when `mode == "raft"`)

```
GET /_xerj-console/api/v1/cluster/raft
→ { "leader_id": "n2", "term": 17, "commit_index": 9842,
    "last_applied": 9842, "log_length": 9842,
    "self": { "id":"n1", "role":"follower", "voting": true } }

GET /_xerj-console/api/v1/cluster/peers
→ [{ "id":"n1", "url":"http://10.0.0.1:9200",
     "role":"follower", "reachable": true,
     "match_index": 9842, "next_index": 9843,
     "last_heartbeat_ms": 47, "version":"1.0.0-rc.1" },
   ...]

GET /_xerj-console/api/v1/cluster/raft/_stream
   SSE: leader changes, term changes, peer reachability flips, snapshot installs
```

`peers` data fetched by the local node from the rest of the cluster on demand — when the local node is a follower, it asks the leader; when the local node is the leader, it answers from its own state. There is no Xerj Console-side aggregator.

### 4.3 Replication health (for the dashboard)

```
GET /_xerj-console/api/v1/cluster/replication
→ { "lag_max_ms": 120,
    "lag_p99_ms": 95,
    "underreplicated_shards": 0,
    "in_progress_snapshots": 0,
    "peers": [ { "id":"n3", "behind_log_entries": 4, "behind_ms": 120 }, ... ] }
```

This is the data the **Cluster** dashboard binds to. SSE on `/cluster/raft/_stream` plus 5-second refresh on this endpoint is enough — replication lag doesn't need millisecond updates.

### 4.4 Admin actions (owner-only)

```
POST   /_xerj-console/api/v1/cluster/peers                    add a peer (provide url + bootstrap token)
DELETE /_xerj-console/api/v1/cluster/peers/:id                remove a peer
POST   /_xerj-console/api/v1/cluster/transfer-leader          { to_node_id }
POST   /_xerj-console/api/v1/cluster/snapshot                 trigger a manual snapshot
```

All four require an `owner`-role session — they are last-resort buttons, gated behind a "type the cluster name to confirm" UX in Xerj Console.

### 4.5 Cross-node addressing

When Xerj Console is opened against any node, it always talks to that node's `/_xerj-console/api/v1/...`. For multi-node clusters where the user wants to pin Xerj Console to the leader, the SPA reads `cluster/raft.leader_id` and shows a "follow leader" toggle that, when enabled, transparently 307-redirects mutating requests to the leader's URL using `cluster/peers[i].url`. Reads stay on the local node. (Why server-side 307 instead of client-side rewrites: the SPA shouldn't have to know peer URLs in its routing layer; the engine already knows them.)

### 4.6 Storage snapshot in `.xerj_cluster_state`

Each node keeps a single doc in `.xerj_cluster_state` keyed by `node_id`. The leader updates its row on every term/commit advance; followers update their own rows on heartbeat. This makes `cluster/peers` queryable as a normal `_search` — handy for the audit-log "who was leader at 03:14" question — without us building a side-channel store.

---

## 5 · Resource catalog

Every endpoint below: green = MVP (must ship for Xerj Console to drop the localStorage hacks), amber = follow-on (plan for, design now, code later).

### 5.1 🟢 Dashboards · `/dashboards`

```
GET    /dashboards                               list dashboards visible to caller
GET    /dashboards/:id                           one dashboard incl. panels
POST   /dashboards                               create
PUT    /dashboards/:id                           replace (If-Match)
PATCH  /dashboards/:id                           partial (rename, hide, reorder, visibility)
DELETE /dashboards/:id                           soft-delete (sets deleted_at)
GET    /dashboards/_stream                       SSE: notify on any visible dashboard change
GET    /dashboards/:id/_stream                   SSE: notify on this dashboard's changes
GET    /dashboards/:id/layout                    layout-only fast path
PUT    /dashboards/:id/layout                    save panel positions (If-Match)
POST   /dashboards/:id:clone                     clone (returns new dashboard with `cloned_from`)
```

Resource shape:

```json
{ "id": "ai-overview",
  "owner": "user-42",
  "org_id": "default",
  "visibility": "private" | "shared" | "default",
  "name": "AI Overview",
  "section": "dashboards",
  "group": "ai",
  "cloned_from": null,
  "panels": [ {"id":"queries","cols":4,"render":"..."} ],
  "filters_default": {...},
  "time_default": "24H",
  "version": 7,
  "etag": "W/\"7\"",
  "created_at": "...",
  "updated_at": "...",
  "deleted_at": null }
```

Replaces: `localStorage["xerj.dashboards"]` and `localStorage["xerj.layout.<id>"]`.

### 5.2 🟢 Saved views · `/views`

```
GET    /views?dashboard=<id>                     list views for a dashboard
GET    /views/:id                                one view
POST   /views                                    create
DELETE /views/:id                                delete
GET    /views/_stream?dashboard=<id>             SSE: new shared views show up live
```

Replaces: `localStorage["xerj.views"]`.

### 5.3 🟢 User preferences · `/prefs`

```
GET    /prefs                                    my prefs (theme, default cluster, time, mobile)
PUT    /prefs                                    upsert
```

Replaces: `localStorage["xerj.theme"]`, `xerj.cluster`, `xerj.time`, `xerj.mobile`, `xerj.edit`.

### 5.4 🟢 Data sources · `/data-sources` — pluggable connections

**Design intent:** Xerj Console is *not* a UI for xerj specifically. It is an observability/exploration UI that ships with xerj as the default data source. The same code base, when extracted as a standalone product (post-v1.0.x roadmap), must connect to Elasticsearch, OpenSearch, a remote xerj cluster, Prometheus, Postgres, anything else a buyer brings — exactly the way Kibana evolved into "Kibana with Elastic Stack" plus alternative sources. We bake that abstraction in **on day one** even though only one connection ships in v1, because retrofitting it later means breaking every dashboard panel binding.

#### 5.4.1 Connection model

A **connection** is a named pointer to a backend. Every dashboard panel, search box, alert rule, and saved view binds to a connection by `id`, never to a hard-coded URL.

```
{ "id": "built-in",
  "name": "Local Xerj",
  "kind": "xerj-local",          ← adapter type, see below
  "url": null,                     ← null for in-process; "https://..." for remote
  "auth": { "kind":"none" },
  "default": true,
  "managed": true,                 ← true ⇒ created by the engine, not user-editable
  "created_at": "...",
  "version_seen": "1.0.0-rc.1",
  "status": "green" | "yellow" | "red" | "unreachable",
  "last_checked_at": "..." }
```

Adapter kinds in scope (v1.0 ships `xerj-local` only; the others are stubs that return 501 until the standalone build needs them):

| `kind` | Backend | Auth | Notes |
|---|---|---|---|
| `xerj-local` | the same in-process xerj engine | none | The built-in default. Bypasses HTTP — calls into `Engine` directly. |
| `xerj-remote` | a remote xerj node over HTTPS | bearer / passkey-session | Identical wire format to built-in, just over the network. |
| `elasticsearch` | ES 7.x / 8.x cluster | api-key / basic / cloud-id | The Kibana-replacement use case. |
| `opensearch` | OpenSearch 2.x | api-key / basic | Sibling of `elasticsearch` with different version detection. |
| `prometheus` | Prometheus / Mimir | bearer / none | For metrics panels. |
| `postgres` | Postgres / Timescale | dsn | For relational panels (alerts on slow-query rows, etc.). |

#### 5.4.2 Endpoints

```
GET    /data-sources/connections                            list
GET    /data-sources/connections/:id                        one
POST   /data-sources/connections                            create  (admin only)
PATCH  /data-sources/connections/:id                        update  (admin; managed connections rejected)
DELETE /data-sources/connections/:id                        delete  (admin; managed connections rejected)
POST   /data-sources/connections/:id:test                   probe — returns version, status, ping_ms

GET    /data-sources/connections/:id/indices                [{ name, docs, bytes, shards, replicas, retention }]
GET    /data-sources/connections/:id/indices/:name/fields   [{ name, type, indexed, encoding, ratio, cardinality }]
GET    /data-sources/connections/:id/_stream                SSE: status flips, doc-count deltas every N seconds
POST   /data-sources/connections/:id/_search                opaque pass-through query
                                                             (body shape adapter-specific; xerj & ES use ES DSL)
```

`built-in` is auto-created on first start, lives in `.xerj_connections`, and has `managed: true` so it cannot be renamed or deleted. The bootstrap admin can add new connections via `POST /connections`; their secrets land in `.xerj_connections.auth.secret` encrypted at rest with the same key used for cookies.

#### 5.4.3 Adapter trait (engine-side, Rust)

This stays out of the public HTTP API but pins down the abstraction:

```rust
#[async_trait]
trait ConsoleSource: Send + Sync {
    fn kind(&self) -> &'static str;
    async fn probe(&self) -> Result<ProbeInfo>;
    async fn list_indices(&self, q: &IndexQuery) -> Result<Vec<IndexInfo>>;
    async fn list_fields(&self, idx: &str) -> Result<Vec<FieldInfo>>;
    async fn search(&self, idx: &str, body: serde_json::Value) -> Result<serde_json::Value>;
    fn supports_streaming(&self) -> bool { false }
    async fn stream_changes(&self) -> Option<BoxStream<'static, ConnectionEvent>>;
}
```

Every Xerj Console endpoint resolves `connection_id` → `&dyn ConsoleSource` once at the top of the handler. Panel bindings, alert rules, search jobs all flow through the same trait. The built-in adapter is a thin wrapper around `Engine`; `xerj-remote` is a wrapper around `reqwest`; `elasticsearch` adds version detection and a few well-known query-shape rewrites.

#### 5.4.4 Storage

```
.xerj_connections   id, name, kind, url, auth (json, secret encrypted),
                      default, managed, created_at, created_by, etag
```

Add this row to §2.5.

#### 5.4.5 Why a stable contract now?

If a panel today is bound to a hard-coded `"index": "logs-ssh-auth"` and a hard-coded `localhost:9200`, then peeling Xerj Console off into its own product requires (a) rewriting every dashboard binding and (b) breaking the "open xerj, see your data" promise of the bundled build. By introducing the connection layer in v1, the migration is "add a non-default connection, point dashboards at it" — a UX flow we want enterprises to walk through anyway.

Replaces the live wiring I added to `playground/src/data/data-sources.js`.

### 5.5 🟢 Users · `/users` (admin/owner) and `GET /me`

The auth flow in §3 owns enrollment and login. This section is the *management* surface — listing, role-changing, disabling.

```
GET    /users                                    list users in my org (admin only)
PATCH  /users/:id                                rename, change role, set status
DELETE /users/:id                                soft-delete (revokes sessions, tokens, passkeys)
GET    /me                                       caller identity
```

Creation is **not** here — users are created via `POST /auth/magic/issue` (admin invite) or first-SSO-login auto-provision (§3.8). There is no `POST /users` body that takes a password or any other primary credential.

API tokens live at `/auth/api-tokens` (§3.7), not under `/users`, because they're a property of the calling session not of an arbitrary admin-managed object.

### 5.6 🟡 Alerts · `/alerts/rules` + `/alerts/fires`

```
GET    /alerts/rules                             list rules in scope
POST   /alerts/rules                             author rule
PUT    /alerts/rules/:id                         replace
DELETE /alerts/rules/:id                         delete
POST   /alerts/rules/:id:test                    dry-run against current data
GET    /alerts/fires?rule=&since=                recent fires
GET    /alerts/fires/_stream                     SSE: new fires push to UI
```

Rule shape:

```json
{ "id": "rule-1",
  "name": "ssh failures spike",
  "query": { "bool": { ... } },
  "schedule": { "every": "1m" },
  "threshold": { "agg":"count", "op":">", "value":1000 },
  "connectors": [ {"type":"webhook","url":"..."} ],
  "owner":"user-42", "org_id":"default", "etag":"W/\"3\"" }
```

### 5.7 🟡 Audit log · `/audit`

```
GET    /audit?resource=&since=&until=            cursor-paginated audit entries
GET    /audit/_stream                            SSE: live tail (admin only)
```

Backed by the existing `_audit/_search` xerj surface — this is just a Xerj Console-friendly facade.

### 5.8 🟡 Search jobs · `/search-jobs` (long queries)

For long-running searches over big indices the UI submits a job and subscribes to partial results.

```
POST   /search-jobs               { index, body, timeout_ms }   → { id }
GET    /search-jobs/:id                                        current state + partial hits
GET    /search-jobs/:id/_stream                                SSE: hits, took, percent_done
DELETE /search-jobs/:id                                        cancel
```

This finally answers the "the SE typed a 30 M-doc terms agg and the browser hung" pain.

---

## 6 · MVP cut for v1.0.x — what to build first

| Order | Scope | Why |
|---|---|---|
| 0 | **Auth bootstrap** — `.xerj_users` + `.xerj_passkeys` + `.xerj_magic_links` + `.xerj_sessions` system indices, first-launch magic-link printout, `POST /auth/magic/redeem`, `POST /auth/passkey/{begin,finish}`, `POST /auth/login/{begin,finish}`, session cookie middleware. | Nothing else can ship without this — every other endpoint requires an authenticated session. |
| 1 | **Cluster info** — `GET /cluster/info`, plus `GET /cluster/raft` and `GET /cluster/peers` *if* the node is in raft mode (returning 404 in standalone). No SSE yet, no admin actions. | Xerj Console's cluster page is mostly read-only on day one; this gates the standalone-vs-raft branch in the SPA. |
| 2 | `GET/PUT /prefs` | Smallest possible end-to-end test of the storage path; gets us off `localStorage["xerj.theme"]` etc. |
| 3 | Dashboards `GET / GET:id / PUT:id / PATCH:id / POST` + `GET /_stream` (SSE) | Replaces the localStorage `xerj.dashboards` + `xerj.layout.<id>` hacks. The SSE channel is what makes the demo "feel live" when an SE renames a dashboard mid-call. |
| 4 | Saved views CRUD | One screen, low surface area, removes another localStorage dependency. |
| 5 | `GET /me` + `GET/PATCH/DELETE /users` + `POST /auth/magic/issue` (admin invite) + `POST/GET/DELETE /auth/api-tokens` | Full identity surface: invite a teammate, revoke a teammate, mint a CI token. |
| 6 | **Data sources** — `GET /data-sources/connections`, `GET /data-sources/connections/:id/indices`, `GET /data-sources/connections/:id/indices/:name/fields`. Default `built-in` connection points at the local xerj engine. | Lets Xerj Console render the Data section without bypassing the abstraction. Even though only one connection ships in v1, the **shape** must already be plural — see §5.4 / §6.1 for why this matters for the standalone Xerj Console product. |

Everything 🟡 (alerts, audit facade, search-jobs, full SSO, RAFT admin actions) is design-now-build-later: storage indices and route shapes exist as stubs returning 501 so the contract is published, but the bodies can be empty until the demand is real.

---

## 7 · What this changes about the demo

After MVP lands:
- Xerj Console drops every `localStorage` write except an emergency cache.
- The "Settings → Persistent state" page becomes "settings on the server" instead of "settings in this browser".
- An SE can run the demo on machine A, and a teammate watching on machine B sees the same dashboard in real time (SSE).
- The playground codebase reverts to being a marketing-page UX mockup — zero engine wiring, no localStorage tricks.

---

## 8 · Open questions for review

1. **Org/tenant model**: should `org_id` exist in v1, or hard-code `"default"` everywhere and add the multi-tenant column in v2? My read: include the column from day one (cheap), enforce it in v2.
2. **Internal indices visibility**: keep `.xerj_*` strictly hidden from `_cat/indices` even for admins? My read: hidden by default, surface them under `_cat/indices?include_system=true`.
3. **SSE vs short-polling fallback**: do we need a polling fallback for clients that can't keep an SSE connection open? My read: not in MVP; SSE is broadly supported and corporate proxies that break it can also break websockets. Re-evaluate if a buyer hits it.
4. **Standalone-Xerj Console storage**: when Xerj Console ships as a separate product (§5.4) it cannot store its own state in `.xerj_*` indices on a remote xerj, because the remote may not be xerj. Do we (a) require the user to point standalone-Xerj Console at a "config xerj" for state, (b) ship an embedded SQLite for state, or (c) make state pluggable? My read: (b) for the standalone build is the cheap path; the bundled-in-xerj build keeps using `.xerj_*` indices unchanged.
5. **Magic-link delivery in v1**: the API returns the link URL to the admin's browser; v1 has no SMTP. Is "copy and paste into your mail client" acceptable for the first release, or do we need a minimal SMTP relay? My read: copy-paste is fine for v1.0; SMTP is a v1.1 addition.
6. **Default session TTL**: 12 h hard expiry + 30 min idle is the proposal. CISO buyers may want shorter — ship as a config knob from day one (`xerj-console.session.max_age`, `xerj-console.session.idle`).
7. **Passkey attestation policy**: do we require attestation (so the org can pin to "only Yubico keys") or accept any conformant authenticator? My read: accept any in v1; expose a config knob in v1.1 once a buyer asks.
8. **API token `?token=` on SSE**: opt-in scope `["sse"]` is the proposal (§3.9). Push back if the better path is "session cookie even for non-browser clients" via a cookie-jar on the CLI.
9. **RAFT cross-node `/cluster/peers` data**: today the proposal is "follower asks leader on demand". Is there a reason to instead have every node continuously gossip its row to `.xerj_cluster_state` so the index *is* the source? My read: gossip is more code, less timely; keep "leader is the source".
10. **Connection encryption-at-rest** (§5.4 follow-up): connection records (`.xerj_connections`) hold credentials for remote ES clusters. We need to encrypt the `auth.secret` field. Master key from env var or from a sealed key file? My read: env-var override + a sealed file under `data_dir/.xerj_master_key` so vanilla deploys still work.
11. **Adapter packaging**: in the bundled build, do we compile every adapter (built-in, http-elasticsearch, http-opensearch, http-xerj-remote, prometheus, postgres) into the binary, or feature-gate them? My read: feature-gate; default features ship `built-in` + `http-elasticsearch` only.

Confirm or push back before I start writing route handlers.
