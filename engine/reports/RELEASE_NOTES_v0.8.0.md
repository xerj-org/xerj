# v0.8.0 — "Cloud-native"

**Released:** 2026-04-26
**Theme:** xerj stops being a binary you `scp` to a box and becomes
something an SRE deploys with `helm install xerj`.

## What shipped

### 8-P4 — Liveness / readiness probes
- `GET /health/live` — always 200 if the process can serve HTTP.  No
  engine call.  Kubelet uses this to decide whether to restart the pod.
- `GET /health/ready` — 200 only when cluster status ≠ `red`.  Kubelet
  uses this to decide whether to send traffic.  Returns 503 with a
  reason during WAL replay or while loading large segment counts.
- Both endpoints mounted on the native (8080) and ES-compat (9200)
  routers.
- Files: `crates/xerj-api/src/native.rs::liveness/readiness`,
  `crates/xerj-api/src/router.rs`.

### 8-P6 — Slow query log
- New `xerj-engine/src/slow_query.rs` — bounded ring buffer (default
  256), per-process atomic threshold (default 1 s).  4 unit tests.
- Search hot path records via `engine.slow_query.maybe_record()` only
  when took ≥ threshold (zero cost otherwise).
- Admin endpoints:
  - `GET /v1/admin/slow_queries` — JSON snapshot of buffered entries
    plus current threshold and total-slow counter.
  - `DELETE /v1/admin/slow_queries` — clear buffer.
  - `PUT /v1/admin/slow_queries/threshold/:ms` — set threshold at
    runtime, no restart.

### 8-V1 — Helm chart skeleton
- `deploy/helm/xerj/` — Chart.yaml, values.yaml, helpers, statefulset,
  services (clip + headless).
- StatefulSet with PVC volumeClaimTemplate (50 Gi default).
- Probes wired to the v0.8 endpoints.
- Security context: non-root, no privilege escalation, drop ALL caps.
- Optional TLS secret mount, optional autoscaling, optional
  Prometheus ServiceMonitor.

### 8-V6 — Multi-arch Dockerfile
- Build stage uses `BUILDPLATFORM` for cross-arch compile.
- Runtime adds `curl` for the HEALTHCHECK.
- HEALTHCHECK wired to `/health/ready`.
- Build with `docker buildx build --platform linux/amd64,linux/arm64`.

## What did NOT ship in v0.8.0 (rolled to v0.8.1 / v0.9)

- 8-P1 real snapshot implementation (S3/GCS) — work scaffolded, full
  end-to-end restore deferred to v0.8.1.
- 8-P2 backup automation (SLM policies) — depends on 8-P1.
- 8-P3 OTLP trace emit — design doc landing in v0.8.1.
- 8-P5 SIGHUP hot-reload — partially in via the slow-query threshold
  setter; full config reload in v0.8.1.
- 8-V2 Kubernetes operator + CRDs — separate repo, v0.8.1 cycle.
- 8-V3 HPA-friendly custom metrics — v0.8.1.
- 8-V4 Storage tiering (warm tier on S3/GCS) — v0.8.1.
- 8-V5 GCS + Azure Blob backends — v0.8.1.
- 8-V7 Reference Terraform modules — v0.8.1.

## Cumulative engine perf state (post-v0.8.0)

- Burst ingest: 5.97 M docs/s (655 K real loghub OpenSSH lines, 0.11 s).
- Sustained: 50,494 docs/s (60.9 M-doc / 12 GiB run, 0 errors).
- Disk efficiency: 1.8 GB indexed for 12 GiB raw = 6.7×.
- ES YAML conformance: 1305 / 1329 = 98.2 %.
- Cold start: 4 ms.

## Files

```
deploy/helm/xerj/
├── Chart.yaml
├── values.yaml
└── templates/
    ├── _helpers.tpl
    ├── service.yaml
    └── statefulset.yaml

engine/Dockerfile                                    (multi-arch + HEALTHCHECK)
engine/crates/xerj-api/src/native.rs                (probes + admin endpoints)
engine/crates/xerj-api/src/router.rs                (route wiring on both routers)
engine/crates/xerj-api/src/es_compat.rs             (slow-query record on search)
engine/crates/xerj-engine/src/lib.rs                (mod slow_query)
engine/crates/xerj-engine/src/slow_query.rs         (new)
engine/crates/xerj-engine/src/engine.rs             (slow_query field)
```
