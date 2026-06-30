# Developer Experience (DX) Pain Points

## Severity: HIGH | Frequency: HIGH

---

## Local Development

### Heavy Resource Footprint
- Managing ES locally "feels like taming a beast"
- Requires JVM tuning, significant RAM, careful configuration even for single-node
- Alternatives let you get "up and running in minutes, not hours"

### Slow CI/CD
- ES containers slow to start in CI
- Must use tmpfs for data/logs to be practical
- Without tricks, 3 integration tests take 25 seconds
- Startup time: 30-60s (vs <5s for Rust-based alternatives)

### Mocking Is Nearly Worthless
- "Mocking databases tends to produce tests that are brittle and offer very little value"
- Changes to DSL queries, config, mappings, or ES version all bypass mock tests
- Real integration tests "become systems integration tests -- scope too big"

### Environment Parity
- "Does every developer have access to the same ES instance as I do? What about CI?"
- Index name collisions between developers

---

## Schema Migration

### No Migration Tooling
- "Elasticsearch doesn't have a robust migration tool"
- "I can't find anything similar for ES" to Flyway/Liquibase
- Current state "feels so 20 years ago"
- Gong engineering built custom migration framework from scratch

### Immutable Mappings Force Full Reindex
- "Once a field has been assigned a type it is immutable"
- Any change = create new index + copy all data via reindex
- No ALTER TABLE equivalent

### Manual Repetition Across Environments
- "I still need to do that create-new-index-then-load-data cycle in each environment"
- "I can't just ask each developer to run a particular script after they pull a commit"

### Zero-Downtime Reindex Is a Minefield
- "Works well only if you can stop indexing temporarily -- not really zero downtime"
- Unsolved: "How do you handle updates/deletes during reindexing?"
- Race conditions can produce "records that should not exist" in new index

---

## Data Modeling Traps

### Denormalization Update Cascades
- If user name changes, "you need to update_by_query all posts -- potentially a million+ documents"
- Changes are "common, far reaching, and concurrent"

### Nested Fields Are Performance Traps
- "Require expensive join operations, increased query time, higher memory consumption"
- "Bloat your index size as each nested object stored separately"
- Updating one nested field "forces reindex of entire document including root and all other nested objects"
- Standard aggregations silently return wrong results unless wrapped in nested aggregation

### update_by_query Is Painfully Slow
- Internally "scan and scroll" holding snapshots
- Can take hours on large indices (one case: 6 hours → 34 mins after aggressive optimization)

---

## Search Relevance Tuning

### Tedious and Unscientific
- "Tuning relevance is extremely important but tedious"
- "Not possible to completely exhaust every possible search scientifically"
- Only "15% of companies dedicate resources to search optimization"

### function_score Bugs
- Known bug: "boost on function_score not applied when only one function present" (GitHub #35123)
- Interplay of boost_mode, score_mode, weight creates confusion
- Default behavior (multiply) is often unexpected

### Synonym Configuration Fragile
- Multi-word synonyms silently break with basic `synonym` filter (must use `synonym_graph`)
- Invalid rules can leave index "in partially created and inoperable state"

### Analyzer Debugging Is a Black Box
- "You can't pry into each step to see what's happening"
- _analyze API shows final output but not intermediate steps
- Third-party tools (Elyzer) needed for step-by-step debugging

---

## Observability of ES Itself

### Profile API Adds Massive Overhead
- "Introduces significant overhead; should not be used in production requests"
- Output "directly exposes Lucene class names and concepts"
- Requires "fairly advanced knowledge of Lucene" to interpret
- Does not measure network latency, fetch phase, or queue time

### Slow Logs Off by Default
- "Disabled in Elasticsearch by default"
- Must be manually configured per-index
- Capture only shard-level processing, not coordinator or network

### Cryptic Error Messages
- "Transport errors are generic"
- "Misplaced bracket or incorrect path often breaks nested grouping without clear error messages"
- Failures can be "anything from missing shards, conflicting settings, network failures"

---

## DX Comparison

### Typesense
- "Developer Happiness with clean well-documented API, clear semantics, smart defaults -- just works out of box"

### Meilisearch
- "Full-text search running in under five minutes with no config files or schema definitions"

### Algolia
- "Working search in literally minutes. RESTful API is intuitive, defaults sensible, typo tolerance works out-of-box"

### Elasticsearch
- "Large piece of software that takes non-trivial effort to setup, administer, scale and fine-tune"
- "Also runs on the JVM, which by itself can be quite an effort to tune"

---

## XERJ.ai Response
- Single binary, <5s startup: practical for local dev and CI
- Schema defined in TOML: explicit, version-controlled, no dynamic mapping surprises
- No reindexing for most schema changes (segment versioning)
- REST API designed for simplicity (10 endpoints, intuitive JSON)
- Clear error messages with suggested fixes
- No JVM to tune, no Lucene internals to understand
- Prometheus metrics built in (no separate profiling tools needed)

## Sources
- DEV Community: Mocking Elasticsearch, How I Failed with ES
- Gong Engineering Blog: Schema Evolution at Scale
- Elastic Forum: Schema Migrations discussion
- BigData Boutique: Nested Fields
- OpenSourceConnections: Elyzer
- Typesense, Meilisearch, Algolia comparison pages
- Opster: Common Errors, UpdateByQuery
- GitHub: #35123
