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

All secret comparisons are constant-time after a length check
(`constant_time_eq`, `auth.rs:364`), on both the admin-key and minted-key paths.

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
- **Four of the seven privileges are never checked.** `SnapshotCreate`,
  `SnapshotRestore`, `SecurityAdmin` and `AuditRead` exist only as error labels
  (`authz.rs:242-251`).
- **The audit endpoints are not privilege-gated.** `/_audit/_search` and
  `/_audit/_verify` (`router.rs:124-125`) are cluster-classified reads, so any
  authenticated principal that reaches the router passes; `AuditRead` is not
  consulted. The startup banner also states plainly that auditing today is
  request tracing, not a tamper-evident log (`main.rs:317`).
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
  `false` (`config.rs:492-501`), and `--insecure` disables both TLS and auth
  (`main.rs:345-349`). The startup banner prints the posture on every start
  (`main.rs:302-319`).
- **No encryption at rest at the engine level.** The startup banner says to use
  OS full-disk encryption or bucket-side encryption instead (`main.rs:318`).
- **Rate limiting covers three Console auth endpoints only.** There is no
  general per-IP request throttle on the data plane (`rate_limit.rs`, and the
  three call sites listed above).
