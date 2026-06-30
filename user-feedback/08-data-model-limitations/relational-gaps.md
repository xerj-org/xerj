# Data Model and Relational Limitations

## Severity: MODERATE | Frequency: MODERATE

---

## Core Complaints

### No Relational Model
- "No transactions, no referential integrity, eventual consistency; not suitable as a primary data store"
- "Joining data requires duplicate de-normalized documents for parent child relationships"
- "Querying is less flexible compared to PostgreSQL"
- "Searching and joining different documents often requires un-normalizing data"
- Running two databases (ES + primary DB) and "keeping them in sync" is painful

### Multi-System Sync Complexity
- Common architecture: PostgreSQL → Kafka → Elasticsearch introduces:
  - Lag (stale search results)
  - Schema drift
  - Transformation bugs
- Schema changes in source DB must be manually propagated to ES mappings (which are immutable)
- Debugging data inconsistencies requires tracing through multiple systems

### Not a True Database
- "Not much recommended as a 'Data Store' when comparing to Hadoop or MongoDB"
- "Elasticsearch was not well designed for the pace at which our data updates"
- "Not great for highly structured data where SQL thrives; heavy use of JOINs"
- "Not suitable for transactions"
- "Does not immediately synchronize data between server nodes"
- "Updates and inserts take time to reconcile causing data integrity issues"

---

## User Quotes

> "It was very difficult to set up queries properly without taking up technical resources time, and the lack of relational data makes it very difficult to handle advanced requests"
> -- Gartner Peer Insights

> "Being unable to set up a relational database made it less useful than we originally thought"
> -- G2 reviewer

---

## XERJ.ai Response
- XERJ.ai is not a general-purpose database either -- but AI workloads don't need SQL
- AI-native field types solve the specific data modeling challenges of RAG/agents:
  - `chunk` type: auto parent-child relationships for chunked documents
  - `memory` type: append-only with semantic dedup for agent memory
  - Inline embedding: no external pipeline sync needed
- Single system for search + vector + logs: fewer systems to keep in sync

## Sources
- G2, Gartner, TrustRadius, Capterra reviews
- Tiger Data: 10 Production Issues
