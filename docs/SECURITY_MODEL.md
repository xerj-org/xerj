# XERJ Security Model

This document describes the authorization boundaries XERJ actually enforces, as
implemented in the source. It is written for operators deciding how to deploy a
node and for contributors adding a handler that touches index data.

Every statement below points at the code that makes it true. Where a control is
partial, unenforced, or absent, that is stated in the same place rather than in
a footnote. The source is authoritative; if this document and the code disagree,
the code is right and this document is a bug.

Primary sources:

| Area | File |
|---|---|
| Authentication, `Principal` | `engine/crates/xerj-api/src/auth.rs` |
| Authorization decision, enforcement points | `engine/crates/xerj-api/src/authz.rs` |
| Engine-side visibility funnel | `engine/crates/xerj-engine/src/index_guard.rs` |
| Privileges, roles, `role_descriptors` parsing | `engine/crates/xerj-engine/src/rbac.rs` |
| Console client identity and rate limiting | `engine/crates/xerj-console-api/src/client_ip.rs`, `.../auth/rate_limit.rs` |
| Cluster control-frame authentication | `engine/crates/xerj-cluster/src/auth.rs` |

## The two layers

Authorization is decided twice, in two different places, for two different
reasons.

### Layer 1: the authz middleware

`authz_middleware` (`xerj-api/src/authz.rs:1224`) runs on both HTTP routers. It
is added to the router before `auth_middleware`, so it runs after it
(`xerj-api/src/router.rs:786-789`): authentication has already resolved the
caller to a `Principal`, and this layer decides what that principal may reach.

It does four things:

1. Classifies the path (`classify`, `authz.rs:444`) into a brain, a memory
   namespace, one or more index expressions, a cluster endpoint, or exempt.
   Path segments are percent-decoded first (`percent_decode`, `authz.rs:421`),
   so `%2Exerj-memory-alice` cannot slip past a check that
   `.xerj-memory-alice` would fail.
2. Works out the privilege the request needs (`required_privilege`,
   `authz.rs:505`). `GET`/`HEAD` need read. A `POST` whose operation segment is
   in `POST_READ_OPS` (`_search`, `_count`, `_msearch`, `_mget`, `_sql` and
   others, `authz.rs:349`) needs read. Operations in `ADMIN_OPS` (`_settings`,
   `_mapping`, `_close`, `_alias`, `_forcemerge` and others, `authz.rs:388`)
   need manage, as does a request with no operation segment at all, because that
   creates or destroys the index. Everything else is treated as a write.
3. Extracts the indices the request body names (`body_targets`, `authz.rs:730`)
   and authorizes each one. The shapes it parses are enumerated in `BodyShape`
   (`authz.rs:578`): `_bulk` action lines, `_msearch` header lines, `_mget`
   `docs[]._index`, `_aliases` actions, `_reindex` `source`/`dest`, snapshot
   `indices`, `PUT /{index}` `aliases`, index-template `index_patterns`, ML job
   and datafeed configs, the native `POST /v1/indices` body, and a `terms`
   lookup or `lookup` runtime field at any depth inside a search body. Aliases
   are resolved to their backing indices before the decision
   (`authorize_expression`, `authz.rs:1168`).
4. Installs the principal as the engine's visibility rule for the rest of the
   request and prunes reserved index names out of metadata responses
   (`prune_response`, `authz.rs:1431`).

A superuser short-circuits on the first line of the middleware
(`authz.rs:1237`): no body buffering, no visibility guard, no response pruning.
For every other principal, the body is buffered only on routes that can name an
index in it, bounded by `limits.max_body_bytes`; a body over that limit is
refused with `413` rather than being authorized unread (`authz.rs:1251-1262`).

A denial at this layer is an ES-shaped `403` with `type: security_exception`
(`forbidden`, `authz.rs:242`; body shape in `xerj-api/src/error.rs:52`). It is
returned before any existence check, so `403` versus `404` cannot be used to
enumerate what exists.

A body that should have named its targets but cannot be parsed is refused, not
ignored (`unresolvable`, `authz.rs:709`).

### Layer 2: the index_guard visibility funnel

The middleware can only check what it knows how to parse. The second layer sits
where a name becomes an index, which every path must pass through.

`authz_middleware` wraps the inner service in
`xerj_engine::index_guard::scoped` (`authz.rs:1272-1273`), which sets a `tokio`
task-local holding an `IndexVisibility` implementation for the principal.
`PrincipalVisibility::visible` (`authz.rs:304`) answers true when the principal
holds read, write, or manage on that index: this layer decides whether the
index may be reached at all, and the middleware still decides which privilege
the request needs on it.

Six engine functions consult it:

| Function | Line |
|---|---|
| `Engine::create_index` | `xerj-engine/src/engine.rs:786` |
| `Engine::delete_index` | `engine.rs:1110` |
| `Engine::get_index` (including the alias branch) | `engine.rs:1165`, `engine.rs:1179` |
| `Engine::get_or_create_index` | `engine.rs:1199` |
| `Engine::index_name_list` | `engine.rs:1254` |
| `Engine::list_indices` | `engine.rs:1281` |

