# Findings — five-stage instruction/data pass

Eight findings survived adversarial verification (each proposed by a lens agent,
then attacked by a separate verifier whose default was "this is wrong"; most were
confirmed by exploiting a live server). Ranked by verifier-corrected severity.

**Reporting note (honesty).** The workflow's automatic synthesis step wrote up
only the first two of these and reported "2 confirmed, 1 refuted." That was an
under-count: the per-finding verifiers marked **8** findings `refuted=false`. The
discrepancy was caught by reconciling the synthesis against the raw verifier
verdicts, and the four HIGH findings were then re-verified by hand against current
code before publication. This document is the reconciled, complete set.

**Shared reachability qualifier.** XERJ has no enforced role model — every minted
key authenticates as the single superuser (`auth.rs:182`; `role_descriptors` are
accepted but not enforced). And `--insecure` or an empty `admin_api_key`
(`auth.rs:96`) disables auth entirely, which promotes every "authenticated"
finding below to **unauthenticated**.

| ID | Severity | Reachability | Status |
|---|---|---|---|
| INJ-01 | High | authenticated (app-mediated) | **fixed** `58fc73f` |
| F-PATH-02 | High | authenticated (unauth under `--insecure`) | **fixed** `58fc73f` |
| S5-1 | High | authenticated (valid session cookie) | **fixed** `58fc73f` |
| DESER-EGRESS-01 | High | unauth on the cluster port (operator-gated by `cluster.enabled`) | tracked (Raft-auth Phase-2) |
| S5-4 | Medium | unauthenticated | open |
| S5-3 | Medium | unauthenticated (needs a valid magic-link token) | open |
| S5-5 | Low | authenticated (superuser-equivalent) | open |
| AUTHZ-2 | Info | unauthenticated | open |

---

## INJ-01 — Search-template params spliced unescaped into the query, then re-parsed  ·  High  ·  FIXED

`crates/xerj-api/src/es_compat.rs:23160` (`render_template`).

**Instruction/data.** The template is the instruction channel (JSON that defines
the query structure); the params are the data channel. `render_template` merged
them with a raw `String::replace` and **zero escaping**, then
`serde_json::from_str` re-parsed the merged string — so a param value carrying
JSON tokens (`"`, `,`, an extra `"query"` key) was interpreted as query
*structure*. A param in a numeric position (`"size": {{size}}`) needed no quote
break-out at all.

**Taint path.** `POST /:index/_search/template` → `search_template` (`Json<SearchTemplateBody>`)
→ `render_template(&tmpl, &params)` → `serde_json::from_str(&rendered)` →
`parse_request` → `idx.search()`. Same sink via `POST /_render/template` and
`POST /_msearch/template`.

**Attacker input / live proof.** Template encodes an ACL filter and takes an
untrusted `size`: `{"query":{"bool":{"filter":[{"term":{"visibility":"public"}}]}},"size":{{size}}}`.
Params `{"size":"50,\"query\":{\"match_all\":{}}"}`. On a throwaway server the
benign `size:50` returned only the public doc; the injected value returned **both
docs including the private `{"secret":"CEO-comp-4.2M"}`** — the visibility filter
was dropped (serde keeps the last of duplicate `query` keys).

**Fix (`58fc73f`).** String replacements are JSON-escaped (the escaped inner
content is emitted), so a value is always one JSON token: transparent inside a
string literal, and in a bare value position a malicious value becomes an invalid
`\"` sequence that fails the re-parse. **Re-proven:** the same payload now returns
`template rendered to invalid JSON` and leaks nothing; 4 regression tests.

---

## F-PATH-02 — Snapshot repository `settings.location` is an unvalidated root  ·  High  ·  FIXED

`crates/xerj-engine/src/engine.rs:1562` (`create_snapshot` / `restore_snapshot` →
`validate_snapshot_path`).

**Instruction/data.** The three filters guarding the snapshot *name* are real and
well-written — they prove the snapshot stays inside its repository. But the
*repository location* itself is an absolute path the client supplies via
`PUT /_snapshot/{repo}` and none of the name-filters look at it. The containment
check is vacuous when the root it contains against is attacker-chosen.

