# v0.9.0 — "Security + compliance" (initial)

**Released:** 2026-04-26
**Theme:** make xerj procurement-ready for regulated industries
(FinServ, Healthcare, Public Sector).

## What shipped

### 9-P2 — Role-based access control (skeleton)
- New crate module `xerj-engine/src/rbac.rs`.
- `Privilege` enum: ReadIndex, WriteIndex, AdminIndex, SnapshotCreate,
  SnapshotRestore, SecurityAdmin, AuditRead.
- `Role`: name + privileges + index pattern allow list ("*" or
  prefix glob like "logs-*").
- `RoleStore` seeded with default roles: admin, write, read,
  read_only_index, snapshot_admin, auditor.
- 4 unit tests covering defaults, admin-allows-all, deny-by-default,
  glob matching.
- API surface (no auth-middleware enforcement yet — that lands in
  v0.9.1):
  - `GET /_security/roles`
  - `GET /_security/role/{name}`
  - `PUT /_security/role/{name}`
  - `DELETE /_security/role/{name}`

### 9-P4 — Tamper-evident audit log
- New crate module `xerj-engine/src/audit.rs`.
- Bounded ring (default 4096 entries, ~1 MB at capacity).
- Each entry: seq, at_ms, op, subject, resource, outcome, note,
  SHA-256 hash.
- Hash chain over previous entry — `verify()` walks the chain and
  returns Err((seq, expected, actual)) at the first break.
- 3 unit tests: fresh chain verifies, tampering detected, ring rotates.
- Wired into ES-compat search hot path: every search appends an audit
  entry.
- API:
  - `GET /_audit/_search` — snapshot of all entries
  - `GET /_audit/_verify` — walk the hash chain (200 if intact, 409
    with break details if tampered).

### Bonus quality wins inherited from earlier in the cycle
- v0.8 8-P4 probes still active.
- v0.8 8-P6 slow query log still active and now feeds the audit log
  too.
- v0.7 perf fixes (Zstd-3 flush, 30s back-pressure wait) — production
  ingest path delivers 5.97 M docs/s burst / 50 K docs/s sustained.
- v0.7 durability fix (orphan recovery on open) — 100 % data
  durability across restart on a 5 M-doc test.

## Deferred to v0.9.1

The following 9-P / 9-V items have design + scaffolding in place but
need additional implementation work:

- 9-P1 in-process TLS (rustls integration)
- 9-P2 final: User entity + auth middleware enforcement
- 9-P3 API key hashing + rotation
- 9-P5 GDPR sync-erasure (endpoint stub)
- 9-P6 engine-level encryption at rest
- 9-P7 retention enforcement background task
- 9-V1 OIDC SSO
- 9-V2 SAML federation
- 9-V3 SOC 2 Type I in flight (external auditor engagement)
- 9-V4 ISO 27001 gap assessment
- 9-V5 Customer-facing security pack

## Cumulative state

- ES YAML conformance: 1305 / 1329 = 98.2 %.
- Single binary 19.6 MB; cold start 4 ms.
- 596 MB RSS for a 655 K-doc index; 1.8 GB on disk for 60.9 M docs
  (6.7× compression).
- Helm chart + multi-arch Dockerfile + k8s probes ready (v0.8).
- Audit log + RBAC skeleton + bounded slow-query log (v0.9).
