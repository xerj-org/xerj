# Multi-Tenancy Problems

## Severity: MODERATE | Frequency: MODERATE

---

## Core Complaints

### Noisy Neighbor Problem
- Shared indices: one tenant's heavy queries degrade performance for all
- Cache invalidation increases as queries target different documents
- No built-in resource quotas per tenant

### Index-per-Tenant Overhead
- Separate indices per tenant provides isolation but causes shard explosion
- Thousands of tenants = thousands of indices = hundreds of thousands of shards
- Cluster state bloat makes master operations slow

### No Good Design Pattern
- Index-per-tenant: shard sprawl
- Shared index with routing: limits schema flexibility, complicates access control
- Both approaches have severe trade-offs at scale
- Field cardinality grows across all tenants in shared indices, slowing aggregations

---

## XERJ.ai Response
- M1 is single-tenant by design (self-hosted/VPC)
- No multi-tenant complexity to manage
- Per-index isolation without shard overhead
- Multi-tenant design considered for M2+ with proper resource isolation

## Sources
- Opster: Noisy Neighbor Issues
- BigData Boutique: Multi-Tenancy with Elasticsearch