A denied name is reported as `XerjError::index_not_found`, not as a refusal.
That is deliberate (`index_guard.rs:87-99`): a brain you may not read is
indistinguishable from a brain that does not exist, and fan-out verbs such as
`GET /_cat/indices` or a `logs-*` wildcard filter to the visible set instead of
failing wholesale.

### Absent guard means allow, and what follows from that

`index_guard::visible` returns `true` when no guard is installed
(`index_guard.rs:125-127`, the `unwrap_or(true)`). This is not "permission
granted": it means no request is in scope. Startup index discovery, WAL replay,
background flush and merge timers, and snapshot restore all run with no
task-local set and must not be restricted.

Three consequences follow, and all three are load-bearing:

- **Detached work loses the guard.** A `tokio::spawn` or `spawn_blocking` starts
  a new task, which does not inherit the task-local. `index_guard::current()`
  captures the rule on the request's task and `scoped_opt` re-installs it inside
  the spawned future (`index_guard.rs:146`, `index_guard.rs:161`). This was a
  live hole, not a hypothetical: the ML datafeed scorer started by
  `POST /_ml/datafeeds/{id}/_start` re-read its source index on a timer with no
  guard installed (`index_guard.rs:34-57`). `es_compat::spawn_datafeed_task` is
  the worked example of carrying the rule across.
- **rayon workers never see the guard.** A rayon worker is an ordinary OS thread
  that `tokio` does not poll tasks on, so the thread-local backing the guard is
  simply unset there. The test `absent_guard_on_a_rayon_worker`
  (`index_guard.rs:247`) pins this. No rayon closure in the tree resolves an
  index name; the bulk fan-out parallelises NDJSON parsing only.
- **A surface outside the middleware must do its own checking.** The Console
  router is merged onto the engine routers after their layers are applied, so
  `authz_middleware` never runs for it and its in-process engine calls carry no
  guard. The Console data-sources proxy therefore refuses the reserved namespace
  itself, with the same `404` a Console system index gets
  (`xerj-console-api/src/data_sources.rs:37-48`). The gRPC listener does the
  same with `Principal::allows_index` (`xerj-server/src/grpc.rs:62-75`) and
  refuses index patterns outright for non-superusers (`grpc.rs:81-88`).

If you add a surface that reaches `Engine` outside `authz_middleware`, neither
layer protects it. Check it explicitly.

## The reserved `.xerj-memory-*` namespace

`RESERVED_INDEX_PREFIX` is `.xerj-memory-` and `is_reserved_index` is a prefix
test (`xerj-common/src/types.rs:190-195`). The definition lives in
`xerj-common` because more than one crate has to refuse the same namespace.

Indices under this prefix are not ordinary indices. They hold agent-memory
namespaces and second brains:

- `POST|GET|DELETE /_memory/{ns}` and `/_memory/{ns}/_recall`,
  `/_memory/{ns}/{id}` operate on `.xerj-memory-{ns}`
  (`memory_namespace_index`, `authz.rs:159`; routes at
  `xerj-api/src/router.rs:768-775`).
- `/_graph/{brain}/link`, `/link/{edge_id}`, `/ego`, `/overview` operate on
  `.xerj-memory-{brain}-edges` (`brain_edges_index`, `authz.rs:153`; routes at
  `router.rs:780-783`), and the graph read paths additionally authorize the
  brain's nodes index (`graph_api.rs:729`, `graph_api.rs:906`).

What makes the namespace special:

- A key minted without usable `role_descriptors` (`Principal::Unscoped`) holds
  **nothing** here, while keeping its historical reach over ordinary indices
  (`Principal::allows_index`, `auth.rs:99-106`). That is the fail-closed half:
  an unconfigured credential is denied the brain, not handed it.
- A name a caller is about to *create* is checked against `may_reach_reserved`
  (`authz.rs:177`), which judges a pattern by the literal text before its first
  `*`. This is what stops an alias, a fresh index, or an index template pattern
  from squatting the prefix and having a brain resolve through it later
  (`authorize_template_patterns`, `authz.rs:989`).
- Handlers under `/_memory` and `/_graph` call
  `authorize_memory_namespace` / `authorize_brain` themselves as well as being
  covered by the middleware (`memory_api.rs:319`, `:491`, `:568`, `:1148`,
  `:1234`, `:1278`; `graph_api.rs:416`, `:578`, `:722`, `:1178`).

One deliberate exception, worth reading before you hand out a key: a grant of
`names: ["*"]` **does** reach the reserved namespace. `Role::applies_to` treats
`*` as matching every index, brains included, and this is documented as an
operator's explicit choice rather than an escalation, because only a superuser
can mint a scoped key at all (`rbac.rs:72-111`). If you want brain isolation,
grant concrete names or a prefix that cannot reach the namespace, such as
`logs-*`.

## Principals

`Principal` (`auth.rs:55-90`) has four variants, in decreasing order of reach.

