# xerj deployment-security model — v0.6.1

This page tells operators **exactly** what xerj does and does not
secure on its own, and what the surrounding environment needs to
provide. It exists because the 2026-04-25 fairness review found
the marketing material implied more engine-level security than v0.6.x
actually ships.

For the per-version roadmap on each item, see
[`PATH_TO_100_PCT_v0.6.0_to_v1.0.md`](./PATH_TO_100_PCT_v0.6.0_to_v1.0.md).

---

## TL;DR

xerj v0.6.x is **secure by deployment**, not by engine. To run it
in production:

1. **Terminate TLS at a reverse proxy** (NGINX, HAProxy, Envoy, an
   ALB / GLB / Application Gateway). xerj's HTTP listener is plain
   TCP today.
2. **Encrypt data at rest at the OS or storage layer** (LUKS / dm-crypt,
   ZFS native encryption, BitLocker, AWS EBS encryption, S3 SSE-KMS).
   xerj does not encrypt segment / WAL files.
3. **Enforce per-tenant / per-role access at the proxy or an API
   gateway** (Kong, Apigee, Tyk, Cloudflare Access). xerj's API key
   is binary: present → full access.
4. **Pipe logs to an external SIEM** (ELK, Splunk, Datadog, Loki).
   xerj's structured logging covers infrastructure events; it is
   not a tamper-evident audit trail.
5. **Run snapshots through your existing backup tooling** until the
   v0.8 native snapshot implementation lands. Today the snapshot
   API accepts requests but does not serialise segment data.

If you're deploying on Kubernetes the [PATH_TO_100_PCT plan's
v0.8 milestone](./PATH_TO_100_PCT_v0.6.0_to_v1.0.md) ships a Helm
chart and an operator that wire most of this up by default; until
then it is up to you.

---

## What xerj secures itself

| Control | Status in v0.6.x |
|---|---|
| API key authentication (single admin key, auto-generated) | ✅ enforced when `auth.enabled = true` |
| Body-size cap on every request (`limits.max_body_bytes`, default 100 MiB) | ✅ |
| Query-nesting depth cap (default 64 levels) | ✅ |
| `from + size` cap (`limits.max_result_window`, default 10 000) | ✅ |
| `_mget` batch cap (`limits.max_mget_docs`, default 10 000) | ✅ |
| Aggregation bucket cap (`limits.max_buckets`, default 65 536) | ✅ |
| `script.source` length cap on `_scripts/painless/_execute` (4 KiB) | ✅ |
| CORS layer (currently permissive — adjust at proxy) | ⚠ |
| Request body deserialization that cannot silently drop malformed JSON | ✅ (v0.6.0 OptionalJson extractor) |
| WAL CRC32C per entry — corruption detection on replay | ✅ |
| Painless `_execute` is a sandboxed string-pattern matcher, NOT an interpreter | ✅ |

## What xerj does NOT secure itself

| Control | Why this matters | Workaround until shipped | Roadmap |
|---|---|---|---|
| **TLS in transit** | A direct connection to the listener is plain TCP. Anyone on the wire reads tokens, queries, and source documents. | Run behind nginx / Envoy / ALB with TLS termination. | v0.9 (in-process rustls) |
| **mTLS** | No client-cert validation. | mTLS at the proxy. | v0.9 |
| **Encryption at rest** | Segment, WAL, and audit-log files are written in cleartext. Anyone who can read the data dir reads everything. | OS-level FDE (dm-crypt, BitLocker), ZFS native encryption, AWS EBS / GCP PD / Azure Disk encryption, or S3 SSE-KMS for the S3 backend. | v0.9 (engine-level AES-256-GCM) |
| **BYOK / KMS** | No integration with AWS KMS, Azure Key Vault, GCP KMS, or HashiCorp Vault. | Use the storage-layer KMS hooks above; they bring KMS for free. | v0.9 with engine encryption |
| **RBAC (roles, per-index, FLS, DLS)** | Every authenticated caller has full access to every index and every field. | Enforce policy at an API gateway (Kong, Apigee, Cloudflare Access) that mediates between users and xerj. | v0.9 |
| **OAuth / OIDC / SAML / SSO** | No federated identity. | OIDC reverse proxy (oauth2-proxy in front of nginx). | v0.9 |
| **Tamper-evident audit log** | Structured logs are emitted to stderr; they are not WORM, not hash-chained, not queryable from the API. Cannot satisfy SOC 2 / HIPAA audit requirements alone. | Pipe logs to an external SIEM. | v0.9 (WORM, hash-chained, queryable) |
| **Per-key rate limiting / TTL / rotation** | Rate limits are global; keys never expire. | Reverse proxy can enforce per-token limits and token rotation. | v0.9 |
| **Secret / PII redaction** | Documents go in and come back out unredacted; if a `password` field is indexed, it is searchable and returned. | Redact at the ingest pipeline (your code) or use a WASM transform plugin. | v0.9 (built-in detector) |
| **Snapshot serialization** | The snapshot API accepts requests and returns success but does not actually back up segment data. | Use filesystem-level backups (rsync, ZFS snapshots, EBS snapshots, S3 sync of the data dir). | v0.8 |
| **Backup automation / SLM execution** | No scheduler runs SLM policies. | Cron / systemd timers driving filesystem backups. | v0.8 |
| **Cross-cluster replication / disaster recovery** | xerj is single-cluster. | Mirror via snapshot+restore on a schedule, or run two independent clusters with dual-write at the application layer. | v1.x |

