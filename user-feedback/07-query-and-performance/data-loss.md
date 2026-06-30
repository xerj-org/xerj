# Data Loss and Corruption Risks

## Severity: CRITICAL | Frequency: LOW (but catastrophic when it occurs)

---

## Core Complaints

### Document Loss During GC Pauses
- VM pause (GC, high IO) can cause loss of acknowledged inserts
- 10% loss rate documented in one failure pattern (GitHub #10933, #10426)
- Acknowledged writes lost = silent data loss (worst kind)

### Filesystem Bug Exposure
- Elasticsearch generates challenging I/O workloads that expose:
  - Filesystem bugs
  - Kernel bugs
  - Firmware bugs in drives/RAID controllers
  - Faulty fsync configuration
- Corruption often detected during merges, shard movements, snapshots
- Sometimes after data has been silently corrupted for unknown duration

### Logstash Pipeline Data Loss
- Logstash drops logs when overloaded
- If it crashes or buffer overflows, all queued events lost
- Single point of failure in the logging pipeline

### Split-Brain Data Divergence
- Network partition → two masters → both accept writes → permanent data divergence
- Reconciliation after healing can cause permanent data loss

### Slow Snapshot Restore
- 5GB restores taking 48 hours documented (especially GCS + FIPS)
- Moderate datasets: 2-3 hours for restore
- Significant delays before restore even begins (master reads all snapshot metadata)

---

## XERJ.ai Response
- No JVM = no GC pause data loss
- WAL with fsync + CRC32 checksums per block
- Single-node = no split-brain
- Segment file integrity verified via checksums (like ES/Lucene CodecUtil)
- Fuzz testing on compression codec to catch corruption

## Sources
- GitHub Issues: #10933, #10426
- Elastic Blog: Six Ways to Crash
- Elastic Docs: Corruption Troubleshooting
