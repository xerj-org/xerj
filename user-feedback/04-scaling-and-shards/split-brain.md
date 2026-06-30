# Split-Brain and Cluster Coordination

## Severity: CRITICAL | Frequency: MODERATE (improved in v7+)

---

## Core Complaints

### Split-Brain Scenarios
- Network partitions isolate node groups, each electing its own master
- Both sub-clusters accept writes independently
- Causes permanent data divergence and potential data loss on reconnection
- "Two conflicting versions of your data" when network heals

### Two-Node Clusters
- Inherently unsafe: neither node can form quorum alone
- If one node fails, remaining node blocks all writes
- Pre-7.x required manual `minimum_master_nodes = (N/2)+1` -- commonly misconfigured
- Post-7.x: auto-quorum improved but partition risks remain

### Master as Single Point of Failure
- Only master handles index creation, shard movement, cluster state updates
- Processes changes serially, waits for ACKs
- Becomes bottleneck when cluster state is large and frequently updated
- Example: mapping explosion can paralyze master operations

### Non-Linear Degradation
- Performance scales ~linearly under capacity
- Degrades exponentially once exceeded
- No graceful degradation -- cluster "falls off a cliff"

---

## XERJ.ai Response
- **No cluster coordination in M1. No split-brain possible.**
- Single-node architecture: one process, one binary
- No master election, no quorum, no network partition risk
- M2 clustering designed to avoid ES's pitfalls

## Sources
- Opster: Split Brain Guide
- BigData Boutique: Avoiding Split Brain
- GitHub Issue #81626
- Moesif: Debug Unresponsive Cluster