**Attacker input / live proof.** `PUT /_snapshot/evil {"type":"fs","settings":{"location":"/tmp/EVIL"}}`
then `PUT /_snapshot/evil/s1` → previously `state: SUCCESS`, and every index's
segment files (postings for a field literally named `secret`) were copied to
`/tmp/EVIL`, outside `data_dir`. `restore_snapshot` is the mirror: read
attacker-staged files from anywhere into an index.

**Fix (`58fc73f`).** New `limits.snapshot_repo_allowlist` (ES `path.repo`
equivalent; default empty = only `data_dir`). `validate_snapshot_path` rejects a
location that canonicalizes outside `data_dir` or the allowlist. **Re-proven:** the
external location is now refused (`location … is outside data_dir`); an
in-`data_dir` repo still snapshots fine; 4 regression tests. This is the item PR
#69 explicitly deferred ("no `path.repo` equivalent").

---

## S5-1 — Session-revocation lost update (best-effort `last_seen` bump clobbers a revoke)  ·  High  ·  FIXED

`crates/xerj-console-api/src/auth/sessions.rs:186`.

**Instruction/data (state machine).** The *check* (session is valid) is the
instruction; the *session record* is the data. The auth extractor read the
session at the top of the request, then at the end blind-wrote a clone with
`last_seen` bumped. A `revoke_session()` that landed in between was overwritten by
the stale copy (`revoked_at: None`) — the record was resurrected and the
revocation silently undone. `put_session` is delete-then-create with no CAS, so
the stale write wins.

**Reachability.** Any session-protected console endpoint (the router is mounted
unconditionally on both listeners) triggers the bump; the attacker needs a valid —
e.g. stolen — cookie and a concurrent admin revoke. Verifier reproduced it with an
integration test against a real engine.

**Fix (`58fc73f`).** `bump_last_seen()` re-reads immediately before writing and
skips entirely if the session is now gone or revoked, writing only the fresh
record — a revocation can no longer be undone. 2 regression tests
(`bump_does_not_resurrect_a_revoked_session`, `bump_refreshes_a_live_session`).

---

## DESER-EGRESS-01 — Unauthenticated Raft control messages on the cluster port  ·  High  ·  tracked

`crates/xerj-cluster/src/transport.rs:59` (`read_frame` → `handle_connection`).

**Instruction/data.** The bytes off a raw TCP socket *are* the control
instruction: `serde_json::from_slice` reconstructs a `RaftMessage` whose variant
tag (`AppendEntries`/`RequestVote`/…) and fields become consensus instructions,
with **no authentication of any kind** before or after the parse. (There is a
10 MiB frame cap, so this is not an unbounded-alloc DoS, and serde_json is not a
gadget-chain deserializer — the risk is forged consensus, not RCE.)

**Attacker input.** One TCP connection to the cluster port (default `9300`):
`[len][node_id]` then `[len]{"RequestVote":{"term":999999999,"candidate_id":"attacker",…}}`
— a term-bump can disrupt/steer the cluster's consensus.

**Reachability.** Operator-gated: only active when `cluster.enabled=true`; then the
port binds `0.0.0.0` and trusts any peer. This is exactly PR #69's deferred
"cluster Raft auth" item.

**Status.** Not fixed in `58fc73f` — the fix (a shared cluster secret / per-frame
HMAC, or mTLS peer-cert pinning before deserialize) is a design change tracked as
the Raft-auth Phase-2 item. Source-verified, not exploited on a live cluster.

---

## S5-4 — `x-forwarded-for` trusted as identity for rate-limiting and audit  ·  Medium  ·  fixed

`crates/xerj-console-api/src/auth/magic.rs:360`, `rate_limit.rs:58`.

**Instruction/data.** An attacker-controlled header is consumed as an identity
instruction with no trust boundary: it is interpolated into the rate-limit bucket
key (`format!("{ip}:{endpoint}:m")`) and recorded as the audit-log source IP.

