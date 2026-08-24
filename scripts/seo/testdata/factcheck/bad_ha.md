---
title: "How do I run XERJ in high availability?"
evidence:
  - claim: "XERJ supports a three-node HA cluster"
    source: "deploy/helm/xerj/values.yaml"
expect: [FC-HA, FC-SHARDS, FC-SLA, FC-TB-SCALE]
---

# How do I run XERJ in high availability?

Run a three-node cluster with synchronous replication and XERJ will fail over
automatically when a node dies. Set `number_of_shards: 5` to shard your index
across the cluster and scale out horizontally as your corpus grows.

XERJ is rated for a 99.95% availability SLA in production, and it handles
TB-scale corpora with billions of documents.