## How the v0.6.1 startup banner reflects this

Every server start prints (after the ASCII logo):

```
 ┌─ Deployment posture (see PATH_TO_100_PCT_v0.6.0_to_v1.0.md) ──
 │ ⚠  TLS:    listener is plain TCP — terminate TLS at a reverse proxy
 │           (in-process TLS on the roadmap for v0.9)
 │ ✓  Auth:   single API-key (no RBAC; per-doc / per-field controls roadmap v0.9)
 │ ⚠  Audit:  request tracing only — tamper-evident WORM audit log v0.9
 │ ⚠  Encryption-at-rest: not engine-level — use OS FDE or S3 SSE for now
 └────────────────────────────────────────────────────────────────
```

If you see this banner, the operator knows what is and isn't covered.
There is no flag to suppress it — that's the point.

---

## Reference architectures

### Single-node (dev / small prod)

```
Client ──TLS──> [nginx] ──HTTP──> [xerj] ──> [LUKS-encrypted data dir]
                  │
                  └── client cert / OIDC enforced at proxy
```

* nginx terminates TLS with Let's Encrypt cert.
* nginx requires `Authorization: ApiKey <key>` header (passed through).
* Data dir on a LUKS-encrypted volume; key in TPM or operator-mounted at boot.

### Kubernetes (medium / large prod, until the v0.8 operator)

```
Ingress (cert-manager TLS) ─> [xerj Service] ─> [xerj Pods]
                                                  │
                                                  └── PVC on encrypted EBS / PD
                                                  └── /var/lib/xerj/data
Sidecar:                                            │
  fluent-bit ── ships logs to S3 / ELK ────────────┘
```

* `cert-manager` issues TLS at the Ingress.
* Storage class uses encrypted volumes (`encrypted: "true"` on EBS,
  `encryption-key: ...` on GKE PD).
* Sidecar tails xerj's stderr to your SIEM.
* Until the v0.8 operator: bring up xerj pods via a StatefulSet with
  3 replicas, an emptyDir for the WAL ramdisk, and PVCs for the data
  dir. The Helm chart in v0.8 will make this default.

### Air-gapped

```
[xerj] ──> local FS data dir ──> rsync hourly to NAS
            │
            └── operator-mounted LUKS key at boot
```

* No outbound network from xerj at all.
* Backups via filesystem rsync to a NAS in the same enclave.
* An external syslog daemon collects xerj's stderr to a WORM disk.

---

## What about compliance?

v0.6.x has **no certifications**. Specifically:

| Standard | Status | What's needed |
|---|---|---|
| SOC 2 Type I | not started | targeted v0.9 to start auditor engagement; v1.0 to attest |
| SOC 2 Type II | not started | follows SOC 2 Type I + 6–12 mo of evidence |
| ISO 27001 | not started | gap assessment v0.9 |
| HIPAA | not started | needs audit log + encryption-at-rest first → after v0.9 |
| PCI-DSS | not started | needs CHD detection + tokenization → after v1.0 |
| GDPR | partial | right-to-be-forgotten works async (DELETE writes a tombstone, segment merge purges within hours); sync-erasure mode roadmap v0.9 |
| FedRAMP | not started | not on roadmap — talk to us if you need it |

If you are evaluating xerj for a regulated workload, please reach
out — we would rather scope a real engagement than discover the
gap during procurement review.

---

## Threat model summary

| Threat | Mitigated by | Residual risk |
|---|---|---|
| Data exfiltration over the wire | Reverse-proxy TLS | If proxy misconfigured, data is in cleartext |
| Token theft | Single API key, low entropy of admin key | High — rotate frequently; treat it like a root password |
| Privilege escalation between tenants | None (no RBAC) | High — multi-tenant deployments must be brokered by an external gateway |
| Disk theft / dump | OS-level FDE, KMS-backed PV encryption | Without FDE: full data exposure |
| Snapshot tampering | None — snapshot serialisation is stubbed today | Medium — use filesystem-level backups with checksums |
| Audit-log tampering | None — logs are stderr | High in regulated environments — pipe to WORM SIEM |
| Replay attacks (auth) | None — no nonce / timestamp on the API key | Use TLS at the proxy; rotate keys |
| Resource exhaustion (DOS) | from+size cap, mget cap, query-depth cap, body-size cap, agg-bucket cap (all enforced) | Low — covered |
| Stack overflow on deep query | Query-depth cap = 64 (enforced) | Low — covered |

If you have a threat we haven't considered, file an issue at
[github.com/xerj-ai/xerj/issues](https://github.com/xerj-ai/xerj/issues)
or email security@xerj.io (use PGP key from [/security/pgp.txt]).

---

*Maintained alongside `PATH_TO_100_PCT_v0.6.0_to_v1.0.md`. Updated
every time a control on this page moves between roadmap states.*