**Attacker input / live proof.** `POST …/auth/magic/redeem` with a rotating
`x-forwarded-for: 1.2.3.$RANDOM` resets the quota every request (brute-force the
magic-link/bootstrap token unthrottled) and writes attacker-chosen source IPs into
the audit log. Confirmed unauthenticated (the endpoint is registered
"unauthenticated" and the handler body runs for anonymous callers).

**Fix.** Derive the caller address from `ConnectInfo<SocketAddr>` (the transport),
and consult `x-forwarded-for` only when the socket peer is a trusted proxy in an
operator-configured CIDR.

**Status.** Fixed. `ConnectInfo<SocketAddr>` is installed on both the plain and
the TLS listener (`into_make_service_with_connect_info`), and handlers read the
caller address through a `ClientIp` extractor
(`crates/xerj-console-api/src/client_ip.rs`). Forwarding headers are consulted
only when the socket peer matches `server.trusted_proxies` — a new setting that
takes addresses/CIDRs and is **empty by default**, so an unconfigured node
believes nobody. The chain is read right-to-left past our own proxies; the
caller-authored left end is never used, and a malformed element stops the walk.
Covered by `tests/trusted_proxy_client_identity.rs` (spoof from an untrusted
peer changes nothing; a declared proxy's forwarded address is honoured) plus
listener-level tests that the real peer reaches handlers over HTTP and HTTPS.

---

## S5-3 — Magic-link single-use / bootstrap-claim is TOCTOU  ·  Medium  ·  open

`crates/xerj-console-api/src/auth/magic.rs:221`.

**Instruction/data (state machine).** "This link may be consumed at most once" is
checked against state read at `magic.rs:210` and only committed at `:299`, several
awaits later, with no exclusive lock — a concurrent request re-reads the un-consumed
state in the window. Two redeems of the same token in one TCP burst can both
succeed; on the bootstrap-claim path that can mean two admin claims. The per-IP
limiter that would cap parallelism is itself bypassable (see S5-4).

**Fix.** Make consumption an atomic state transition performed first, under a
per-token exclusive lock, before any purpose-specific work.

---

## S5-5 — `max_fields_per_index` bypassed on the explicit-mapping path  ·  Low  ·  open

`crates/xerj-engine/src/index.rs:14198` (`Index::add_fields`).

**Instruction/data (missing-guard).** This is the "where is PR #69's pattern
missing" result. #69 added the field-cap guard (and a re-check under the write
lock) to both *dynamic-mapping* paths, but the *explicit* path —
`PUT /:index/_mapping` and `POST /v1/schema/:name/evolve` → `add_fields` — never
checks the cap. Send the fields explicitly instead of letting them be inferred and
the mapping-explosion limit does not apply.

**Severity.** Low: reachable only by a superuser-equivalent credential that already
holds destructive powers (delete index, unbounded bulk). It is an operator-limit
bypass / ES-compat divergence, not a privilege escalation.

**Fix.** Move the invariant to the mutation point — make `add_field`/`ManagedSchema`
enforce the limit the way `apply_document(value, limit)` already does.

---

## AUTHZ-2 — `/_xerj-console/api/v1/cluster/info` unauthenticated disclosure  ·  Info  ·  open

`crates/xerj-console-api/src/router.rs:49` (`cluster::info`).

**Privilege-map gap** (not an instruction/data bug). `cluster::info` takes only
`State`, no `AuthSession`, so unlike every other console endpoint it applies no
auth check — and the console router is merged onto both listeners. Live, with
`auth.enabled=true` and a non-empty admin key, `curl http://host:9200/_xerj-console/api/v1/cluster/info`
returns `node_id`, exact `version` (`1.0.0-rc.6`), and uptime with no credential.

**Fix.** Drop `version`/`node_id` from the pre-auth response and gate the full
payload behind `AuthSession`, or accept and document it as an intentional pre-auth
health probe.

---

## Refuted (the load-bearing number)

Eight proposed findings did **not** survive verification — a 50% cull. The most
instructive was a path-traversal candidate (`F-PATH-01`) where every code
observation the finder made was correct and the finding was still wrong: the
guard it claimed was missing lived in the caller. A finder without an independent,
refutation-first verifier would have published it. That is why every number here
comes from the surviving half, and every HIGH was additionally re-verified by hand.
