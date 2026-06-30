# v1.0.0-rc.1 — Release Candidate

**Released:** 2026-04-26
**Theme:** First public release candidate.  Cumulative of every fix
and feature shipped during the v0.5.x → v1.0 cycle.

## Cumulative engine state

### Performance (verified 2026-04-26 against real loghub OpenSSH data)

| Workload | Result |
|---|---:|
| 1 M docs / 8 workers — burst (in-memtable, WAL-durable) | **3.98 M docs/s** |
| 655 K real loghub lines / 8 workers — burst | **5.97 M docs/s** |
| 5 M docs / 8 workers — sustained, segment-durable | **1.43 M docs/s** |
| 60.9 M docs / 12 GiB / 8 workers — sustained | **50,494 docs/s** |
| Cold start | **4 ms** |
| Disk efficiency on 60.9 M repetitive log corpus | **6.7×** smaller than raw |
| RSS for 655 K-doc index, server idle | **596 MB** |
| Single binary size | **19.6 MB** |

### Correctness

- ES 8.13 YAML conformance: **1305 / 1329 = 98.2 %**.
- Data durability across restart on a 5 M-doc test: **100 %**
  (ingest reported 5,000,000 sent; `_count` after restart returned
  5,000,000).
- Orphan-segment recovery on open closes the v0.7-era 3 % silent
  data-loss window from concurrent flush + process exit.

### Surface area

#### Wire compatibility
- ES wire protocol on `:9200`: 38 query types, all aggregations,
  bulk, scroll, index templates, aliases, kNN, hybrid, semantic.
- Native REST on `:8080` for first-class xerj idioms (search-
  discover, dashboard summary, OTLP ingest, syslog ingest).
- gRPC on `:8081` (placeholder for v1.x).

#### Bundled UI
- Xerj Console embedded SPA at `http://<host>:9200/_xerj-console/`.
- 36 static files compiled into the binary at build time.
- Zero external dependencies.

#### Cloud-native (v0.8)
- `/health/live` + `/health/ready` k8s probes on both ports.
- Slow query log + admin endpoints (`GET/DELETE
  /v1/admin/slow_queries`, `PUT
  /v1/admin/slow_queries/threshold/:ms`).
- Helm chart at `deploy/helm/xerj/` (StatefulSet + PVC + probes
  + headless service + non-root security context).
- Multi-arch Dockerfile with HEALTHCHECK wired to readiness.

#### Security & compliance (v0.9)
- Tamper-evident audit log with SHA-256 hash chain.
  `GET /_audit/_search`, `GET /_audit/_verify`.
- RBAC role store with default roles seeded (admin, write, read,
  read_only_index, snapshot_admin, auditor).
  `GET /_security/roles`, `GET/PUT/DELETE /_security/role/:name`.

## Compared with Elasticsearch (real measurements, same 32-core
host, identical wire payloads)

| Dimension | ES 8.13 | Xerj | Delta |
|---|---:|---:|---:|
| Cold start | 6.05 s | 4 ms | **863× faster** |
| RSS (1 K-vector index) | 2,857 MB | 727 MB | **3.9× less** |
| Index creation | 93.9 ms | 2.4 ms | **39× faster** |
| Term query p50 | 0.79 ms | 0.31 ms | **2.5×** |
| Match query p50 | 1.28 ms | 0.33 ms | **3.9×** |
| Top-K sort p50 | 4.28 ms | 0.34 ms | **12.6×** |
| Bulk 1 K vector throughput | 10,219 docs/s | 22,974 docs/s | **2.25×** |

## Known limitations (target v1.0 final or v1.x)

| Item | Status |
|---|---|
| 9-P1 in-process TLS (rustls integration) | scaffolded; v1.0 final |
| 9-P3 API key hashing + rotation | v1.0 final |
| 9-P5 GDPR sync-erasure | v1.0 final |
| 9-P6 encryption at rest | v1.0 final |
| 8-P1 real S3/GCS snapshot impl | v1.0 final |
| 8-V2 k8s operator + CRDs | v1.x |
| Multi-node clustering | v1.x |
| Multi-region failover | v1.x |
| Terms-agg / date_histogram correctness on multi-segment indexes | v1.0-rc.2 (regression filed during demo capture) |

## Migration

For users on v0.7.x or v0.8.x:
- No data-format breaking changes.
- Restart server against the existing data dir; orphan-recovery on
  open will re-attach any pre-fix orphan segments and the count will
  increase to the true durable total.
- Update Helm values to `image.tag: 1.0.0-rc.1`.
- New endpoints (`/_audit/*`, `/_security/role/*`,
  `/v1/admin/slow_queries*`, `/health/live`, `/health/ready`) are
  additive — existing clients are unaffected.

## Tagging policy

- `v1.0.0-rc.1` — this release.
- `release/v1.0` LTS branch will be cut at v1.0.0 final.
- Backports per the policy in `engine/reports/PATH_TO_100_PCT_v0.6.0_to_v1.0.md`
  (P0 / P1 only, no features).
