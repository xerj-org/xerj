# Cyber scope guard (#2767, #2769)

`xerj_common::cyber_scope` provides a shared, network-free authorization
primitive for future cyber planning recipes. It is not yet a registered
recipe, MCP tool, scanner, or authorization-signature verifier.

## Trust boundary

The integration must authenticate a work order through its control plane
before calling `ScopeGuard::from_work_order`. Required fields are `id`,
`authorization` (a nonblank statement), `targets`, `not_before`, and
`expires_at` (UTC timestamps). Deserialization rejects unknown fields; the
validated guard itself cannot be deserialized or mutated through its API.
This validates a declaration, not whether the declaring person legally
controls the targets.

`InputOrigin` is assigned by the trusted transport layer, not inferred from
target text or accepted from a repository JSON field. Both construction of a
scope and authorization of a candidate reject `UntrustedRepository`,
including an otherwise in-scope hostname. A repository README, source comment,
or finding cannot expand a scope, replace its dates, or supply authorization.
Promoting a suggested target requires an explicit control-plane decision
and a new check. Relabeling untrusted data as `AuthorizedControlPlane`
defeats this boundary; integrations must not do that.

## Matching and time policy

- DNS names are ASCII LDH labels, lowercased, with one optional terminal root
  dot removed. Names match exactly, not by suffix. ASCII punycode spelling
  is accepted as an exact name; Unicode/IDNA conversion is not performed.
- IP literals use strict standard-library parsing. IPv4-mapped IPv6 becomes
  IPv4. Numeric/hexadecimal pseudo-host spellings such as `127.1` and
  `0x7f000001` are refused, not treated as DNS.
- CIDRs mask host bits before matching and echo the normalized network.
  Prefixes must fit their address family. Mapped IPv6 networks require
  prefixes of at least 96 bits and become IPv4 networks. An explicit `/0`
  authorizes that address family; it must only be supplied intentionally.
- A request is a bare hostname or IP, never a CIDR, URL, userinfo, wildcard,
  path, port, percent-encoded name, or whitespace-containing string.
- DNS names are not resolved and do not inherit an IP/CIDR permission.
  Conversely, authorizing a DNS name does not authorize an arbitrary resolved
  IP. This guard does not prevent DNS rebinding in a future network executor.
- Each `authorize` call checks the inclusive interval
  `[not_before, expires_at]` against the supplied trusted clock. A reversed
  interval is rejected at construction. An expired guard cannot keep issuing
  permits just because an earlier request passed.

## Results and integration obligations

Missing scope, missing authorization, tainted input, invalid targets, requests
before/after the window, and out-of-scope targets return typed `ScopeError`
refusals. Error messages do not echo attacker-controlled text.
`ScopePermit` serializes the normalized target, check timestamp, and complete
declared scope, including its authorization statement. Use `guard.scope()` to
echo the same trusted scope when reporting a refusal.

A permit is an analysis/planning result, not a network execution capability.
Every independently selected target or redirect destination needs a fresh
check; no previous permit or repository instruction extends the scope.
Ports, URL schemes, credentials, DNS resolution, redirect following, signed
work-order verification, and recipe-runner integration are deliberately
outside this module. Xerj sends no attack traffic here.

## Deterministic validation

The module's unit tests cover missing/out-of-scope refusal, exact hostname
matching, malformed and ambiguous targets, masked IPv4/IPv6 CIDRs, mapped
addresses, inclusive date boundaries and expiry on reuse, and repository
prompt-injection payloads that attempt to smuggle a target or authority.
They use fixed timestamps and in-memory inputs, with no network or filesystem
effects. The focused command is
`cargo test -p xerj-common --lib cyber_scope --quiet` from `engine`.