| Principal | How you become one | Reach |
|---|---|---|
| `Superuser` | `auth.enabled = false` (which `--insecure` sets), or an empty `admin_api_key`, or presenting the configured admin key | Everything. Skips the whole authorization path |
| `Scoped` | A key minted by `POST /_security/api_key` **with** usable `role_descriptors` | Only the indices its roles name, with the privileges those roles carry |
| `Unscoped` | A key minted **without** usable `role_descriptors` | Ordinary indices as before; nothing in `.xerj-memory-*` |
| `Denied` | No valid credential, or an expired, invalidated, or unknown one | Nothing |

Accepted credential forms (`authenticate`, `auth.rs:214`):

- `Authorization: ApiKey <key>` and `Authorization: Bearer <key>`, where `<key>`
  is either the admin key or the `encoded` value of a minted key
  (`base64("id:api_key")`).
- `Authorization: Basic <base64(user:pass)>`, where the password half is the
  admin key (any username), or the decoded `user:pass` is read as a minted
  key's `id:secret` (`basic_principal`, `auth.rs:257`). Kibana's basic-realm
  login sends this shape.

All secret comparisons are constant-time after a length check: the admin key
against the configured value (`constant_time_eq`, `auth.rs`), a minted key
against its stored hash (`secret_hash::verify_secret`).

Two paths are exempt from authentication: `/health/live` and `/health/ready`
(`AUTH_EXEMPT_PATHS`, `auth.rs:41`), so a hardened deployment's probes do not
crashloop. Neither handler touches index data. `/_security/_authenticate` and
`/v1/metrics` are exempt from *authorization* classification only
(`authz.rs:448-453`); `/v1/metrics` still needs the admin key unless
`auth.metrics_token` is configured, in which case that token opens that one
endpoint and nothing else (`auth.rs:290-301`).

With `auth.enabled = true` (the default) and no configured key, the server
generates a 64-character hex admin key on first run, prints it, and writes it to
`<data_dir>/admin.key` with mode 0600, reusing it on later starts
(`ensure_admin_key`, `xerj-server/src/main.rs:500-548`).

## API keys and privileges

### What a scoped key can and cannot do

A minted key is **never** a superuser (`check_minted_key`, `auth.rs:330-360`).
The strongest thing it can be is `Unscoped`.

Minting is gated (`security_create_api_key`,
`xerj-api/src/es_compat.rs:25507-25580`):

- A `Scoped` caller may not mint at all, and gets a `403`.
- An `Unscoped` caller may mint, but any `role_descriptors` it supplies are
  ignored with a warning, so it can only produce more unscoped keys.
- Only the superuser can mint a scoped key.

`role_descriptors` are parsed by `roles_from_role_descriptors`
(`rbac.rs:159`), one `Role` per `indices[]` entry. The ES privilege names map
onto XERJ privileges as follows (`es_index_privilege`, `rbac.rs:123`):

| ES privilege in `role_descriptors` | Granted |
|---|---|
| `all` | read + write + manage |
| `read`, `read_cross_cluster`, `view_index_metadata`, `monitor` | read |
| `write`, `index`, `create`, `create_doc`, `delete`, `maintenance` | write |
| `manage`, `create_index`, `delete_index`, `manage_ilm`, `manage_follow_index` | manage |
| anything else | nothing |

Index-name matching (`Role::applies_to`, `rbac.rs:99`): `*` matches everything,
a literal name matches exactly, and a **trailing** `*` matches by prefix. A `*`
in the middle of a pattern matches nothing.

Not honoured, and each one fails closed by granting nothing
(`rbac.rs:150-158`):

- `cluster` privileges inside a descriptor are ignored. XERJ has no
  cluster-privilege enforcement.
- `field_security` and `query` (field-level and document-level security) are
  ignored. A descriptor that only makes sense with FLS or DLS therefore
  over-grants at the field level within an index it was already granted. Do not
  rely on this for field-level control.
- A descriptor that names no index, or names indices but no recognised
  privilege, produces no role at all, which makes the key `Unscoped`.

A key also carries expiry and invalidation, both checked on every request before
the secret comparison (`auth.rs:338-349`).

### Key secrets at rest

