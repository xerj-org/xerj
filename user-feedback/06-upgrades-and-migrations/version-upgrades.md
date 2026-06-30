# Version Upgrade and Migration Pain

## Severity: CRITICAL | Frequency: HIGH

---

## Core Complaints

### No Skip-Version Upgrades
- Must upgrade through each major version sequentially: 5.x → 6.x → 7.x → 8.x
- Cannot skip: must be on 7.17.x before any 8.x upgrade
- No path from 7.15 directly to 8.x
- Each major upgrade is a project in itself

### Breaking Changes Across Versions
- 7.x → 8.x: security forced on by default, mapping types removed, transport client eliminated, Java 17 minimum
- Deprecated `_type` field removal breaking existing tests/queries
- Null handling changes in alias creation
- Date range query behavior changes
- Default setting changes breaking wildcard operations
- Nested field sorting requiring undocumented specification
- camelCase date format deprecation
- Translog retention setting changes
- Legacy template format changes

### Rolling Upgrade Risks
- Primary shards on new-version nodes cannot have replicas on old-version nodes
- Cluster stays Yellow until enough nodes upgraded
- Must manually disable shard allocation before stopping each node
- Must re-enable after restart to avoid unnecessary I/O from premature rebalancing
- Each upgrading node reduces cluster capacity, increasing load on remaining nodes
- Multiple nodes upgrading simultaneously cause cluster state conflicts

### Reindexing Requirements
- Many upgrade paths require full reindexing of all data
- For large datasets (hundreds of TB): takes days or weeks
- Zero-downtime reindexing requires alias-based dual-write pattern
- Adds significant application-level complexity

### Cannot Downgrade
- After upgrade, data directory layout changes prevent downgrade
- Indices with deprecated settings in 7.x can prevent 8.x from starting
- At that point, you cannot go back

### Real-World Horror Stories
- **Zalando:** ES 7→8 migration revealed: deprecated `_type` removal, null handling changes, date range behavior changes, default setting breaks, undocumented sorting requirements
- **GitHub:** Seven-year struggle with Elasticsearch upgrades. Administrators had to follow maintenance steps in exactly the right order or search indexes became damaged. Took years until CCR solved it.

---

## User Quotes

> "Each major version upgrade requires you to update your indexes, which is not easy"
> -- M. Serhat D., Senior Software Engineer, E-Learning (Capterra)

> "The elasticsearch version management is bad because next version could have critical changes from previous"
> -- Software Advice reviewer

> "Features removed between releases with no backward compatibility considerations from the customers' perspective"
> -- PeerSpot reviewer

> "Upgrading from older versions is not simple"
> -- Senior Software Developer, Hospitality (Capterra)

---

## XERJ.ai Response
- **Single binary.** Upgrade = replace binary + restart.
- Backward-compatible segment format with version field in header
- No cluster coordination during upgrade (single-node M1)
- No shard allocation dance
- No JVM version coupling
- Segment format designed for forward compatibility (section-based with offset table)

## Sources
- Zalando Engineering: Migrating from ES 7 to 8 Learnings
- GitHub Blog: Rebuilt Search Architecture for HA
- Opster: Upgrade Guide
- codecentric: Zero Downtime Reindexing Problems
- Elastic Docs: Upgrade
