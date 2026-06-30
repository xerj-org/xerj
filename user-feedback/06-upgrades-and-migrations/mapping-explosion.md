# Mapping Explosion and Schema Problems

## Severity: HIGH | Frequency: MODERATE

---

## Core Complaints

### Dynamic Mapping Explosion
- Dynamic mapping auto-creates a field for every unique key in incoming data
- One customer payload with 500 unique keys → 500+ new mappings
- Default limit: 1,000 fields per index (raising it masks the problem)
- A test indexing 30,000 docs with unique field names consumed disproportionate heap (far exceeding 7MB of actual data)

### Cluster State Bloat
- Mappings stored in cluster state, replicated to ALL nodes on every change
- Hundreds of indices with thousands of fields each → hundreds of MB of cluster state
- Slows master operations, causes update timeouts (30-second default window)
- Continuous cluster state broadcasts paralyze the cluster

### Immutable Mappings
- Once a field's type is set, CANNOT be changed without full reindexing
- `text` cannot become `keyword` or `integer` without creating new index + reindex all data
- Bad initial mapping contaminates everything built on top
- Schema evolution is painful and error-prone

### Auto-Detection Inaccuracies
- Elasticsearch's automatic type detection frequently chooses wrong field types
- Causes indexing errors downstream
- Must manually define mappings for reliable operation, defeating purpose of "dynamic"

---

## XERJ.ai Response
- Schema defined explicitly in TOML (no surprise dynamic mappings)
- Field types chosen deliberately at index creation
- No cluster state broadcast (single-node M1)
- Schema changes handled via index versioning, not in-place mutation
- AI-native field types (`chunk`, `embedding`, `memory`) defined from start

## Sources
- Elastic Blog: 3 Ways to Prevent Mapping Explosion
- Elastic Blog: Six Ways to Crash Elasticsearch
- GitHub Issue #23687: Mapping explosion