Minted secrets are stored as a **salted SHA-256 hash**, never in the clear
(`secret_hash.rs`, issue #201). `<data_dir>/api_keys.json` holds
`$ssha256$<salt>$<digest>` per key; authentication re-hashes the presented
secret under that record's salt and compares digests in constant time
(`ApiKeyRecord::verify_secret`). A store written before this lands is migrated
on boot — hashed, and the file rewritten without the plaintext — before the
server accepts a request.

Exactly two on-disk shapes are credentials: a `secret_hash` that decodes (the
post-#201 form, and the winner whenever both are present), and a non-empty
`secret` with no `secret_hash` at all (the pre-#201 form, which is what
migration is for). Every other record is **dropped on load with an error** —
including one whose `secret_hash` carries the `$ssha256$` tag but does not
decode, from a hand edit or an encoding a later build wrote. Nothing could ever
authenticate as such a record, so restoring it and listing it through
`GET /_security/api_key` would be a lie about which credentials exist. The
discriminator is `secret_hash::is_usable_hash`, a full decode rather than a tag
check, and both the load path and `verify_secret` use that one function so they
cannot disagree about what counts as a credential. The drop is in memory: the
file is only rewritten when something migrated, so a dropped record normally
stays on disk for inspection.

The hash is deliberately *fast*, not a password hash. The secret is two v4
UUIDs of CSPRNG output (244 bits), never chosen or reused by a human, so an
offline guessing attack is not the threat; a per-request Argon2/scrypt would
buy nothing against it and would hand an attacker a CPU-exhaustion DoS. This is
the same choice Elasticsearch makes for API keys (its default
`xpack.security.authc.api_key.hashing.algorithm` is `SSHA256`).

What this does **not** protect against: an attacker who can read the process's
memory, or who intercepts the one response that carries the plaintext. The file
stays 0600, because a hash plus a key id is still an offline target and still
enumerates which credentials exist.

### Introspecting your own credential

`GET /_security/_authenticate` reports the principal that made the call
(`security_authenticate`, issue #201): `authentication_type: "api_key"` in the
`_es_api_key` realm, with an `api_key: {id, name}` block and the
`role_descriptors` names the key was minted with, for a minted key;
`authentication_type: "realm"` with `roles: ["superuser"]` for the admin key or
open mode. A key minted without usable `role_descriptors` reports
`roles: ["unscoped"]` — it holds no grant in the reserved namespace but keeps
its historical reach over ordinary indices, so neither `["superuser"]` nor `[]`
would be true. Before #201 this endpoint answered `superuser` for every caller.

One deliberate divergence from ES: ES reports `roles: []` for an API-key call
and expects the client to read `GET /_security/api_key` for the descriptors.
xerj reports the names instead, because it has no named-role store to point at.

Because those names are caller-chosen free text, `superuser` and `unscoped` —
the two labels xerj assigns on its own authority — are **not mintable from a
`role_descriptors` key**. A descriptor named `superuser` is reported as
`api_key_role:superuser` (`es_compat.rs`, `reported_role_label`), so a key
confined to `logs-*` by that descriptor cannot be handed back a `roles` array
that reads as the superuser. Without that guard the divergence would re-open
the exact drift this endpoint was fixed for: one `POST /_security/api_key` with
the right descriptor name and `_authenticate` says `{"roles":["superuser"]}`
for a read-only key. With it, the divergence is additive and no key is
described as holding more than it holds. The guarantee is specifically that no
caller-chosen name yields `superuser` or `unscoped`; the qualified form is not
itself reserved, so a descriptor literally named `api_key_role:superuser` comes
back verbatim — that collides with another caller's name, not with a label xerj
assigns.

`POST /_security/user/_has_privileges` is **not** fixed: it still answers `true`
to every privilege named in the request, which is wrong for a scoped key. Treat
it as a stub, not as an authorization oracle.

Neither are the Kibana user-profile routes (`POST /_security/profile/_activate`,
`GET /_security/profile/{uid}`): they return one fixed built-in profile whose
`user.roles` is `["superuser"]` for every caller, because xerj has a single
owner identity and these exist to get Kibana's bootstrap past a 404. They
describe that owner, not the credential that called them. `_authenticate` is
the endpoint that answers "who am I".

### Privileges that exist but are not decided

`Privilege` (`rbac.rs:34-49`) has seven variants. Only three of them are ever
demanded of a principal: `ReadIndex`, `WriteIndex`, and `AdminIndex`.
`SnapshotCreate`, `SnapshotRestore`, `SecurityAdmin`, and `AuditRead` appear
only as labels in a `403` message and in a sort key
(`authz.rs:242-251`, `authz.rs:972-982`, `es_compat.rs:25522`). Nothing checks
whether a principal holds them, because `role_descriptors` cannot grant them.

### The named role store is data only

`PUT /_security/role/{name}` stores a role and enforces nothing. Every response
from these handlers carries `"enforced": false` plus a warning string
(`xerj-api/src/native.rs:900-949`; routes at `router.rs:127-133`), and the PUT
logs a warning. `RoleStore`'s seeded roles (`admin`, `write`, `read`,
`read_only_index`, `snapshot_admin`, `auditor`, `rbac.rs:238-278`) are likewise
inert. The same is true of the application-privilege store behind
`/_security/privilege` (`es_compat.rs:25628-25631`).

Broad RBAC over the general ES-compatible surface is not implemented. What is
enforced is the reserved namespace and the confinement of a `Scoped` key to its
named indices (`authz.rs:94-101`).

## Snapshot and restore

Snapshot and restore are the one place where a wildcard is decided by the
middleware instead of being expanded by the engine. `Engine::create_snapshot`
walks the index map itself, and `Engine::restore_snapshot` expands against the
snapshot's own manifest and then rewrites the index directory, so neither passes
the `index_guard` funnel (`expansion_for` and its rationale,
`authz.rs:1141-1153`).

The rules, from `decide` (`authz.rs:1329-1361`):

- A snapshot or restore that names **no** indices covers every index on the
  node, brains included, so it is superuser-only. For a non-superuser it is a
  `403` naming `<all indices>`.
- A restore upgrades every named target's demand to write, because a restore
  overwrites what it names.
- A pattern that `may_reach_reserved` requires an explicit grant covering that
  pattern; a superuser passes, a `Scoped` key must hold the pattern itself, and
  anything else is refused (`authorize_expression`, `authz.rs:1175-1193`).
  Narrower patterns such as `logs-*` are unaffected, so ordinary per-tenant
  backups keep working.

Routes: `PUT /_snapshot/{repo}/{snapshot}` and
`POST /_snapshot/{repo}/{snapshot}/_restore` (`router.rs:485-496`).

## The Console rate limiter keys on the socket peer

The Console's auth endpoints charge a per-IP bucket before doing work
(`xerj-console-api/src/auth/rate_limit.rs`):

- 10 hits per minute and 100 per hour, per `(ip, endpoint)` pair
  (`PER_MINUTE`, `PER_HOUR`, `rate_limit.rs:22-25`).
- Over the limit returns `429` with no body, so a caller cannot learn whether
  the email or token was valid (`rate_limit.rs:8-9`).
- Charged on exactly three endpoints today:
  `/_xerj-console/api/v1/auth/login/begin`, `.../login/finish`, and
  `.../magic/redeem` (`auth/login.rs:54`, `auth/login.rs:126`,
  `auth/magic.rs:203`).

The `ip` in the key comes from the `ClientIp` extractor
(`xerj-console-api/src/client_ip.rs`), which reads the TCP peer out of
`ConnectInfo<SocketAddr>`. Forwarding headers are consulted only when the socket
peer itself matches `server.trusted_proxies`, which is **empty by default**, so
an unconfigured node believes nobody (`client_ip.rs:53-107`;
`server.trusted_proxies` at `xerj-common/src/config.rs:294-311`, defaulting to
an empty list at `config.rs:322`). When the peer is a declared proxy, the
`X-Forwarded-For` chain is read right to left, skipping entries that are
themselves declared proxies; a malformed element stops the walk and the peer is
used instead. Failing that, a single-valued `X-Real-IP` is honoured.

If no peer address is available at all, the key is the literal string
`"unknown"` (`UNKNOWN_IP`, `client_ip.rs:45`), which throttles rather than
exempts.

**Operational consequence.** If you terminate TLS or load-balance in front of
XERJ and do not list the proxy in `server.trusted_proxies`, the peer address is
the proxy's for every request, so every one of your users shares a single bucket
of 10 login attempts per minute. Set `server.trusted_proxies` to the addresses
or CIDR blocks of proxies you operate, and only those: anything listed there can
claim to be any client, and so can move its own bucket. Invalid entries fail
startup rather than silently widening or narrowing trust
(`config.rs:143-144`).

Note that `ClientIp` is also used for audit fields on endpoints that are not
rate limited, for example passkey enrolment (`auth/passkey.rs:168`). Only the
three endpoints listed above charge a bucket.

## Reach: what a fresh node exposes (issue #228)

`server.bind_address` defaults to **`127.0.0.1`**. A node you start without
configuring anything is reachable from the machine it runs on and from nowhere
else.

That default is chosen against the one that used to ship, `0.0.0.0`. Auth is on
by default and the admin key is generated 0600 on first start, which is the
part that is genuinely good — but TLS is *off* by default, so binding every
interface meant the admin API key travelled in an `Authorization` header over
plain HTTP to anything that could route to the host. Nothing about the
experience said so: `curl` worked, the health endpoint answered, and the key
was accepted. `user-feedback/09-security/insecure-defaults.md` collects the
field reports this is aimed at — a list of eight-and-nine-figure record
exposures whose common factor is a search node reachable from the internet in
whatever state it shipped in. A shipped default is the configuration most
installs will ever run.

### Exposing the node is a two-part statement

Set `server.bind_address` (or `--bind` / `XERJ_BIND_ADDRESS`) to `0.0.0.0` or a
specific address. With `tls.enabled = false`, startup then **refuses** unless
`server.allow_insecure_network_bind = true` (env:
`XERJ_ALLOW_INSECURE_NETWORK_BIND`).

`Config::cleartext_exposed_off_loopback` (`config.rs`) is true when TLS is off
**and** the bind address is not loopback **and** the opt-out is unset; `main.rs`
step 3c aborts non-zero before the data directory is created, before a first-run
admin key is minted and printed, and before any listener exists.

```
Error: server.bind_address = "0.0.0.0" is not loopback and tls.enabled = false,
so every listener (8080, 9200, 8081) would serve plain HTTP on a
network-reachable interface — the API key in every Authorization header, and
every document body, would cross the network in cleartext. Refusing to start.
…declare it: server.allow_insecure_network_bind = true (env:
XERJ_ALLOW_INSECURE_NETWORK_BIND=true).
```

Scope of the refusal, all pinned by tests:

- **`0.0.0.0` and `::` count as exposed**, not as "unset" — they bind every
  interface the host has.
- **Link-local counts as exposed too** — still reachable by every other host on
  the link.
- **Loopback binds are untouched**, so local development, the quickstarts and
  the ES-YAML conformance harness keep working with TLS off.
- **`--insecure` does not evade it.** It clears `tls.enabled`, so it trips the
  check like any other cleartext configuration — and it drops auth as well, so
  the configuration it would otherwise produce is an unauthenticated node on
  every interface.
- **The opt-out relaxes only this check.** With TLS on it is not consulted at
  all; the residual gRPC h2c exposure stays governed by
  `tls.allow_insecure_grpc_h2c` (#229).
- **The bind address must be an IP literal.** Host names are not resolved, and
  a node given one is refused at `main.rs` step 3a — *before* either exposure
  check runs, so neither can describe an address it could not parse. Without
  that ordering, `bind_address = "localhost"` was refused by 3c with a message
  claiming localhost "is not loopback", and the opt-out that message names
  merely deferred the same failure past the data directory and a printed
  first-run admin key. Resolution is deliberately not attempted: a name maps to
  different addresses on different hosts and at different times, so "which
  interfaces did this node just publish itself on?" would stop having a fixed
  answer.

```
Error: server.bind_address = "localhost" is not an IP address — it must be an
IPv4 or IPv6 literal such as "127.0.0.1" (the default), "0.0.0.0" or "::1".
Host names are not resolved.
```

The escape hatch does not make anything safe. It records that you know, and it
is what you set when a reverse proxy, sidecar, mesh or ingress terminates TLS in
front of the node — or when the boundary is a container's network namespace,
which is why the shipped Docker image and Helm chart set it. The startup banner
then names the exposure on every boot, and every listener line carries the bind
address so a loopback node and a world-facing one no longer look identical.

## Transport encryption, listener by listener

`tls.enabled` does not mean "the node is encrypted". It covers two of the three
data-plane listeners, and the third is cleartext by construction.

| listener | default port | with `tls.enabled = true` |
|---|---|---|
| Native REST | 8080 | TLS (in-process rustls) |
| ES-compat | 9200 | TLS (in-process rustls) |
| gRPC `XerjSearch` | 8081 | **cleartext h2c — never TLS** |
| Cluster control | 9300 | cleartext, authenticated only (see below) |

REST and ES-compat are wrapped by `axum_server::bind_rustls`, which handshakes
every accepted connection (`main.rs:788-810`). The gRPC listener is served by
`tonic::transport::Server` with no TLS configuration at all
(`grpc.rs:377-392`): tonic is deliberately built without its `tls` feature, so
that a second crypto backend is not pulled in beside axum-server's `ring`. The
consequence is that `tls.enabled` has no effect whatsoever on `:8081`.

Auth is not the gap here — every RPC goes through the same API-key check as the
HTTP surfaces (`GrpcAuth`, `grpc.rs:340-367`), so the port is not an open door.
Confidentiality is the gap: the credential itself, and every document body,
cross the wire in the clear.

### The startup check (issue #229)

Left alone, this is a silent mismatch. gRPC clients keep working whether or not
TLS is on, so an operator who enabled TLS and expected three encrypted
listeners gets no error, no failed connection, and no symptom of any kind.

So startup refuses the dangerous combination. `Config::grpc_h2c_exposed_off_loopback`
(`config.rs`) is true when TLS is enabled **and** `server.bind_address` is not
loopback **and** `tls.allow_insecure_grpc_h2c` is unset; `main.rs` step 5b then
aborts non-zero before binding anything or writing a certificate. Loopback
binds are unaffected, and `--insecure` clears `tls.enabled` so it never trips.

`0.0.0.0` and `::` count as exposed, not as "unset" — they bind every interface
the host has. (They were also the shipped default until #228 made it loopback;
either way this check fires on what the config says, not on what it omits.)
Link-local addresses count as exposed too; they are reachable by every other
host on the link. Both
choices are pinned by tests in `xerj-common/src/config.rs`, and the refusal
itself by `xerj-server/tests/grpc_h2c_fail_closed.rs`.

The escape hatch, `tls.allow_insecure_grpc_h2c = true`, does not make anything
safe — it records that you know, and it is what you set when a sidecar, mesh
or reverse proxy terminates TLS for `:8081` on your behalf. The startup banner
keeps naming the uncovered listener on every boot.

Wiring tonic's own `tls` feature would close this properly. It is not done: it
means a second TLS stack in the binary beside `axum-server` + `ring`, and that
trade has not been made. Until it is, treat `:8081` as a plaintext port.

## Cluster control-frame authentication

Cluster mode is off by default (`ClusterConfig::default`,
`config.rs:1195-1205`). When it is on, the control transport on `cluster.port`
(default 9300) authenticates every frame.

### Fails closed without a secret

Confirmed in two independent places:

1. `ClusterConfig::validate` (`config.rs:1237-1260`) returns a configuration
   error when `enabled = true` and neither `cluster.auth_secret` nor
   `XERJ_CLUSTER_AUTH_SECRET` supplies a secret.
2. Startup refuses again before the transport is built
   (`xerj-server/src/main.rs:1621-1636`), with a comment stating that this must
   abort startup and is deliberately not folded into the degraded single-node
   fallback that covers bind failures.

There is no unauthenticated cluster mode to fall back to: `ClusterSecret` is the
only source of frame tags and has no "no secret" variant
(`xerj-cluster/src/auth.rs:84-92`).

### Minimum secret length: 16 characters

Also confirmed in two places, which agree:

- `xerj_cluster::auth::MIN_SECRET_LEN = 16`, enforced by `ClusterSecret::new`
  after trimming surrounding whitespace (`auth.rs:55`, `auth.rs:108-121`).
- `ClusterConfig::MIN_AUTH_SECRET_LEN = 16`, enforced by config validation
  (`config.rs:1211-1215`, `config.rs:1250-1257`).

The transport enforces the floor independently of the config, so a mismatch
fails closed. Precedence: a non-empty `cluster.auth_secret` wins over the
environment variable, so a stray env var cannot silently re-key a cluster
(`resolve_auth_secret`, `config.rs:1228-1235`). Every node must be configured
with the same secret. The config documentation suggests generating one with
`openssl rand -hex 32`.

### What the tags bind

Each frame carries an HMAC-SHA256 tag over a domain-separated binding of the
protocol context (`hello` versus `frame`), the wire version byte, a 32-byte
random challenge the *receiver* generates per accepted connection, the sender's
node id, the frame's zero-based position in the connection, and the payload
(`auth.rs:125-154`). The sequence number is never transmitted: both ends count
independently, so a replayed, dropped, or reordered frame lands on the wrong
sequence and fails. Fields are length-prefixed so concatenations cannot collide.
Tags are compared in constant time after a length check (`tags_match`,
`auth.rs:178-183`). `ClusterSecret`'s `Debug` never renders key material
(`auth.rs:94-99`).

### What it does not give you

Stated in the module's own words (`auth.rs:21-29`):

- **No confidentiality.** Frames are plaintext JSON on the wire. The HMAC
  authenticates, it does not encrypt. Cluster traffic still belongs on a trusted
  network segment.
- **No per-node identity.** The secret is cluster-wide, so it proves that the
  peer knows the cluster secret, not that the peer is a particular node. A
  compromised member can impersonate any other member. mTLS is not implemented.

The wire format changed with this authentication work and does not interoperate
with 1.0.0-rc.9 or earlier in either direction, so upgrading a cluster requires
stopping every node rather than a rolling restart (`CHANGELOG.md`, 1.0.0-rc.10,
"Breaking").

## Worked example

Start a node with auth enabled (the default) and take the admin key it writes to
`<data_dir>/admin.key`. Mint a key scoped to one brain. Only the admin key can
do this:

```bash
curl -s -X POST 'http://localhost:9200/_security/api_key' \
  -H "Authorization: ApiKey $(cat ./data/admin.key)" \
  -H 'Content-Type: application/json' \
  -d '{
        "name": "alice-agent",
        "role_descriptors": {
          "alice-brain": {
            "indices": [
              {
                "names": [".xerj-memory-alice", ".xerj-memory-alice-edges"],
                "privileges": ["read", "write"]
              }
            ]
          }
        }
      }'
```

The response carries `id`, `name`, `expiration`, `api_key` and `encoded`
(`es_compat.rs:25600-25606`). `encoded` is `base64("id:api_key")` and is what
you present:

```bash
KEY='<the encoded value from the response>'

# Alice's own memory namespace: allowed by the grant above.
curl -s -H "Authorization: ApiKey $KEY" 'http://localhost:9200/_memory/alice'

# Another tenant's brain: refused, without revealing whether it exists.
curl -s -H "Authorization: ApiKey $KEY" 'http://localhost:9200/_graph/bob/overview'
```

The second call is classified as a brain read, resolved to
`.xerj-memory-bob-edges`, and denied with:

```json
{
  "error": {
    "root_cause": [
      {
        "type": "security_exception",
        "reason": "action [read] is unauthorized for this credential on [.xerj-memory-bob-edges]: mint a key whose role_descriptors grant [read] on that index (POST /_security/api_key), or use the configured admin key"
      }
    ],
    "type": "security_exception",
    "reason": "action [read] is unauthorized for this credential on [.xerj-memory-bob-edges]: mint a key whose role_descriptors grant [read] on that index (POST /_security/api_key), or use the configured admin key"
  },
  "status": 403
}
```

The reason string is built in `forbidden` (`authz.rs:242-264`) and the JSON
shape in `error.rs:52-74`. Enumeration behaves the same way: `GET /_cat/indices`
with this key lists only the indices it may see, because `Engine::list_indices`
filters through the visibility guard.

## Known limits

Everything here is a limit of the current code, verified in the source. None of
it is on a schedule this document can promise.

- **No general RBAC.** An `Unscoped` key keeps superuser-equivalent reach over
  ordinary (non-reserved) indices. The reserved namespace is a real boundary and
  a `Scoped` key is confined to its grants; nothing else is
  (`authz.rs:94-101`).
- **No field-level or document-level security.** `field_security` and `query`
  in a `role_descriptors` entry are parsed and discarded (`rbac.rs:152-155`).
- **The named role store and application privileges are inert.**
  `/_security/role*` and `/_security/privilege*` store data and gate nothing;
  the role responses say so with `"enforced": false`
  (`native.rs:900-949`, `es_compat.rs:25628-25631`).
- **Three of the seven privileges are never checked.** `SnapshotCreate`,
  `SnapshotRestore` and `SecurityAdmin` exist only as error labels
  (`authz.rs:242-251`). `AuditRead` is enforced on `/_audit/*` as of #329.
- **`read_audit` is granted as an index privilege, not a cluster one.** ES
  spells it `cluster: ["read_audit"]`; xerj parses it out of an `indices` entry
  (`rbac.rs::es_index_privilege`), because that parser is the only path from a
  wire request to a `Privilege` — the `cluster` block is still ignored. A grant
  therefore needs `"names": ["*"]` or `["_audit"]` to reach the endpoint.
- **`/_audit/*` is readable by any `Unscoped` key.** The gate is
  `Principal::allows_index("_audit", AuditRead)`, and `Unscoped` answers `true`
  for every non-reserved name (`authz.rs:94-101`) — the same
  superuser-equivalent reach it keeps everywhere else, kept here so an operator
  dashboard scraping `/_audit` with a minted key does not break. A `Scoped` key
  needs a grant that carries `read_audit`.
- **The audit log is a rolling window, not an archive.** It is a hash-chained,
  restart-surviving log (`audit.rs`, issues #201 and #329) of the last
  `audit.capacity` entries (4096 by default) in `<data_dir>/audit.jsonl`.
  Coverage is now every data-changing request, every refused request, searches,
  and the three `_security/api_key` operations — but at **one entry per
  request**, so a bulk of 10 000 documents is one entry naming the batch size
  and the indices touched, not 10 000 entries. A node ingesting single-document
  writes fills 4096 entries in seconds; a deployment that must keep audit
  history raises `audit.capacity` and ships the file off the node. Requests
  refused at *authentication* (401) leave no entry, deliberately — otherwise
  anyone who can reach the port could push real evidence out of the ring. The
  file is not `fsync`ed per entry unless `audit.sync_every` is set; by default
  it survives a process restart, not a power cut. Anyone who can write the file
  can rewrite the whole chain, seed line included — tamper-*evidence* requires
  pinning a known-good head externally.
- **Only the two HTTP routers are audited.** The write hook lives in
  `auth::auth_middleware`, so anything that reaches the engine without passing
  through it leaves no entry: the gRPC listener (`grpc.rs`, which shares
  `is_authorized` but not the hook), the Console API peer router (which keeps
  its own separate `.xerj_audit` login trail), `xerj index` and any embedder
  using `Engine` directly. An absent entry is still not proof a write did not
  happen — it is proof no *audited HTTP request* made it.
- **`names: ["*"]` reaches the reserved namespace.** This is intended and pinned
  by a test, but it means one careless grant hands over every brain
  (`rbac.rs:72-98`).
- **A surface mounted outside `authz_middleware` is unprotected by default**,
  because an absent guard allows. The Console proxy and the gRPC listener each
  carry their own check; a new surface would need its own
  (`data_sources.rs:37-48`, `grpc.rs:62-88`).
- **Detached work must carry the rule by hand.** `tokio::spawn` and
  `spawn_blocking` do not inherit the guard, and rayon workers never see it. The
  audit of every spawn site in the workspace is in `index_guard.rs:59-85`; a new
  spawn that can resolve an index name must use `current()` and `scoped_opt`.
- **Cluster traffic is unencrypted and membership-authenticated only.** No
  confidentiality, no per-node identity, no mTLS (`auth.rs:21-29`).
- **TLS is off by default.** `TlsConfig` derives `Default`, so `tls.enabled` is
  `false`, and `--insecure` disables both TLS and auth. The startup banner
  prints the posture on every start. The mitigation is reach, not encryption:
  the default bind is loopback and exposing the node in cleartext has to be
  declared — see [Reach](#reach-what-a-fresh-node-exposes-issue-228).
- **A declared cleartext exposure is still cleartext.** Nothing about
  `server.allow_insecure_network_bind = true` encrypts anything; it only means
  the operator said so. Every deployment that publishes a port — including the
  shipped Docker image and Helm chart — needs TLS terminated in front of it.
- **The gRPC listener is never TLS.** `tls.enabled` covers REST and ES-compat
  only; `:8081` is cleartext h2c in every configuration (`grpc.rs:377-392`).
  Startup refuses the combination "TLS on + non-loopback bind" rather than let
  that pass unnoticed — see
  [Transport encryption](#transport-encryption-listener-by-listener).
- **No encryption at rest at the engine level.** The startup banner says to use
  OS full-disk encryption or bucket-side encryption instead (`main.rs:318`).
- **Rate limiting covers three Console auth endpoints only.** There is no
  general per-IP request throttle on the data plane (`rate_limit.rs`, and the
  three call sites listed above).
